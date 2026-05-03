//! `toolhub serve` — local web UI on 127.0.0.1.

use std::net::IpAddr;

use crate::db_path::default_db_path;

pub async fn run(host: IpAddr, port: u16, open: bool) -> anyhow::Result<()> {
    let db_path = default_db_path()?;
    toolhub_web::serve(toolhub_web::WebConfig {
        db_path,
        host,
        port,
        open,
    })
    .await
}
