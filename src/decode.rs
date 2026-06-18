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

/// Decode any supported still or RAW image to a `DynamicImage`.
pub fn decode_image(path: &Path) -> Result<image::DynamicImage> {
    let is_raw = path
        .extension()
        .and_then(|e| e.to_str())
        .map(is_raw_extension)
        .unwrap_or(false);

    if is_raw {
        decode_raw(path)
    } else {
        image::open(path).with_context(|| format!("decoding image {}", path.display()))
    }
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

    const RAW_FIXTURE: &str = "tests/fixtures/sample.dng";

    #[test]
    fn decodes_real_raw_to_nonempty_image() {
        let p = std::path::Path::new(RAW_FIXTURE);
        if !p.exists() {
            eprintln!("skipping: RAW fixture {RAW_FIXTURE} not present");
            return;
        }
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
