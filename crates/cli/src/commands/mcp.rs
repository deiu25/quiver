//! `quiver mcp` — start the stdio MCP server.

pub async fn run() -> anyhow::Result<()> {
    quiver_mcp_server::serve_stdio().await
}
