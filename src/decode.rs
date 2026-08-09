//! Single decode seam for every pixel-decode in imgfind.
//!
//! RAW files (by extension) are decoded via `rawler` — largest embedded preview
//! first, full demosaic as a fallback (see `decode_raw`). All other extensions use
//! the `image` crate. Either way, `decode_image` then applies the file's EXIF
//! orientation tag so the returned image is upright.
//!
//! Because imgfind decodes arbitrary user files with third-party decoders that
//! `panic!` on some malformed input, every public entry point here runs under
//! `guard_decoder_panic`: a decoder panic comes back as an ordinary `Err` naming
//! the file, so one bad image is marked failed instead of killing the run.

use anyhow::{Context, Result, anyhow};
use std::any::Any;
use std::cell::Cell;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Once;

/// Lowercased extensions the `image` crate decodes (imgfind's historical set).
pub const STILL_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "bmp", "tiff", "webp"];

/// Lowercased RAW extensions `rawler` can decode. This is the single, explicit
/// place where RAW format coverage is declared.
pub const RAW_EXTENSIONS: &[&str] = &[
    "nef", "nrw", // Nikon
    "dng", // Adobe / generic
    "orf", // Olympus
    "cr2", "cr3", "crw", // Canon
    "arw", "sr2", "srf", // Sony
    "raf", // Fujifilm
    "rw2", // Panasonic
    "pef", // Pentax
    "srw", // Samsung
    "erf", // Epson
    "mrw", // Minolta
    "raw", "rwl", // Leica / misc
    "iiq", "3fr", "fff", // Phase One / Hasselblad
    "mef", "mos", "kdc", "dcr", // Mamiya / Leaf / Kodak
];

/// True if `ext` (with or without case) is a RAW format we decode via rawler.
pub fn is_raw_extension(ext: &str) -> bool {
    let ext = ext.to_ascii_lowercase();
    RAW_EXTENSIONS.contains(&ext.as_str())
}

/// True if `ext` is any image format the scanner should pick up (still or RAW).
pub fn is_supported_extension(ext: &str) -> bool {
    let ext = ext.to_ascii_lowercase();
    STILL_EXTENSIONS.contains(&ext.as_str()) || RAW_EXTENSIONS.contains(&ext.as_str())
}

thread_local! {
    /// Set while this thread is inside [`guard_decoder_panic`]. Read by the hook
    /// installed by [`install_decoder_panic_hook`] so a contained decoder panic
    /// does not print a panic message + backtrace for every corrupt file.
    static IN_GUARDED_DECODE: Cell<bool> = const { Cell::new(false) };
}

static DECODER_PANIC_HOOK: Once = Once::new();

/// Install a process-wide panic hook that stays quiet for panics raised inside
/// [`guard_decoder_panic`] and delegates everything else to the previous hook.
///
/// Without this, a library holding many corrupt RAWs prints a full panic message
/// per file even though each one is caught and turned into an ordinary error.
/// Contained panics are still visible at `RUST_LOG=debug`, and the resulting
/// `Err` always names the offending path. Genuine bugs elsewhere keep the
/// default hook's output, including backtraces.
fn install_decoder_panic_hook() {
    DECODER_PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // `try_with` because a thread tearing down may have destroyed the
            // thread-local already; treat that as "not contained".
            if IN_GUARDED_DECODE.try_with(Cell::get).unwrap_or(false) {
                tracing::debug!("contained decoder panic: {info}");
            } else {
                previous(info);
            }
        }));
    });
}

/// Best-effort human-readable text from a panic payload (`&str` or `String`).
fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string())
}

