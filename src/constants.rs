//! Shared constants for the hianime.at backend (ani-cli 5.1.5).

pub const HIANIME_BASE: &str = "https://hianime.at";
pub const HIANIME_REFERER: &str = "https://hianime.at/";
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
/// Repeating XOR key for `window.__P` on the ZokoAnime embed (ASCII `otaku-embed-v1`).
pub const EMBED_XOR_KEY: &[u8] = b"otaku-embed-v1";

pub const MEGAPLAY_BASE: &str = "https://megaplay.buzz";
/// MegaPlay playlists and segments 403 without this `Referer`.
pub const MEGAPLAY_REFERER: &str = "https://megaplay.buzz/";
/// Key for the `enc` payload of `getSources`; the player zero-pads it to 32
/// bytes for AES-256-CBC.
pub const MEGAPLAY_SOURCES_KEY: &[u8] = b"i?LMTAx0Q6,:}50U";
/// IV for the `enc` payload of `getSources` (exactly 16 bytes).
pub const MEGAPLAY_SOURCES_IV: &[u8; 16] = b"W0;27ToaUpl_P%'c";
