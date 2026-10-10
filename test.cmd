@echo off
rem Outpost backend tests — shim forwarding to src-tauri\test.cmd (the manifest
rem wrapper; bare `cargo test` cannot work here — see that file's header).
rem Usage: test.cmd [args...]   (e.g. test.cmd -- --nocapture)
cd /d "%~dp0src-tauri"
call test.cmd %*
