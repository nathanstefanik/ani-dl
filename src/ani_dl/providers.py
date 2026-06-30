"""Per-provider stream-link extraction and AllAnime decryption.

This module ports the link-resolution logic from ani-cli (the ``get_links`` /
``provider_init`` / ``process_response`` shell functions) into Python.

No video player is referenced anywhere. We only ever resolve direct stream URLs.
"""

from __future__ import annotations

import base64
import json
import re
import sys
from dataclasses import dataclass

from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

from . import ALLANIME_BASE, ALLANIME_KEY, REFERER

MP4UPLOAD_REFERER = "https://www.mp4upload.com"


@dataclass
class Stream:
    """A single resolvable stream candidate."""

    height: int          # vertical resolution, 0 if unknown
    url: str             # direct stream URL (mp4 or m3u8)
    referer: str         # referer required to fetch the stream
    provider: str        # human-readable provider name


def _warn(msg: str) -> None:
    print(f"  ! {msg}", file=sys.stderr)


# --- crypto -------------------------------------------------------------------

def _aes_ctr_decrypt(key_hex: str, iv_hex: str, ciphertext: bytes) -> bytes:
    key = bytes.fromhex(key_hex)
    iv = bytes.fromhex(iv_hex)
    cipher = Cipher(algorithms.AES(key), modes.CTR(iv))
    dec = cipher.decryptor()
    return dec.update(ciphertext) + dec.finalize()


def decrypt_source_list(b64: str) -> str:
    """Decrypt an AllAnime ``tobeparsed`` blob into the JSON source list.

    Layout (matches ani-cli ``process_response``):
      byte 0        : ignored
      bytes 1..12   : IV (12 bytes)
      bytes 13..-16 : ciphertext (last 16 bytes dropped)
      CTR nonce     : IV(hex) + "00000002"
    """
    blob = base64.b64decode(b64)
    iv_hex = blob[1:13].hex()
    ctr_hex = iv_hex + "00000002"
    ciphertext = blob[13:len(blob) - 16]
    return _aes_ctr_decrypt(ALLANIME_KEY, ctr_hex, ciphertext).decode(
        "utf-8", "replace"
    )


def decode_source_url(url: str) -> str:
    """Decode an obfuscated AllAnime sourceUrl.

    When a sourceUrl begins with ``--`` it is hex-encoded with every byte XORed
    by 0x38. ani-cli expresses this as a giant sed substitution table; the XOR
    is the underlying transform.
    """
    if not url.startswith("--"):
        return url
    hexstr = url[2:]
    chars = []
    for i in range(0, len(hexstr) - 1, 2):
        try:
            chars.append(chr(int(hexstr[i:i + 2], 16) ^ 0x38))
        except ValueError:
            break
    decoded = "".join(chars)
    return decoded.replace("/clock", "/clock.json")


# --- source list parsing ------------------------------------------------------

def _iter_source_dicts(obj):
    """Yield {sourceName, sourceUrl} dicts from arbitrarily nested JSON."""
    if isinstance(obj, dict):
        if "sourceUrl" in obj:
            yield obj
        for v in obj.values():
            yield from _iter_source_dicts(v)
    elif isinstance(obj, list):
        for v in obj:
            yield from _iter_source_dicts(v)


def parse_source_urls(text: str) -> list[tuple[str, str]]:
    """Return [(sourceName, sourceUrl), ...] from a (possibly encrypted) reply."""
    if '"tobeparsed"' in text:
        m = re.search(r'"tobeparsed":"([^"]*)"', text)
        if m:
            text = decrypt_source_list(m.group(1))

    pairs: list[tuple[str, str]] = []
    try:
        data = json.loads(text)
        for d in _iter_source_dicts(data):
            pairs.append((d.get("sourceName", ""), d.get("sourceUrl", "")))
    except (ValueError, TypeError):
        pass
    if pairs:
        return pairs

    # Regex fallback for partial / non-JSON payloads.
    clean = text.replace("\\u002F", "/").replace("\\/", "/")
    for block in re.findall(r"\{[^{}]*\}", clean):
        su = re.search(r'"sourceUrl":"([^"]*)"', block)
        sn = re.search(r'"sourceName":"([^"]*)"', block)
        if su:
            pairs.append((sn.group(1) if sn else "", su.group(1)))
    return pairs


