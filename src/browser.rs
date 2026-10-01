//! Thin wrapper around a single lazily-launched chromiumoxide `Browser` +
//! `Page`, shared across all MCP tool calls.
//!
//! Chrome is expensive to start, so we launch it once on first use and keep
//! it (and one "current" page) alive for the lifetime of the MCP server
//! process. All access goes through a `tokio::sync::Mutex` so tool calls are
//! serialized against the single page (simple and predictable for an LLM
//! driving the browser turn by turn).

use anyhow::{anyhow, Context, Result};
use chromiumoxide::cdp::browser_protocol::page::{CaptureScreenshotFormat, PrintToPdfParams};
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::{Browser, BrowserConfig, Page};
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

struct Session {
    browser: Browser,
    // Keep the CDP event-loop handler task alive for as long as the browser is.
    _handler_task: JoinHandle<()>,
    page: Page,
    profile_dir: std::path::PathBuf,
}

pub struct BrowserManager {
    headless: bool,
    chrome_path: Option<String>,
    chrome_flags: Vec<String>,
    session: Mutex<Option<Session>>,
}

impl BrowserManager {
    pub fn new(headless: bool, chrome_path: Option<String>) -> Self {
        let chrome_flags = std::env::var("CHROME_FLAGS")
            .unwrap_or_default()
            .split_whitespace()
            .map(String::from)
            .collect();
        Self {
            headless,
            chrome_path,
            chrome_flags,
            session: Mutex::new(None),
        }
    }

    /// Ensure a browser + page exist (launching one if needed), then run
    /// `f` against a clone of the current page while holding the lock.
    async fn with_page<F, Fut, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(Page) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut guard = self.session.lock().await;
        if guard.is_none() {
            let mut builder = BrowserConfig::builder();
            if !self.headless {
                builder = builder.with_head();
            }
            if let Some(ref path) = self.chrome_path {
                builder = builder.chrome_executable(path);
            }
            // Use a per-process profile dir instead of chromiumoxide's fixed
            // default (`%TEMP%/chromiumoxide-runner`). If a previous run was
            // killed ungracefully and left an orphaned Chrome process behind,
            // a shared/fixed profile dir means every future launch collides
            // with that stale lock and dies immediately. A unique dir per
            // process sidesteps that entirely.
            let profile_dir = std::env::temp_dir().join(format!(
                "chromium-mcp-profile-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
            ));
            builder = builder.user_data_dir(&profile_dir);
            for flag in &self.chrome_flags {
                // chromiumoxide's `arg()` treats a bare string as a switch *key*
                // and re-adds the `--` prefix itself, so passing
                // "--enable-features=X" through it yields the nonsense switch
                // "----enable-features=X". Split on the first `=` and hand
                // chromiumoxide a (key, value) pair instead; it then merges the
                // value into any default for the same switch (one
                // `--enable-features=a,b` on the final command line) rather than
                // emitting a duplicate that Chrome would ignore.
                match flag.split_once('=') {
                    Some((key, value)) => {
                        let key = key.strip_prefix("--").unwrap_or(key);
                        builder = builder.arg((key, value));
                    }
                    None => {
                        let key = flag.strip_prefix("--").unwrap_or(flag);
                        builder = builder.arg(key);
                    }
                }
            }
            let config = builder
                .no_sandbox()
                .build()
                .map_err(|e| anyhow!("failed to build browser config: {e}"))?;

            let (browser, mut handler) = Browser::launch(config)
                .await
                .context("failed to launch chrome/chromium (is it installed and on PATH?)")?;

            let _handler_task = tokio::spawn(async move {
                while let Some(event) = handler.next().await {
                    if let Err(e) = event {
                        tracing::warn!("chromiumoxide handler error: {e}");
                        break;
                    }
                }
            });

            let page = browser.new_page("about:blank").await?;

            *guard = Some(Session {
                browser,
                _handler_task,
                page,
                profile_dir,
            });
        }

