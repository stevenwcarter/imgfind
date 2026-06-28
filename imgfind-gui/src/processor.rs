//! Background process-worker for the GUI.
//!
//! Drives the three-phase post-index pipeline on a dedicated thread that is
//! separate from the interactive [`crate::loader`] thumbnail-worker:
// Task 7 wires every public item in this module into the Slint UI progress
// pill; until then all of them appear unused to rustc's dead-code lint.
#![allow(dead_code)]
//!
//! 1. **Thumbnails (300 px)** — prerequisite for CLIP embedding.
//! 2. **Embeddings** — CLIP-vectorise each image from its 300 px thumbnail.
//! 3. **Full thumbnails** — generate 512 px and 2048 px renditions for the
//!    GUI grid, detail panel, and lightbox.
//!
//! The worker processes one small batch at a time and sleeps between batches
//! so interactive thumbnail loads are never starved.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use imgfind::processing::{self, FULL_THUMBNAIL_SIZES, ProcessCounts, ProcessPhase};

use crate::backend::Backend;

/// Maximum items per `process_next_batch` call.
///
/// Small enough not to block the interactive thumb-worker; large enough to
/// make meaningful forward progress per iteration.
const BATCH: usize = 16;

/// Backoff sleep between iterations when paused, when the CLIP model is not
/// yet loaded, or when a batch reports zero progress. Prevents busy-spinning.
const BACKOFF: Duration = Duration::from_millis(200);

/// How long the worker parks after all phases are complete before re-checking
/// for new work (e.g. after a subsequent `index` run adds more images).
const IDLE_RECHECK: Duration = Duration::from_secs(30);

/// Operational state reported by the background process-worker.
#[derive(Clone, Debug)]
pub enum WorkerState {
    /// A batch was processed; more work remains.
    Running,
    /// The controller's pause flag is set; the worker is waiting.
    Paused,
    /// All phases are drained; the worker is idle.
    Idle,
}

/// A progress snapshot sent from the worker to the UI thread after each batch.
#[derive(Clone, Debug)]
pub struct ProcessProgress {
    /// Remaining work counts as of this snapshot.
    pub counts: ProcessCounts,
    /// Worker's current operational state.
    pub state: WorkerState,
}

/// A handle that lets the UI thread pause and resume the background
/// process-worker without blocking either side.
///
/// Wraps a shared `AtomicBool` polled between batches. Pass the same
/// `Arc<AtomicBool>` to both `ProcessController::new` and
/// [`spawn_process_worker`].
pub struct ProcessController {
    paused: Arc<AtomicBool>,
}

impl ProcessController {
    /// Create a controller backed by `paused`.
    pub fn new(paused: Arc<AtomicBool>) -> Self {
        Self { paused }
    }

    /// Signal the worker to stop processing after the current batch.
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    /// Allow the worker to resume processing.
    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    /// Returns `true` if the paused flag is currently set.
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
}

/// Returns a human-readable label for each processing phase.
pub fn phase_label(p: ProcessPhase) -> &'static str {
    match p {
        ProcessPhase::Thumbnails300 => "Thumbnails (300px)",
        ProcessPhase::Embeddings => "Embeddings",
        ProcessPhase::FullThumbnails => "Full thumbnails",
    }
}

/// Compute `(done, total)` for a global progress pill.
///
/// `baseline` is the [`ProcessCounts`] captured when processing started (or
/// re-captured after new images were indexed). `now` is the most recent
/// remaining-work snapshot from the worker.
///
/// - `total = baseline.total_remaining()`
/// - `done  = total.saturating_sub(now.total_remaining())`
///
/// `saturating_sub` handles the re-baseline boundary gracefully: when `now`
/// has more remaining work than `baseline` (a concurrent `index` run added
/// images), `done` is `0` rather than wrapping.
pub fn overall(now: &ProcessCounts, baseline: &ProcessCounts) -> (usize, usize) {
    let total = baseline.total_remaining();
    let done = total.saturating_sub(now.total_remaining());
    (done, total)
}

