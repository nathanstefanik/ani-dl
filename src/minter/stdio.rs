//! Binding A — spawned subprocess (NDJSON stdin/stdout).

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex;

use crate::api::TranslationType;
use crate::minter::client::{build_request, parse_response};
use crate::minter::material::{Capabilities, Material, Token};
use crate::minter::protocol::{RpcRequest, RpcResponse};
use crate::minter::MaterialProvider;
use crate::providers::SourceUrl;

pub struct StdioMinter {
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<tokio::process::ChildStdout>>,
    _child: Arc<Mutex<Child>>,
}

impl StdioMinter {
    pub async fn spawn(command: &str) -> Result<Self> {
        let mut parts = command.split_whitespace();
        let program = parts.next().context("empty minter command")?;
        let mut cmd = Command::new(program);
        cmd.args(parts)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);

        let mut child = cmd.spawn().context("spawning minter")?;
        let stdin = child.stdin.take().context("minter stdin")?;
        let stdout = child.stdout.take().context("minter stdout")?;

        Ok(Self {
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            _child: Arc::new(Mutex::new(child)),
        })
    }

    async fn call_raw(&self, req: RpcRequest) -> Result<RpcResponse> {
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.call_raw_inner(req),
        )
        .await
        .context("minter RPC timed out (30s)")?
    }

    async fn call_raw_inner(&self, req: RpcRequest) -> Result<RpcResponse> {
        let line = serde_json::to_string(&req)?;
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;

        let mut stdout = self.stdout.lock().await;
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = stdout.read_line(&mut buf).await?;
            if n == 0 {
                anyhow::bail!("minter stdout closed");
            }
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                continue;
            }
            return Ok(serde_json::from_str(trimmed).context("parsing minter response")?);
        }
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T> {
        let req = build_request(method, params);
        let id = req.id;
        let resp = self.call_raw(req).await?;
        if resp.id != id {
            anyhow::bail!("minter response id mismatch: expected {id}, got {}", resp.id);
        }
        parse_response(resp)
    }
}

#[async_trait]
impl MaterialProvider for StdioMinter {
    async fn hello(&self) -> Result<Capabilities> {
        self.call(
            "hello",
            json!({
                "core": format!("ani-dl/{}", env!("CARGO_PKG_VERSION")),
                "protocol": crate::minter::protocol::PROTOCOL_VERSION,
            }),
        )
        .await
    }

    async fn material(&self, source: &str, force: bool) -> Result<Material> {
        self.call("material.get", json!({ "source": source, "force": force }))
            .await
    }

    async fn sign(
        &self,
        source: &str,
        query_hash: &str,
        ts: Option<i64>,
    ) -> Result<Token> {
        let mut params = json!({ "source": source, "queryHash": query_hash });
        if let Some(ts) = ts {
            params["ts"] = json!(ts);
        }
        self.call("token.sign", params).await
    }

    async fn resolve(
        &self,
        source: &str,
        show_id: &str,
        episode: &str,
        tt: TranslationType,
    ) -> Result<Vec<SourceUrl>> {
        #[derive(serde::Deserialize)]
        struct ResolveResult {
            sources: Vec<SourceUrl>,
        }
        let result: ResolveResult = self
            .call(
                "episode.resolve",
                json!({
                    "source": source,
                    "showId": show_id,
                    "episode": episode,
                    "translationType": tt.as_str(),
                }),
            )
            .await?;
        Ok(result.sources)
    }

    async fn ping(&self) -> Result<()> {
        #[derive(serde::Deserialize)]
        struct PingResult {
            #[allow(dead_code)]
            ok: bool,
        }
        let _: PingResult = self.call("ping", json!({})).await?;
        Ok(())
    }
}
