use serde::Deserialize;
use std::path::PathBuf;

#[allow(dead_code)]
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    pub desktop: Option<DesktopOptions>,
    pub machines: Option<MachineOptions>,
    pub test_computer_bridge: Option<TestBridge>,
}
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(dead_code)]
pub struct DesktopOptions {
    pub data_dir: Option<PathBuf>,
    #[serde(default)]
    pub test_runtime: bool,
    pub enabled: Option<bool>,
    pub max: Option<usize>,
    pub idle_ms: Option<i64>,
    pub lease_dir: Option<PathBuf>,
    pub geometry: Option<String>,
    pub test_only_proxy_target: Option<super::proxy::TestTarget>,
}
impl Options {
    pub fn environment() -> crate::db::Result<Self> {
        let data_dir = std::env::var_os("MINARBOT_DESKTOP_DATA_DIR").map(PathBuf::from);
        if data_dir
            .as_ref()
            .is_some_and(|path| !path.is_absolute() || !path.is_dir())
        {
            return Err("MINARBOT_DESKTOP_DATA_DIR must be an existing absolute directory.".into());
        }
        let desktop = DesktopOptions {
            data_dir,
            enabled: std::env::var("MINARBOT_AGENT_DESKTOP")
                .ok()
                .map(|value| value == "1"),
            max: std::env::var("MINARBOT_AGENT_DESKTOP_MAX")
                .ok()
                .map(|v| v.parse())
                .transpose()?,
            idle_ms: std::env::var("MINARBOT_AGENT_DESKTOP_IDLE_MS")
                .ok()
                .map(|v| v.parse())
                .transpose()?,
            geometry: std::env::var("MINARBOT_AGENT_DESKTOP_GEOMETRY").ok(),
            ..Default::default()
        };
        Ok(Self {
            desktop: Some(desktop),
            ..Default::default()
        })
    }
    pub fn validate(&self) -> crate::db::Result<()> {
        if let Some(bridge) = &self.test_computer_bridge {
            let url = url::Url::parse(&bridge.origin)?;
            if std::env::var("MINARBOT_TEST_SERVER").as_deref() != Ok("rust")
                || url.scheme() != "http"
                || url.host_str() != Some("127.0.0.1")
                || url.origin().ascii_serialization() != bridge.origin
                || bridge.token.len() != 64
            {
                return Err("Invalid test computer bridge".into());
            }
        }
        let d = self.desktop.clone().unwrap_or_default();
        if !(1..=16).contains(&d.max.unwrap_or(2))
            || !(1000..=86400000).contains(&d.idle_ms.unwrap_or(900000))
        {
            return Err("Desktop limits: max 1..16; idle timeout 1000..86400000 ms.".into());
        }
        dimensions(d.geometry.as_deref().unwrap_or("1280x800"))?;
        if d.test_runtime && self.test_computer_bridge.is_none() {
            return Err("Desktop fixture runtime requires test bridge".into());
        }
        if let Some(target) = &d.test_only_proxy_target {
            if std::env::var("MINARBOT_TEST_SERVER").as_deref() != Ok("rust") || !target.valid() {
                return Err("Invalid test-only proxy fixture.".into());
            }
        }
        Ok(())
    }
}
pub fn dimensions(value: &str) -> crate::db::Result<(u32, u32)> {
    let (w, h) = value.split_once('x').unwrap_or(("", ""));
    let width = w.parse::<u32>().unwrap_or(0);
    let height = h.parse::<u32>().unwrap_or(0);
    if w.len() != 4
        || !(3..=4).contains(&h.len())
        || !(1024..=1920).contains(&width)
        || !(640..=1200).contains(&height)
    {
        return Err("Desktop geometry: width 1024..1920; height 640..1200.".into());
    }
    Ok((width, height))
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MachineOptions {
    pub health_interval_ms: Option<u64>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestBridge {
    pub origin: String,
    pub token: String,
}

impl TestBridge {
    pub async fn call(
        &self,
        operation: &str,
        args: serde_json::Value,
    ) -> super::Result<serde_json::Value> {
        let response = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(70))
            .build()
            .map_err(|_| crate::web::Failure::storage())?
            .post(&self.origin)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({"operation":operation,"args":args}))
            .send()
            .await
            .map_err(|_| crate::web::Failure::storage())?;
        let value: serde_json::Value = response
            .json()
            .await
            .map_err(|_| crate::web::Failure::storage())?;
        if let Some(error) = value.get("error") {
            return Err(super::error(
                error["status"].as_u64().unwrap_or(503) as u16,
                error["message"]
                    .as_str()
                    .unwrap_or("Desktop fixture failed."),
            ));
        }
        Ok(value["value"].clone())
    }
}
