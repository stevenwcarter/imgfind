//! Per-connection telnet session: negotiate, login, search, render.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use image::DynamicImage;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::time;

use crate::database::Database;
use crate::decode::decode_image;
use crate::relative_to_abs_path;
use crate::search::SearchEngine;
use crate::telnet::protocol::{TelnetEvent, TelnetParser, initial_negotiation};
use crate::telnet::render::render_halfblock;
use crate::units::{DistanceThreshold, MaxK};

/// Map a cosine distance in [0, 2] to a 0-100 "match" percentage.
pub fn match_percent(distance: f32) -> u8 {
    let pct = ((1.0 - distance / 2.0) * 100.0).round();
    pct.clamp(0.0, 100.0) as u8
}

/// An action decoded from the leading key/escape-sequence of a results-screen
/// input buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultsAction {
    /// Bare Esc while info is shown: hide the caption/hint chrome.
    HideInfo,
    /// `h` or Left-arrow (`ESC [ D`): previous image.
    Prev,
    /// `l` or Right-arrow (`ESC [ C`): next image.
    Next,
    /// Any other ordinary key: return to the search box.
    ToSearch,
    /// Bare Esc while info is already hidden, or an ignored escape sequence
    /// (e.g. up/down arrow): do nothing.
    Noop,
    /// A possibly-incomplete escape sequence; the caller should read more
    /// bytes before deciding.
    NeedMore,
}

/// A bare Esc resolves to `HideInfo` if info is currently shown, else `Noop`.
fn bare_esc(info_shown: bool) -> ResultsAction {
    if info_shown {
        ResultsAction::HideInfo
    } else {
        ResultsAction::Noop
    }
}

/// Decode the first key/sequence from `data`. `info_shown` disambiguates a
/// bare Esc (hide vs. no-op). `complete = true` means no more bytes will
/// arrive, so a trailing lone Esc (or an incomplete CSI sequence) must be
/// finalized rather than reported as `NeedMore`. Returns
/// `(action, bytes_consumed)`; `NeedMore` always consumes 0.
pub fn results_action(data: &[u8], info_shown: bool, complete: bool) -> (ResultsAction, usize) {
    let Some(&first) = data.first() else {
        return (ResultsAction::Noop, 0);
    };
    if first != ESC {
        return match first {
            b'h' => (ResultsAction::Prev, 1),
            b'l' => (ResultsAction::Next, 1),
            _ => (ResultsAction::ToSearch, 1),
        };
    }

    // Leading byte is Esc: either a bare Esc, or the start of a CSI arrow
    // sequence (`ESC [ <final>`).
    let Some(&second) = data.get(1) else {
        return if complete {
            (bare_esc(info_shown), 1)
        } else {
            (ResultsAction::NeedMore, 0)
        };
    };
    if second != b'[' {
        // Esc followed by a byte other than '[' can't be an arrow sequence,
        // so it resolves immediately regardless of `complete`.
        return (bare_esc(info_shown), 1);
    }
    match data.get(2) {
        None if complete => (ResultsAction::Noop, data.len()),
        None => (ResultsAction::NeedMore, 0),
        Some(b'D') => (ResultsAction::Prev, 3),
        Some(b'C') => (ResultsAction::Next, 3),
        Some(_) => (ResultsAction::Noop, 3),
    }
}

/// One-line caption under the art.
pub fn caption(filename: &str, percent: u8) -> String {
    format!("{filename} \u{00b7} {percent}% match")
}

/// A request to the shared embedder worker (defined/wired in server.rs).
pub struct EmbedRequest {
    pub query: String,
    pub reply: oneshot::Sender<Result<Vec<f32>>>,
}

/// Everything a session needs, cloned per connection.
pub struct SessionCtx {
    pub db: Arc<Database>,
    pub embed_tx: mpsc::Sender<EmbedRequest>,
    pub auth: bool,
    pub threshold: DistanceThreshold,
    pub max_k: MaxK,
}

const ESC: u8 = 0x1b;
const CR: u8 = b'\r';
const LF: u8 = b'\n';
const BS: u8 = 0x08;
const DEL: u8 = 0x7f;

