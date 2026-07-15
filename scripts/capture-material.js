// Paste into DevTools console on mkissa.to after WAF (normal UI visible).
// Automatic: scrapes mask/buildId from same-origin JS + fetches bootstrap for partB/epoch.
//
// Save: pbpaste > ~/.config/ani-dl/material.json
// Status: banner bottom-right + window.__aniDlCapture
//
// Ignore console noise about cloudflareinsights / analytics — those are skipped.

window.__aniDlCapture = { ok: false, status: "running" };

function aniDlCaptureBanner(title, body, ok) {
  const id = "ani-dl-capture-status";
  let el = document.getElementById(id);
  if (!el) {
    el = document.createElement("pre");
    el.id = id;
    Object.assign(el.style, {
      position: "fixed",
      bottom: "12px",
      right: "12px",
      zIndex: "2147483647",
      maxWidth: "min(640px, 90vw)",
      maxHeight: "40vh",
      overflow: "auto",
      margin: "0",
      padding: "12px",
      background: "#1a2a3d",
      color: "#fff",
      border: "2px solid #64b5f6",
      borderRadius: "8px",
      fontSize: "12px",
      lineHeight: "1.4",
      whiteSpace: "pre-wrap",
      wordBreak: "break-word",
    });
    document.body.appendChild(el);
  }
  if (ok === true) {
    el.style.background = "#1a3d1a";
    el.style.borderColor = "#4caf50";
  } else if (ok === false) {
    el.style.background = "#3d1a1a";
    el.style.borderColor = "#f44336";
  } else {
    el.style.background = "#1a2a3d";
    el.style.borderColor = "#64b5f6";
  }
  el.textContent = title + "\n\n" + body;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const SKIP_HOST =
  /cloudflareinsights|google-analytics|googletagmanager|facebook|hotjar|sentry\.io|newrelic|segment\.|clarity\.ms|doubleclick/i;

function isAppScriptUrl(url) {
  if (!url || url === "<inline>") return true;
  let u;
  try {
    u = new URL(url, location.href);
  } catch {
    return false;
  }
  if (SKIP_HOST.test(u.hostname)) return false;
  // Same origin always OK (no CORS).
  if (u.origin === location.origin) return true;
  // Related CDNs that often host the SPA chunks.
  if (/\b(allanime|allmanga|mkissa|youtu-chan|cdn\.allanime)\b/i.test(u.hostname)) {
    return true;
  }
  return false;
}

function isValidAaCrypto(c) {
  return (
    c &&
    typeof c === "object" &&
    typeof c.partB === "string" &&
    c.partB.length > 0 &&
    (typeof c.switchAt !== "number" || Date.now() < c.switchAt)
  );
}

async function fetchBootstrap(buildId) {
  const url =
    location.origin +
    "/client-crypto/v1/bootstrap?buildId=" +
    encodeURIComponent(buildId);
  const errors = [];

  for (const credentials of ["omit", "include"]) {
    try {
      const r = await fetch(url, {
        method: "GET",
        credentials,
        cache: "no-store",
        headers: { "x-build-id": buildId },
      });
      if (!r.ok) {
        errors.push(`${credentials}: HTTP ${r.status}`);
        continue;
      }
      const n = await r.json();
      if (n?.partB) {
        window.__aaCrypto = n;
        return n;
      }
      errors.push(`${credentials}: response missing partB`);
    } catch (e) {
      errors.push(`${credentials}: ${e.message || e}`);
    }
  }

  throw new Error("bootstrap failed (" + url + "): " + errors.join("; "));
}

async function resolveAaCrypto(buildId) {
  // Prefer live window state if the SPA already bootstrapped.
  if (isValidAaCrypto(window.__aaCrypto)) return window.__aaCrypto;

  for (let attempt = 0; attempt < 3; attempt++) {
    try {
      return await fetchBootstrap(buildId);
    } catch (_) {
      if (attempt === 2) break;
      await sleep(1000);
    }
  }

  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    await sleep(500);
    if (isValidAaCrypto(window.__aaCrypto)) return window.__aaCrypto;
  }

  throw new Error(
    "could not fetch /client-crypto/v1/bootstrap — pass WAF, open a show page, set console to 'top'",
  );
}

