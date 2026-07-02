//! AllAnime GraphQL client (async, connection-pooled reqwest).

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, ORIGIN, REFERER as H_REFERER, USER_AGENT as H_UA};
use serde_json::json;

use crate::constants::{ALLANIME_API, EPISODE_QUERY_HASH, REFERER, USER_AGENT};
use crate::providers::{self, SourceUrl};

// GraphQL documents copied verbatim from the Python `api.py`.
const SEARCH_GQL: &str = "query( $search: SearchInput $limit: Int $page: Int \
    $translationType: VaildTranslationTypeEnumType \
    $countryOrigin: VaildCountryOriginEnumType ) { \
    shows( search: $search limit: $limit page: $page \
    translationType: $translationType countryOrigin: $countryOrigin ) { \
    edges { _id name availableEpisodes __typename } } }";

const EPISODES_LIST_GQL: &str = "query ($showId: String!) { show( _id: $showId ) \
    { _id availableEpisodesDetail } }";

const EPISODE_EMBED_GQL: &str = "query ($showId: String!, \
    $translationType: VaildTranslationTypeEnumType!, $episodeString: String!) { \
    episode( showId: $showId translationType: $translationType \
    episodeString: $episodeString ) { episodeString sourceUrls } }";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationType {
    Sub,
    Dub,
}

impl TranslationType {
    pub fn as_str(&self) -> &'static str {
        match self {
            TranslationType::Sub => "sub",
            TranslationType::Dub => "dub",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ShowResult {
    pub id: String,
    pub name: String,
    pub episodes: u32,
}

pub struct AllAnimeClient {
    pub client: reqwest::Client,
}

fn default_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(H_UA, HeaderValue::from_static(USER_AGENT));
    headers.insert(H_REFERER, HeaderValue::from_static(REFERER));
    headers.insert(ORIGIN, HeaderValue::from_static(REFERER));
    headers
}

/// Client for video downloads. Unlike the API client, this must NOT set a
/// total request timeout — reqwest's `timeout` covers the entire body read,
/// which kills any download longer than the limit ("error decoding response
/// body"). Downloads instead get a connect timeout and a per-read stall
/// timeout, so a hung connection still errors out but a slow multi-minute
/// download does not.
pub fn download_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .default_headers(default_headers())
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .build()
        .context("building download client")
}

impl AllAnimeClient {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .default_headers(default_headers())
            .timeout(Duration::from_secs(20))
            .build()
            .context("building reqwest client")?;
        Ok(Self { client })
    }

    pub async fn search(
        &self,
        query: &str,
        mode: TranslationType,
    ) -> Result<Vec<ShowResult>> {
        // AllAnime's search index chokes on apostrophes (even exact titles like
        // "Hell's Paradise" return nothing), but treats them as token breaks in
        // its own data — so map them to spaces and collapse the whitespace.
        let query = query
            .replace(['\'', '\u{2018}', '\u{2019}', '`'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let variables = json!({
            "search": { "allowAdult": false, "allowUnknown": false, "query": query },
            "limit": 40,
            "page": 1,
            "translationType": mode.as_str(),
            "countryOrigin": "ALL",
        });
        let body = json!({ "variables": variables, "query": SEARCH_GQL });
        let resp = self
            .client
            .post(format!("{ALLANIME_API}/api"))
            .json(&body)
            .send()
            .await
            .context("search request failed")?;
        let v: serde_json::Value = resp.json().await.context("search: bad JSON")?;

        let mut out = Vec::new();
        if let Some(edges) = v
            .get("data")
            .and_then(|d| d.get("shows"))
            .and_then(|s| s.get("edges"))
            .and_then(|e| e.as_array())
        {
            for e in edges {
                let count = e
                    .get("availableEpisodes")
                    .and_then(|a| a.get(mode.as_str()))
                    .and_then(|n| n.as_u64())
                    .unwrap_or(0) as u32;
                if count == 0 {
                    continue;
                }
                out.push(ShowResult {
                    id: e.get("_id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    name: e.get("name").and_then(|x| x.as_str()).unwrap_or("?").to_string(),
                    episodes: count,
                });
            }
        }
        Ok(out)
    }

    pub async fn episode_list(
        &self,
        show_id: &str,
        mode: TranslationType,
    ) -> Result<Vec<String>> {
        let body = json!({
            "variables": { "showId": show_id },
            "query": EPISODES_LIST_GQL,
        });
        let resp = self
            .client
            .post(format!("{ALLANIME_API}/api"))
            .json(&body)
            .send()
            .await
            .context("episode_list request failed")?;
        let v: serde_json::Value = resp.json().await.context("episode_list: bad JSON")?;

        let mut eps: Vec<String> = v
            .get("data")
            .and_then(|d| d.get("show"))
            .and_then(|s| s.get("availableEpisodesDetail"))
            .and_then(|d| d.get(mode.as_str()))
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        eps.sort_by(|a, b| {
            let fa: f64 = a.parse().unwrap_or(0.0);
            let fb: f64 = b.parse().unwrap_or(0.0);
            fa.partial_cmp(&fb).unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(eps)
    }

    pub async fn episode_sources(
        &self,
        show_id: &str,
        ep: &str,
        mode: TranslationType,
    ) -> Result<Vec<SourceUrl>> {
        let variables = json!({
            "showId": show_id,
            "translationType": mode.as_str(),
            "episodeString": ep,
        });
        let extensions = json!({
            "persistedQuery": { "version": 1, "sha256Hash": EPISODE_QUERY_HASH }
        });

        // Try the persisted-query GET first.
        let mut text = String::new();
        if let Ok(resp) = self
            .client
            .get(format!("{ALLANIME_API}/api"))
            .query(&[
                ("variables", variables.to_string()),
                ("extensions", extensions.to_string()),
            ])
            .send()
            .await
        {
            text = resp.text().await.unwrap_or_default();
        }

        // Fall back to a plain POST of the full query.
        if !text.contains("tobeparsed") && !text.contains("sourceUrl") {
            let body = json!({ "variables": variables, "query": EPISODE_EMBED_GQL });
            let resp = self
                .client
                .post(format!("{ALLANIME_API}/api"))
                .json(&body)
                .send()
                .await
                .context("episode_sources POST failed")?;
            text = resp.text().await.unwrap_or_default();
        }

        Ok(providers::parse_source_urls(&text))
    }
}