/// Drive one connection to completion. Errors are logged by the caller.
pub async fn run(mut stream: TcpStream, ctx: SessionCtx) -> Result<()> {
    stream.write_all(&initial_negotiation()).await?;
    stream.flush().await?;

    let mut parser = TelnetParser::new();
    let (mut cols, mut rows): (u16, u16) = (80, 24);
    let mut buf = [0u8; 1024];

    // --- Login ---
    if ctx.auth {
        let mut attempts = 0;
        loop {
            write_str(&mut stream, "\x1b[2J\x1b[H\r\nimgfind telnet\r\nUsername: ").await?;
            let username = read_line(
                &mut stream,
                &mut parser,
                &mut buf,
                &mut cols,
                &mut rows,
                true,
            )
            .await?;
            write_str(&mut stream, "\r\nPassword: ").await?;
            let password = read_line(
                &mut stream,
                &mut parser,
                &mut buf,
                &mut cols,
                &mut rows,
                false,
            )
            .await?;

            let ok = match ctx.db.get_telnet_user(username.trim()).await? {
                Some(u) => crate::telnet::auth::verify_password(&password, &u.password_hash),
                None => false,
            };
            if ok {
                break;
            }
            attempts += 1;
            if attempts >= 3 {
                write_str(&mut stream, "\r\nToo many failed attempts. Goodbye.\r\n").await?;
                return Ok(());
            }
            write_str(&mut stream, "\r\nInvalid credentials.\r\n").await?;
        }
    }

    // --- Search / results loop ---
    let mut current_art: Option<String> = None; // full screen (art + caption)
    loop {
        // Draw the search box.
        write_str(
            &mut stream,
            "\x1b[2J\x1b[H\r\nSearch (Enter to run, Esc to dismiss):\r\n> ",
        )
        .await?;
        // Read a query line; Esc during input dismisses to art if present.
        let query = match read_query(&mut stream, &mut parser, &mut buf, &mut cols, &mut rows)
            .await?
        {
            QueryOutcome::Submit(q) => q,
            QueryOutcome::Dismiss => {
                if let Some(art) = &current_art {
                    write_str(&mut stream, art).await?;
                    // Wait for any key, then loop back to the search box.
                    wait_any_key(&mut stream, &mut parser, &mut buf, &mut cols, &mut rows).await?;
                }
                continue;
            }
            QueryOutcome::Closed => return Ok(()),
        };
        if query.trim().is_empty() {
            continue;
        }

        // Embed via the shared worker.
        let (tx, rx) = oneshot::channel();
        ctx.embed_tx
            .send(EmbedRequest {
                query: query.trim().to_string(),
                reply: tx,
            })
            .await
            .context("embed worker gone")?;
        let embedding = match rx.await.context("embed worker dropped reply")? {
            Ok(e) => e,
            Err(e) => {
                write_str(&mut stream, &format!("\r\nSearch error: {e}\r\n")).await?;
                continue;
            }
        };

        // Top 5 results, navigable.
        let engine = SearchEngine::new(&ctx.db);
        let results = engine
            .search(&embedding, 5, ctx.threshold, ctx.max_k)
            .await?;

        // Lazily decode each result on first visit, skipping ones that fail.
        let mut cache: Vec<DecodeState> =
            (0..results.len()).map(|_| DecodeState::NotTried).collect();
        let Some(mut idx) = first_decodable(&mut cache, &results, &ctx.db.parent_dir, 0).await
        else {
            write_str(
                &mut stream,
                &format!(
                    "\x1b[2J\x1b[H\r\nNo matches for \"{}\".\r\n(any key: search)",
                    query.trim()
                ),
            )
            .await?;
            wait_any_key(&mut stream, &mut parser, &mut buf, &mut cols, &mut rows).await?;
            continue;
        };

        // Results screen: navigate with h/l or arrow keys, Esc hides the
        // caption/hint chrome, any other key returns to the search box. Input
        // is drained from a `pending` byte buffer so multiple keys landing in
        // one socket read (fast key-repeat, or a key pressed mid-render) are
        // all handled instead of only the first.
        let mut show_info = true;
        let mut pending: Vec<u8> = Vec::new();
        current_art = Some(
            render_results_frame(&mut stream, &results, &cache, idx, cols, rows, show_info).await?,
        );
        loop {
            if pending.is_empty() {
                match pump(&mut stream, &mut parser, &mut buf, &mut cols, &mut rows).await? {
                    Some(d) if !d.is_empty() => pending = d,
                    Some(_) => continue, // only negotiation/NAWS arrived; keep waiting
                    None => return Ok(()), // EOF: connection closed
                }
            }

            let (probe, probe_consumed) = results_action(&pending, show_info, false);
            let action = if probe == ResultsAction::NeedMore {
                // Possibly-incomplete escape sequence at the end of the
                // buffer: give the client one short window to finish sending
                // it (arrow keys arrive as 3 bytes in quick succession)
                // before finalizing a trailing lone Esc as a real bare Esc.
                match time::timeout(
                    Duration::from_millis(200),
                    pump(&mut stream, &mut parser, &mut buf, &mut cols, &mut rows),
                )
                .await
                {
                    Ok(Ok(Some(d))) if !d.is_empty() => {
                        pending.extend(d);
                        continue; // re-interpret with the fuller buffer
                    }
                    Ok(Ok(Some(_))) => continue, // NAWS-only: retry the read
                    _ => {
                        // Timeout, EOF, or error: finalize (complete = true)
                        // so a lone trailing Esc becomes a real bare Esc.
                        let (fin, fin_consumed) = results_action(&pending, show_info, true);
                        let take = fin_consumed.max(1).min(pending.len());
                        pending.drain(0..take);
                        fin
                    }
                }
            } else {
                let take = probe_consumed.max(1).min(pending.len());
                pending.drain(0..take);
                probe
            };

            match apply_results_action(
                action,
                &results,
                &mut cache,
                &ctx.db.parent_dir,
                &mut idx,
                &mut show_info,
            )
            .await
            {
                ResultsEffect::Leave => break,
                ResultsEffect::Redraw => {
                    current_art = Some(
                        render_results_frame(
                            &mut stream,
                            &results,
                            &cache,
                            idx,
                            cols,
                            rows,
                            show_info,
                        )
                        .await?,
                    );
                }
                ResultsEffect::Unchanged => {}
            }
        }
    }
}

