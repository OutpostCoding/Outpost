// ── surface: backend ──────────────────────────────────
// API keys in the OS credential store (keyring); the webview only learns "a key is set".
// Exports: set_key, get_key, has_key, tavily_key, delete_key · Related: commands.rs

//! Secure API-key storage in the OS credential store (Windows Credential Manager
//! / macOS Keychain / Linux secret service) via the `keyring` crate. Keys are
//! keyed by connection id (e.g. "anthropic", "mistral"). They never touch the
//! webview's storage — only "is a key set" is ever exposed to the UI.

use keyring::Entry;

/// Credential-store service name; connection id is the per-entry "user".
const SERVICE: &str = "outpost";

fn entry(id: &str) -> keyring::Result<Entry> {
    Entry::new(SERVICE, id)
}

/// Store (or overwrite) a connection's API key.
pub fn set_key(id: &str, key: &str) -> anyhow::Result<()> {
    entry(id)?.set_password(key)?;
    Ok(())
}

/// Fetch a connection's API key, if one is stored.
pub fn get_key(id: &str) -> Option<String> {
    if let Ok(e) = entry(id) {
        if let Ok(pwd) = e.get_password() {
            return Some(pwd);
        }
    }
    None
}

/// Whether a key is stored for this connection.
pub fn has_key(id: &str) -> bool {
    get_key(id).is_some()
}

/// The Tavily web-search key — the in-app keychain entry takes precedence, then
/// the `TAVILY_API_KEY` env var (legacy `.env` / dev). Read per-call so it can be
/// set or changed in Settings without a restart.
pub fn tavily_key() -> Option<String> {
    get_key("tavily").or_else(|| {
        std::env::var("TAVILY_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
    })
}

/// Remove a connection's stored key (no-op if absent).
pub fn delete_key(id: &str) -> anyhow::Result<()> {
    match entry(id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
