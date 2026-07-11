//! Binding B — loopback HTTP (`POST /rpc`).

use std::path::PathBuf;

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::TranslationType;
use crate::minter::client::{build_request, parse_response};
use crate::minter::material::{Capabilities, Material, Token};
use crate::minter::protocol::RpcResponse;
use crate::minter::MaterialProvider;
use crate::providers::SourceUrl;

#[derive(Debug, Deserialize)]
struct Handshake {
    port: u16,
    token: String,
}

pub struct HttpMinter {
    client: reqwest::Client,
    rpc_url: String,
    token: String,
}

impl HttpMinter {
    pub fn connect() -> Result<Self> {
        let hs = read_handshake()?;
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .context("building HTTP minter client")?,
            rpc_url: format!("http://127.0.0.1:{}/rpc", hs.port),
            token: hs.token,
        })
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T> {
        let req = build_request(method, params);
        let resp = self
            .client
            .post(&self.rpc_url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(CONTENT_TYPE, "application/json")
            .json(&req)
            .send()
            .await
            .context("HTTP minter request")?;
        let rpc: RpcResponse = resp.json().await.context("HTTP minter response")?;
        if rpc.id != req.id {
            anyhow::bail!(
                "minter response id mismatch: expected {}, got {}",
                req.id,
                rpc.id
            );
        }
        parse_response(rpc)
    }
}

fn read_handshake() -> Result<Handshake> {
    let path = handshake_path()?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading minter handshake {}", path.display()))?;
    serde_json::from_str(&text).context("parsing minter handshake")
}

pub fn handshake_path() -> Result<PathBuf> {
    let base = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok()
        .or_else(dirs::runtime_dir)
        .context("cannot determine runtime dir for minter handshake")?;
    Ok(base.join("ani-dl-minter.json"))
}

#[async_trait]
impl MaterialProvider for HttpMinter {
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
