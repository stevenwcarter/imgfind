//! Model-free loopback integration test for the telnet search server.
//!
//! Drives the real `telnet::session::run` over a real TCP loopback pair, with
//! a fake embed worker standing in for the CLIP model (it just echoes back a
//! pre-seeded unit vector). No model download, no network access beyond the
//! loopback pair itself — this runs in the normal (non-ignored) test suite.

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use image::{Rgb, RgbImage};
use imgfind::database::Database;
use imgfind::telnet::session::{EmbedRequest, SessionCtx, run};
use imgfind::units::{DistanceThreshold, MaxK};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time;

/// Negotiate → submit a search query → receive rendered half-block art, all
/// over a real socket pair with a fake embedder standing in for CLIP.
#[tokio::test]
async fn telnet_session_negotiates_searches_and_renders_art() -> Result<()> {
    // ── 1. Hermetic DB under a TempDir (auto-cleaned on drop at test end) ──
    let tmp = tempfile::tempdir()?;
    let db_path = tmp.path().join(".imgfind").join("imgfind.db");
    fs::create_dir_all(db_path.parent().expect("db_path has a parent"))?;
    let db = Database::new(&db_path).await?;

    // ── 2. A 512-dim unit embedding (already L2-normalized) ────────────────
    let mut embedding = vec![0f32; 512];
    embedding[0] = 1.0;

    // ── 3. A real small PNG so the session's decode step succeeds ──────────
    let png_path = tmp.path().join("test.png");
    RgbImage::from_pixel(8, 8, Rgb([200, 40, 40])).save(&png_path)?;

    // ── 4. Seed the image row + vector via the public batch API ────────────
    db.insert_images_batch(&[(
        "test.png".to_string(),
        "testhash".to_string(),
        embedding.clone(),
    )])
    .await?;

    // ── 5. Fake embed worker: always replies with the seeded vector, so the
    // search finds the seeded image at distance ~0 regardless of query text.
    let (embed_tx, mut embed_rx) = mpsc::channel::<EmbedRequest>(8);
    tokio::spawn(async move {
        while let Some(req) = embed_rx.recv().await {
            let _ = req.reply.send(Ok(embedding.clone()));
        }
    });

    // ── 6. Real TCP loopback pair ────────────────────────────────────────
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (client_res, accept_res) = tokio::join!(TcpStream::connect(addr), listener.accept());
    let mut client = client_res?;
    let (server_stream, _) = accept_res?;

    // ── 7. Drive the real session on the server side ───────────────────────
    let ctx = SessionCtx {
        db: Arc::new(db),
        embed_tx,
        auth: false,
        threshold: DistanceThreshold(1.3),
        max_k: MaxK(200),
    };
    tokio::spawn(async move {
        let _ = run(server_stream, ctx).await;
    });

    // ── 8. Submit a query, terminated by CR per the session's read_query ───
    client.write_all(b"cat\r").await?;
    client.flush().await?;

    // ── 9. Read the response with a timeout so a hang fails fast in CI ─────
    let mut collected: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match time::timeout(Duration::from_secs(5), client.read(&mut chunk)).await {
            Ok(Ok(0)) => break, // peer closed
            Ok(Ok(n)) => {
                collected.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&collected);
                if text.contains("\u{1b}[38;2;") && text.contains('\u{2580}') {
                    break;
                }
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => break, // 5s timeout elapsed; let assertions below fail with detail
        }
    }

    // ── 10. Assert the rendered half-block art arrived ──────────────────────
    let response = String::from_utf8_lossy(&collected).into_owned();
    assert!(
        response.contains("\u{1b}[38;2;"),
        "response missing truecolor fg escape, got: {response:?}"
    );
    assert!(
        response.contains('\u{2580}'),
        "response missing half-block glyph, got: {response:?}"
    );
    assert!(
        response.contains("\r\n"),
        "response missing CRLF line ending, got: {response:?}"
    );

    Ok(())
}
