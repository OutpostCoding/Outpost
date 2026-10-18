// ── surface: backend ──────────────────────────────────
// Head prompt parts + coding orientation — env sentence, model identity, today's date (archive/DESIGN_OLD.md §3.1/§3.2).
// Exports: grounding_message, coding_orientation, connection_kind, resolve_timezone, display_datetime · Related: agent.rs, commands.rs

//! Environment and current-date grounding (spec §10e). Models anchor to their
//! training cutoff and reason/search relative to it; injecting "today is …" each
//! The timezone is configurable via `OUTPOST_TIMEZONE` (an IANA name),
//! defaulting to the host OS timezone (or UTC).
//!
//! The environment half was added because the app's NAME had leaked into the
//! prompt before it was ever introduced: the `<memory>` block says "inserted
//! automatically by Outpost", and a model that has never been told what Outpost
//! is has been handed a proper noun instead of an explanation — the opposite of
//! what that wrapper is for. Anything else that names the app to a model can now
//! assume the referent exists.
//!
//! The line itself was reframed 2026-08-24: it states WHAT the app is in the
//! third person ("Outpost is a desktop app for chat and coding that runs on the
//! user's own machine") — deliberately not "You are running inside Outpost, a
//! local-first …". The old phrasing made the MODEL the subject of a locality
//! claim ("you … on the user's own machine"), so models locked onto it and read
//! themselves as locally hosted even on a hosted connection. Where the model
//! RUNS is the session-facts line's job ("local connection" / "hosted connection
//! (openai API)"), never the environment line — and the environment line makes
//! no role claim either ("you are its AI assistant" would pre-empt whatever
//! system prompt the user supplies).
//!
//! Head vs tail (archive/DESIGN_OLD.md §3.2): the head carries ONE merged grounding block — the
//! environment sentence, the model identity ("Current model: …") and the current DATE, day-granular
//! only — chosen to be session-static, so the prompt-cache prefix holds until the date rolls over. The
//! clock that briefly rode the TAIL is gone from the prompt entirely (2026-09-04): a per-minute value
//! invalidated everything after it on essentially every turn. The date line is a BARE MECHANICAL FACT
//! (2026-08-30): the present-moment clause it carried ("treat this as the present moment… do not
//! assume your training cutoff…") made very small models (Nemotron 4b) loop on the INSTRUCTION —
//! re-asserting the date fact instead of doing the work. A date in a system message already reads as
//! harness-provided present time; the clause added a thing to obsess over without adding meaning. The
//! UI readout is served separately by `display_datetime`, never the prompt.

use chrono::Utc;
use chrono_tz::Tz;


/// Mirrors `productName` in `tauri.conf.json`. Hardcoded rather than read from
/// the config because reaching it needs an `AppHandle` threaded through both
/// call sites for a string that does not change — the identifier and product
/// name are deliberately fixed in this project. Change both together.
const APP_NAME: &str = "Outpost";

pub fn resolve_timezone() -> Tz {
    std::env::var("OUTPOST_TIMEZONE")
        .ok()
        .or_else(|| iana_time_zone::get_timezone().ok())
        .and_then(|s| s.trim().parse::<Tz>().ok())
        .unwrap_or(chrono_tz::UTC)
}

/// The environment-only opening shared by both surfaces. Third-person facts
/// about the APP only: what Outpost is and where it runs. Deliberately not
/// "You are running inside …" — a model-subject locality claim reads as "I am
/// local", which is wrong on a hosted connection; the session-facts line states
/// where the model runs as data. No role line either: "you are its AI
/// assistant" would pre-empt the user's own system prompt. Built from
/// `APP_NAME` so the non-test build also enforces that the app name appears.
fn env_line() -> String {
    format!(
        "{APP_NAME} is a desktop app for chat and coding that runs on the \
         user's own machine."
    )
}

/// The merged HEAD block for BOTH surfaces: the environment sentence, then the
/// model identity on its own line (2026-08-29 — the two items the inspector
/// used to show separately, "environment" and "session facts", now ship as one
/// block).
/// "Current model: …" states identity as DATA (the §3.1 "model: …" framing,
/// §5.3), never "You are …" — identity-as-assertion amnesia in small models.
/// `connection_kind` is "local" or "hosted" (see `connection_kind`).
/// We deliberately do not state wire protocol dialect ("openai" / "anthropic")
/// because wire format is an internal client transport detail; declaring
/// "hosted connection (anthropic API)" or "(openai API)" confuses multi-provider
/// models (e.g. Xiaomi MiMo, DeepSeek, Qwen) into hallucinating false host or
/// creator identities. `model` empty (the "Auto" case, composer mid connection-switch)
/// → environment only, no dangling identity.
///
/// Session-static to the day: the DATE rides here, day-granular ("Today is
/// Friday, 4 September 2026."), so the prompt-cache PREFIX survives every turn
/// except the rollover at midnight. No clock time (a per-minute value would
/// invalidate everything after it — profile, orientation, and the entire
/// transcript) and no approval posture (it changes mid-session, §7.4); the UI
/// clock is served separately by `display_datetime`, never the prompt.
/// Otherwise terse (2026-08-19/24 trims): no toolchain line (`project_info` reports toolchains
/// in one call), no /compact note (user-facing workflow), no pinned-docs
/// clause (the tail block header covers it).
pub fn grounding_message(model: &str, connection_kind: &str, _dialect: &str, tz: chrono_tz::Tz) -> String {
    let mut msg = env_line();
    if !model.trim().is_empty() {
        let conn = if connection_kind == "local" {
            "local connection"
        } else {
            "hosted connection"
        };
        msg.push_str(&format!("\nCurrent model: {model}, {conn}."));
    }
    let now = chrono::Utc::now().with_timezone(&tz);
    let date_str = now.format("%A, %-d %B %Y");
    msg.push_str(&format!("\nToday is {date_str}."));
    msg
}

