//! Shared constants and the derived AllAnime AES key.
//!
//! These mirror the *live* ani-cli source (verified against the working Python
//! implementation), NOT the stale literals in older docs. The `sync` module
//! re-validates them daily against upstream.

use std::sync::OnceLock;

use sha2::{Digest, Sha256};

pub const ALLANIME_BASE: &str = "allanime.day";
pub const ALLANIME_API: &str = "https://api.allanime.day";
/// Referer/Origin required by the API. Upstream moved this off allanime.day.
pub const REFERER: &str = "https://youtu-chan.com";
pub const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:150.0) Gecko/20100101 Firefox/150.0";
pub const MP4UPLOAD_REFERER: &str = "https://www.mp4upload.com";

/// Seed hashed (sha256) into the AES-256 key used for the `tobeparsed` blob.
pub const KEY_SEED: &str = "Xot36i3lK3:v1";
/// Persisted-query hash for the episode-embed GraphQL request.
pub const EPISODE_QUERY_HASH: &str =
    "d405d0edd690624b66baba3068e0edc3ac90f1597d898a1ec8db4e5c43c00fec";
pub const ANICLI_RAW_URL: &str =
    "https://raw.githubusercontent.com/pystardust/ani-cli/master/ani-cli";

/// The AES-256 key as a 64-char hex string: `sha256(KEY_SEED)`.
pub fn allanime_key_hex() -> String {
    let mut hasher = Sha256::new();
    hasher.update(KEY_SEED.as_bytes());
    hex::encode(hasher.finalize())
}

static ACTIVE_KEY: OnceLock<String> = OnceLock::new();

/// Override the compile-time key with the config's value (which the sync
/// daemon rewrites when upstream rotates the seed). Call once at startup.
pub fn set_active_key(hex: String) {
    let _ = ACTIVE_KEY.set(hex);
}

/// The key decryption should actually use: the config override if set,
/// otherwise the compile-time default.
pub fn active_key_hex() -> String {
    ACTIVE_KEY.get().cloned().unwrap_or_else(allanime_key_hex)
}
