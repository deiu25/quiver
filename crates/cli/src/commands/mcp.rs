//! `toolhub mcp` — start the stdio MCP server.

pub async fn run() -> anyhow::Result<()> {
    toolhub_mcp_server::serve_stdio().await
}