/// Run `decode`, converting a panic raised by the underlying image decoder into
/// an ordinary `Err` that names `path`.
///
/// Third-party decoders are not panic-free on malformed input. `rawler`'s ORF
/// path is the known case: `OrfDecoder::decode_compressed` returns a plain
/// `PixU16` (no error channel at all) and calls `panic!("Can't refill bitpump,
/// buffer exhausted")` when the dimensions declared in the file's IFDs demand
/// more compressed data than the strip actually holds — i.e. on any truncated or
/// corrupt ORF. `image` has had similar panics on malformed input.
///
/// imgfind decodes arbitrary user files, so a decoder panic is a property of the
/// *file*, not a bug in imgfind, and must be handled exactly like a decode error:
/// report the path, mark the image failed, keep going. Left uncaught it unwinds
/// through rayon (which re-raises it on the calling thread) and kills the whole
/// `index`/`process` run — and because no failure marker is written, the next run
/// hits the same file and dies again.
fn guard_decoder_panic<T>(path: &Path, decode: impl FnOnce() -> Result<T>) -> Result<T> {
    install_decoder_panic_hook();
    let outer = IN_GUARDED_DECODE.with(|flag| flag.replace(true));
    // AssertUnwindSafe: `decode` only borrows `path` and returns an owned value.
    // A decoder that unwinds leaves no imgfind state observably half-updated —
    // the caller either gets the decoded image or this error, never both.
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(decode));
    IN_GUARDED_DECODE.with(|flag| flag.set(outer));

    outcome.unwrap_or_else(|payload| {
        Err(anyhow!(
            "image decoder panicked on {}: {}",
            path.display(),
            panic_message(&*payload)
        ))
    })
}

/// Read the EXIF Orientation tag (0x0112, primary IFD) as its raw 1–8 value.
/// Best-effort: returns `None` on any read failure or when the tag is absent —
/// never propagates an error, so a missing/broken EXIF block can't fail a decode.
fn read_exif_orientation(path: &Path) -> Option<u8> {
    use exif::{In, Reader, Tag};

    let file = std::fs::File::open(path).ok()?;
    let mut bufreader = std::io::BufReader::new(&file);
    let reader = Reader::new().read_from_container(&mut bufreader).ok()?;
    let field = reader.get_field(Tag::Orientation, In::PRIMARY)?;
    u8::try_from(field.value.get_uint(0)?).ok()
}

/// Apply the file's EXIF orientation to `img` in place (best-effort; no-op if absent).
fn apply_exif_orientation(img: &mut image::DynamicImage, path: &Path) {
    if let Some(orientation) =
        read_exif_orientation(path).and_then(image::metadata::Orientation::from_exif)
    {
        img.apply_orientation(orientation);
    }
}

/// Long-edge pixel floor for a RAW embedded preview to be used as-is for full-screen
/// viewing; below this we demosaic the sensor for full native resolution.
const FULL_RAW_MIN_LONG_EDGE: u32 = 2000;

/// True when an image of these dimensions is large enough to use as-is for full view.
fn preview_meets_full_threshold(width: u32, height: u32) -> bool {
    width.max(height) >= FULL_RAW_MIN_LONG_EDGE
}

/// Decode any supported still or RAW image to a `DynamicImage`, corrected for
/// EXIF orientation (0x0112) so the result is upright.
///
/// A panic inside the underlying decoder is contained and returned as an `Err`
/// naming `path` (see [`guard_decoder_panic`]).
pub fn decode_image(path: &Path) -> Result<image::DynamicImage> {
    guard_decoder_panic(path, || decode_image_inner(path))
}

fn decode_image_inner(path: &Path) -> Result<image::DynamicImage> {
    let is_raw = path
        .extension()
        .and_then(|e| e.to_str())
        .map(is_raw_extension)
        .unwrap_or(false);

    let mut img = if is_raw {
        decode_raw(path)?
    } else {
        image::open(path).with_context(|| format!("decoding image {}", path.display()))?
    };

    apply_exif_orientation(&mut img, path);
    Ok(img)
}

