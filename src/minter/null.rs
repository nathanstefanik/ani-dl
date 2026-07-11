//! Null provider: no minter; core uses legacy static-key paths.

use anyhow::Result;
use async_trait::async_trait;

use crate::api::TranslationType;
use crate::minter::material::{Capabilities, Material, Token};
use crate::minter::MaterialProvider;
use crate::providers::SourceUrl;

pub struct NullProvider;

#[async_trait]
impl MaterialProvider for NullProvider {
    async fn hello(&self) -> Result<Capabilities> {
        Ok(Capabilities {
            minter: "null".to_string(),
            protocol: crate::minter::protocol::PROTOCOL_VERSION,
            levels: vec![],
            sources: vec![],
            browser: None,
        })
    }

    async fn material(&self, _source: &str, _force: bool) -> Result<Material> {
        anyhow::bail!("minter disabled")
    }

    async fn sign(
        &self,
        _source: &str,
        _query_hash: &str,
        _ts: Option<i64>,
    ) -> Result<Token> {
        anyhow::bail!("minter disabled")
    }

    async fn resolve(
        &self,
        _source: &str,
        _show_id: &str,
        _episode: &str,
        _tt: TranslationType,
    ) -> Result<Vec<SourceUrl>> {
        anyhow::bail!("minter disabled")
    }

    async fn ping(&self) -> Result<()> {
        Ok(())
    }
}
