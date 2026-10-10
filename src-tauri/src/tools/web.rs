// ── surface: backend ──────────────────────────────────
// Web tools: search, fetch, and browser perception.
// Exports: Tools::web_search, Tools::web_fetch, Tools::browser_*

use serde_json::Value;
use super::*;

impl Tools {
    pub(crate) async fn web_search(&self, arguments: &str) -> String {
        let query = serde_json::from_str::<Value>(arguments)
            .ok()
            .and_then(|v| v["query"].as_str().map(str::to_string));
        let Some(query) = query else {
            return "error: web_search requires a 'query' string argument".into();
        };
        match self.search.search(&query).await {
            Ok(results) if results.is_empty() => "No results found.".into(),
            Ok(results) => results
                .iter()
                .enumerate()
                .map(|(i, r)| format!("{}. {}\n   {}\n   {}", i + 1, r.title, r.url, r.snippet))
                .collect::<Vec<_>>()
                .join("\n\n"),
            Err(e) => format!("search error: {e}"),
        }
    }

    pub(crate) async fn web_fetch(&self, arguments: &str) -> String {
        let url = serde_json::from_str::<Value>(arguments)
            .ok()
            .and_then(|v| v["url"].as_str().map(str::to_string));
        let Some(url) = url else {
            return "error: web_fetch requires a 'url' string argument".into();
        };
        let max_chars = parse_max_chars(arguments, MAX_FETCH_CHARS);
        match self.fetch.fetch(&url).await {
            Ok(content) if content.chars().count() > max_chars => {
                let truncated: String = content.chars().take(max_chars).collect();
                format!(
                    "{truncated}\n\n[content truncated at {max_chars} chars — pass max_chars \
                     to raise the cap (max {MAX_CHARS_CEILING})]"
                )
            }
            Ok(content) => content,
            Err(e) => format!("fetch error: {e}"),
        }
    }

    /// Run Python in the webview's offline sandbox (Pyodide). Emits a RunCode

    pub(crate) async fn browser_screenshot(&self) -> String {
        use base64::Engine as _;

        let png = match crate::capture::capture_perception_png(&self.app).await {
            Ok(png) => png,
            Err(msg) => return format!("error: {msg}"),
        };
        if png.len() > MAX_IMAGE_BYTES {
            return format!(
                "error: the screenshot came out at {:.1} MB, over the {:.1} MB limit. Make the browser view smaller and try again.",
                png.len() as f64 / 1_000_000.0,
                MAX_IMAGE_BYTES as f64 / 1_000_000.0
            );
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
        self.queue_image(format!("data:image/png;base64,{encoded}"));
        format!(
            "Captured the browser view ({:.0} KB) — screenshot attached for inspection this turn.",
            png.len() as f64 / 1000.0
        )
    }

    /// WebView2 is Windows-only, so there's nothing to capture elsewhere.
    #[cfg(not(windows))]
    pub(crate) async fn browser_screenshot(&self) -> String {
        "error: browser_screenshot needs WebView2, which is only available on Windows.".into()
    }


    pub(crate) async fn browser_open(&self, arguments: &str) -> String {
        // A console-capture shim, injected at document-start of every page load so
        // browser_read can surface runtime errors, not just the DOM text.
        const CONSOLE_SHIM: &str = r#"(function(){
if(window.__perc)return;
window.__perc={logs:[]};
var push=function(level,msg){try{window.__perc.logs.push({level:level,msg:String(msg)});if(window.__perc.logs.length>300)window.__perc.logs.shift();}catch(e){}};
['log','info','warn','error','debug'].forEach(function(level){
  var orig=console[level];
  console[level]=function(){
    try{var m=Array.prototype.slice.call(arguments).map(function(a){try{return typeof a==='string'?a:JSON.stringify(a);}catch(e){return String(a);}}).join(' ');push(level,m);}catch(e){}
    if(orig)return orig.apply(console,arguments);
  };
});
window.addEventListener('error',function(e){push('error',(e.message||'error')+' @ '+(e.filename||'')+':'+(e.lineno||''));});
window.addEventListener('unhandledrejection',function(e){push('error','unhandled rejection: '+((e.reason&&e.reason.message)?e.reason.message:String(e.reason)));});
})();"#;