/// Decode an image at full/high resolution for full-screen viewing (the GUI lightbox).
/// Non-RAW: the original (already full-res). RAW: the largest embedded preview if its
/// long edge is >= `FULL_RAW_MIN_LONG_EDGE`, else a full sensor demosaic. EXIF
/// orientation applied. For thumbnails/embeddings use the faster `decode_image` instead.
/// A panic inside the underlying decoder is contained and returned as an `Err`
/// naming `path` (see [`guard_decoder_panic`]).
pub fn decode_full_image(path: &Path) -> Result<image::DynamicImage> {
    guard_decoder_panic(path, || decode_full_image_inner(path))
}

fn decode_full_image_inner(path: &Path) -> Result<image::DynamicImage> {
    let is_raw = path
        .extension()
        .and_then(|e| e.to_str())
        .map(is_raw_extension)
        .unwrap_or(false);

    let mut img = if is_raw {
        decode_raw_full(path)?
    } else {
        image::open(path).with_context(|| format!("decoding image {}", path.display()))?
    };

    apply_exif_orientation(&mut img, path);
    Ok(img)
}

/// Decode a RAW at full resolution: the largest embedded preview when its long edge is
/// `>= FULL_RAW_MIN_LONG_EDGE`, else a full sensor demosaic. If demosaic fails, falls
/// back to the (small) preview rather than erroring.
fn decode_raw_full(path: &Path) -> Result<image::DynamicImage> {
    use rawler::decoders::RawDecodeParams;
    use rawler::imgop::develop::RawDevelop;
    use rawler::rawsource::RawSource;

    let source =
        RawSource::new(path).with_context(|| format!("opening RAW file {}", path.display()))?;
    let decoder = rawler::get_decoder(&source)
        .with_context(|| format!("no RAW decoder for {}", path.display()))?;
    let params = RawDecodeParams::default();

    // Largest embedded preview: full_image preferred, else preview_image.
    let best_preview = match decoder
        .full_image(&source, &params)
        .with_context(|| format!("reading embedded full image for {}", path.display()))?
    {
        Some(img) => Some(img),
        None => decoder
            .preview_image(&source, &params)
            .with_context(|| format!("reading embedded preview for {}", path.display()))?,
    };

    let preview_big_enough = best_preview
        .as_ref()
        .map(|img| preview_meets_full_threshold(img.width(), img.height()))
        .unwrap_or(false);
    if preview_big_enough {
        return Ok(best_preview.expect("checked Some above"));
    }

    // Preview too small or absent: demosaic the sensor to full native resolution.
    let demosaic = (|| -> Result<image::DynamicImage> {
        let raw = decoder
            .raw_image(&source, &params, false)
            .with_context(|| format!("decoding RAW sensor data for {}", path.display()))?;
        let intermediate = RawDevelop::default()
            .develop_intermediate(&raw)
            .with_context(|| format!("developing RAW image {}", path.display()))?;
        intermediate
            .to_dynamic_image()
            .with_context(|| format!("converting developed RAW to image for {}", path.display()))
    })();

    match demosaic {
        Ok(img) => Ok(img),
        // A small preview beats nothing; only error if we have no preview at all.
        Err(e) => best_preview.ok_or(e),
    }
}

/// Decode `path` to a linear-light RGB image for high-fidelity editing.
///
/// RAW files are demosaiced from the **sensor** (ignoring the embedded preview)
/// via a custom `RawDevelop` that omits the final sRGB gamma step, so highlight
/// headroom above the camera-JPEG white point is preserved. Non-RAW files are
/// decoded normally and converted sRGB -> linear. EXIF orientation is applied.
/// A panic inside the underlying decoder is contained and returned as an `Err`
/// naming `path` (see [`guard_decoder_panic`]).
pub fn decode_linear(path: &Path) -> Result<crate::edits::LinearRgb> {
    guard_decoder_panic(path, || decode_linear_inner(path))
}

fn decode_linear_inner(path: &Path) -> Result<crate::edits::LinearRgb> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if is_raw_extension(&ext) {
        decode_raw_linear(path)
    } else {
        let img = decode_image(path)?; // already EXIF-oriented sRGB
        Ok(crate::edits::LinearRgb::from_srgb8(&img.to_rgb8()))
    }
}