async function loadBundleOnce() {
  const seen = new Set();
  const parts = [];
  const loaded = [];

  const add = (url, text) => {
    if (!text || seen.has(url)) return;
    seen.add(url);
    parts.push(text);
    loaded.push({ url, bytes: text.length });
  };

  const tryFetch = async (url) => {
    if (!isAppScriptUrl(url) || seen.has(url)) return;
    try {
      const r = await fetch(url, { credentials: "same-origin", cache: "force-cache" });
      if (r.ok) add(url, await r.text());
      else loaded.push({ url, error: "HTTP " + r.status });
    } catch (e) {
      // Cross-origin without CORS — skip quietly (do not surface as capture failure).
      loaded.push({ url, error: "skip: " + (e.message || e) });
    }
  };

  for (const s of document.scripts) {
    if (s.textContent && s.textContent.length > 50) {
      add(s.src || "<inline>", s.textContent);
    }
    if (s.src) await tryFetch(s.src);
  }

  for (const e of performance.getEntriesByType("resource")) {
    if (e.initiatorType !== "script" || !e.name) continue;
    await tryFetch(e.name);
  }

  return { text: parts.join("\n"), loaded };
}

function extractMask(source) {
  for (const hm of source.matchAll(/hasMask:\!\!(\w+)/g)) {
    const re = new RegExp(
      `(?:const|let|var)\\s+${hm[1]}\\s*=\\s*"([0-9a-f]{64})"`,
      "i",
    );
    const m = source.match(re);
    if (m) return m[1].toLowerCase();
  }

  // Known layout from current AllAnime chunks: const Ju="<64 hex>",sr=...
  const ju = source.match(
    /(?:const|let|var)\s+\w+\s*=\s*"([0-9a-f]{64})"\s*,\s*\w+\s*=\s*[^=]*!==\s*"string"\s*\?\s*"\d+"/i,
  );
  if (ju) return ju[1].toLowerCase();

  const near = source.search(/hasMask|partBLen|client-crypto|__aaCrypto/i);
  if (near >= 0) {
    const ctx = source.slice(Math.max(0, near - 800), near + 800);
    const m = ctx.match(/"([0-9a-f]{64})"/i);
    if (m) return m[1].toLowerCase();
  }

  return null;
}

function extractBuildId(source) {
  const m =
    source.match(/!==\s*"string"\?\s*"(\d+)"\s*:\s*""/) ||
    source.match(/!==\s*"string"\s*\?\s*"(\d+)"\s*:\s*""/);
  if (m) return m[1];

  const lit = source.match(/buildId:\s*\w+\|\|\s*"(\d+)"/);
  if (lit) return lit[1];

  const boot = source.match(/bootstrap\?buildId=/);
  if (boot) {
    const near = source.slice(boot.index, boot.index + 200);
    const n = near.match(/"(\d{1,4})"/);
    if (n) return n[1];
  }

  return null;
}

window.__aniDlCapturePromise = (async () => {
  try {
    aniDlCaptureBanner("ani-dl capture", "loading same-origin JS…", null);

    let bundle = "";
    let loaded = [];
    const deadline = Date.now() + 20000;
    while (Date.now() < deadline) {
      ({ text: bundle, loaded } = await loadBundleOnce());
      if (extractMask(bundle) && extractBuildId(bundle)) break;
      await sleep(500);
    }

    const okLoaded = loaded.filter((x) => x.bytes);
    if (!bundle || okLoaded.length === 0) {
      throw new Error(
        "no readable app scripts (CORS blocks third-party; need same-origin chunks)\n" +
          "Open a show page, wait for UI, console context = top, re-paste.\n\n" +
          loaded
            .slice(0, 20)
            .map((x) => "  " + x.url + (x.error ? " → " + x.error : " → " + x.bytes + "B"))
            .join("\n"),
      );
    }

    const mask = extractMask(bundle);
    if (!mask) {
      throw new Error(
        "mask not found in " +
          okLoaded.length +
          " script(s) / " +
          bundle.length +
          " bytes — bundle layout may have changed",
      );
    }

    const buildId = extractBuildId(bundle);
    if (!buildId) throw new Error("buildId not found in bundle");

    aniDlCaptureBanner(
      "ani-dl capture",
      `mask+buildId=${buildId} ok — fetching bootstrap…`,
      null,
    );

    const c = await resolveAaCrypto(buildId);

    const material = {
      epoch: c.epoch,
      partB: c.partB,
      mask,
      buildId,
      referer: location.origin,
      apiBase: "https://api.allanime.day",
      cdnBase: "https://cdn.allanime.day",
      expiresAt: c.switchAt,
      graceMs: 300000,
    };

    const json = JSON.stringify(material, null, 2);
    if (typeof copy === "function") copy(json);

    window.__aniDlCapture = { ok: true, material, json, loaded: okLoaded };
    aniDlCaptureBanner(
      "ani-dl capture OK — save to ~/.config/ani-dl/material.json",
      json,
      true,
    );
    console.log("ani-dl capture OK — window.__aniDlCapture");
    return material;
  } catch (err) {
    const msg = err && err.stack ? err.stack : String(err);
    window.__aniDlCapture = { ok: false, error: msg };
    aniDlCaptureBanner("ani-dl capture FAILED", msg, false);
    console.error("ani-dl capture FAILED — window.__aniDlCapture");
    throw err;
  }
})();