/// Decode outcome for one search result, cached lazily since not every
/// navigable position is visited in a session.
enum DecodeState {
    NotTried,
    Failed,
    Decoded(DynamicImage),
}

/// Decode result `i` on first visit and cache the outcome; a cache hit is
/// free. Returns `true` if the image is (now) decodable.
async fn ensure_decoded(
    cache: &mut [DecodeState],
    results: &[(String, f32)],
    parent_dir: &Path,
    i: usize,
) -> bool {
    if matches!(cache[i], DecodeState::NotTried) {
        let abs = relative_to_abs_path(Path::new(&results[i].0), parent_dir);
        let decoded = tokio::task::spawn_blocking(move || decode_image(&abs)).await;
        cache[i] = match decoded {
            Ok(Ok(img)) => DecodeState::Decoded(img),
            _ => DecodeState::Failed,
        };
    }
    matches!(cache[i], DecodeState::Decoded(_))
}

/// Find the first decodable index at or after `from` (inclusive), skipping
/// results that fail to decode. Returns `None` if none decode.
async fn first_decodable(
    cache: &mut [DecodeState],
    results: &[(String, f32)],
    parent_dir: &Path,
    from: usize,
) -> Option<usize> {
    for i in from..results.len() {
        if ensure_decoded(cache, results, parent_dir, i).await {
            return Some(i);
        }
    }
    None
}

/// Move one position from `from` in `dir` (+1 or -1), skipping results that
/// fail to decode, with no wraparound. Returns `None` if `from` is already at
/// that end (or every remaining candidate fails to decode).
async fn step_decodable(
    cache: &mut [DecodeState],
    results: &[(String, f32)],
    parent_dir: &Path,
    from: usize,
    dir: i32,
) -> Option<usize> {
    let mut i = from as i64 + i64::from(dir);
    while i >= 0 && (i as usize) < results.len() {
        let idx = i as usize;
        if ensure_decoded(cache, results, parent_dir, idx).await {
            return Some(idx);
        }
        i += i64::from(dir);
    }
    None
}

enum QueryOutcome {
    Submit(String),
    Dismiss,
    Closed,
}