# --- per-provider resolution --------------------------------------------------

def _height_from(text: str) -> int:
    m = re.search(r"(\d{3,4})", text or "")
    return int(m.group(1)) if m else 0


def _expand_m3u8_master(session, url: str, referer: str) -> list[Stream]:
    """Fetch an HLS master playlist and enumerate its variant streams."""
    try:
        r = session.get(url, headers={"Referer": referer}, timeout=15)
    except Exception as e:  # noqa: BLE001
        _warn(f"master.m3u8 fetch failed: {e}")
        return []
    text = r.text
    if "EXTM3U" not in text:
        return []
    base = url.rsplit("/", 1)[0] + "/"
    lines = text.splitlines()
    out: list[Stream] = []
    for i, line in enumerate(lines):
        if line.startswith("#EXT-X-STREAM-INF") and i + 1 < len(lines):
            m = re.search(r"RESOLUTION=\d+x(\d+)", line)
            height = int(m.group(1)) if m else 0
            variant = lines[i + 1].strip()
            if variant and not variant.startswith("#"):
                if not variant.startswith("http"):
                    variant = base + variant
                out.append(Stream(height, variant, referer, "hls"))
    return out


def _expand_wixmp(url: str) -> list[Stream]:
    """Expand a repackager.wixmp.com urlset into per-quality mp4 URLs."""
    m = re.search(r"/,([^/]*),/mp4", url)
    if not m:
        return []
    extract = re.sub(r"\.urlset.*", "", url.replace("repackager.wixmp.com/", ""))
    out: list[Stream] = []
    for q in (q for q in m.group(1).split(",") if q):
        variant = re.sub(r",[^/]*", q, extract)
        out.append(Stream(_height_from(q), variant, REFERER, "wixmp"))
    return out


def resolve(session, source_name: str, source_url: str) -> list[Stream]:
    """Resolve one (sourceName, sourceUrl) pair into a list of Stream candidates."""
    link = decode_source_url(source_url)
    name = source_name or "?"

    # --- mp4upload: scrape the mp4 src out of the embed HTML -------------------
    if "mp4upload" in link:
        try:
            r = session.get(link, headers={"Referer": REFERER}, timeout=15)
            m = re.search(r'src:\s*"([^"]*)"', r.text)
            if m:
                return [Stream(0, m.group(1), MP4UPLOAD_REFERER, "mp4upload")]
        except Exception as e:  # noqa: BLE001
            _warn(f"mp4upload resolve failed: {e}")
        return []

    # --- fast4speed / youtube: the decoded link is the stream itself ----------
    if "tools.fast4speed.rsvp" in link:
        return [Stream(0, link, REFERER, "fast4speed")]

    # --- everything else: the AllAnime apiv2 "clock" JSON endpoint -------------
    full = f"https://{ALLANIME_BASE}{link}" if link.startswith("/") else link
    try:
        r = session.get(full, headers={"Referer": REFERER}, timeout=15)
    except Exception as e:  # noqa: BLE001
        _warn(f"{name}: clock fetch failed: {e}")
        return []

    raw: list[Stream] = []
    try:
        data = r.json()
        entries = data.get("links", []) if isinstance(data, dict) else []
        for entry in entries:
            url = entry.get("link") or entry.get("url") or entry.get("src")
            if not url:
                continue
            height = _height_from(entry.get("resolutionStr", "")) or _height_from(
                str(entry.get("height", ""))
            )
            raw.append(Stream(height, url, REFERER, name))
    except (ValueError, AttributeError):
        # Regex fallback over the raw text.
        clean = r.text
        for m in re.finditer(r'"(?:link|url)":"([^"]*\.m3u8[^"]*)"', clean):
            raw.append(Stream(0, m.group(1).replace("\\/", "/"), REFERER, name))

    # --- post-process: expand wixmp urlsets and HLS masters -------------------
    streams: list[Stream] = []
    for s in raw:
        if "repackager.wixmp.com" in s.url:
            expanded = _expand_wixmp(s.url)
            streams.extend(expanded or [s])
        elif "master.m3u8" in s.url and s.height == 0:
            expanded = _expand_m3u8_master(session, s.url, s.referer)
            streams.extend(expanded or [s])
        else:
            streams.append(s)
    return streams