fn decode_raw_linear(path: &Path) -> Result<crate::edits::LinearRgb> {
    use rawler::decoders::RawDecodeParams;
    use rawler::imgop::develop::{ProcessingStep, RawDevelop};
    use rawler::rawsource::RawSource;

    let source =
        RawSource::new(path).with_context(|| format!("opening RAW file {}", path.display()))?;
    let decoder = rawler::get_decoder(&source)
        .with_context(|| format!("no RAW decoder for {}", path.display()))?;
    let params = RawDecodeParams::default();
    let raw = decoder
        .raw_image(&source, &params, false)
        .with_context(|| format!("decoding RAW sensor data for {}", path.display()))?;

    // Develop to LINEAR: same steps as the default pipeline minus the final SRgb gamma.
    let develop = RawDevelop {
        steps: vec![
            ProcessingStep::Rescale,
            ProcessingStep::Demosaic,
            ProcessingStep::CropActiveArea,
            ProcessingStep::WhiteBalance,
            ProcessingStep::Calibrate,
            ProcessingStep::CropDefault,
        ],
    };
    let intermediate = develop
        .develop_intermediate(&raw)
        .with_context(|| format!("developing RAW image (linear) {}", path.display()))?;
    let mut dynimg = intermediate.to_dynamic_image().with_context(|| {
        format!(
            "rawler intermediate produced no image for {}",
            path.display()
        )
    })?;
    apply_exif_orientation(&mut dynimg, path);
    Ok(crate::edits::LinearRgb::from_linear_u16(&dynimg.to_rgb16()))
}

/// Decode a RAW file via rawler: try the largest embedded preview (camera-rendered
/// JPEG) first for speed, then fall back to full demosaic of the sensor data.
fn decode_raw(path: &Path) -> Result<image::DynamicImage> {
    use rawler::decoders::RawDecodeParams;
    use rawler::imgop::develop::RawDevelop;
    use rawler::rawsource::RawSource;

    let source =
        RawSource::new(path).with_context(|| format!("opening RAW file {}", path.display()))?;
    let decoder = rawler::get_decoder(&source)
        .with_context(|| format!("no RAW decoder for {}", path.display()))?;
    let params = RawDecodeParams::default();

    // Preview-first: camera-rendered JPEG (fast common path).
    if let Some(img) = decoder
        .full_image(&source, &params)
        .with_context(|| format!("reading embedded full image for {}", path.display()))?
    {
        return Ok(img);
    }
    if let Some(img) = decoder
        .preview_image(&source, &params)
        .with_context(|| format!("reading embedded preview for {}", path.display()))?
    {
        return Ok(img);
    }

    // Fallback: demosaic the sensor data to sRGB.
    let raw = decoder
        .raw_image(&source, &params, false)
        .with_context(|| format!("decoding RAW sensor data for {}", path.display()))?;
    let intermediate = RawDevelop::default()
        .develop_intermediate(&raw)
        .with_context(|| format!("developing RAW image {}", path.display()))?;
    intermediate
        .to_dynamic_image()
        .with_context(|| format!("converting developed RAW to image for {}", path.display()))
}

/// Panic containment at the decode seam.
///
/// These pin the load-bearing behaviour: a decoder that panics must surface as an
/// `Err` naming the file, because `thumbnail::generate_missing_thumbnails_batch`
/// only records a `thumbnail_failures` marker on the `Err` path. A panic that
/// escapes instead unwinds through rayon and aborts the whole `process` run.
#[cfg(test)]
mod panic_guard_tests {
    use super::*;

