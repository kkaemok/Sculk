use sculk_bot::{
    BoxFuture, MessageId, Outbound, OutboundError, OutboundReceipt, OutgoingMessage, RoomId,
    SessionOutbound,
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

    async fn attempt(
        &self,
        request: BoxFuture<'_, Result<OutboundReceipt, OutboundError>>,
    ) -> Result<OutboundReceipt, OutboundError> {
        {
            let mut outcome = self.outcome.lock().map_err(|_| OutboundError::NotSent)?;
            if *outcome != Outcome::Unused {
                return Err(OutboundError::NotSent);
            }
            *outcome = Outcome::Pending;
        }
        let result = request.await;
        let observed = match &result {
            Ok(OutboundReceipt::ServerAcknowledged(receipt)) => Outcome::Acknowledged(receipt.id),
            Err(OutboundError::Rejected(status)) => Outcome::Rejected(*status),
            Err(OutboundError::NotSent | OutboundError::Unavailable | OutboundError::Cancelled) => {
                Outcome::NotSubmitted
            }
            _ => Outcome::Unknown,
        };
        *self
            .outcome
            .lock()
            .map_err(|_| OutboundError::DeliveryUnknown)? = observed;
        result
    }
}

impl Outbound for OneWrite {
    fn send<'a>(
        &'a self,
        room: RoomId,
        text: &'a str,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<OutboundReceipt, OutboundError>> {
        Box::pin(self.attempt(self.adapter.send(room, text, cancel)))
    }

    fn send_message<'a>(
        &'a self,
        room: RoomId,
        message: &'a OutgoingMessage,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<OutboundReceipt, OutboundError>> {
        Box::pin(self.attempt(self.adapter.send_message(room, message, cancel)))
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
