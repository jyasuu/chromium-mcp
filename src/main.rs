mod browser;
mod server;

use std::net::SocketAddr;
use std::sync::Arc;

use clap::{Parser, ValueEnum};
use rmcp::transport::stdio;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::StreamableHttpService;
use rmcp::ServiceExt;

use browser::BrowserManager;
use server::ChromiumServer;

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Transport {
    Stdio,
    Http,
}

/// MCP server exposing headless-Chrome browser automation (via chromiumoxide)
/// as tools, over either stdio or streamable-HTTP transport.
#[derive(Parser, Debug)]
#[command(name = "chromium-mcp", version, about)]
struct Args {
    /// Which MCP transport to serve on.
    #[arg(long, value_enum, default_value = "stdio")]
    transport: Transport,

    /// Address to bind when using --transport http.
    #[arg(long, default_value = "127.0.0.1:8787")]
    addr: SocketAddr,

    /// Run Chrome with a visible window instead of headless. Mostly useful
    /// for local debugging; headless is what you want on a server.
    #[arg(long, default_value_t = false)]
    headed: bool,

    /// Path to Chrome/Chromium binary. If omitted, auto-detects by looking
    /// for `google-chrome`, `google-chrome-stable`, `chromium`, or
    /// `chromium-browser` on PATH.
    #[arg(long = "chrome-path", env = "CHROME_PATH")]
    chrome_path: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Always log to stderr: stdout is reserved for the MCP stdio transport's
    // JSON-RPC traffic.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let headless = !args.headed;

    match args.transport {
        Transport::Stdio => run_stdio(headless, args.chrome_path).await,
        Transport::Http => run_http(headless, args.addr, args.chrome_path).await,
    }
}

async fn run_stdio(headless: bool, chrome_path: Option<String>) -> anyhow::Result<()> {
    tracing::info!("starting chromium-mcp on stdio (headless={headless})");
    let service = ChromiumServer::new(headless, chrome_path)
        .serve(stdio())
        .await
        .inspect_err(|e| tracing::error!("serving error: {e:?}"))?;
    service.waiting().await?;
    Ok(())
}

async fn run_http(headless: bool, addr: SocketAddr, chrome_path: Option<String>) -> anyhow::Result<()> {
    // One browser shared by every HTTP session, so `navigate` in one call
    // and `click`/`screenshot` in the next operate on the same tab.
    let browser = Arc::new(BrowserManager::new(headless, chrome_path));

    let service = StreamableHttpService::new(
        move || Ok(ChromiumServer::with_shared_browser(browser.clone())),
        LocalSessionManager::default().into(),
        Default::default(),
    );

    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("chromium-mcp listening on http://{addr}/mcp (headless={headless})");

    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
