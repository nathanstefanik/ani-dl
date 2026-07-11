//! Reference minter sidecar — NDJSON stdio (default) or loopback HTTP.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::Context;
use base64::Engine;
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const PROTOCOL: u32 = 1;
const VERSION: &str = "ani-dl-minter/0.1.0";

#[derive(Parser)]
#[command(name = "ani-dl-minter", version)]
struct Args {
    /// Loopback HTTP binding (binding B). Omit for NDJSON stdio (binding A).
    #[arg(long)]
    listen: Option<SocketAddr>,
    /// Path to a JSON fixture for `material.get` (see docs/minter-protocol.md).
    #[arg(long, env = "ANI_DL_MINTER_FIXTURE")]
    fixture: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RpcRequest {
    id: u64,
    v: u32,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RpcResponse {
    id: u64,
    v: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RpcError {
    code: String,
    message: String,
    #[serde(rename = "retryAfterMs", skip_serializing_if = "Option::is_none")]
    retry_after_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
struct State {
    material: Option<Value>,
    ingested: HashMap<String, Value>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let fixture = args.fixture.as_ref().and_then(load_fixture);
    let state = Arc::new(Mutex::new(State {
        material: fixture,
        ingested: HashMap::new(),
    }));

    if let Some(addr) = args.listen {
        run_http(addr, state)
    } else {
        run_stdio(state)
    }
}

fn load_fixture(path: &PathBuf) -> Option<Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

fn run_stdio(state: Arc<Mutex<State>>) -> anyhow::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: RpcRequest = serde_json::from_str(&line)?;
        let resp = dispatch(&state, req);
        writeln!(stdout, "{}", serde_json::to_string(&resp)?)?;
        stdout.flush()?;
    }
    Ok(())
}

fn run_http(addr: SocketAddr, state: Arc<Mutex<State>>) -> anyhow::Result<()> {
    if !addr.ip().is_loopback() {
        anyhow::bail!("--listen must bind loopback only");
    }

    let token = hex::encode(rand_token());
    let hs_path = handshake_path()?;
    if let Some(dir) = hs_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let handshake = json!({ "port": addr.port(), "token": token });
    std::fs::write(&hs_path, serde_json::to_string_pretty(&handshake)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hs_path, std::fs::Permissions::from_mode(0o600))?;
    }

    eprintln!("listening on http://{addr}");
    eprintln!("handshake: {}", hs_path.display());

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let token = Arc::new(token);
        loop {
            let (stream, _) = listener.accept().await?;
            let state = state.clone();
            let token = token.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_http_conn(stream, state, token).await {
                    eprintln!("http conn error: {e:#}");
                }
            });
        }
    })
}

