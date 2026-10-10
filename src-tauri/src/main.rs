// ── surface: backend ──────────────────────────────────
// Entry point — the no-console-window attribute is required in release. DO NOT REMOVE.
// Exports: main · Related: lib.rs

// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    frontend_lib::run()
}
