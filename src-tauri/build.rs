fn main() {
    // Re-run this build script — and re-embed the window/app icon — whenever anything
    // in the icons folder changes. Tauri's default build doesn't track the icon files,
    // so a regenerated icon otherwise stays cached in the compiled binary until a
    // manual `cargo clean`. Tracking the dir makes icon swaps apply on the next build.
    println!("cargo:rerun-if-changed=icons");

    // NOTE — test-exe manifest: TEST binaries get no manifest from tauri-build (it
    // embeds one into the main bin only, via a bin-scoped .res). Without a
    // common-controls v6 manifest, the loader binds a test exe to
    // System32\comctl32.dll — which does NOT export TaskDialogIndirect (that
    // export lives only in the WinSxS common-controls v6 assembly) — so a test exe
    // that links tauri-plugin-dialog's rfd path dies at startup (0xc0000139).
    // The fix lives in test.cmd: RUSTFLAGS with /MANIFESTINPUT + the script's
    // 8.3 short path (RUSTFLAGS applies to every unit cargo builds — dependency
    // build scripts link from the registry cache, so the path must be absolute
    // and space-free). build.rs cannot scope this: a build script's PROFILE env
    // is "debug" for BOTH cargo test and cargo build (and CARGO_CFG_TEST is
    // never set), and unscoped build.rs link-args would collide with tauri's
    // manifest in the app bin (CVT1100 duplicate resource). Run tests with
    // `test.cmd` (repo root shim, or directly in src-tauri), not `cargo test`.
    tauri_build::build()
}
