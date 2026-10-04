//! Talks to the daemon when it is running, otherwise runs the engine in-process.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::config::Config;
use crate::daemon::socket_path;
use crate::engine::Engine;
use crate::protocol::{Request, Response};

#[derive(Clone)]
pub enum Client {
    Local(Arc<Engine>),
    Daemon(PathBuf),
}

impl Client {
    pub async fn connect(cfg: &Config, local_only: bool) -> Result<Self> {
        let path = socket_path();
        if !local_only && path.exists() {
            let c = Client::Daemon(path);
            if tokio::time::timeout(Duration::from_millis(500), c.call(Request::Ping)).await.is_ok_and(|r| r.is_ok()) {
                return Ok(c);
            }
        }
        Ok(Client::Local(Engine::new(cfg.clone())?))
    }

    pub async fn call(&self, req: Request) -> Result<Response> {
        let resp = match self {
            Client::Local(engine) => engine.handle(req).await,
            Client::Daemon(path) => daemon_call(path, &req).await?,
        };
        match resp {
            Response::Error(e) => Err(anyhow!(e)),
            r => Ok(r),
        }
    }
}

pub async fn daemon_call(path: &PathBuf, req: &Request) -> Result<Response> {
    let stream = UnixStream::connect(path).await.context("connecting to oxd")?;
    let (read, mut write) = stream.into_split();
    let mut line = serde_json::to_vec(req)?;
    line.push(b'\n');
    write.write_all(&line).await?;
    let mut buf = String::new();
    BufReader::new(read).read_line(&mut buf).await?;
    if buf.is_empty() {
        bail!("oxd closed the connection");
    }
    Ok(serde_json::from_str(&buf)?)
}

/// Unwrap a specific response variant, e.g. `expect!(resp, Response::Gex)`.
#[macro_export]
macro_rules! expect {
    ($resp:expr, $variant:path) => {
        match $resp {
            $variant(v) => Ok(v),
            other => Err(anyhow::anyhow!("unexpected response: {:?}", std::mem::discriminant(&other))),
        }
    };
}