async fn write_str(stream: &mut TcpStream, s: &str) -> Result<()> {
    stream.write_all(s.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

/// Pump one socket read through the parser, applying NAWS updates, returning
/// the data bytes seen. Returns `Ok(None)` on EOF (connection closed).
async fn pump(
    stream: &mut TcpStream,
    parser: &mut TelnetParser,
    buf: &mut [u8],
    cols: &mut u16,
    rows: &mut u16,
) -> Result<Option<Vec<u8>>> {
    let n = stream.read(buf).await?;
    if n == 0 {
        return Ok(None);
    }
    let mut data = Vec::new();
    for ev in parser.feed(&buf[..n]) {
        match ev {
            TelnetEvent::Data(b) => data.push(b),
            TelnetEvent::WindowSize { cols: c, rows: r } => {
                if c > 0 {
                    *cols = c;
                }
                if r > 0 {
                    *rows = r;
                }
            }
            TelnetEvent::Negotiation => {}
        }
    }
    Ok(Some(data))
}

/// Read a line terminated by CR (or LF). If `echo`, echo typed chars back.
async fn read_line(
    stream: &mut TcpStream,
    parser: &mut TelnetParser,
    buf: &mut [u8],
    cols: &mut u16,
    rows: &mut u16,
    echo: bool,
) -> Result<String> {
    let mut line = String::new();
    loop {
        let data = match pump(stream, parser, buf, cols, rows).await? {
            Some(d) => d,
            None => return Ok(line), // EOF
        };
        for b in data {
            match b {
                CR | LF => return Ok(line),
                BS | DEL => {
                    if line.pop().is_some() && echo {
                        write_str(stream, "\x08 \x08").await?;
                    }
                }
                0 => {}
                _ => {
                    line.push(b as char);
                    if echo {
                        stream.write_all(&[b]).await?;
                        stream.flush().await?;
                    }
                }
            }
        }
    }
}

/// Like `read_line` but Esc yields `Dismiss` and EOF yields `Closed`.
async fn read_query(
    stream: &mut TcpStream,
    parser: &mut TelnetParser,
    buf: &mut [u8],
    cols: &mut u16,
    rows: &mut u16,
) -> Result<QueryOutcome> {
    let mut line = String::new();
    loop {
        let data = match pump(stream, parser, buf, cols, rows).await? {
            Some(d) => d,
            None => return Ok(QueryOutcome::Closed),
        };
        for b in data {
            match b {
                ESC => return Ok(QueryOutcome::Dismiss),
                CR | LF => return Ok(QueryOutcome::Submit(line)),
                BS | DEL => {
                    if line.pop().is_some() {
                        write_str(stream, "\x08 \x08").await?;
                    }
                }
                0 => {}
                _ => {
                    line.push(b as char);
                    stream.write_all(&[b]).await?;
                    stream.flush().await?;
                }
            }
        }
    }
}

/// Block until any single key arrives (or EOF).
async fn wait_any_key(
    stream: &mut TcpStream,
    parser: &mut TelnetParser,
    buf: &mut [u8],
    cols: &mut u16,
    rows: &mut u16,
) -> Result<()> {
    loop {
        match pump(stream, parser, buf, cols, rows).await? {
            None => return Ok(()), // EOF: let the outer loop end on next read
            Some(d) if !d.is_empty() => return Ok(()),
            Some(_) => continue, // only negotiation/NAWS arrived; keep waiting
        }
    }
}

/// Render one results-screen frame. When `show_info` the art is shrunk by 2
/// rows to make room for a caption + hint line; when hidden the art alone
/// fills the full terminal height. Always clears the screen first.
fn render_result_screen(
    img: &DynamicImage,
    cols: u16,
    rows: u16,
    filename: &str,
    pct: u8,
    show_info: bool,
) -> String {
    let art_rows = if show_info {
        rows.saturating_sub(2).max(1)
    } else {
        rows.max(1)
    };
    let art = render_halfblock(img, cols, art_rows);
    if show_info {
        format!(
            "\x1b[2J\x1b[H{art}\x1b[0m\r\n{}\r\n(h/l: prev/next \u{00b7} Esc: hide info \u{00b7} any key: search)",
            caption(filename, pct)
        )
    } else {
        format!("\x1b[2J\x1b[H{art}")
    }
}

/// Render the frame for `results[idx]`, write it to the socket, and return
/// it (for the caller to stash as `current_art`, redrawn on Esc-to-search
/// dismissal).
async fn render_results_frame(
    stream: &mut TcpStream,
    results: &[(String, f32)],
    cache: &[DecodeState],
    idx: usize,
    cols: u16,
    rows: u16,
    show_info: bool,
) -> Result<String> {
    let (rel, dist) = &results[idx];
    let filename = Path::new(rel)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| rel.clone());
    let pct = match_percent(*dist);
    let img = match &cache[idx] {
        DecodeState::Decoded(img) => img,
        DecodeState::NotTried | DecodeState::Failed => {
            unreachable!("idx always points at an already-decoded entry")
        }
    };
    let screen = render_result_screen(img, cols, rows, &filename, pct, show_info);
    write_str(stream, &screen).await?;
    Ok(screen)
}

/// Outcome of applying a fully-resolved (non-`NeedMore`) [`ResultsAction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultsEffect {
    /// Nothing changed (e.g. `Noop`, or `Prev`/`Next` already at an edge).
    Unchanged,
    /// The visible index or `show_info` changed: redraw the frame.
    Redraw,
    /// Return to the search box.
    Leave,
}

