@echo off
rem Run the Rust test suite.
rem
rem WHY THIS EXISTS: tauri-build embeds a Windows manifest only into the app bin,
rem so a bare `cargo test` exe gets none. Without a common-controls v6 manifest
rem the loader binds System32\comctl32.dll, which does NOT export
rem TaskDialogIndirect (the export lives only in the WinSxS common-controls v6
rem assembly) — so any test exe linking tauri-plugin-dialog's rfd dies at
rem startup: 0xc0000139. The RUSTFLAGS below embed test-manifest.xml (the v6
rem dependency) into every unit cargo builds.
rem
rem Why /MANIFESTINPUT with a SHORT path: RUSTFLAGS applies to EVERY unit cargo
rem builds, including dependency build scripts, which link with the registry
rem cache as their cwd — a relative manifest file can't be found there. RUSTFLAGS
rem has no quote support for a spaced path, so %~sdp0 (the script's 8.3 short
rem path — this volume has 8.3 names enabled) keeps the flag space-free. The
rem /MANIFESTDEPENDENCY alternative fails too: without spaces link.exe emits a
rem manifest the SxS loader rejects (os error 14001).
rem
rem `--lib` matters: all tests live in the lib unittest exe. Building the
rem main-bin unittest exe would get these flags AND tauri's bin-scoped manifest
rem on one link (CVT1100: duplicate resource, type MANIFEST), so plain
rem `cargo test` does not work here.
rem
rem QUIET BY DEFAULT: with no args this runs `cargo test -q` plus libtest's
rem `--format terse`, so the output is just failures + the `test result:`
rem summary — small enough that an output-capped harness keeps the one line
rem that matters.
rem
rem Usage: test.cmd [cargo test args...]   (e.g. test.cmd -- --nocapture)
rem The shim at the repo root forwards here, so `test.cmd` works from either.
cd /d "%~dp0"
set RUSTFLAGS=-C link-arg=/MANIFEST:EMBED -C link-arg=/MANIFESTINPUT:%~sdp0test-manifest.xml
if "%~1"=="" (
  cargo test --lib -q -- --format terse
) else (
  cargo test --lib %*
)
