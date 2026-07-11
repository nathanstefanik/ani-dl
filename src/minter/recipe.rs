//! Level-0 aaReq signing (pure Rust recipe from reversed __aaCrypto).

use anyhow::{Context, Result};
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::Engine;
use sha2::{Digest, Sha256};

const BUCKET_MS: i64 = 5 * 60 * 1000;

pub fn bucket_ts(now_ms: i64) -> i64 {
    (now_ms / BUCKET_MS) * BUCKET_MS
}

pub fn sign_aa_req(
    part_b_b64: &str,
    mask_hex: &str,
    epoch: u64,
    build_id: &str,
    query_hash: &str,
    ts: i64,
) -> Result<String> {
    let part_b = base64::engine::general_purpose::STANDARD
        .decode(part_b_b64.trim())
        .context("decoding partB")?;
    let mask = hex::decode(mask_hex).context("decoding mask")?;

    let mut key = [0u8; 32];
    for (i, slot) in key.iter_mut().enumerate() {
        *slot = part_b.get(i).copied().unwrap_or(0) ^ mask.get(i).copied().unwrap_or(0);
    }

    let iv_input = format!("{epoch}:{build_id}:{query_hash}:{ts}");
    let hash = Sha256::digest(iv_input.as_bytes());
    let iv = &hash[..12];

    let plaintext = serde_json::json!({
        "v": 1,
        "ts": ts,
        "epoch": epoch,
        "buildId": build_id,
        "qh": query_hash,
    });
    let pt_bytes = serde_json::to_vec(&plaintext)?;

    let cipher = Aes256Gcm::new_from_slice(&key).context("AES-GCM key")?;
    let nonce = Nonce::from_slice(iv);
    let ciphertext = cipher
        .encrypt(nonce, pt_bytes.as_ref())
        .map_err(|e| anyhow::anyhow!("AES-GCM encrypt: {e}"))?;

    let mut out = Vec::with_capacity(1 + 12 + ciphertext.len());
    out.push(0x01);
    out.extend_from_slice(iv);
    out.extend_from_slice(&ciphertext);

    Ok(base64::engine::general_purpose::STANDARD.encode(out))
}