async fn handle_http_conn(
    stream: tokio::net::TcpStream,
    state: Arc<Mutex<State>>,
    token: Arc<String>,
) -> anyhow::Result<()> {
    use tokio::io::AsyncReadExt;

    let mut buf = vec![0u8; 65536];
    let n = stream.readable().await?;
    let _ = n;
    let mut stream = stream;
    let read = stream.read(&mut buf).await?;
    let req_text = String::from_utf8_lossy(&buf[..read]);
    let (method, path, auth, body) = parse_http(&req_text)?;

    if method == "POST" && path == "/ingest" {
        if auth != Some(format!("Bearer {token}")) {
            write_http(&mut stream, 401, r#"{"error":"unauthorized"}"#).await?;
            return Ok(());
        }
        let v: Value = serde_json::from_str(body)?;
        let source = v
            .get("source")
            .and_then(|x| x.as_str())
            .unwrap_or("allanime")
            .to_string();
        if let Some(material) = v.get("material").cloned() {
            state.lock().unwrap().ingested.insert(source, material);
        }
        write_http(&mut stream, 200, r#"{"ok":true}"#).await?;
        return Ok(());
    }

    if method == "POST" && path == "/rpc" {
        if auth != Some(format!("Bearer {token}")) {
            write_http(&mut stream, 401, r#"{"error":"unauthorized"}"#).await?;
            return Ok(());
        }
        let req: RpcRequest = serde_json::from_str(body)?;
        let resp = dispatch(&state, req);
        write_http(&mut stream, 200, &serde_json::to_string(&resp)?).await?;
        return Ok(());
    }

    write_http(&mut stream, 404, r#"{"error":"not found"}"#).await
}

async fn write_http(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: &str,
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        _ => "Not Found",
    };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    Ok(())
}

fn parse_http(raw: &str) -> anyhow::Result<(String, String, Option<String>, &str)> {
    let mut lines = raw.split("\r\n");
    let request = lines.next().context("empty request")?;
    let mut parts = request.split_whitespace();
    let method = parts.next().context("method")?.to_string();
    let path = parts.next().context("path")?.to_string();
    let mut auth = None;
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Authorization:") {
            auth = Some(v.trim().to_string());
        }
    }
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or("");
    Ok((method, path, auth, body))
}

fn dispatch(state: &Arc<Mutex<State>>, req: RpcRequest) -> RpcResponse {
    if req.v != PROTOCOL {
        return err_resp(
            req.id,
            "METHOD_NOT_FOUND",
            format!("protocol v{} != v{PROTOCOL}", req.v),
        );
    }

    match req.method.as_str() {
        "hello" => ok_resp(req.id, hello_result()),
        "ping" => ok_resp(req.id, json!({ "ok": true })),
        "material.get" => handle_material_get(state, req),
        "token.sign" => handle_token_sign(state, req),
        "episode.resolve" => handle_episode_resolve(req),
        _ => err_resp(req.id, "METHOD_NOT_FOUND", format!("unknown method {}", req.method)),
    }
}

fn hello_result() -> Value {
    json!({
        "minter": VERSION,
        "protocol": PROTOCOL,
        "levels": [0, 1, 2],
        "sources": ["allanime"],
        "browser": "fixture/none",
    })
}

fn handle_material_get(state: &Arc<Mutex<State>>, req: RpcRequest) -> RpcResponse {
    let source = req
        .params
        .get("source")
        .and_then(|x| x.as_str())
        .unwrap_or("allanime");
    let force = req.params.get("force").and_then(|x| x.as_bool()).unwrap_or(false);

    let guard = state.lock().unwrap();
    if !force {
        if let Some(m) = guard.ingested.get(source) {
            return ok_resp(req.id, m.clone());
        }
        if let Some(m) = &guard.material {
            return ok_resp(req.id, m.clone());
        }
    }

    err_resp(
        req.id,
        "BROWSER_UNAVAILABLE",
        "no fixture material — set ANI_DL_MINTER_FIXTURE or POST /ingest",
    )
}

fn handle_token_sign(state: &Arc<Mutex<State>>, req: RpcRequest) -> RpcResponse {
    let source = req
        .params
        .get("source")
        .and_then(|x| x.as_str())
        .unwrap_or("allanime");
    let query_hash = match req.params.get("queryHash").and_then(|x| x.as_str()) {
        Some(h) => h,
        None => return err_resp(req.id, "INTERNAL", "missing queryHash"),
    };
    let ts = req
        .params
        .get("ts")
        .and_then(|x| x.as_i64())
        .unwrap_or_else(|| bucket_ts(chrono::Utc::now().timestamp_millis()));

    let material = {
        let guard = state.lock().unwrap();
        guard
            .ingested
            .get(source)
            .cloned()
            .or_else(|| guard.material.clone())
    };
    let Some(material) = material else {
        return err_resp(req.id, "STALE", "no material loaded");
    };

    let part_b = material.get("partB").and_then(|x| x.as_str()).unwrap_or("");
    let mask = material.get("mask").and_then(|x| x.as_str()).unwrap_or("");
    let epoch = material.get("epoch").and_then(|x| x.as_u64()).unwrap_or(0);
    let build_id = material
        .get("buildId")
        .and_then(|x| x.as_str())
        .unwrap_or("");

    match sign_aa_req(part_b, mask, epoch, build_id, query_hash, ts) {
        Ok(aa_req) => ok_resp(
            req.id,
            json!({
                "aaReq": aa_req,
                "epoch": epoch,
                "expiresAt": material.get("expiresAt").cloned().unwrap_or(json!(0)),
            }),
        ),
        Err(e) => err_resp(req.id, "INTERNAL", format!("{e:#}")),
    }
}

fn handle_episode_resolve(req: RpcRequest) -> RpcResponse {
    err_resp(
        req.id,
        "LEVEL_UNSUPPORTED",
        "episode.resolve requires a browser-backed minter",
    )
}

fn ok_resp(id: u64, result: Value) -> RpcResponse {
    RpcResponse {
        id,
        v: PROTOCOL,
        result: Some(result),
        error: None,
    }
}

fn err_resp(id: u64, code: &str, message: impl Into<String>) -> RpcResponse {
    RpcResponse {
        id,
        v: PROTOCOL,
        result: None,
        error: Some(RpcError {
            code: code.to_string(),
            message: message.into(),
            retry_after_ms: None,
        }),
    }
}

fn handshake_path() -> anyhow::Result<PathBuf> {
    let base = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|_| dirs::runtime_dir().ok_or_else(|| anyhow::anyhow!("no runtime dir")))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(base.join("ani-dl-minter.json"))
}

fn rand_token() -> [u8; 32] {
    use std::time::{SystemTime, UNIX_EPOCH};
    let mut h = Sha256::new();
    h.update(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_le_bytes(),
    );
    h.update(std::process::id().to_le_bytes());
    let out = h.finalize();
    let mut token = [0u8; 32];
    token.copy_from_slice(&out[..32]);
    token
}

const BUCKET_MS: i64 = 5 * 60 * 1000;

fn bucket_ts(now_ms: i64) -> i64 {
    (now_ms / BUCKET_MS) * BUCKET_MS
}

fn sign_aa_req(
    part_b_b64: &str,
    mask_hex: &str,
    epoch: u64,
    build_id: &str,
    query_hash: &str,
    ts: i64,
) -> anyhow::Result<String> {
    let part_b = base64::engine::general_purpose::STANDARD.decode(part_b_b64.trim())?;
    let mask = hex::decode(mask_hex)?;

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

    let cipher = Aes256Gcm::new_from_slice(&key)?;
    let nonce = Nonce::from_slice(iv);
    let ciphertext = cipher
        .encrypt(nonce, pt_bytes.as_ref())
        .map_err(|e| anyhow::anyhow!("encrypt: {e}"))?;

    let mut out = Vec::with_capacity(1 + 12 + ciphertext.len());
    out.push(0x01);
    out.extend_from_slice(iv);
    out.extend_from_slice(&ciphertext);

    Ok(base64::engine::general_purpose::STANDARD.encode(out))
}
