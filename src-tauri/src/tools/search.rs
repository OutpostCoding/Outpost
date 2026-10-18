// ── surface: backend ──────────────────────────────────
// Web search behind a swappable trait (Tavily today).
// Exports: SearchProvider, TavilySearch, SearchResult · Related: tools/mod.rs, tools/fetch.rs

//! Web search behind a swappable provider interface (spec §5.1). Tavily is the
//! chosen backend (search + extract, hard-stop free tier), but the trait keeps
//! it swappable to Brave/SearXNG/etc. at zero cost.

use async_trait::async_trait;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[async_trait]
pub trait SearchProvider: Send + Sync {
    async fn search(&self, query: &str) -> anyhow::Result<Vec<SearchResult>>;
}

pub struct TavilySearch {
    http: reqwest::Client,
}

impl TavilySearch {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { http }
    }
}

#[async_trait]
impl SearchProvider for TavilySearch {
    async fn search(&self, query: &str) -> anyhow::Result<Vec<SearchResult>> {
        let Some(api_key) = crate::keystore::tavily_key() else {
            anyhow::bail!(
                "no web-search key set — add a Tavily key in Settings → Web search (free at tavily.com)"
            );
        };

        // Send the key both ways (body + Bearer header) for compatibility across
        // Tavily API revisions.
        let body = json!({
            "api_key": api_key,
            "query": query,
            "max_results": 5,
            "search_depth": "basic"
        });

        let resp = self
            .http
            .post("https://api.tavily.com/search")
            .bearer_auth(&api_key)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            anyhow::bail!("Tavily returned {status}: {detail}");
        }

        let v: Value = resp.json().await?;
        let results = v["results"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|r| {
                        let url = r["url"].as_str()?;
                        Some(SearchResult {
                            title: r["title"].as_str().unwrap_or("").to_string(),
                            url: url.to_string(),
                            snippet: r["content"].as_str().unwrap_or("").to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(results)
    }
}
