use crate::files::rich_html::{MCP_INPUT_LIMIT, MCP_PREVIEW_LIMIT};
use serde_json::Value;
use std::io::{BufRead, Read, Write};
pub async fn run() -> crate::db::Result<()> {
    let url = url::Url::parse(&std::env::var("MINARBOT_MCP_URL")?)?;
    if url.scheme() != "http"
        || !url.host_str().is_some_and(|v| {
            v.parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        })
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/mcp"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Invalid local MCP address".into());
    }
    let token = std::env::var("MINARBOT_MCP_TOKEN")?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(130))
        .build()?;
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    loop {
        let mut line = Vec::new();
        if (&mut input)
            .take(MCP_INPUT_LIMIT as u64 + 1)
            .read_until(b'\n', &mut line)?
            == 0
        {
            break;
        }
        if line.len() > MCP_INPUT_LIMIT {
            return Err("MCP input exceeds the size limit".into());
        }
        let message: Value = serde_json::from_slice(&line)?;
        let mut request = client.post(url.clone()).bearer_auth(&token).json(&message);
        if message["method"] == "tools/call" && message["params"]["name"] == "render_component" {
            request = request.timeout(std::time::Duration::from_secs(
                crate::files::components::FORM_WAIT_SECONDS + 10,
            ));
        }
        let mut response = request.send().await?;
        let limit =
            if message["method"] == "tools/call" && message["params"]["name"] == "html_preview" {
                MCP_PREVIEW_LIMIT
            } else {
                256 * 1024
            };
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() + chunk.len() > limit {
                return Err("MCP response exceeds the size limit".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        if message.get("id").is_none() {
            continue;
        }
        let reply = if response.status().is_success() {
            serde_json::from_slice::<Value>(&bytes)?
        } else {
            serde_json::json!({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32603,"message":"MCP request failed."}})
        };
        let mut output = std::io::stdout().lock();
        serde_json::to_writer(&mut output, &reply)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}
