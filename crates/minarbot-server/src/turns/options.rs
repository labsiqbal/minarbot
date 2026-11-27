use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    pub agent_limits: Option<Value>,
}
impl Options {
    pub fn environment() -> crate::db::Result<Self> {
        Ok(Self::default())
    }
    pub fn limit(&self, name: &str, default: i64) -> i64 {
        self.agent_limits
            .as_ref()
            .and_then(|v| v[name].as_i64())
            .unwrap_or(default)
    }
    pub fn validate(&self) -> crate::db::Result<()> {
        if self.agent_limits.as_ref().is_some_and(|v| {
            v.as_object().is_none_or(|v| {
                v.iter().any(|(k, v)| {
                    ![
                        "rate",
                        "rateWindowMs",
                        "cooldownMs",
                        "depth",
                        "concurrentChildren",
                    ]
                    .contains(&k.as_str())
                        || v.as_i64()
                            .is_none_or(|v| !(1..=9_007_199_254_740_991).contains(&v))
                })
            })
        }) {
            return Err("Invalid agent loop limits: use positive integers.".into());
        }
        Ok(())
    }
}
