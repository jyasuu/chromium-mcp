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
            for flag in &self.chrome_flags {
                builder = builder.arg(flag.as_str());
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
        }
        Ok(())
    }
}
