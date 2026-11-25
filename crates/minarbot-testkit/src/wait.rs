use crate::server::{Result, WAIT};
use std::{future::Future, time::Duration};
use tokio::time::{sleep, timeout};

pub async fn until<T, F, Fut>(label: impl Into<String>, check: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<T>>>,
{
    until_with(label, WAIT, Duration::from_millis(25), check).await
}

pub async fn until_with<T, F, Fut>(
    label: impl Into<String>,
    duration: Duration,
    interval: Duration,
    mut check: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<T>>>,
{
    let label = label.into();
    timeout(duration, async {
        loop {
            if let Some(value) = check().await? {
                return Ok(value);
            }
            sleep(interval).await;
        }
    })
    .await
    .map_err(|_| -> Box<dyn std::error::Error + Send + Sync> {
        format!(
            "timed out waiting for {label} after {} seconds",
            duration.as_secs()
        )
        .into()
    })?
}
