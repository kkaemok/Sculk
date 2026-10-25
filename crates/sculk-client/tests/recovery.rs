use sculk_client::recovery::{connect_with_retry, AttemptError, RetryPolicy};
use std::{cell::Cell, time::Duration};
use tokio::sync::watch;
#[tokio::test]
async fn retries_transient_only_and_stops_at_limit() {
    let (_tx, cancel) = watch::channel(false);
    let calls = Cell::new(0);
    let policy = RetryPolicy {
        attempts: 3,
        initial: Duration::from_millis(1),
        maximum: Duration::from_millis(2),
    };
    let result: Result<(), _> = connect_with_retry(policy, cancel.clone(), || {
        calls.set(calls.get() + 1);
        async { Err(AttemptError::Transient("io")) }
    })
    .await;
    assert_eq!(calls.get(), 3);
    assert_eq!(result, Err(AttemptError::Transient("io")));
    calls.set(0);
    let result: Result<(), _> = connect_with_retry(policy, cancel, || {
        calls.set(calls.get() + 1);
        async { Err(AttemptError::Permanent("auth")) }
    })
    .await;
    assert_eq!(calls.get(), 1);
    assert_eq!(result, Err(AttemptError::Permanent("auth")));
}
#[tokio::test]
async fn recovery_success_and_cancellation_during_attempt() {
    let (tx, cancel) = watch::channel(false);
    let calls = Cell::new(0);
    let result = connect_with_retry(RetryPolicy::default(), cancel.clone(), || {
        calls.set(calls.get() + 1);
        let n = calls.get();
        async move {
            if n < 2 {
                Err(AttemptError::Transient("io"))
            } else {
                Ok(7)
            }
        }
    })
    .await;
    assert_eq!(result, Ok(7));
    let task = tokio::spawn(async move {
        connect_with_retry(RetryPolicy::default(), cancel, || async {
            std::future::pending::<Result<(), AttemptError<()>>>().await
        })
        .await
    });
    tx.send(true).unwrap();
    assert_eq!(task.await.unwrap(), Err(AttemptError::Cancelled));
}

#[tokio::test]
async fn cancellation_interrupts_backoff_and_invalid_policy_never_connects() {
    use tokio::sync::oneshot;
    let (tx, cancel) = watch::channel(false);
    let (called, observed) = oneshot::channel();
    let mut called = Some(called);
    let policy = RetryPolicy {
        attempts: 5,
        initial: Duration::from_secs(100),
        maximum: Duration::from_secs(100),
    };
    let task = tokio::spawn(async move {
        connect_with_retry(policy, cancel, || {
            if let Some(tx) = called.take() {
                let _ = tx.send(());
            }
            async { Err::<(), _>(AttemptError::Transient("io")) }
        })
        .await
    });
    observed.await.unwrap();
    tx.send(true).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap(),
        Err(AttemptError::Cancelled)
    );
    let (_tx, cancel) = watch::channel(false);
    let result: Result<(), AttemptError<()>> = connect_with_retry(
        RetryPolicy {
            attempts: 0,
            ..policy
        },
        cancel,
        || async {
            panic!("must not connect");
        },
    )
    .await;
    assert_eq!(result, Err(AttemptError::InvalidPolicy));
}

#[tokio::test]
async fn false_watch_update_does_not_cancel_retry() {
    let (tx, cancel) = watch::channel(false);
    tx.send(false).unwrap();
    let result: Result<_, AttemptError<()>> =
        connect_with_retry(RetryPolicy::default(), cancel, || async { Ok(42) }).await;
    assert_eq!(result, Ok(42));
}