/// Apply the state-changing effect of a resolved results-screen action.
/// Rendering itself is left to the caller (it happens once, at the call
/// site, when the returned effect is `Redraw`).
async fn apply_results_action(
    action: ResultsAction,
    results: &[(String, f32)],
    cache: &mut [DecodeState],
    parent_dir: &Path,
    idx: &mut usize,
    show_info: &mut bool,
) -> ResultsEffect {
    match action {
        ResultsAction::HideInfo => {
            *show_info = false;
            ResultsEffect::Redraw
        }
        ResultsAction::Prev => match step_decodable(cache, results, parent_dir, *idx, -1).await {
            Some(new_idx) => {
                *idx = new_idx;
                ResultsEffect::Redraw
            }
            None => ResultsEffect::Unchanged,
        },
        ResultsAction::Next => match step_decodable(cache, results, parent_dir, *idx, 1).await {
            Some(new_idx) => {
                *idx = new_idx;
                ResultsEffect::Redraw
            }
            None => ResultsEffect::Unchanged,
        },
        ResultsAction::Noop => ResultsEffect::Unchanged,
        ResultsAction::ToSearch => ResultsEffect::Leave,
        ResultsAction::NeedMore => {
            unreachable!("NeedMore is resolved by the caller before applying")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_percent_maps_distance_to_0_100() {
        assert_eq!(match_percent(0.0), 100); // identical
        assert_eq!(match_percent(2.0), 0); // opposite
        assert_eq!(match_percent(1.0), 50); // orthogonal
        // Clamps out-of-range distances.
        assert_eq!(match_percent(-0.5), 100);
        assert_eq!(match_percent(3.0), 0);
    }

    #[test]
    fn caption_includes_filename_and_percent() {
        let c = caption("beach.jpg", 92);
        assert!(c.contains("beach.jpg"));
        assert!(c.contains("92%"));
    }

    #[test]
    fn results_action_h_is_prev() {
        assert_eq!(results_action(b"h", true, true), (ResultsAction::Prev, 1));
    }

    #[test]
    fn results_action_l_is_next() {
        assert_eq!(results_action(b"l", true, true), (ResultsAction::Next, 1));
    }

    #[test]
    fn results_action_left_arrow_is_prev() {
        assert_eq!(
            results_action(b"\x1b[D", true, true),
            (ResultsAction::Prev, 3)
        );
    }

    #[test]
    fn results_action_right_arrow_is_next() {
        assert_eq!(
            results_action(b"\x1b[C", true, true),
            (ResultsAction::Next, 3)
        );
    }

    #[test]
    fn results_action_up_arrow_is_noop() {
        assert_eq!(
            results_action(b"\x1b[A", true, true),
            (ResultsAction::Noop, 3)
        );
    }

    #[test]
    fn results_action_ordinary_key_goes_to_search() {
        assert_eq!(
            results_action(b"x", true, true),
            (ResultsAction::ToSearch, 1)
        );
    }

    #[test]
    fn results_action_cr_goes_to_search() {
        // CR/LF count as "any other key".
        assert_eq!(
            results_action(b"\r", true, true),
            (ResultsAction::ToSearch, 1)
        );
    }

    #[test]
    fn results_action_finalized_bare_esc_hides_info_when_shown() {
        assert_eq!(
            results_action(b"\x1b", true, true),
            (ResultsAction::HideInfo, 1)
        );
    }

    #[test]
    fn results_action_finalized_bare_esc_is_noop_when_already_hidden() {
        assert_eq!(
            results_action(b"\x1b", false, true),
            (ResultsAction::Noop, 1)
        );
    }

    #[test]
    fn results_action_lone_esc_not_complete_needs_more() {
        assert_eq!(
            results_action(b"\x1b", true, false),
            (ResultsAction::NeedMore, 0)
        );
    }

    #[test]
    fn results_action_incomplete_csi_not_complete_needs_more() {
        assert_eq!(
            results_action(b"\x1b[", true, false),
            (ResultsAction::NeedMore, 0)
        );
    }

    #[test]
    fn results_action_incomplete_csi_finalized_is_noop() {
        assert_eq!(
            results_action(b"\x1b[", true, true),
            (ResultsAction::Noop, 2)
        );
    }

    #[test]
    fn results_action_empty_is_noop() {
        assert_eq!(results_action(b"", true, true), (ResultsAction::Noop, 0));
    }

    #[test]
    fn results_action_esc_followed_by_non_csi_byte_resolves_immediately() {
        // Esc immediately followed by a byte other than '[' can't be the
        // start of an arrow sequence, so it resolves as a bare Esc without
        // waiting for more bytes — even when `complete` is false.
        assert_eq!(
            results_action(b"\x1bq", true, false),
            (ResultsAction::HideInfo, 1)
        );
        assert_eq!(
            results_action(b"\x1bq", false, true),
            (ResultsAction::Noop, 1)
        );
    }
}
