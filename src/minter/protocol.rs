//! JSON-RPC framing for the ani-dl ⇄ minter protocol (v1).

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub id: u64,
    pub v: u32,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub id: u64,
    pub v: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
    #[serde(rename = "retryAfterMs", default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

impl RpcError {
    pub fn is_expired(&self) -> bool {
        self.code == "EXPIRED" || self.code == "STALE"
    }

    pub fn is_level_unsupported(&self) -> bool {
        self.code == "LEVEL_UNSUPPORTED"
    }
}

pub fn redact_secret(s: &str) -> String {
    let prefix_len = s.len().min(8);
    format!("{}… ({} chars)", &s[..prefix_len], s.len())
}
