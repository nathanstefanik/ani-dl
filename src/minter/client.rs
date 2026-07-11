//! Shared JSON-RPC call helper.

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::minter::protocol::{RpcError, RpcRequest, RpcResponse, PROTOCOL_VERSION};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

pub fn build_request(method: &str, params: Value) -> RpcRequest {
    RpcRequest {
        id: next_id(),
        v: PROTOCOL_VERSION,
        method: method.to_string(),
        params,
    }
}

pub fn parse_response<T: DeserializeOwned>(resp: RpcResponse) -> Result<T> {
    if resp.v != PROTOCOL_VERSION {
        anyhow::bail!("minter protocol mismatch: got v{}", resp.v);
    }
    if let Some(err) = resp.error {
        return Err(rpc_err(err));
    }
    let result = resp.result.context("minter response missing result")?;
    serde_json::from_value(result).context("decoding minter result")
}

pub fn rpc_err(err: RpcError) -> anyhow::Error {
    let mut msg = format!("{}: {}", err.code, err.message);
    if let Some(ms) = err.retry_after_ms {
        msg.push_str(&format!(" (retry after {ms}ms)"));
    }
    anyhow::anyhow!(msg)
}
