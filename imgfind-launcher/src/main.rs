slint::include_modules!();

mod recents;
mod runner;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::Parser;
use slint::ComponentHandle;

use recents::{Recents, default_recents_path, now_secs};

#[derive(Parser, Debug)]
#[command(name = "imgfind-launcher", about = "Desktop launcher for imgfind")]
struct Args {}

fn main() -> Result<()> {
    let _args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Load and prune recents.
    let recents_path = default_recents_path();
    let mut recents = recents_path
        .as_deref()
        .map(Recents::load_from)
        .unwrap_or_default();
    recents.prune_missing();

    let window = MainWindow::new().context("creating MainWindow")?;

    // Populate the recents model.
    let now = now_secs();
    let home_dir = dirs::home_dir();
    let recent_rows: Vec<RecentRow> = recents
        .entries
        .iter()
        .map(|e| {
            let name = e
                .root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| e.root.to_string_lossy().into_owned());
            let path = abbrev_home(&e.root.to_string_lossy(), home_dir.as_deref());
            let when = relative_time(now.saturating_sub(e.last_opened));
            let root = e.root.to_string_lossy().into_owned();
            RecentRow {
                name: name.into(),
                path: path.into(),
                when: when.into(),
                root: root.into(),
            }
        })
        .collect();

    let recents_model = Rc::new(slint::VecModel::from(recent_rows));
    window.set_recents(recents_model.into());

    // `pending_folder` is UI-thread-only state — Rc<RefCell<>> is fine.
    let pending_folder: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));

    // `resolved_root` is written from a background thread via invoke_from_event_loop
    // and read from UI-thread callbacks — use Arc<Mutex<>> for Send-safety.
    let resolved_root: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));

    // ── on_open_root ────────────────────────────────────────────────────────
    {
        let weak = window.as_weak();
        let recents_path = recents_path.clone();
        window.on_open_root(move |root_str| {
            let root = PathBuf::from(root_str.as_str());
            if let Some(path) = recents_path.as_deref() {
                let mut r = Recents::load_from(path);
                r.record(&root, now_secs());
                r.save_to(path)
                    .unwrap_or_else(|e| tracing::warn!("failed to save recents: {e}"));
            }
            spawn_gui(&root);
            if let Some(w) = weak.upgrade() {
                let _ = w.hide();
            }
            slint::quit_event_loop().ok();
        });
    }

    // ── on_open_other ───────────────────────────────────────────────────────
    {
        let weak = window.as_weak();
        let pending_folder = Rc::clone(&pending_folder);
        window.on_open_other(move || {
            let Some(folder) = rfd::FileDialog::new().pick_folder() else {
                return;
            };
            if let Some(root) = imgfind::find_db_root_upward(&folder) {
                spawn_gui(&root);
                if let Some(w) = weak.upgrade() {
                    let _ = w.hide();
                }
                slint::quit_event_loop().ok();
            } else {
                *pending_folder.borrow_mut() = Some(folder);
                if let Some(w) = weak.upgrade() {
                    w.set_ask_existing_root("".into());
                    w.set_view("index".into());
                    w.set_status_line("".into());
                    w.set_log_text("".into());
                    w.set_can_open_indexed(false);
                    w.set_indexing(false);
                }
            }
        });
    }

    // ── on_start_index ──────────────────────────────────────────────────────
    {
        let weak = window.as_weak();
        let pending_folder = Rc::clone(&pending_folder);
        window.on_start_index(move || {
            let Some(folder) = rfd::FileDialog::new().pick_folder() else {
                return;
            };
            let ancestor = imgfind::find_db_root_upward(&folder);
            let ask = ancestor
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            *pending_folder.borrow_mut() = Some(folder);
            if let Some(w) = weak.upgrade() {
                w.set_ask_existing_root(ask.into());
                w.set_view("index".into());
                w.set_status_line("".into());
                w.set_log_text("".into());
                w.set_can_open_indexed(false);
                w.set_indexing(false);
            }
        });
    }

    // ── on_confirm_index ────────────────────────────────────────────────────
    {
        let weak = window.as_weak();
        let pending_folder = Rc::clone(&pending_folder);
        let resolved_root = Arc::clone(&resolved_root);
        let recents_path = recents_path.clone();
        window.on_confirm_index(move |create_new| {
            let folder = match pending_folder.borrow().clone() {
                Some(f) => f,
                None => {
                    tracing::warn!("confirm-index called with no pending folder");
                    return;
                }
            };
            // Compute the root that will be opened after indexing.
            // Do this before the thread spawn — Rc<RefCell<>> is !Send.
            let run_root: PathBuf = if create_new {
                folder.clone()
            } else {
                imgfind::find_db_root_upward(&folder).unwrap_or_else(|| folder.clone())
            };

            if let Some(w) = weak.upgrade() {
                w.set_indexing(true);
                w.set_log_text("".into());
                w.set_status_line("Indexing...".into());
                w.set_can_open_indexed(false);
            }

            let specs = runner::plan(&folder, create_new);
            // Clone Send-safe values for the background thread.
            let weak_bg = weak.clone();
            let recents_path_bg = recents_path.clone();
            let resolved_root_bg = Arc::clone(&resolved_root);

            std::thread::spawn(move || {
                let weak_line = weak_bg.clone();
                let result = runner::run_plan(&specs, move |line| {
                    let appended = line + "\n";
                    let w = weak_line.clone();
                    slint::invoke_from_event_loop(move || {
                        if let Some(win) = w.upgrade() {
                            let current = win.get_log_text();
                            win.set_log_text(
                                (current.as_str().to_owned() + &appended).into(),
                            );
                        }
                    })
                    .ok();
                });

                slint::invoke_from_event_loop(move || {
                    if let Some(win) = weak_bg.upgrade() {
                        match result {
                            Ok(()) => {
                                if let Some(path) = recents_path_bg.as_deref() {
                                    let mut r = Recents::load_from(path);
                                    r.record(&run_root, now_secs());
                                    r.save_to(path).unwrap_or_else(|e| {
                                        tracing::warn!("failed to save recents: {e}");
                                    });
                                }
                                // Store the resolved root; guard drops before UI calls.
                                *resolved_root_bg.lock().unwrap() = Some(run_root);
                                win.set_status_line("Done".into());
                                win.set_can_open_indexed(true);
                                win.set_indexing(false);
                            }
                            Err(e) => {
                                win.set_status_line(format!("Failed: {e}").into());
                                win.set_indexing(false);
                            }
                        }
                    }
                })
                .ok();
            });
        });
    }

    // ── on_back_home ────────────────────────────────────────────────────────
    {
        let weak = window.as_weak();
        let pending_folder = Rc::clone(&pending_folder);
        let resolved_root = Arc::clone(&resolved_root);
        window.on_back_home(move || {
            *pending_folder.borrow_mut() = None;
            *resolved_root.lock().unwrap() = None;
            if let Some(w) = weak.upgrade() {
                w.set_view("home".into());
                w.set_status_line("".into());
                w.set_log_text("".into());
                w.set_ask_existing_root("".into());
                w.set_can_open_indexed(false);
                w.set_indexing(false);
            }
        });
    }

    // ── on_open_indexed ─────────────────────────────────────────────────────
    {
        let weak = window.as_weak();
        let resolved_root = Arc::clone(&resolved_root);
        window.on_open_indexed(move || {
            let root = match resolved_root.lock().unwrap().clone() {
                Some(r) => r,
                None => {
                    tracing::warn!("open-indexed called with no resolved root");
                    return;
                }
            };
            spawn_gui(&root);
            if let Some(w) = weak.upgrade() {
                let _ = w.hide();
            }
            slint::quit_event_loop().ok();
        });
    }

    window.run().context("running event loop")?;
    Ok(())
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Spawn `imgfind-gui --dir <root>` in the background (fire-and-forget).
fn spawn_gui(root: &std::path::Path) {
    let bin = imgfind::resolve_sibling_binary("imgfind-gui");
    if let Err(e) = std::process::Command::new(&bin).arg("--dir").arg(root).spawn() {
        tracing::error!("failed to spawn imgfind-gui: {e}");
    }
}

