//! Pure, UI-agnostic helpers for the detail panel: metadata formatting and the
//! selection identity. Kept free of Slint so it is unit-testable.

use imgfind::database::ImageMetadata;

/// The seed image the detail panel is showing. Holds the image's OWN identity
/// (not a grid index), so replacing the grid (search-similar) never invalidates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailState {
    pub path: String,
    pub filename: String,
}

/// Last path component for display.
pub fn filename_of(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

pub fn select(path: String) -> DetailState {
    let filename = filename_of(&path);
    DetailState { path, filename }
}

/// One line per present metadata field; `None` fields omitted entirely.
pub fn format_metadata(meta: &ImageMetadata) -> String {
    let mut lines = Vec::new();
    if let (Some(w), Some(h)) = (meta.width, meta.height) {
        lines.push(format!("Dimensions: {w}×{h}"));
    }
    if let Some(size) = meta.file_size {
        lines.push(format!("Size: {} KB", size / 1024));
    }
    match (&meta.camera_make, &meta.camera_model) {
        (Some(make), Some(model)) => lines.push(format!("Camera: {make} {model}")),
        (Some(make), None) => lines.push(format!("Camera: {make}")),
        (None, Some(model)) => lines.push(format!("Camera: {model}")),
        (None, None) => {}
    }
    if let Some(dt) = &meta.datetime_taken {
        lines.push(format!("Taken: {dt}"));
    }
    if let Some(c) = &meta.coords {
        lines.push(format!("GPS: {:.5}, {:.5}", c.lat, c.lon));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use imgfind::database::GpsCoords;

    fn empty_meta() -> ImageMetadata {
        ImageMetadata {
            file_size: None,
            width: None,
            height: None,
            coords: None,
            camera_make: None,
            camera_model: None,
            datetime_taken: None,
        }
    }

    #[test]
    fn filename_takes_last_component() {
        assert_eq!(filename_of("a/b/c.jpg"), "c.jpg");
        assert_eq!(filename_of("c.jpg"), "c.jpg");
    }

    #[test]
    fn select_captures_path_and_filename() {
        let d = select("sub/dir/photo.png".to_string());
        assert_eq!(d.path, "sub/dir/photo.png");
        assert_eq!(d.filename, "photo.png");
    }

    #[test]
    fn format_metadata_omits_none_fields() {
        let meta = empty_meta();
        assert_eq!(format_metadata(&meta), "");
    }

    #[test]
    fn format_metadata_renders_present_fields() {
        let mut meta = empty_meta();
        meta.width = Some(800);
        meta.height = Some(600);
        meta.file_size = Some(2048);
        meta.camera_make = Some("Canon".to_string());
        meta.camera_model = Some("R6".to_string());
        meta.datetime_taken = Some("2024:01:02 03:04:05".to_string());
        meta.coords = Some(GpsCoords {
            lat: 37.7749,
            lon: -122.4194,
        });
        let out = format_metadata(&meta);
        assert!(out.contains("Dimensions: 800×600"));
        assert!(out.contains("Size: 2 KB"));
        assert!(out.contains("Camera: Canon R6"));
        assert!(out.contains("Taken: 2024:01:02 03:04:05"));
        assert!(out.contains("GPS: 37.77490, -122.41940"));
    }

    #[test]
    fn format_metadata_partial_camera() {
        let mut meta = empty_meta();
        meta.camera_make = Some("Sony".to_string());
        assert_eq!(format_metadata(&meta), "Camera: Sony");
    }
}
