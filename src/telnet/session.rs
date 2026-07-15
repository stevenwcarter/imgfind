//! Per-connection telnet session: negotiate, login, search, render.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};

use crate::database::Database;
use crate::decode::decode_image;
use crate::relative_to_abs_path;
use crate::search::SearchEngine;
use crate::telnet::protocol::{TelnetEvent, TelnetParser, initial_negotiation};
use crate::telnet::render::render_halfblock;
use crate::units::{DistanceThreshold, MaxK};

/// Which screen the client is currently looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Login,
    SearchBox,
    Results,
    NoResults,
}

/// Map a cosine distance in [0, 2] to a 0-100 "match" percentage.
pub fn match_percent(distance: f32) -> u8 {
    let pct = ((1.0 - distance / 2.0) * 100.0).round();
    pct.clamp(0.0, 100.0) as u8
}

/// Given the current screen and a pressed byte, decide the next screen.
/// `has_art` is whether a result is currently rendered (affects Esc).
pub fn next_screen_on_key(current: Screen, byte: u8, has_art: bool) -> Screen {
    match current {
        Screen::Results => Screen::SearchBox,
        Screen::NoResults => Screen::SearchBox,
        Screen::SearchBox => {
            if byte == ESC {
                if has_art {
                    Screen::Results
                } else {
                    Screen::SearchBox
                }
            } else {
                Screen::SearchBox
            }
        }
        Screen::Login => Screen::Login,
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

        // Search top result.
        let engine = SearchEngine::new(&ctx.db);
        let results = engine
            .search(&embedding, 5, ctx.threshold, ctx.max_k)
            .await?;

        // Find the first result whose image decodes.
        let mut shown: Option<(String, f32, image::DynamicImage)> = None;
        for (rel, dist) in &results {
            let abs = relative_to_abs_path(Path::new(rel), &ctx.db.parent_dir);
            let decoded = tokio::task::spawn_blocking({
                let abs = abs.clone();
                move || decode_image(&abs)
            })
            .await;
            if let Ok(Ok(img)) = decoded {
                shown = Some((rel.clone(), *dist, img));
                break;
            }
        }

        match shown {
            Some((rel, dist, img)) => {
                let art = render_halfblock(&img, cols, rows.saturating_sub(2).max(1));
                let filename = Path::new(&rel)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| rel.clone());
                let pct = match_percent(dist);
                let screen = format!(
                    "\x1b[2J\x1b[H{art}\x1b[0m\r\n{}\r\n(any key: search \u{00b7} Esc: dismiss)",
                    caption(&filename, pct)
                );
                write_str(&mut stream, &screen).await?;
                current_art = Some(screen);
                // Any key returns to the search box (top of loop).
                wait_any_key(&mut stream, &mut parser, &mut buf, &mut cols, &mut rows).await?;
            }
            None => {
                write_str(
                    &mut stream,
                    &format!(
                        "\x1b[2J\x1b[H\r\nNo matches for \"{}\".\r\n(any key: search)",
                        query.trim()
                    ),
                )
                .await?;
                wait_any_key(&mut stream, &mut parser, &mut buf, &mut cols, &mut rows).await?;
            }
        }
    }
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
    fn any_key_on_results_opens_search_box() {
        assert_eq!(
            next_screen_on_key(Screen::Results, b'x', true),
            Screen::SearchBox
        );
        assert_eq!(
            next_screen_on_key(Screen::Results, b' ', true),
            Screen::SearchBox
        );
    }

    #[test]
    fn esc_in_search_box_returns_to_results_when_art_exists() {
        // ESC = 0x1b
        assert_eq!(
            next_screen_on_key(Screen::SearchBox, 0x1b, true),
            Screen::Results
        );
    }

    #[test]
    fn esc_in_search_box_with_no_art_stays_in_search_box() {
        assert_eq!(
            next_screen_on_key(Screen::SearchBox, 0x1b, false),
            Screen::SearchBox
        );
    }

    #[test]
    fn any_key_on_no_results_opens_search_box() {
        assert_eq!(
            next_screen_on_key(Screen::NoResults, b'k', false),
            Screen::SearchBox
        );
    }

    #[test]
    fn caption_includes_filename_and_percent() {
        let c = caption("beach.jpg", 92);
        assert!(c.contains("beach.jpg"));
        assert!(c.contains("92%"));
    }
}