        let page = guard.as_ref().unwrap().page.clone();
        drop(guard);
        f(page).await
    }

    pub async fn navigate(&self, url: &str) -> Result<String> {
        self.with_page(|page| async move {
            page.goto(url).await?;
            page.wait_for_navigation().await?;
            let title = page.get_title().await?.unwrap_or_default();
            Ok(title)
        })
        .await
    }

    pub async fn current_url(&self) -> Result<String> {
        self.with_page(|page| async move { Ok(page.url().await?.unwrap_or_default()) })
            .await
    }

    /// Returns the full page HTML, or (if `selector` is given) the inner
    /// HTML of the first matching element.
    pub async fn get_content(&self, selector: Option<String>) -> Result<String> {
        self.with_page(|page| async move {
            match selector {
                None => Ok(page.content().await?),
                Some(sel) => {
                    let el = page
                        .find_element(&sel)
                        .await
                        .with_context(|| format!("no element matching selector `{sel}`"))?;
                    Ok(el.inner_html().await?.unwrap_or_default())
                }
            }
        })
        .await
    }

    /// Returns the visible (rendered) text of the first element matching
    /// `selector`, or of the whole page (`body`) if no selector is given.
    pub async fn get_text(&self, selector: Option<String>) -> Result<String> {
        let sel = selector.unwrap_or_else(|| "body".to_string());
        self.with_page(|page| async move {
            let el = page
                .find_element(&sel)
                .await
                .with_context(|| format!("no element matching selector `{sel}`"))?;
            Ok(el.inner_text().await?.unwrap_or_default())
        })
        .await
    }

    pub async fn click(&self, selector: &str) -> Result<()> {
        let selector = selector.to_string();
        self.with_page(|page| async move {
            page.find_element(&selector)
                .await
                .with_context(|| format!("no element matching selector `{selector}`"))?
                .click()
                .await?;
            Ok(())
        })
        .await
    }

    /// Click the element matched by `selector`, type `text` into it, and
    /// optionally press a key afterwards (e.g. "Enter" to submit a form).
    pub async fn type_text(
        &self,
        selector: &str,
        text: &str,
        press_key_after: Option<&str>,
    ) -> Result<()> {
        let selector = selector.to_string();
        let text = text.to_string();
        let press_key_after = press_key_after.map(|k| k.to_string());
        self.with_page(|page| async move {
            let el = page
                .find_element(&selector)
                .await
                .with_context(|| format!("no element matching selector `{selector}`"))?;
            el.click().await?.type_str(&text).await?;
            if let Some(key) = press_key_after {
                el.press_key(&key).await?;
            }
            Ok(())
        })
        .await
    }

    pub async fn eval_js(&self, script: &str) -> Result<Value> {
        let script = script.to_string();
        self.with_page(|page| async move {
            let result = page.evaluate(script).await?;
            Ok(result.into_value().unwrap_or(Value::Null))
        })
        .await
    }

    /// Discover WebMCP tools registered by the current page.
    ///
    /// Prefers the standard surface (`document.modelContext` /
    /// `navigator.modelContext` -> `getTools()`), and falls back to Chromium's
    /// testing interface (`navigator.modelContextTesting.listTools()`).
    /// Returns `{ available, source, tools }` so callers can tell "no API" apart
    /// from "API present, no tools".
    pub async fn webmcp_list(&self) -> Result<Value> {
        const SCRIPT: &str = r#"(async () => {
  const mc = document.modelContext || navigator.modelContext;
  const tst = navigator.modelContextTesting;
  const norm = (t) => {
    let schema = t.inputSchema;
    if (typeof schema === 'string') { try { schema = JSON.parse(schema); } catch (_) {} }
    return { name: t.name, description: t.description || '', inputSchema: schema ?? null };
  };
  try {
    if (mc && typeof mc.getTools === 'function') {
      const tools = await mc.getTools();
      return JSON.stringify({ available: true, source: 'modelContext', tools: tools.map(norm) });
    }
    if (tst && typeof tst.listTools === 'function') {
      const tools = await tst.listTools();
      return JSON.stringify({ available: true, source: 'modelContextTesting', tools: Array.from(tools).map(norm) });
    }
    return JSON.stringify({ available: false, source: null, tools: [] });
  } catch (e) {
    return JSON.stringify({ available: true, error: String((e && e.message) || e), tools: [] });
  }
})()"#;
        let raw = self.eval_js(SCRIPT).await?;
        let text = raw
            .as_str()
            .ok_or_else(|| anyhow!("unexpected non-string result from WebMCP discovery script"))?;
        serde_json::from_str(text).context("failed to parse WebMCP discovery result")
    }

    /// Execute a WebMCP tool registered by the current page.
    ///
    /// `arguments` is embedded as a JS object literal, so no argument content
    /// is ever interpreted as code. The JSON-string form is kept as a fallback
    /// because `navigator.modelContextTesting.executeTool` wants a string while
    /// `document.modelContext.executeTool` wants an object.
    pub async fn webmcp_call(&self, name: &str, arguments: &Value) -> Result<Value> {
        let args_json = serde_json::to_string(arguments)?;
        // Double-encode: JSON string literal of the JSON text.
        let name_lit = serde_json::to_string(name)?;
        let args_lit = serde_json::to_string(&args_json)?;

        let script = format!(
            r#"(async () => {{
  const name = {name_lit};
  const argsJson = {args_lit};
  let args;
  try {{
    args = (argsJson && typeof argsJson === 'string') ? JSON.parse(argsJson) : {args_json:?};
  }} catch (_) {{
    args = {{}};
  }}
  const mc = document.modelContext || navigator.modelContext;
  const tst = navigator.modelContextTesting;
  const parse = (r) => {{
    if (typeof r === 'string') {{ try {{ return JSON.parse(r); }} catch (_) {{ return r; }} }}
    return r;
  }};
  try {{
    let result;
    if (mc && typeof mc.getTools === 'function' && typeof mc.executeTool === 'function') {{
      const tools = await mc.getTools();
      const tool = tools.find((t) => t.name === name);
      if (!tool) return JSON.stringify({{ ok: false, error: 'no WebMCP tool named ' + name, available: tools.map((t) => t.name) }});
      try {{
        result = await mc.executeTool(tool, args);
      }} catch (_) {{
        try {{
          result = await mc.executeTool(tool, argsJson);
        }} catch (e2) {{
          throw e2;
        }}
      }}
    }} else if (tst && typeof tst.executeTool === 'function') {{
      try {{
        result = await tst.executeTool(name, args);
      }} catch (_) {{
        result = await tst.executeTool(name, argsJson);
      }}
    }} else {{
      return JSON.stringify({{ ok: false, error: 'WebMCP is not available on this page/browser' }});
    }}
    // executeTool resolves to null when the call triggered a navigation.
    return JSON.stringify({{ ok: true, result: result === undefined ? null : parse(result), navigated: result === null }});
  }} catch (e) {{
    return JSON.stringify({{ ok: false, error: String((e && e.message) || e) }});
  }}
}})()"#
        );
        let raw = self.eval_js(&script).await?;
        let text = raw
            .as_str()
            .ok_or_else(|| anyhow!("unexpected non-string result from WebMCP call script"))?;
        serde_json::from_str(text).context("failed to parse WebMCP call result")
    }

    /// Returns PNG bytes of the current page.
    pub async fn screenshot(&self, full_page: bool) -> Result<Vec<u8>> {
        self.with_page(|page| async move {
            let params = ScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Png)
                .full_page(full_page)
                .build();
            Ok(page.screenshot(params).await?)
        })
        .await
    }

    /// Returns PDF bytes of the current page. Only works in headless mode.
    pub async fn pdf(&self) -> Result<Vec<u8>> {
        self.with_page(|page| async move { Ok(page.pdf(PrintToPdfParams::default()).await?) })
            .await
    }

    pub async fn close(&self) -> Result<()> {
        let mut guard = self.session.lock().await;
        if let Some(mut session) = guard.take() {
            let _ = session.browser.close().await;
            let _ = session.browser.wait().await;
            // Best-effort cleanup; a locked/in-use file here isn't fatal.
            let _ = std::fs::remove_dir_all(&session.profile_dir);
        }
        Ok(())
    }
}
