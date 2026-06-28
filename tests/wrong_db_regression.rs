//! Regression test: `imgfind process --dir X` must write thumbnails to X's
//! database even when the process cwd resolves to a *different* database.
//!
//! ## The bug (C1)
//!
//! `generate_missing_thumbnails_batch` used to open its writer DB with
//! `get_db_path(None)` (cwd walk-up / global fallback), ignoring the `db`
//! handle it was passed. When cwd=Y (a separate library with its own DB),
//! the writer thread targeted Y's DB while the reader counted missing
//! thumbnails from X's DB. Because the count never reached zero, the
//! zero-progress guard never fired, and the worker looped forever.
//!
//! ## What this test proves
//!
//! 1. The command terminates within 30 s (no infinite loop).
//! 2. After the command, X's DB has 0 images missing a 300 px thumbnail
//!    (thumbnails reached the right file).
//! 3. Y's DB was not written to (no images there; Y's image count stays 0).

use image::{ImageBuffer, Rgb};
use imgfind::{ThumbnailSize, block_on, database::Database};
use std::path::Path;

/// Create a fresh temp directory pair `(X, Y)` that are siblings so they share
/// no common `.imgfind` ancestor. Uses `tempfile` so the directories are cleaned
/// up automatically when the guards drop.
fn two_temp_dirs() -> (tempfile::TempDir, tempfile::TempDir) {
    let x = tempfile::Builder::new()
        .prefix("imgfind_wrongdb_x_")
        .tempdir()
        .expect("create dir_x");
    let y = tempfile::Builder::new()
        .prefix("imgfind_wrongdb_y_")
        .tempdir()
        .expect("create dir_y");
    (x, y)
}

/// Write a small but real RGB PNG that can be decoded by the thumbnail pipeline.
fn write_fixture_png(path: &Path) {
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(64, 64, Rgb([100u8, 150, 200]));
    img.save(path).expect("save fixture PNG");
}

/// Open a fresh `Database` at `<parent>/.imgfind/imgfind.db`, running all
/// migrations. Returns the database handle.
fn open_db(parent: &Path) -> Database {
    let db_path = parent.join(".imgfind").join("imgfind.db");
    block_on(Database::new(&db_path)).expect("Database::new")
}

#[test]
fn process_writes_thumbnails_to_dir_db_not_cwd_db() {
    let (dir_x_guard, dir_y_guard) = two_temp_dirs();
    let dir_x = dir_x_guard.path();
    let dir_y = dir_y_guard.path();

    // ── Setup X: one decodable image + a DB row for it ───────────────────────
    write_fixture_png(&dir_x.join("fixture.png"));

    {
        let db_x = open_db(dir_x);
        block_on(db_x.insert_image_rows_batch(&[(
            "fixture.png".to_string(),
            "wrongdb_regression_hash".to_string(),
        )]))
        .expect("insert image row into X");
        // db_x drops here; connections are released before the subprocess opens
        // the same file.
    }

    // ── Setup Y: a fresh empty DB so cwd walk-up resolves to Y, not HOME ─────
    // Without this, get_db_path(None) from Y's cwd would fall back to
    // ~/.imgfind/imgfind.db, polluting the user's real database.
    {
        let _db_y = open_db(dir_y); // creates migrations-ready empty DB
    }

    // ── Sanity check: X has 1 image missing a 300 px thumbnail ───────────────
    {
        let db_x = open_db(dir_x);
        let missing = block_on(db_x.count_images_without_thumbnails(ThumbnailSize(300))).unwrap();
        assert_eq!(
            missing, 1,
            "sanity: X must have exactly 1 image missing a 300px thumbnail before processing"
        );
    }

    // ── Run `imgfind process --no-embeddings --dir X` with cwd=Y ─────────────
    //
    // Pre-fix: the writer thread called get_db_path(None) and opened Y's DB
    // (cwd walk-up). X's count never decremented → infinite loop.
    //
    // Post-fix: the writer derives the path from db.parent_dir → X's DB.
    // The count reaches 0 and the loop terminates.
    let bin = env!("CARGO_BIN_EXE_imgfind");
    let mut child = std::process::Command::new(bin)
        .args([
            "process",
            "--no-embeddings",
            "--dir",
            dir_x.to_str().expect("UTF-8 path"),
        ])
        .current_dir(dir_y)
        .spawn()
        .expect("spawn imgfind process");

    // The process MUST terminate — a 30-second deadline catches an infinite loop.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        match child.try_wait().expect("try_wait on child process") {
            Some(s) => break s,
            None => {
                if std::time::Instant::now() > deadline {
                    child.kill().ok();
                    panic!(
                        "imgfind process hung after 30 s — likely the \
                         thumbnail writer targeted the wrong DB (C1 bug)"
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
    };
    assert!(
        status.success(),
        "imgfind process must exit successfully, got: {status}"
    );

    // ── Assert: X now has 0 images missing a 300 px thumbnail ────────────────
    {
        let db_x = open_db(dir_x);
        let missing = block_on(db_x.count_images_without_thumbnails(ThumbnailSize(300))).unwrap();
        assert_eq!(
            missing, 0,
            "thumbnails must be written to X's DB (the --dir library), \
             not the cwd-resolved DB"
        );
    }

    // ── Assert: Y's DB was not written to (no images were indexed there) ─────
    {
        let db_y = open_db(dir_y);
        let count = block_on(db_y.get_image_count()).unwrap();
        assert_eq!(
            count, 0,
            "Y's DB must have no images — process must not touch the cwd DB"
        );
    }
}
