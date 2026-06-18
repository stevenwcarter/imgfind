//! Single decode seam for every pixel-decode in imgfind.
//!
//! RAW files (by extension) are decoded via `rawler` — largest embedded preview
//! first, full demosaic as a fallback (see `decode_raw`, Task 2). Every other
//! extension uses the `image` crate exactly as before.

use anyhow::{Context, Result};
use std::path::Path;

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

/// Decode any supported still or RAW image to a `DynamicImage`, corrected for
/// EXIF orientation (0x0112) so the result is upright.
pub fn decode_image(path: &Path) -> Result<image::DynamicImage> {
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

    // Apply EXIF orientation uniformly (RAW preview / non-RAW alike). Best-effort:
    // no tag or unreadable EXIF leaves the image as-decoded.
    if let Some(orientation) =
        read_exif_orientation(path).and_then(image::metadata::Orientation::from_exif)
    {
        img.apply_orientation(orientation);
    }

    Ok(img)
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

#[cfg(test)]
mod tests {
    use super::*;

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
