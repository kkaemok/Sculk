//! Bounded reconnection for caller-supplied secure-session establishment.
//! Requests are never replayed; authentication failures terminate immediately.
use rand::Rng;
use std::{future::Future, time::Duration};
use tokio::sync::watch;

/// Outcome of a connection attempt, classified by the session provider.
#[derive(Debug, PartialEq, Eq)]
pub enum AttemptError<E> {
    /// Transient I/O failure that may be retried.
    Transient(E),
    /// Authentication, verification or permission failure; never retried.
    Permanent(E),
    /// Cancellation interrupted an attempt or its backoff.
    Cancelled,
    /// Retry policy was invalid; no attempt was made.
    InvalidPolicy,
}
/// Finite exponential backoff with full jitter. Reconnecting does not imply replay.
#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    /// Maximum number of attempts, including the initial one.
    pub attempts: u16,
    /// Initial maximum jitter delay.
    pub initial: Duration,
    /// Upper delay bound.
    pub maximum: Duration,
}
impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 5,
            initial: Duration::from_millis(250),
            maximum: Duration::from_secs(30),
        }
    }
}
/// Retry only explicitly transient connection failures. Dropping an attempt future
/// must cancel the caller's connector. A true or closed watch cancels immediately.
pub async fn connect_with_retry<T, E, F, Fut>(
    policy: RetryPolicy,
    mut cancel: watch::Receiver<bool>,
    mut connect: F,
) -> Result<T, AttemptError<E>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, AttemptError<E>>>,
{
    if policy.attempts == 0
        || policy.initial.is_zero()
        || policy.initial > policy.maximum
        || policy.maximum > Duration::from_secs(86400)
    {
        return Err(AttemptError::InvalidPolicy);
    }
    let mut delay = policy.initial;
    for n in 0..policy.attempts {
        if *cancel.borrow() {
            return Err(AttemptError::Cancelled);
        }
        let result = tokio::select! { biased;
            _ = cancelled(&mut cancel) => return Err(AttemptError::Cancelled),
            result = connect() => result,
        };
        match result {
            Err(AttemptError::Transient(_)) if n + 1 < policy.attempts => {
                let jitter =
                    Duration::from_nanos(rand::thread_rng().gen_range(0..=delay.as_nanos() as u64));
                tokio::select! { biased;
                    _ = cancelled(&mut cancel) => return Err(AttemptError::Cancelled),
                    _ = tokio::time::sleep(jitter) => {}
                }
                delay = delay.saturating_mul(2).min(policy.maximum);
            }
            other => return other,
        }
    }
    unreachable!("nonzero attempts")
}

/// Wait until cancellation is true or its sender is dropped. False updates are harmless.
pub(crate) async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    loop {
        if *cancel.borrow_and_update() || cancel.changed().await.is_err() {
            return;
        }
    }
}
