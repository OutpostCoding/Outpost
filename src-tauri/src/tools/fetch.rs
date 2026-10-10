// ── surface: backend ──────────────────────────────────
// Web page fetch/extract behind a swappable trait (Tavily /extract today).
// Exports: FetchProvider, TavilyExtract · Related: tools/mod.rs, tools/search.rs

//! Web page fetch + extract behind a swappable provider interface (spec §5.1),
//! kept separate from search so the model picks shallow-snippet vs full-page per
//! need. Tavily `/extract` is the backend; the trait keeps it swappable.

use async_trait::async_trait;
use serde_json::{json, Value};

#[async_trait]
pub trait FetchProvider: Send + Sync {
    /// Fetch a URL and return its extracted text content.
    async fn fetch(&self, url: &str) -> anyhow::Result<String>;
}

pub struct TavilyExtract {
    http: reqwest::Client,
}

impl TavilyExtract {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl FetchProvider for TavilyExtract {
    async fn fetch(&self, url: &str) -> anyhow::Result<String> {
        let Some(api_key) = crate::keystore::tavily_key() else {
            anyhow::bail!(
                "no web-search key set — add a Tavily key in Settings → Web search (free at tavily.com)"
            );
        };

        let body = json!({ "api_key": api_key, "urls": [url] });

        let resp = self
            .http
            .post("https://api.tavily.com/extract")
            .bearer_auth(&api_key)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            anyhow::bail!("Tavily extract returned {status}: {detail}");
        }

        let v: Value = resp.json().await?;
        if let Some(content) = v["results"][0]["raw_content"].as_str() {
            return Ok(content.to_string());
        }
        if let Some(content) = v["results"][0]["content"].as_str() {
            return Ok(content.to_string());
        }
        if let Some(err) = v["failed_results"][0]["error"].as_str() {
            anyhow::bail!("extract failed: {err}");
        }
        anyhow::bail!("extract returned no content for {url}");
    }
}
