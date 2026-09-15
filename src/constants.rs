//! Shared constants for the hianime.at backend (ani-cli 5.1.2).

pub const HIANIME_BASE: &str = "https://hianime.at";
pub const HIANIME_REFERER: &str = "https://hianime.at/";
pub const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
/// Repeating XOR key for `window.__P` on the ZokoAnime embed (ASCII `otaku-embed-v1`).
pub const EMBED_XOR_KEY: &[u8] = b"otaku-embed-v1";