/// Spawn the dedicated background process-worker thread.
///
/// The thread runs until the [`Sender`] is dropped (app exit) or the UI drops
/// its [`Receiver`]. When all phases are drained it parks for [`IDLE_RECHECK`]
/// so a later `index` run can wake it via [`thread::Thread::unpark`].
///
/// ## Loop
///
/// 1. Read current remaining-work counts via `imgfind::processing::counts`.
/// 2. If `next_phase()` is `None`, send `Idle` and park.
/// 3. If `paused`, send `Paused` and sleep [`BACKOFF`].
/// 4. For `Embeddings`, wait until the CLIP model is loaded (polling with
///    [`BACKOFF`] sleep) before calling `process_next_batch`.
/// 5. Call `process_next_batch` for the current phase with [`BATCH`] items.
/// 6. Zero-progress guard: if the batch advanced 0 items, sleep [`BACKOFF`]
///    (permanently-undecodable images should not cause a busy-spin).
/// 7. Read fresh counts and send a `Running` snapshot.
pub fn spawn_process_worker(
    backend: Backend,
    paused: Arc<AtomicBool>,
    progress: Sender<ProcessProgress>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("process-worker".into())
        .spawn(move || {
            // Own a separate Database clone: the sole heavy writer on this thread.
            // All DB access goes through imgfind::block_on and the shared runtime.
            let mut db = backend.db_clone();
            let embedder_lock = backend.embedder_arc();

            loop {
                // --- 1. Read current remaining work ---
                let current_counts = match imgfind::block_on(processing::counts(&db)) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("process-worker: counts query failed: {e:#}");
                        thread::sleep(BACKOFF);
                        continue;
                    }
                };

                // --- 2. Idle check ---
                let Some(phase) = current_counts.next_phase() else {
                    if progress
                        .send(ProcessProgress {
                            counts: current_counts,
                            state: WorkerState::Idle,
                        })
                        .is_err()
                    {
                        return; // UI dropped receiver → clean exit
                    }
                    // Park until unparked (e.g. after a new index run) or until
                    // IDLE_RECHECK elapses, then re-read counts.
                    thread::park_timeout(IDLE_RECHECK);
                    continue;
                };

                // --- 3. Pause check ---
                if paused.load(Ordering::SeqCst) {
                    if progress
                        .send(ProcessProgress {
                            counts: current_counts,
                            state: WorkerState::Paused,
                        })
                        .is_err()
                    {
                        return;
                    }
                    thread::sleep(BACKOFF);
                    continue;
                }

                // --- 4. Wait for the CLIP model if this phase needs it ---
                let embedder_opt = if phase == ProcessPhase::Embeddings {
                    match embedder_lock.get() {
                        Some(emb) => Some(emb),
                        None => {
                            // Model not yet loaded; notify the UI and back off.
                            if progress
                                .send(ProcessProgress {
                                    counts: current_counts,
                                    state: WorkerState::Running,
                                })
                                .is_err()
                            {
                                return;
                            }
                            thread::sleep(BACKOFF);
                            continue;
                        }
                    }
                } else {
                    None
                };

                // --- 5. Process one batch ---
                let done = match processing::process_next_batch(
                    &mut db,
                    embedder_opt,
                    phase,
                    BATCH,
                    &FULL_THUMBNAIL_SIZES,
                ) {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::warn!("process-worker: batch error ({phase:?}): {e:#}");
                        0
                    }
                };

                // --- 6. Zero-progress guard ---
                if done == 0 {
                    // All remaining images in this phase are permanently
                    // undecodable. Back off so we do not spin hot; counts() will
                    // re-read next iteration and next_phase() may advance.
                    thread::sleep(BACKOFF);
                    continue;
                }

                // --- 7. Send fresh snapshot ---
                let fresh = match imgfind::block_on(processing::counts(&db)) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("process-worker: post-batch counts failed: {e:#}");
                        current_counts // best-effort; next iteration will re-read
                    }
                };
                if progress
                    .send(ProcessProgress {
                        counts: fresh,
                        state: WorkerState::Running,
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .expect("failed to spawn process-worker thread")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use imgfind::processing::ProcessCounts;

    use super::{ProcessController, overall};

    #[test]
    fn pause_flag_round_trips() {
        let f = Arc::new(AtomicBool::new(false));
        let c = ProcessController::new(f.clone());
        assert!(!c.is_paused());
        c.pause();
        assert!(c.is_paused());
        assert!(f.load(Ordering::SeqCst));
        c.resume();
        assert!(!c.is_paused());
    }

    #[test]
    fn overall_progress_done_total() {
        let baseline = ProcessCounts { thumbs300: 10, embeddings: 10, full_thumbs: 20 };
        let now = ProcessCounts { thumbs300: 0, embeddings: 4, full_thumbs: 20 };
        // total = 40; remaining now = 24; done = 16
        assert_eq!(overall(&now, &baseline), (16, 40));

        // now == baseline → no work done yet
        assert_eq!(overall(&baseline, &baseline), (0, 40));

        // now all-zero → all work complete
        let zero = ProcessCounts { thumbs300: 0, embeddings: 0, full_thumbs: 0 };
        assert_eq!(overall(&zero, &baseline), (40, 40));
    }
}