/// Abbreviate the home directory prefix with `~`.
fn abbrev_home(path: &str, home: Option<&std::path::Path>) -> String {
    if let Some(h) = home {
        let home_str = h.to_string_lossy();
        if let Some(rest) = path.strip_prefix(home_str.as_ref()) {
            return format!("~{rest}");
        }
    }
    path.to_owned()
}

/// Coarse human-readable relative time from a duration in seconds.
fn relative_time(secs: u64) -> String {
    if secs < 60 {
        "just now".to_owned()
    } else if secs < 3_600 {
        format!("{} min ago", secs / 60)
    } else if secs < 86_400 {
        format!("{} hr ago", secs / 3_600)
    } else if secs < 7 * 86_400 {
        format!("{} days ago", secs / 86_400)
    } else {
        format!("{} weeks ago", secs / (7 * 86_400))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_boundaries() {
        assert_eq!(relative_time(0), "just now");
        assert_eq!(relative_time(59), "just now");
        assert_eq!(relative_time(60), "1 min ago");
        assert_eq!(relative_time(3599), "59 min ago");
        assert_eq!(relative_time(3600), "1 hr ago");
        assert_eq!(relative_time(86399), "23 hr ago");
        assert_eq!(relative_time(86400), "1 days ago");
        assert_eq!(relative_time(6 * 86400), "6 days ago");
        assert_eq!(relative_time(7 * 86400), "1 weeks ago");
    }

    #[test]
    fn abbrev_home_replaces_prefix() {
        let home = std::path::Path::new("/home/steve");
        assert_eq!(abbrev_home("/home/steve/photos", Some(home)), "~/photos");
        assert_eq!(abbrev_home("/other/path", Some(home)), "/other/path");
        assert_eq!(abbrev_home("/any/path", None), "/any/path");
    }
}
