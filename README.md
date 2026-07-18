# chromium-mcp

An MCP server that exposes headless-Chrome browser automation, built on:

- [`chromiumoxide`](https://docs.rs/chromiumoxide) — drives Chrome/Chromium over the Chrome DevTools Protocol (CDP)
- [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk) — the official Rust MCP SDK

It supports **both** transports from the same binary:

- `--transport stdio` (default) — for Claude Desktop, Claude Code, etc.
- `--transport http` — a streamable-HTTP server other MCP clients can connect to over the network

A single Chrome tab is shared across all tool calls (and, in HTTP mode, across all sessions), so you can `navigate`, then `click`/`type_text`/`screenshot` against the same page in follow-up calls.

## Prerequisites

- Rust (recent stable — see note below)
- Google Chrome or Chromium installed. Auto-detects `google-chrome`, `google-chrome-stable`, `chromium`, or `chromium-browser` on PATH. Override with `--chrome-path` or `CHROME_PATH` env var.

### Installing Chromium

**Debian / Ubuntu:**
```bash
sudo apt-get update && sudo apt-get install -y chromium
```

**Alpine Linux:**
```bash
apk add chromium
```

**macOS (Homebrew):**
```bash
brew install --cask chromium
```

**Fedora / RHEL:**
```bash
sudo dnf install chromium
```

**Arch:**
```bash
sudo pacman -S chromium
```

Or download from [https://www.chromium.org/getting-involved/download-chromium](https://www.chromium.org/getting-involved/download-chromium) and place it on PATH.

## Build

```bash
cargo build --release
```

## Run

Stdio (default — what most MCP clients expect):

```bash
./target/release/chromium-mcp
# or explicitly:
./target/release/chromium-mcp --transport stdio
```

Streamable HTTP:

```bash
./target/release/chromium-mcp --transport http --addr 127.0.0.1:8787
# MCP endpoint: http://127.0.0.1:8787/mcp
```

Visible (non-headless) browser window, for debugging:

```bash
./target/release/chromium-mcp --headed
```

Specify Chrome binary (otherwise auto-detected on PATH):

```bash
./target/release/chromium-mcp --chrome-path /usr/bin/chromium
# or via environment variable:
CHROME_PATH=/usr/bin/chromium ./target/release/chromium-mcp
```

## Tools exposed

| Tool | Description |
|---|---|
| `navigate` | Go to a URL, wait for load, return the page title |
| `get_url` | Current page URL |
| `get_content` | Full page HTML, or `innerHTML` of an element matching a CSS selector |
| `get_text` | Rendered/visible text of the page or of an element |
| `click` | Click the first element matching a CSS selector |
| `type_text` | Click an input, type text, optionally press a key (e.g. `Enter`) afterwards. `press_key_after` is a string (empty = skip) |
| `eval_js` | Evaluate a JS expression/function in the page and return the JSON result |
| `screenshot` | PNG screenshot (viewport or full page) — saved to `/tmp/screenshot_<timestamp>.png` and returned as MCP image content |
| `pdf` | Render the page to PDF, returned base64-encoded (headless only) |
| `close_browser` | Shut down the shared Chrome instance |

## Client configuration

### Docker

```bash
docker build -t chromium-mcp .
docker run --rm -p 8787:8787 chromium-mcp
# MCP endpoint: http://localhost:8787/mcp
```

### Claude Desktop / Claude Code (stdio)

```json
{
  "mcpServers": {
    "chromium": {
      "command": "/absolute/path/to/target/release/chromium-mcp"
    }
  }
}
```

### HTTP client

Point your MCP client's streamable-HTTP transport at `http://127.0.0.1:8787/mcp` after starting the server with `--transport http`.

## Notes / design choices

- **One shared page.** This keeps the tool surface simple (no `page_id` juggling) at the cost of concurrency — tool calls are serialized behind a mutex. If you need multiple independent tabs/sessions, the natural extension is to add a `page_id` parameter to each tool and keep a `HashMap<PageId, Page>` in `BrowserManager`.
- **Lazy launch.** Chrome only starts on the first tool call that needs it (`navigate`, `eval_js`, etc.), not at server startup.
- **Headless by default**, since that's what you want for a server; pass `--headed` for local debugging.

## A note on this build

This was written against the current published APIs of `rmcp` (1.7.x) and `chromiumoxide` (0.9.x), verified against their docs.rs pages. The default browser config includes `.no_sandbox()` — remove that call in `src/browser.rs` if you're running in a sandboxed or non-root environment.
