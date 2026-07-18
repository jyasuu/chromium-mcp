//! The MCP tool surface: thin adapters between rmcp's `#[tool]` macros and
//! the `BrowserManager`.

use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content};
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};

use crate::browser::BrowserManager;

fn internal_error(e: anyhow::Error) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NavigateParams {
    /// The URL to navigate to, e.g. "https://example.com"
    pub url: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SelectorParams {
    /// CSS selector. If omitted, applies to the whole page / <body>.
    pub selector: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClickParams {
    /// CSS selector of the element to click, e.g. "#submit" or "a.next"
    pub selector: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TypeTextParams {
    /// CSS selector of the input/textarea to type into
    pub selector: String,
    /// Text to type
    pub text: String,
    /// Optional key to press after typing, e.g. "Enter" to submit a search or form. Leave empty to skip.
    #[serde(default)]
    pub press_key_after: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EvalJsParams {
    /// A JavaScript expression or function to evaluate in the page context,
    /// e.g. "document.title" or "() => document.querySelectorAll('a').length"
    pub script: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ScreenshotParams {
    /// Capture the full scrollable page instead of just the viewport. Default: false.
    pub full_page: Option<bool>,
}

#[derive(Clone)]
pub struct ChromiumServer {
    browser: Arc<BrowserManager>,
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

#[tool_router]
impl ChromiumServer {
    pub fn new(headless: bool, chrome_path: Option<String>) -> Self {
        Self::with_shared_browser(Arc::new(BrowserManager::new(headless, chrome_path)))
    }

    /// Build a server instance backed by an already-existing `BrowserManager`.
    /// Used by the HTTP transport so every session shares the same browser
    /// tab instead of each session launching its own Chrome process.
    pub fn with_shared_browser(browser: Arc<BrowserManager>) -> Self {
        Self {
            browser,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Navigate the (single, shared) browser tab to a URL and wait for load. Returns the resulting page title. Launches a headless Chrome instance on first use."
    )]
    async fn navigate(
        &self,
        Parameters(NavigateParams { url }): Parameters<NavigateParams>,
    ) -> Result<String, McpError> {
        let title = self.browser.navigate(&url).await.map_err(internal_error)?;
        Ok(format!("Navigated to {url}. Page title: {title}"))
    }

    #[tool(description = "Get the current page's URL.")]
    async fn get_url(&self) -> Result<String, McpError> {
        self.browser.current_url().await.map_err(internal_error)
    }

    #[tool(
        description = "Get HTML content of the current page. If `selector` is given, returns the inner HTML of the first matching element instead of the whole document."
    )]
    async fn get_content(
        &self,
        Parameters(SelectorParams { selector }): Parameters<SelectorParams>,
    ) -> Result<String, McpError> {
        self.browser
            .get_content(selector)
            .await
            .map_err(internal_error)
    }

    #[tool(
        description = "Get the visible, rendered text of the page (or of the element matching `selector`), similar to what a user would see/select on screen."
    )]
    async fn get_text(
        &self,
        Parameters(SelectorParams { selector }): Parameters<SelectorParams>,
    ) -> Result<String, McpError> {
        self.browser
            .get_text(selector)
            .await
            .map_err(internal_error)
    }

    #[tool(description = "Click the first element matching a CSS selector.")]
    async fn click(
        &self,
        Parameters(ClickParams { selector }): Parameters<ClickParams>,
    ) -> Result<String, McpError> {
        self.browser
            .click(&selector)
            .await
            .map_err(internal_error)?;
        Ok(format!("Clicked `{selector}`"))
    }

    #[tool(
        description = "Click an input/textarea matching a CSS selector, then type text into it. Optionally press a key afterwards (e.g. \"Enter\" to submit a search or form)."
    )]
    async fn type_text(
        &self,
        Parameters(TypeTextParams {
            selector,
            text,
            press_key_after,
        }): Parameters<TypeTextParams>,
    ) -> Result<String, McpError> {
        self.browser
            .type_text(&selector, &text, if press_key_after.is_empty() { None } else { Some(&press_key_after) })
            .await
            .map_err(internal_error)?;
        Ok(format!("Typed into `{selector}`"))
    }

    #[tool(
        description = "Evaluate a JavaScript expression or function in the page's context and return the JSON-serialized result."
    )]
    async fn eval_js(
        &self,
        Parameters(EvalJsParams { script }): Parameters<EvalJsParams>,
    ) -> Result<String, McpError> {
        let value = self.browser.eval_js(&script).await.map_err(internal_error)?;
        serde_json::to_string(&value).map_err(|e| McpError::internal_error(e.to_string(), None))
    }

    #[tool(
        description = "Take a PNG screenshot of the current page and save it to /tmp/screenshot.png (also returns the image). If called multiple times, files are named screenshot_1.png, screenshot_2.png, etc."
    )]
    async fn screenshot(
        &self,
        Parameters(ScreenshotParams { full_page }): Parameters<ScreenshotParams>,
    ) -> Result<CallToolResult, McpError> {
        let bytes = self
            .browser
            .screenshot(full_page.unwrap_or(false))
            .await
            .map_err(internal_error)?;

        // Save to a numbered file in /tmp
        let path = std::env::temp_dir().join(format!(
            "screenshot_{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));
        std::fs::write(&path, &bytes).map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let data = STANDARD.encode(&bytes);
        Ok(CallToolResult::success(vec![
            Content::text(format!("Screenshot saved to {}", path.display())),
            Content::image(data, "image/png".to_string()),
        ]))
    }

    #[tool(
        description = "Render the current page as a PDF and return it base64-encoded. Only works when running headless (the default)."
    )]
    async fn pdf(&self) -> Result<String, McpError> {
        let bytes = self.browser.pdf().await.map_err(internal_error)?;
        Ok(STANDARD.encode(&bytes))
    }

    #[tool(description = "Close the shared browser instance, if one is running. A new one will be launched automatically on the next navigate/etc. call.")]
    async fn close_browser(&self) -> Result<String, McpError> {
        self.browser.close().await.map_err(internal_error)?;
        Ok("Browser closed".to_string())
    }
}

#[tool_handler(
    name = "chromium-mcp",
    version = "0.1.0",
    instructions = "Tools for driving a headless Chrome/Chromium browser via the Chrome DevTools Protocol (chromiumoxide). A single browser tab is shared across calls: call `navigate` first, then use `click`/`type_text`/`get_text`/`get_content`/`eval_js`/`screenshot`/`pdf` against the current page."
)]
impl ServerHandler for ChromiumServer {}
