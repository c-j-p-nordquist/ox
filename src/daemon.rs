//! `oxd`: a background process that keeps provider connections and caches warm and serves
//! requests over a Unix socket, so every `ox` invocation answers from memory.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::client::daemon_call;
use crate::config::{state_dir, Config};
use crate::engine::Engine;
use crate::protocol::{Request, Response, Status};

pub fn socket_path() -> PathBuf {
    state_dir().join("oxd.sock")
}

fn log_path() -> PathBuf {
    state_dir().join("oxd.log")
}

pub async fn status() -> Option<Status> {
    let path = socket_path();
    let r = tokio::time::timeout(Duration::from_millis(500), daemon_call(&path, &Request::Ping)).await;
    match r {
        Ok(Ok(Response::Pong(s))) => Some(s),
        _ => None,
    }
}

/// Run the daemon in the foreground.
pub async fn run(cfg: Config) -> Result<()> {
    std::fs::create_dir_all(state_dir())?;
    let path = socket_path();
    if path.exists() {
        if status().await.is_some() {
            bail!("oxd is already running ({})", path.display());
        }
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    let engine = Engine::new(cfg)?;
    tokio::spawn(engine.clone().prefetch_loop());
    eprintln!("oxd {} listening on {}", env!("CARGO_PKG_VERSION"), path.display());

    let (stop_tx, mut stop_rx) = tokio::sync::mpsc::channel::<()>(1);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                tokio::spawn(serve(stream, engine.clone(), stop_tx.clone()));
            }
            _ = stop_rx.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    let _ = std::fs::remove_file(&path);
    eprintln!("oxd stopped");
    Ok(())
}

async fn serve(stream: UnixStream, engine: Arc<Engine>, stop: tokio::sync::mpsc::Sender<()>) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let (resp, shutdown) = match serde_json::from_str::<Request>(&line) {
            Ok(Request::Shutdown) => (Response::Ok, true),
            Ok(req) => (engine.handle(req).await, false),
            Err(e) => (Response::Error(format!("bad request: {e}")), false),
        };
        let mut out = serde_json::to_vec(&resp).unwrap_or_default();
        out.push(b'\n');
        if write.write_all(&out).await.is_err() {
            break;
        }
        if shutdown {
            let _ = stop.send(()).await;
            break;
        }
    }
}

/// Spawn the daemon detached and wait until it answers.
pub async fn start() -> Result<Status> {
    if let Some(s) = status().await {
        return Ok(s);
    }
    std::fs::create_dir_all(state_dir())?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(log_path())?;
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["daemon", "run"]).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().context("spawning oxd")?;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(60)).await;
        if let Some(s) = status().await {
            return Ok(s);
        }
    }
    bail!("oxd did not come up; see {}", log_path().display())
}

pub async fn stop() -> Result<bool> {
    if status().await.is_none() {
        return Ok(false);
    }
    daemon_call(&socket_path(), &Request::Shutdown).await?;
    Ok(true)
}
