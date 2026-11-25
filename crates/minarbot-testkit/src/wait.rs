//! Polling waits with informative timeouts.
//!
//! Every wait has a label and, for [`until_value`], reports the last observed
//! value on timeout, so a failing test says what it saw instead of only
//! "timed out".

use crate::{server::WAIT, Error, Result};
use std::{fmt::Debug, future::Future, time::Duration};
use tokio::time::{sleep, Instant};

/// Poll interval for kit waits.
pub const INTERVAL: Duration = Duration::from_millis(15);

/// Poll `check` until it returns `Some`, for up to [`WAIT`].
pub async fn until<T, F, Fut>(label: impl Into<String>, check: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<T>>>,
{
    until_within(label, WAIT, check).await
}

/// Poll `check` until it returns `Some`, for up to `limit`.
pub async fn until_within<T, F, Fut>(
    label: impl Into<String>,
    limit: Duration,
    mut check: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<T>>>,
{
    let label = label.into();
    let deadline = Instant::now() + limit;
    loop {
        if let Some(value) = check().await? {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out after {limit:?} waiting for {label}").into());
        }
        sleep(INTERVAL).await;
    }
}

/// Poll `read` until `done(&value)` holds; the timeout error shows the last value.
pub async fn until_value<T, F, Fut, P>(label: impl Into<String>, mut read: F, done: P) -> Result<T>
where
    T: Debug,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
    P: Fn(&T) -> bool,
{
    let label = label.into();
    let deadline = Instant::now() + WAIT;
    loop {
        let value = read().await?;
        if done(&value) {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(Error::from(format!(
                "timed out after {WAIT:?} waiting for {label}; last value: {value:?}"
            )));
        }
        sleep(INTERVAL).await;
    }
}

/// Require `check` to stay `false` for `period` (absence of an effect).
pub async fn never<F, Fut>(label: impl Into<String>, period: Duration, mut check: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<bool>>,
{
    let label = label.into();
    let deadline = Instant::now() + period;
    while Instant::now() < deadline {
        if check().await? {
            return Err(format!("unexpected: {label}").into());
        }
        sleep(INTERVAL).await;
    }
    Ok(())
}
