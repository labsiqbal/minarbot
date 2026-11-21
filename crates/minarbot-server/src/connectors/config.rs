use serde::Deserialize;

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Composio {
    #[serde(rename = "baseURL")]
    pub base_url: Option<String>,
    pub timeout: Option<u64>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bridge {
    pub origin: String,
    pub token: String,
}
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    pub composio: Option<Composio>,
    pub test_connector_bridge: Option<Bridge>,
    pub discord_test_origin: Option<String>,
    pub mail_api_key: Option<String>,
    pub mail_endpoint: Option<String>,
}
impl Options {
    pub fn environment() -> Self {
        Self {
            mail_api_key: std::env::var("RESEND_API_KEY").ok(),
            mail_endpoint: std::env::var("MINARBOT_MAIL_ENDPOINT").ok(),
            ..Self::default()
        }
    }
    pub fn validate(&self) -> crate::db::Result<()> {
        if let Some(base) = self.composio.as_ref().and_then(|c| c.base_url.as_ref()) {
            loopback(base).map_err(|_| "Composio test endpoint must be loopback HTTP.")?;
        }
        if let Some(bridge) = &self.test_connector_bridge {
            if std::env::var("MINARBOT_TEST_SERVER").as_deref() != Ok("rust") {
                return Err("Connector fixture bridge requires Rust test mode.".into());
            }
            loopback(&bridge.origin)?;
            if bridge.token.len() != 64 || !bridge.token.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("Invalid connector fixture bridge token.".into());
            }
        }
        if let Some(origin) = &self.discord_test_origin {
            if std::env::var("MINARBOT_TEST_SERVER").as_deref() != Ok("rust")
                || loopback(origin)?.origin().ascii_serialization() != *origin
            {
                return Err("Discord fixture requires loopback HTTP and Rust test mode.".into());
            }
        }
        if let Some(endpoint) = &self.mail_endpoint {
            if endpoint != "https://api.resend.com/emails" {
                loopback(endpoint).map_err(|_| {
                    "Mail endpoint must be Resend or a development loopback fixture"
                })?;
            }
        }
        if std::env::var("MINARBOT_TEST_SERVER").as_deref() == Ok("rust")
            && self.mail_api_key.is_some()
            && self
                .mail_endpoint
                .as_deref()
                .is_none_or(|s| loopback(s).is_err())
        {
            return Err("Rust mail tests require a loopback fixture endpoint.".into());
        }
        Ok(())
    }
}
pub fn loopback(value: &str) -> crate::db::Result<url::Url> {
    let url = url::Url::parse(value)?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Fixture endpoint must be loopback HTTP.".into());
    }
    Ok(url)
}
