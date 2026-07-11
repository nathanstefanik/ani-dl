//! Minter capability and material types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub minter: String,
    pub protocol: u32,
    pub levels: Vec<u8>,
    pub sources: Vec<String>,
    #[serde(default)]
    pub browser: Option<String>,
}

impl Capabilities {
    pub fn supports_level(&self, level: u8) -> bool {
        self.levels.contains(&level)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Material {
    pub epoch: u64,
    #[serde(rename = "partB")]
    pub part_b: String,
    pub mask: String,
    #[serde(rename = "buildId")]
    pub build_id: String,
    pub referer: String,
    #[serde(rename = "apiBase")]
    pub api_base: String,
    #[serde(rename = "cdnBase")]
    pub cdn_base: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: i64,
    #[serde(rename = "graceMs", default)]
    pub grace_ms: u64,
}

impl Material {
    pub fn is_fresh(&self, now_ms: i64) -> bool {
        now_ms < self.expires_at - self.grace_ms as i64
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    #[serde(rename = "aaReq")]
    pub aa_req: String,
    pub epoch: u64,
    #[serde(rename = "expiresAt")]
    pub expires_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedContext {
    pub mask: String,
    #[serde(rename = "buildId")]
    pub build_id: String,
    pub referer: String,
    #[serde(rename = "apiBase")]
    pub api_base: String,
    #[serde(rename = "cdnBase")]
    pub cdn_base: String,
    pub updated_at: String,
}
