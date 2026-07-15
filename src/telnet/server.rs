//! Telnet server: TCP accept loop + a single shared CLIP embedder worker.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use clipper::ClipEmbedder;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, mpsc, oneshot};
use tracing::{error, info};

use crate::database::Database;
use crate::telnet::session::{self, EmbedRequest, SessionCtx};
use crate::units::{DistanceThreshold, MaxK};

/// Start the telnet server and run until the listener errors.
pub async fn run_server(
    db: Database,
    bind: IpAddr,
    port: u16,
    auth: bool,
    max_connections: usize,
) -> Result<()> {
    // Resolve the active model name up front (fail fast with a clear message).
    let model_name = db.active_model().await.context("no active model")?.name;

    // Spawn the embedder worker on a dedicated OS thread. It owns the loaded
    // model (never shared, so no Sync bound), and serves embed requests over
    // a channel — all connections share this one loaded model.
    let (embed_tx, mut embed_rx) = mpsc::channel::<EmbedRequest>(64);
    let (ready_tx, ready_rx) = oneshot::channel::<Result<()>>();
    let model_name_for_worker = model_name.clone();
    std::thread::spawn(move || {
        let embedder = match ClipEmbedder::from_model(&model_name_for_worker, false)
            .context("failed to load CLIP model for telnet server")
        {
            Ok(e) => {
                let _ = ready_tx.send(Ok(()));
                e
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        while let Some(req) = embed_rx.blocking_recv() {
            let result = embedder
                .get_text_embedding(&req.query)
                .context("failed to embed query");
            let _ = req.reply.send(result);
        }
    });
    // Propagate a model-load failure before binding.
    ready_rx.await.context("embed worker died")??;
    info!("telnet: CLIP model '{model_name}' loaded");

    let addr = SocketAddr::new(bind, port);
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind telnet listener on {addr}"))?;
    info!("telnet server listening on {addr} (auth={auth}, max={max_connections})");

    let db = Arc::new(db);
    let sem = Arc::new(Semaphore::new(max_connections));

    loop {
        let (stream, peer) = listener.accept().await.context("accept failed")?;
        let permit = match sem.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                info!("telnet: refusing {peer} (connection cap reached)");
                drop(stream);
                continue;
            }
        };
        let ctx = SessionCtx {
            db: db.clone(),
            embed_tx: embed_tx.clone(),
            auth,
            threshold: DistanceThreshold(1.3),
            max_k: MaxK(200),
        };
        info!("telnet: connection from {peer}");
        tokio::spawn(async move {
            let _permit = permit; // released on task end
            if let Err(e) = session::run(stream, ctx).await {
                error!("telnet: session {peer} ended with error: {e}");
            } else {
                info!("telnet: {peer} disconnected");
            }
        });
    }
}