    /// The exact production failure: `rawler`'s ORF decoder walks off the end of
    /// its bit pump when the dimensions declared in the file demand more
    /// compressed data than the strip holds. Driving the real rawler function
    /// (rather than a synthetic `panic!`) makes this a canary too — if a future
    /// rawler returns an error instead of panicking, this test says so.
    ///
    /// `bps = 14` makes the decoder skip 8 header bytes, so 32 bytes of payload
    /// leaves 24 for a 64x64 image and the pump runs dry in both debug and release
    /// builds — reproducing the shipped panic verbatim. (At `bps = 12` a release
    /// build instead returns silent garbage pixels, so it is no good as a fixture.)
    #[test]
    fn contains_real_rawler_orf_panic() {
        use rawler::buffer::PaddedBuf;
        use rawler::decoders::orf::OrfDecoder;

        let path = Path::new("/library/tmp/_2100062.ORF");
        let err = guard_decoder_panic(path, || {
            let buf = PaddedBuf::new_owned(vec![0u8; 32], 32);
            let _ = OrfDecoder::decode_compressed(&buf, 64, 64, 14, false);
            Ok(())
        })
        .expect_err("a panicking decoder must yield Err, not unwind");

        let msg = format!("{err:#}");
        assert!(
            msg.contains("Can't refill bitpump, buffer exhausted"),
            "error must carry the decoder's own panic message, got: {msg}"
        );
        assert!(
            msg.contains("_2100062.ORF"),
            "error must name the offending file so the run can report it, got: {msg}"
        );
    }

    #[test]
    fn passes_through_success_and_ordinary_errors() {
        let path = Path::new("/some/file.orf");

        assert_eq!(guard_decoder_panic(path, || Ok(7)).unwrap(), 7);

        let err = guard_decoder_panic(path, || -> Result<u8> { Err(anyhow!("plain failure")) })
            .expect_err("ordinary errors must pass through unchanged");
        assert!(format!("{err:#}").contains("plain failure"));
        assert!(
            !format!("{err:#}").contains("panicked"),
            "a non-panic error must not be relabelled as a panic"
        );
    }

    #[test]
    fn reports_both_str_and_string_payloads() {
        let path = Path::new("/f.orf");

        let e = guard_decoder_panic(path, || -> Result<()> { panic!("static payload") })
            .expect_err("panic must be contained");
        assert!(format!("{e:#}").contains("static payload"));

        let e = guard_decoder_panic(path, || -> Result<()> {
            panic!("{}", String::from("owned payload"))
        })
        .expect_err("panic must be contained");
        assert!(format!("{e:#}").contains("owned payload"));
    }

    /// `decode_linear` delegates to `decode_image` for non-RAW files, so the guard
    /// nests. The inner guard must not clear the outer one's containment flag.
    #[test]
    fn nested_guards_restore_the_outer_flag() {
        let path = Path::new("/f.orf");
        let outcome = guard_decoder_panic(path, || {
            guard_decoder_panic(path, || Ok(())).expect("inner guard succeeds");
            assert!(
                IN_GUARDED_DECODE.with(Cell::get),
                "inner guard must restore, not clear, the outer flag"
            );
            Ok(())
        });
        assert!(outcome.is_ok());
        assert!(
            !IN_GUARDED_DECODE.with(Cell::get),
            "flag must be clear once the outermost guard returns"
        );
    }

    /// A contained panic must leave the flag clear, or every later panic on this
    /// thread would be silently swallowed by the hook.
    #[test]
    fn flag_is_cleared_after_a_contained_panic() {
        let path = Path::new("/f.orf");
        let _ = guard_decoder_panic(path, || -> Result<()> { panic!("boom") });
        assert!(!IN_GUARDED_DECODE.with(Cell::get));
    }

    /// The public entry points must stay wired to the guard — that wiring is what
    /// makes every caller (thumbnails, embeddings, TUI, GUI, telnet) panic-safe.
    #[test]
    fn public_entry_points_return_err_on_undecodable_file() {
        let dir = tempfile::tempdir().unwrap();
        // A RAW extension whose content is not a RAW at all: routed to rawler,
        // which declines it. Must be an error, never a panic or a hang.
        let path = dir.path().join("garbage.orf");
        std::fs::write(&path, b"definitely not an ORF").unwrap();

        assert!(decode_image(&path).is_err());
        assert!(decode_full_image(&path).is_err());
        assert!(decode_linear(&path).is_err());
    }
}

