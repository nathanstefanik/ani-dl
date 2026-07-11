//! Core ⇄ minter seam: `MaterialProvider` trait and factory.

mod cache;
mod client;
mod http;
mod material;
mod null;
mod protocol;
mod recipe;
mod stdio;

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;

pub use cache::MaterialCache;
pub use material::{Capabilities, Material, PersistedContext, Token};
pub use protocol::PROTOCOL_VERSION;
pub use recipe::{bucket_ts, sign_aa_req};

use crate::api::TranslationType;
use crate::config::MinterConfig;
use crate::providers::SourceUrl;

use self::http::HttpMinter;
use self::null::NullProvider;
use self::stdio::StdioMinter;

pub const ALLANIME_SOURCE: &str = "allanime";

#[async_trait]
pub trait MaterialProvider: Send + Sync {
    async fn hello(&self) -> Result<Capabilities>;
    async fn material(&self, source: &str, force: bool) -> Result<Material>;
    async fn sign(&self, source: &str, query_hash: &str, ts: Option<i64>) -> Result<Token>;
    async fn resolve(
        &self,
        source: &str,
        show_id: &str,
        episode: &str,
        tt: TranslationType,
    ) -> Result<Vec<SourceUrl>>;
    async fn ping(&self) -> Result<()>;
}

pub async fn from_config(
    cfg: &MinterConfig,
) -> Result<(Arc<dyn MaterialProvider>, Capabilities)> {
    match cfg.transport.as_str() {
        "none" | "" => {
            let arc: Arc<dyn MaterialProvider> = Arc::new(NullProvider);
            let caps = arc.hello().await?;
            Ok((arc, caps))
        }
        "stdio" => {
            let minter = StdioMinter::spawn(&cfg.command).await?;
            let arc: Arc<dyn MaterialProvider> = Arc::new(minter);
            let caps = init_minter(&arc, ALLANIME_SOURCE).await?;
            Ok((arc, caps))
        }
        "http" => {
            let minter = HttpMinter::connect()?;
            let arc: Arc<dyn MaterialProvider> = Arc::new(minter);
            let caps = init_minter(&arc, ALLANIME_SOURCE).await?;
            Ok((arc, caps))
        }
        other => anyhow::bail!("unknown minter.transport '{other}'"),
    }
}

async fn init_minter(
    provider: &Arc<dyn MaterialProvider>,
    source: &str,
) -> Result<Capabilities> {
    provider.ping().await.context("minter ping")?;
    let caps = provider.hello().await.context("minter hello")?;
    if caps.protocol != PROTOCOL_VERSION {
        anyhow::bail!(
            "minter protocol v{} != core v{PROTOCOL_VERSION}",
            caps.protocol
        );
    }
    if !caps.sources.iter().any(|s| s == source) {
        anyhow::bail!("minter does not support source '{source}'");
    }
    Ok(caps)
}

pub fn load_persisted_context(source: &str) -> Option<PersistedContext> {
    let path = persisted_context_path().ok()?;
    let text = std::fs::read_to_string(path).ok()?;
    let map: std::collections::HashMap<String, PersistedContext> =
        serde_json::from_str(&text).ok()?;
    map.get(source).cloned()
}

pub fn save_persisted_context(source: &str, ctx: &PersistedContext) -> Result<()> {
    let path = persisted_context_path()?;
    let mut map: std::collections::HashMap<String, PersistedContext> = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&path)?).unwrap_or_default()
    } else {
        std::collections::HashMap::new()
    };
    map.insert(source.to_string(), ctx.clone());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&map)?)?;
    Ok(())
}

fn persisted_context_path() -> Result<std::path::PathBuf> {
    Ok(crate::config::config_dir()?.join("material_context.json"))
}
