// ── surface: backend ──────────────────────────────────
// Visual perception — screenshots the perception WebView2 window as PNG for the vision path.
// Exports: capture_perception_png · Related: tools/mod.rs (browser_screenshot), agent.rs

//! Visual perception — capturing what the perception WebView2 window is actually
//! rendering, as a PNG, for the vision path (see tools/mod.rs `browser_screenshot`
//! and agent.rs's synthetic-image turn).
//!
//! This is the one place we drop to raw COM. Tauri has no screenshot wrapper, so we
//! reach through `WebviewWindow::with_webview` to the `ICoreWebView2Controller`,
//! call `CapturePreview` into an in-memory `IStream`, and read the bytes back. The
//! image never touches disk.
//!
//! Two things make this fiddlier than it looks:
//!
//! 1. **Version pinning.** `webview2-com` and `windows` must be the SAME versions
//!    Tauri itself uses, or the COM types are distinct and nothing unifies. Verified
//!    against Cargo.lock: tauri 2.11.3 → webview2-com 0.38.2 → windows 0.61.3. If a
//!    Tauri bump changes those, this file stops compiling — that's the intended
//!    failure mode (loud), not a silent runtime break.
//!
//! 2. **A hidden webview may not composite.** The perception window is created with
//!    `visible(false)` (Phase B), and `CapturePreview` captures what the compositor
//!    has — so a never-shown window can yield an empty stream. We therefore park the
//!    window far offscreen, show it, capture, and hide it again. The dance lives
//!    HERE rather than in `browser_open`, deliberately: browser_open's text path is
//!    verified working and shouldn't be disturbed by a visual-only requirement.

#![cfg(windows)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{Manager, PhysicalPosition};
use webview2_com::CapturePreviewCompletedHandler;
use webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG;
use windows::Win32::System::Com::{IStream, STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET};
use windows::Win32::UI::Shell::SHCreateMemStream;

/// Label of the hidden webview Phase B creates; we capture whatever it's showing.
const PERCEPTION_WINDOW: &str = "perception";

/// How long to wait for the compositor after showing the window before capturing.
/// Too short and we photograph a blank frame; this is the one genuinely empirical
/// number in the file and the likeliest thing to need nudging on real hardware.
const COMPOSITE_SETTLE: Duration = Duration::from_millis(350);

/// Ceiling on waiting for CapturePreview's completion handler.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);

type Shot = Result<Vec<u8>, String>;

/// Hand the result to the waiting task exactly once. The completion handler and the
/// several error paths all race to finish first; whoever gets there wins and the
/// rest no-op. (Same shape as browser_read's oneshot bridge.)
fn finish(slot: &Mutex<Option<tokio::sync::oneshot::Sender<Shot>>>, outcome: Shot) {
    if let Ok(mut held) = slot.lock() {
        if let Some(tx) = held.take() {
            let _ = tx.send(outcome);
        }
    }
}

/// Drain an `IStream` into a Vec. Caller guarantees the capture completed OK.
///
/// SAFETY: `stream` is a live COM object owned by the caller; every call below is a
/// plain vtable call on it, and the read is bounded by the size `Stat` reported.
unsafe fn read_stream(stream: &IStream) -> Shot {
    let mut stat = STATSTG::default();
    unsafe { stream.Stat(&mut stat, STATFLAG_NONAME) }
        .map_err(|e| format!("couldn't size the capture: {e}"))?;

    let size = stat.cbSize as usize;
    if size == 0 {
        return Err(
            "the capture came back empty — the perception window may not be rendering yet. \
             Give the page a moment after browser_open, then try again."
                .into(),
        );
    }

    unsafe { stream.Seek(0, STREAM_SEEK_SET, None) }
        .map_err(|e| format!("couldn't rewind the capture: {e}"))?;

    let mut buf = vec![0u8; size];
    let mut read: u32 = 0;
    let hr = unsafe { stream.Read(buf.as_mut_ptr() as *mut _, size as u32, Some(&mut read)) };
    if hr.is_err() {
        return Err(format!("couldn't read the capture: {hr:?}"));
    }
    buf.truncate(read as usize);
    if buf.is_empty() {
        return Err("the capture read back zero bytes".into());
    }
    Ok(buf)
}

/// Capture the perception window as PNG bytes.
///
/// Returns `Err(message)` — already phrased for the model — on every failure path,
/// so the tool layer can hand it straight back without translating COM errors.
pub async fn capture_perception_png(app: &tauri::AppHandle) -> Shot {
    let Some(window) = app.get_webview_window(PERCEPTION_WINDOW) else {
        return Err(
            "there's no perception window open — call browser_open with your dev-server URL first, \
             then screenshot it."
                .into(),
        );
    };

    // Park it offscreen BEFORE showing, so making it composite never flashes a
    // window at the user or steals focus from the app.
    let _ = window.set_skip_taskbar(true);
    let _ = window.set_position(PhysicalPosition::new(-32_000, -32_000));
    let shown = window.show().is_ok();
    tokio::time::sleep(COMPOSITE_SETTLE).await;

    let (tx, rx) = tokio::sync::oneshot::channel::<Shot>();
    let slot = Arc::new(Mutex::new(Some(tx)));

    // with_webview hops to the main thread; everything COM happens inside.
    let dispatched = {
        let slot = slot.clone();
        window.with_webview(move |webview| {
            let controller = webview.controller();

            // SAFETY: all of these are vtable calls on live COM objects handed to us
            // by Tauri (controller) or created here (stream), on the main thread,
            // which is where WebView2 requires them.
            unsafe {
                let core = match controller.CoreWebView2() {
                    Ok(core) => core,
                    Err(e) => {
                        finish(&slot, Err(format!("no CoreWebView2 on the controller: {e}")));
                        return;
                    }
                };
                let Some(stream) = SHCreateMemStream(None) else {
                    finish(&slot, Err("couldn't allocate a capture buffer".into()));
                    return;
                };

                let handler = {
                    let slot = slot.clone();
                    let stream = stream.clone();
                    // webview2-com's ClosureArg impl turns the raw HRESULT into a
                    // Result before it reaches us, so this takes Result<(), Error>.
                    CapturePreviewCompletedHandler::create(Box::new(move |result| {
                        match result {
                            Ok(()) => finish(&slot, read_stream(&stream)),
                            Err(e) => finish(&slot, Err(format!("CapturePreview failed: {e}"))),
                        }
                        Ok(())
                    }))
                };

                if let Err(e) = core.CapturePreview(
                    COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
                    &stream,
                    &handler,
                ) {
                    finish(&slot, Err(format!("couldn't start the capture: {e}")));
                }
            }
        })
    };

    if let Err(e) = dispatched {
        finish(&slot, Err(format!("couldn't reach the perception webview: {e}")));
    }

    let outcome = match tokio::time::timeout(CAPTURE_TIMEOUT, rx).await {
        Ok(Ok(shot)) => shot,
        Ok(Err(_)) => Err("the capture was dropped before it completed".into()),
        Err(_) => Err("the capture timed out".into()),
    };

    // Always put the window back the way Phase B expects to find it.
    if shown {
        let _ = window.hide();
    }
    outcome
}