        let url_str = serde_json::from_str::<Value>(arguments)
            .ok()
            .and_then(|v| v["url"].as_str().map(str::to_string))
            .filter(|u| !u.trim().is_empty());
        let Some(url_str) = url_str else {
            return "error: browser_open requires a 'url' (e.g. 'http://localhost:5173').".into();
        };
        let trimmed = url_str.trim();
        let normalized = if !trimmed.contains("://") {
            format!("http://{trimmed}")
        } else {
            trimmed.to_string()
        };
        let url = match tauri::Url::parse(&normalized) {
            Ok(u) => u,
            Err(e) => return format!("error: invalid url '{url_str}': {e}"),
        };
        if url.scheme() != "http" && url.scheme() != "https" {
            return format!(
                "error: browser_open only supports http:// or https:// URLs; got scheme '{}'.",
                url.scheme()
            );
        }
        let host = url.host_str().unwrap_or("");
        if !matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "::1" | "[::1]") {
            return format!(
                "error: browser_open is limited to a local dev server (localhost / 127.0.0.1); got host '{host}'. Start your app with process_start, then open its localhost URL. For fetching web pages, use web_fetch."
            );
        }

        // Confirm something is actually SERVING before we navigate. The old blind
        // 1.5s sleep raced slow-starting servers: the view would land on a browser
        // error page, that page then STUCK, and every later screenshot faithfully
        // re-photographed the stale error — which reads as "the screenshot is
        // broken" when the server was the problem (observed 2026-07-18). Refusing
        // here, with the reason, beats leaving a phantom in the view.
        if !server_responds(&url).await {
            return format!(
                "error: nothing answered at {url_str} within {SERVER_WAIT_SECS}s. The server may still be starting, may have failed to bind (port already in use — an earlier process_start can still hold it), or may have exited. Check process_logs and process_list, stop any stale server, or use a different port."
            );
        }

        match tauri::Manager::get_webview_window(&self.app, "perception") {
            Some(win) => {
                if let Err(e) = win.navigate(url.clone()) {
                    return format!("error: couldn't navigate the perception view: {e}");
                }
            }
            None => {
                let built = tauri::WebviewWindowBuilder::new(
                    &self.app,
                    "perception",
                    tauri::WebviewUrl::External(url.clone()),
                )
                .title("Outpost perception")
                .inner_size(1280.0, 800.0)
                .visible(false)
                .initialization_script(CONSOLE_SHIM)
                .build();
                if let Err(e) = built {
                    return format!("error: couldn't open the perception view: {e}");
                }
            }
        }
        // The server answered, so this is render time only, not connect time.
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        format!(
            "Perception view is at {url_str} (server responded). Use browser_read for the rendered text + console, or browser_screenshot to see it. If the page still looks half-built, browser_reload and look again."
        )
    }

    /// `browser_reload` — re-navigate the perception view to the page it's already
    /// on. Needed because re-running browser_screenshot only re-photographs whatever
    /// is in the view: if the page loaded badly, nothing but a fresh navigation will
    /// change it, and without this the model has to remember the original URL.
    pub(crate) async fn browser_reload(&self) -> String {
        let Some(win) = tauri::Manager::get_webview_window(&self.app, "perception") else {
            return "error: no perception view is open — call browser_open(url) first.".into();
        };
        let url = match win.url() {
            Ok(u) => u,
            Err(e) => return format!("error: couldn't read the view's current url: {e}"),
        };
        if !server_responds(&url).await {
            return format!(
                "error: nothing answered at {url} within {SERVER_WAIT_SECS}s — the server looks down. Check process_logs and process_list before reloading."
            );
        }
        if let Err(e) = win.navigate(url.clone()) {
            return format!("error: couldn't reload the perception view: {e}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        format!("Reloaded {url}. Use browser_read or browser_screenshot to see it now.")
    }

    /// `browser_read` — read what the perception webview currently renders (visible
    /// text) plus its captured console messages. The model's eyes on a frontend.
    pub(crate) async fn browser_read(&self) -> String {
        let Some(win) = tauri::Manager::get_webview_window(&self.app, "perception") else {
            return "error: no perception view is open — call browser_open(url) first.".into();
        };
        const JS: &str = r#"(function(){try{
var t=document.body?document.body.innerText:'';
if(t.length>8000)t=t.slice(0,8000)+'\n…(truncated)';
var logs=(window.__perc&&window.__perc.logs)?window.__perc.logs:[];
var id=1;
var interactables=[];
var els=document.querySelectorAll('a, button, input, select, textarea, [tabindex], [role="button"]');
for(var i=0;i<els.length;i++){
    var el=els[i];
    var r=el.getBoundingClientRect();
    var s=window.getComputedStyle(el);
    if(r.width===0||r.height===0||s.visibility==='hidden'||s.display==='none')continue;
    el.setAttribute('data-perc-id',id);
    if(interactables.length < 100) {
        var tag=el.tagName.toLowerCase();
        var ty=el.type?' type="'+el.type+'"':'';
        var name=el.name?' name="'+el.name+'"':'';
        var txt=(el.innerText||el.value||el.placeholder||el.title||el.name||'').trim().substring(0, 50).replace(/\n/g,' ');
        interactables.push('['+id+'] <'+tag+ty+name+'> '+txt);
    }
    id++;
}
if(id - 1 > 100) {
    interactables.push('... and ' + (id - 1 - 100) + ' more interactive elements');
}
return{url:location.href,title:document.title,text:t,logs:logs,interactables:interactables};
}catch(e){return{error:String(e)};}})()"#;

        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        let tx = std::sync::Mutex::new(Some(tx));
        if let Err(e) = win.eval_with_callback(JS, move |result| {
            if let Ok(mut guard) = tx.lock() {
                if let Some(tx) = guard.take() {
                    let _ = tx.send(result);
                }
            }
        }) {
            return format!("error: couldn't read the perception view: {e}");
        }
        let raw_str = match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(s)) => s,
            _ => {
                return "error: the perception view didn't respond (still loading? re-open it, or wait and retry browser_read).".into()
            }
        };
        let v: Value = serde_json::from_str(&raw_str).unwrap_or(Value::Null);
        if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
            return format!("page read error: {err}");
        }
        let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
        let title = v
            .get("title")
            .and_then(|x| x.as_str())
            .unwrap_or("(untitled)");
        let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("").trim();
        let logs: Vec<String> = v
            .get("logs")
            .and_then(|l| l.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|e| {
                        let level = e.get("level").and_then(|x| x.as_str()).unwrap_or("log");
                        let msg = e.get("msg").and_then(|x| x.as_str()).unwrap_or("");
                        format!("[{level}] {msg}")
                    })
                    .collect()
            })
            .unwrap_or_default();
        let interactables: Vec<String> = v
            .get("interactables")
            .and_then(|l| l.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let body = if text.is_empty() {
            "(the page rendered no visible text — it may still be loading, or be blank / an error page)"
        } else {
            text
        };
        let mut out = format!("{title} — {url}\n\n=== Rendered text ===\n{body}");
        if !interactables.is_empty() {
            out.push_str(&format!(
                "\n\n=== Interactive Elements ({}) ===\n(Use browser_act to interact by ID)\n{}",
                interactables.len(),
                interactables.join("\n")
            ));
        }
        if logs.is_empty() {
            out.push_str("\n\n=== Console ===\n(no messages)");
        } else {
            out.push_str(&format!(
                "\n\n=== Console ({}) ===\n{}",
                logs.len(),
                logs.join("\n")
            ));
        }
        out
    }

    /// `browser_act` — run an interaction script against a tagged element
    pub(crate) async fn browser_act(&self, arguments: &str) -> String {
        let Some(win) = tauri::Manager::get_webview_window(&self.app, "perception") else {
            return "error: no perception view is open — call browser_open(url) first.".into();
        };
        let v: serde_json::Value = serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);
        let id = match v.get("id").and_then(|id| id.as_i64()) {
            Some(i) => i,
            None => return "error: missing or invalid 'id'.".into(),
        };
        let action = match v.get("action").and_then(|a| a.as_str()) {
            Some(a) => a.to_string(),
            None => return "error: missing or invalid 'action'.".into(),
        };
        let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
        let text_json = serde_json::to_string(text).unwrap_or("\"\"".to_string());

        let js = format!(r#"(function(){{try{{
            var el = document.querySelector('[data-perc-id="{id}"]');
            if(!el) return "error: element [{id}] not found (did you run browser_read recently?)";
            var act = "{action}";
            var txt = {text_json};
            var setNativeValue = function(element, val) {{
                var proto = element.tagName === 'TEXTAREA' ? window.HTMLTextAreaElement.prototype : window.HTMLInputElement.prototype;
                var setter = Object.getOwnPropertyDescriptor(proto, 'value');
                if (setter && setter.set) {{
                    setter.set.call(element, val);
                }} else {{
                    element.value = val;
                }}
            }};
            if(act === "click") {{
                el.scrollIntoView({{block: 'center'}});
                el.click();
                return "Clicked element [{id}]";
            }} else if(act === "type") {{
                el.scrollIntoView({{block: 'center'}});
                el.focus();
                setNativeValue(el, txt);
                el.dispatchEvent(new Event('input', {{bubbles: true}}));
                el.dispatchEvent(new Event('change', {{bubbles: true}}));
                return "Typed into element [{id}]";
            }} else if(act === "clear") {{
                el.scrollIntoView({{block: 'center'}});
                el.focus();
                setNativeValue(el, "");
                el.dispatchEvent(new Event('input', {{bubbles: true}}));
                el.dispatchEvent(new Event('change', {{bubbles: true}}));
                return "Cleared element [{id}]";
            }} else if(act === "select") {{
                el.value = txt;
                el.dispatchEvent(new Event('change', {{bubbles: true}}));
                return "Selected '" + txt + "' on element [{id}]";
            }} else {{
                return "error: unknown action " + act;
            }}
        }}catch(e){{return "error: " + String(e);}}}})()"#);

        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        let tx = std::sync::Mutex::new(Some(tx));
        if let Err(e) = win.eval_with_callback(&js, move |result| {
            if let Ok(mut guard) = tx.lock() {
                if let Some(tx) = guard.take() {
                    let _ = tx.send(result);
                }
            }
        }) {
            return format!("error: couldn't interact with the perception view: {e}");
        }
        let raw_str = match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(s)) => s,
            _ => {
                return "error: the perception view didn't respond.".into()
            }
        };
        // Give DOM and framework state 250ms to settle before returning
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let res: serde_json::Value = serde_json::from_str(&raw_str).unwrap_or(serde_json::Value::Null);
        if let Some(s) = res.as_str() {
            s.to_string()
        } else {
            raw_str
        }
    }

    /// `browser_close` — tear down the perception webview.
    pub(crate) fn browser_close(&self) -> String {
        match tauri::Manager::get_webview_window(&self.app, "perception") {
            Some(win) => {
                let _ = win.destroy();
                "Closed the perception view.".into()
            }
            None => "No perception view was open.".into(),
        }
    }


}

const SERVER_WAIT_SECS: u64 = 10;

/// Poll `url` until it responds, up to [`SERVER_WAIT_SECS`]. Any HTTP reply counts
/// as ready — a 404 still proves something is listening and serving, which is what
/// we need to know before pointing the webview at it. `no_proxy` matters: a system
/// proxy configured for the browser must not be consulted for localhost.
async fn server_responds(url: &tauri::Url) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(SERVER_WAIT_SECS);
    loop {
        if client.get(url.clone()).send().await.is_ok() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}




