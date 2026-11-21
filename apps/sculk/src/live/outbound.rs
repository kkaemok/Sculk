use sculk_bot::{
    BoxFuture, MessageId, Outbound, OutboundError, OutboundReceipt, RoomId, SessionOutbound,
};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Unused,
    Pending,
    Acknowledged(MessageId),
    Rejected(i64),
    NotSubmitted,
    Unknown,
}

/// One authorized attempt for the active message. If a script deadline or
/// cancellation drops this future after admission, Pending remains ambiguous.
pub(super) struct OneWrite {
    adapter: Arc<SessionOutbound>,
    outcome: Mutex<Outcome>,
}

impl OneWrite {
    pub fn new(adapter: Arc<SessionOutbound>) -> Self {
        Self {
            adapter,
            outcome: Mutex::new(Outcome::Unused),
        }
    }

    pub fn outcome(&self) -> Outcome {
        self.outcome
            .lock()
            .map(|outcome| *outcome)
            .unwrap_or(Outcome::Unknown)
    }
}

impl Outbound for OneWrite {
    fn send<'a>(
        &'a self,
        room: RoomId,
        text: &'a str,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<OutboundReceipt, OutboundError>> {
        Box::pin(async move {
            {
                let mut outcome = self.outcome.lock().map_err(|_| OutboundError::NotSent)?;
                if *outcome != Outcome::Unused {
                    return Err(OutboundError::NotSent);
                }
                *outcome = Outcome::Pending;
            }
            let result = self.adapter.send(room, text, cancel).await;
            let observed = match &result {
                Ok(OutboundReceipt::ServerAcknowledged(receipt)) => {
                    Outcome::Acknowledged(receipt.id)
                }
                Err(OutboundError::Rejected(status)) => Outcome::Rejected(*status),
                Err(OutboundError::NotSent | OutboundError::Unavailable) => Outcome::NotSubmitted,
                _ => Outcome::Unknown,
            };
            *self
                .outcome
                .lock()
                .map_err(|_| OutboundError::DeliveryUnknown)? = observed;
            result
        })
    }
}

pub(super) struct ReadOnly;

impl Outbound for ReadOnly {
    fn send<'a>(
        &'a self,
        _: RoomId,
        _: &'a str,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<OutboundReceipt, OutboundError>> {
        Box::pin(async { Err(OutboundError::Unavailable) })
    }
}