/// Detect whether a URL points to localhost, a LAN address, or an RFC1918 subnet.
pub(crate) fn is_local_url(url: &str) -> bool {
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("")
        .rsplit('@') // strip any user:pass@
        .next()
        .unwrap_or("")
        .trim_start_matches('[');
    let host = host.split(']').next().unwrap_or(host); // ipv6 literal
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();

    if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "0.0.0.0") {
        return true;
    }
    // A bare machine name (no dot) or an mDNS/LAN suffix is a local box.
    if !host.contains('.') || host.ends_with(".local") || host.ends_with(".lan") {
        return true;
    }
    // RFC1918 private ranges.
    let octets: Vec<u8> = host.split('.').filter_map(|p| p.parse::<u8>().ok()).collect();
    match octets.as_slice() {
        [10, _, _, _] => true,
        [192, 168, _, _] => true,
        [172, b, _, _] => (16..=31).contains(b),
        _ => false,
    }
}

/// Classify the connection for the identity line by WHERE the model actually
/// runs, not merely whether a base_url is set: "local" for the app's default
/// LM Studio (empty base_url) and for any base_url on this machine or the LAN
/// (localhost, bare hostnames, `.local`/`.lan`, RFC1918), else "hosted" (a cloud endpoint).
/// A local server mislabeled "hosted" reads as a claim that the model runs in the cloud,
/// which models take literally.
pub fn connection_kind(base_url: Option<&str>) -> &'static str {
    match base_url {
        Some(b) if !b.trim().is_empty() && !is_local_url(b.trim()) => "hosted",
        _ => "local",
    }
}


/// The coding-session orientation: WHERE the model works, exploration guidance, working memory lifecycle, and permissions posture.
/// Session-static so it sits cleanly in the cached prefix.
pub fn coding_orientation(root: &str, auto_writes: bool) -> String {
    let mut text = format!(
        "You are in a coding session rooted at: {root}.\n\
         Tool Selection Contract (always prefer specialized native tools over shell commands):\n\
         - Exploration & Search: Start with `code_search` as your primary discovery tool to locate relevant logic, concepts, and features across the codebase. Reach for `grep_search` only when you already know an exact identifier, symbol, or regex and need an exhaustive list of all call sites. Never run shell search commands (grep, findstr, find).\n\
         - Inspection: ALWAYS use `read_file` (pass `start` and `lines` when reading specific ranges of large files) or `find_symbols` (for structural outlines). Never use shell commands to view files (cat, type, head, tail).\n\
         - Modification: ALWAYS use `edit_file` (for targeted changes with snippet matching or symbol replacement) or `write_file` (to create or overwrite whole files). Never use shell commands or ad-hoc scripts to edit files.\n\
         - Execution & Git: `run_command` is reserved for executing test suites, build tools, package managers, git commands, and launching services (e.g. `cargo check`, `npm test`, `python main.py`). Read-only git inspection (`git status`, `git log`, `git diff`, `git show`, `git blame`) is auto-approved without prompting. Before running `git commit`, inspect staged changes with `git diff --staged` or `git status` (or `get_diff`) to verify your modifications.\n\
         Working memory: tool results from earlier turns are archived into compact receipts (`receipt — read_file(...) returned...`) to conserve context. This is standard harness operation, not a tool failure. Re-read files or inspect symbols at any time as needed.\n\
         File safety: Outpost automatically snapshots files before editing or deleting. Use `get_diff` to inspect diffs and `restore_file` to undo changes when needed."
    );
    if auto_writes {
        text.push_str("\nPermissions: Auto-approval is active for `write_file` and `edit_file` — you can create, modify, and scaffold files directly without pausing for confirmation.");
    }
    text
}



/// A compact current date/time string for the UI readout.
pub fn display_datetime(tz: Tz) -> String {
    Utc::now()
        .with_timezone(&tz)
        .format("%a %-d %b %Y, %H:%M %Z")
        .to_string()
}

