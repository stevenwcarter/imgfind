//! Decode image bytes / decoded images into a Slint image.

use anyhow::{Context, Result};
use image::DynamicImage;
use slint::{Image, SharedPixelBuffer};

/// Convert an already-decoded `DynamicImage` into a Slint `Image`.
pub fn dynamic_to_slint_image(img: &DynamicImage) -> Image {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let buffer = SharedPixelBuffer::clone_from_slice(rgba.as_raw(), w, h);
    Image::from_rgba8(buffer)
}

pub fn jpeg_to_slint_image(bytes: &[u8]) -> Result<Image> {
    let img = image::load_from_memory(bytes).context("Failed to decode image bytes")?;
    Ok(dynamic_to_slint_image(&img))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1×1 red PNG (image crate decodes PNG and JPEG alike); proves bytes
    /// become a non-empty Slint image rather than panicking.
    #[test]
    fn decodes_valid_image_bytes() {
        // 1×1 red pixel, encoded as PNG via the image crate at test time.
        let mut png: Vec<u8> = Vec::new();
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let slint_img = jpeg_to_slint_image(&png).expect("decode");
        assert_eq!(slint_img.size().width, 1);
    }

    #[test]
    fn rejects_garbage_bytes() {
        assert!(jpeg_to_slint_image(&[0, 1, 2, 3]).is_err());
    }

    #[test]
    fn dynamic_to_slint_image_preserves_dimensions() {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            3,
            2,
            image::Rgba([10, 20, 30, 255]),
        ));
        let slint_img = dynamic_to_slint_image(&img);
        assert_eq!(slint_img.size().width, 3);
        assert_eq!(slint_img.size().height, 2);
    }
}