#[cfg(test)]
mod linear_decode_tests {
    use super::*;

    #[test]
    fn decode_linear_nonraw_roundtrips_at_zero_ev() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.png");
        let mut img = image::RgbImage::new(4, 4);
        for p in img.pixels_mut() {
            *p = image::Rgb([40, 130, 210]);
        }
        img.save(&path).unwrap();

        let lin = decode_linear(&path).unwrap();
        let out = lin.render(&crate::edits::ImageEdits {
            exposure: 0.0,
            ..crate::edits::ImageEdits::identity()
        });
        let p = out.get_pixel(0, 0);
        for c in 0..3 {
            assert!((p[c] as i32 - img.get_pixel(0, 0)[c] as i32).abs() <= 1);
        }
    }

    #[test]
    fn decode_linear_raw_fixture_nonempty() {
        let p = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sample.dng"
        ));
        let lin = decode_linear(p).expect("decode_linear on sample.dng");
        assert!(lin.0.width() > 0 && lin.0.height() > 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_threshold_uses_long_edge() {
        assert!(preview_meets_full_threshold(2000, 100));
        assert!(preview_meets_full_threshold(100, 2500));
        assert!(preview_meets_full_threshold(3000, 2000));
        assert!(!preview_meets_full_threshold(1999, 1999));
    }

    #[test]
    fn decode_full_image_decodes_raw_fixture() {
        let p = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sample.dng"
        ));
        let img = decode_full_image(p).expect("decode_full_image on sample.dng");
        assert!(img.width() > 0 && img.height() > 0);
    }

    #[test]
    fn decode_full_image_non_raw_matches_image_open() {
        use image::{ImageBuffer, Rgb};
        let buf: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(6, 4, |x, _| Rgb([(x * 30) as u8, 80, 120]));
        let path = std::env::temp_dir().join(format!("imgfind_full_{}.png", std::process::id()));
        buf.save(&path).expect("save");
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _c = Cleanup(path.clone());

        let via_full = decode_full_image(&path).expect("decode_full_image");
        let via_open = image::open(&path).expect("image::open");
        assert_eq!(
            (via_full.width(), via_full.height()),
            (via_open.width(), via_open.height())
        );
        assert_eq!((via_full.width(), via_full.height()), (6, 4)); // full-res, no shrink
    }

    const RAW_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.dng");

    #[test]
    fn decodes_real_raw_to_nonempty_image() {
        let p = std::path::Path::new(RAW_FIXTURE);
        let img = decode_image(p).expect("RAW decode should succeed");
        assert!(img.width() > 0 && img.height() > 0);
    }

    #[test]
    fn recognizes_still_extensions_case_insensitively() {
        assert!(is_supported_extension("jpg"));
        assert!(is_supported_extension("JPG"));
        assert!(is_supported_extension("Png"));
        assert!(!is_raw_extension("jpg"));
    }

    #[test]
    fn recognizes_raw_extensions_case_insensitively() {
        assert!(is_raw_extension("nef"));
        assert!(is_raw_extension("NEF"));
        assert!(is_raw_extension("dng"));
        assert!(is_raw_extension("orf"));
        assert!(is_supported_extension("nef")); // union includes raw
    }

    #[test]
    fn rejects_unknown_extension() {
        assert!(!is_supported_extension("txt"));
        assert!(!is_raw_extension("txt"));
        assert!(!is_supported_extension(""));
    }

    /// Build JPEG bytes for a `w`x`h` image carrying an EXIF Orientation tag.
    /// The `image` JPEG encoder does not emit EXIF, so we splice a minimal big-endian
    /// EXIF APP1 segment (single SHORT Orientation entry) right after the SOI marker.
    fn jpeg_with_orientation(w: u32, h: u32, exif_orientation: u8) -> Vec<u8> {
        use image::{ImageBuffer, ImageFormat, Rgb};
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(w, h, |x, _| Rgb([(x * 40) as u8, 100, 150]));
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut jpeg), ImageFormat::Jpeg)
            .expect("encode jpeg");

        // APP1: marker FFE1, length 0x0022 (34), "Exif\0\0", TIFF(MM, 0x002A, IFD0@8),
        // 1 entry: tag 0x0112 (Orientation), type 0x0003 (SHORT), count 1,
        // value left-justified big-endian SHORT = 00 <orient> 00 00; then next-IFD = 0.
        let app1: Vec<u8> = vec![
            0xFF,
            0xE1,
            0x00,
            0x22,
            b'E',
            b'x',
            b'i',
            b'f',
            0x00,
            0x00,
            0x4D,
            0x4D,
            0x00,
            0x2A,
            0x00,
            0x00,
            0x00,
            0x08,
            0x00,
            0x01,
            0x01,
            0x12,
            0x00,
            0x03,
            0x00,
            0x00,
            0x00,
            0x01,
            0x00,
            exif_orientation,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
        ];

        // Splice the APP1 segment in immediately after the 2-byte SOI (FFD8).
        let mut out = Vec::with_capacity(jpeg.len() + app1.len());
        out.extend_from_slice(&jpeg[..2]);
        out.extend_from_slice(&app1);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    struct TempFile(std::path::PathBuf);
    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn decode_image_applies_exif_orientation() {
        // 4x2 landscape tagged Orientation=6 (Rotate90 CW) -> upright should be 2x4.
        let bytes = jpeg_with_orientation(4, 2, 6);
        let path = std::env::temp_dir().join(format!("imgfind_orient6_{}.jpg", std::process::id()));
        std::fs::write(&path, &bytes).expect("write temp jpeg");
        let _cleanup = TempFile(path.clone());

        let decoded = decode_image(&path).expect("decode_image");
        assert_eq!(
            (decoded.width(), decoded.height()),
            (2, 4),
            "Orientation=6 must rotate the 4x2 image to 2x4"
        );
    }

    #[test]
    fn decode_image_without_orientation_tag_is_unchanged() {
        // Same image, no EXIF tag at all -> stays 4x2.
        use image::{ImageBuffer, ImageFormat, Rgb};
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(4, 2, |x, _| Rgb([(x * 40) as u8, 100, 150]));
        let path =
            std::env::temp_dir().join(format!("imgfind_noorient_{}.jpg", std::process::id()));
        image::DynamicImage::ImageRgb8(img)
            .save_with_format(&path, ImageFormat::Jpeg)
            .expect("save jpeg");
        let _cleanup = TempFile(path.clone());

        let decoded = decode_image(&path).expect("decode_image");
        assert_eq!((decoded.width(), decoded.height()), (4, 2));
    }

    #[test]
    fn decode_image_non_raw_matches_image_open() {
        use image::{ImageBuffer, Rgb};

        // Write a tiny 2×2 RGB PNG to a temp file and verify decode_image
        // produces the same dimensions as image::open directly.
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(2, 2, |x, y| Rgb([(x * 64) as u8, (y * 64) as u8, 128u8]));

        // Unique per-process name so concurrent test runners sharing /tmp
        // (cargo nextest, CI matrices) don't race on the same file.
        let tmp_path =
            std::env::temp_dir().join(format!("imgfind_decode_test_{}.png", std::process::id()));
        img.save(&tmp_path).expect("failed to save test PNG");

        // Ensure cleanup even on panic.
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = Cleanup(tmp_path.clone());

        let via_seam = decode_image(&tmp_path).expect("decode_image failed on PNG");
        let via_image = image::open(&tmp_path).expect("image::open failed on PNG");

        assert_eq!(via_seam.width(), via_image.width());
        assert_eq!(via_seam.height(), via_image.height());
        // Both should be 2×2.
        assert_eq!(via_seam.width(), 2);
        assert_eq!(via_seam.height(), 2);
    }
}
