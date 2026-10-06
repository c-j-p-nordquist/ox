//! `ox serve`: a local web page for the GEX heatmap, with the picker's plan drawn on it.
//!
//! A deliberately small HTTP/1.1 server (GET only, one request per connection) bound to
//! 127.0.0.1. Every API route maps to one `protocol::Request`, so it works through the daemon
//! or in-process exactly like the CLI.

use anyhow::{Context, Result};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::analytics::pick::contracts::ContractParams;
use crate::analytics::pick::PickParams;
use crate::client::Client;
use crate::config::Config;
use crate::protocol::{Request, Response};

const PAGE: &str = include_str!("web/gex.html");

pub async fn run(client: Client, cfg: Config, port: u16, open: bool) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await.with_context(|| format!("binding 127.0.0.1:{port}"))?;
    let url = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    println!(
        "ox serving {url}  ({})  — ctrl-c to stop",
        if matches!(client, Client::Daemon(_)) { "via oxd" } else { "in-process" }
    );
    if open {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let _ = std::process::Command::new(opener).arg(&url).spawn();
    }
    let port = listener.local_addr()?.port();
    loop {
        let (stream, _) = listener.accept().await?;
        let (client, cfg) = (client.clone(), cfg.clone());
        tokio::spawn(async move {
            if let Err(e) = handle(stream, port, &client, &cfg).await {
                eprintln!("serve: {e:#}");
            }
        });
    }
}

async fn handle(stream: TcpStream, port: u16, client: &Client, cfg: &Config) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    // Read headers (GET requests carry no body); only `Host` matters.
    let mut host = None;
    let mut line = String::new();
    while reader.read_line(&mut line).await? > 2 {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("host") {
                host = Some(v.trim().to_ascii_lowercase());
            }
        }
        line.clear();
    }

    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let param = |name: &str| {
        query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| decode(v))
    };

    let (status, ctype, body) = if !host_allowed(host.as_deref(), port) {
        // A page on another site that rebinds its DNS name to 127.0.0.1 still sends its own Host.
        (403, "text/plain", "forbidden host".into())
    } else if method != "GET" {
        (405, "text/plain", "method not allowed".into())
    } else {
        match path {
            "/" | "/index.html" => (200, "text/html; charset=utf-8", PAGE.to_string()),
            "/api/symbols" => json(Ok(symbols(cfg))),
            "/api/gexmap" => match param("symbol") {
                Some(symbol) => {
                    let max_dte = param("dte").and_then(|d| d.parse().ok());
                    json(
                        client
                            .call(Request::GexMap { symbol, max_dte })
                            .await
                            .and_then(|r| crate::expect!(r, Response::GexMap)),
                    )
                }
                None => (400, "text/plain", "symbol required".into()),
            },
            "/api/pick" => match param("symbol") {
                Some(symbol) => {
                    let params = PickParams {
                        bias: None,
                        entry: None,
                        stop: None,
                        t1: None,
                        t2: None,
                        force_contracts: false,
                        contracts: ContractParams {
                            commission: cfg.costs.commission_per_contract,
                            ..Default::default()
                        },
                    };
                    json(
                        client
                            .call(Request::Pick { symbol, params })
                            .await
                            .and_then(|r| crate::expect!(r, Response::Pick)),
                    )
                }
                None => (400, "text/plain", "symbol required".into()),
            },
            _ => (404, "text/plain", "not found".into()),
        }
    };

    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        match status {
            200 => "OK",
            400 => "Bad Request",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            _ => "Internal Server Error",
        },
        body.len()
    );
    write.write_all(head.as_bytes()).await?;
    write.write_all(body.as_bytes()).await?;
    Ok(())
}

fn json<T: Serialize>(r: Result<T>) -> (u16, &'static str, String) {
    match r.and_then(|v| Ok(serde_json::to_string(&v)?)) {
        Ok(body) => (200, "application/json", body),
        Err(e) => (500, "application/json", serde_json::json!({ "error": format!("{e:#}") }).to_string()),
    }
}

/// Watchlist first, then portfolio underlyings, deduplicated.
fn symbols(cfg: &Config) -> Vec<String> {
    let (mut syms, _) = crate::watchlist::effective(&cfg.watchlist);
    if let Ok(Some(p)) = crate::portfolio::load() {
        syms.extend(p.positions.iter().map(|x| x.symbol.to_ascii_uppercase()));
    }
    let mut seen = std::collections::HashSet::new();
    syms.retain(|s| seen.insert(s.clone()));
    syms
}

/// Only requests addressed to this server by its loopback name (blocks DNS rebinding).
fn host_allowed(host: Option<&str>, port: u16) -> bool {
    host.is_some_and(|h| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"))
}

/// Percent-decoding for query values (`+` as space).
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{decode, host_allowed};

    #[test]
    fn only_loopback_hosts() {
        assert!(host_allowed(Some("127.0.0.1:8642"), 8642));
        assert!(host_allowed(Some("localhost:8642"), 8642));
        assert!(!host_allowed(Some("evil.example:8642"), 8642));
        assert!(!host_allowed(Some("localhost:9000"), 8642));
        assert!(!host_allowed(None, 8642));
    }

    #[test]
    fn decodes_query_values() {
        assert_eq!(decode("BRK%2EB"), "BRK.B");
        assert_eq!(decode("a+b"), "a b");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
    }
}
