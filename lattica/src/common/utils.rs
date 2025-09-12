use std::time::Duration;
use tokio::time::sleep;

#[derive(Clone)]
pub struct RetryConfig {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub backoff_multiplier: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
            backoff_multiplier: 2.0,
        }
    }
}

pub async fn retry_with_backoff<F, Fut, T, E>(operation: F, config: RetryConfig,) -> Result<T, E>
where F: Fn() ->Fut, Fut:Future<Output = Result<T, E>>, E: std::fmt::Debug + Clone {
    let mut current_delay = config.base_delay;

    for attempt in 1..=config.max_retries {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                if attempt == config.max_retries {
                    return Err(e);
                }

                // jitter
                let jitter = current_delay.as_millis() as f64 * 0.1 * rand::random::<f64>();
                let delay_with_jitter = current_delay + Duration::from_millis(jitter as u64);

                tracing::error!(
                    "RPC call attempt {} failed: {:?}, retrying in {:?}", 
                    attempt, e, delay_with_jitter
                );

                sleep(delay_with_jitter).await;

                // update delay
                current_delay = std::cmp::min(
                    Duration::from_millis((current_delay.as_millis() as f64 * config.backoff_multiplier) as u64),
                    config.max_delay
                );
            }
        }
    }

    unreachable!()
}