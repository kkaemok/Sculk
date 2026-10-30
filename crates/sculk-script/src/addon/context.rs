use super::Capability;
use sculk_bot::{Context, OutboundError, OutboundReceipt};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use tokio::{sync::watch, time::Instant};

/// Native errors are deliberately payload-free. Addons can retain additional
/// application diagnostics themselves; Sculk never formats native error sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AddonError {
    #[error("addon failed")]
    Failed,
    #[error("addon parameter or result type mismatch")]
    Type,
    #[error("addon capability was not declared and granted")]
    Capability,
    #[error("addon execution step budget exceeded")]
    Steps,
    #[error("addon deadline exceeded; partial effects may have occurred")]
    Deadline,
    #[error("addon cancelled; partial effects may have occurred")]
    Cancelled,
    #[error("addon invocation has ended")]
    Closed,
    #[error("addon string limit exceeded")]
    StringLimit,
    #[error("addon event context is unavailable")]
    Context,
    #[error(transparent)]
    Outbound(#[from] OutboundError),
}
pub(crate) struct Meter {
    used: AtomicUsize,
    replies: AtomicUsize,
    exhausted: AtomicBool,
    limit: usize,
    pub deadline: Instant,
}
impl Meter {
    pub fn new(used: usize, limit: usize, deadline: Instant) -> Self {
        Self {
            used: AtomicUsize::new(used),
            replies: AtomicUsize::new(0),
            exhausted: AtomicBool::new(false),
            limit,
            deadline,
        }
    }
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }
    pub fn replies(&self) -> usize {
        self.replies.load(Ordering::Relaxed)
    }
    pub fn charge(&self, cost: usize) -> Result<(), AddonError> {
        if self.exhausted.load(Ordering::Relaxed) {
            return Err(AddonError::Steps);
        }
        if Instant::now() >= self.deadline {
            return Err(AddonError::Deadline);
        }
        let mut used = self.used();
        loop {
            let Some(next) = used.checked_add(cost).filter(|next| *next <= self.limit) else {
                self.exhausted.store(true, Ordering::Relaxed);
                return Err(AddonError::Steps);
            };
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return Ok(()),
                Err(actual) => used = actual,
            }
        }
    }
}
/// Restricted, invocation-scoped Sculk facilities. There is deliberately no raw
/// Context, Packet, Session, credential, filesystem or HTTP endpoint accessor.
/// Clones expire when the native callback/future completes or is dropped.
#[derive(Clone)]
pub struct AddonContext {
    ctx: Context,
    meter: Arc<Meter>,
    capabilities: Arc<[Capability]>,
    lease: watch::Receiver<bool>,
    string_limit: usize,
}
pub(crate) struct CallGuard {
    _sender: watch::Sender<bool>,
}
impl AddonContext {
    pub(crate) fn new(
        ctx: Context,
        meter: Arc<Meter>,
        capabilities: Arc<[Capability]>,
        string_limit: usize,
    ) -> (Self, CallGuard) {
        let (sender, lease) = watch::channel(false);
        (
            Self {
                ctx,
                meter,
                capabilities,
                lease,
                string_limit,
            },
            CallGuard { _sender: sender },
        )
    }
    /// Charge additional cooperative native work. Sculk facility calls charge
    /// independently, even if a callback's declared cost is small. Zero checks
    /// cancellation/deadline without consuming a step.
    pub fn charge(&self, cost: usize) -> Result<(), AddonError> {
        if *self.lease.borrow() || self.lease.has_changed().is_err() {
            return Err(AddonError::Closed);
        }
        if self.ctx.is_cancelled() {
            return Err(AddonError::Cancelled);
        }
        self.meter.charge(cost)
    }
    pub fn has_capability(&self, capability: &Capability) -> bool {
        self.charge(1).is_ok() && self.capabilities.contains(capability)
    }
    fn require(&self, capability: Capability) -> Result<(), AddonError> {
        self.charge(1)?;
        if !self.capabilities.contains(&capability) {
            return Err(AddonError::Capability);
        }
        Ok(())
    }
    pub fn message(&self) -> Result<&str, AddonError> {
        self.require(Capability::ReadMessage)?;
        let text = self.ctx.content().ok_or(AddonError::Context)?;
        if text.len() > self.string_limit {
            return Err(AddonError::StringLimit);
        }
        Ok(text)
    }
    pub fn sender_id(&self) -> Result<i64, AddonError> {
        self.require(Capability::ReadIdentity)?;
        self.ctx
            .sender()
            .map(|id| id.get())
            .ok_or(AddonError::Context)
    }
    pub fn room_id(&self) -> Result<i64, AddonError> {
        self.require(Capability::ReadIdentity)?;
        self.ctx
            .room()
            .map(|id| id.get())
            .ok_or(AddonError::Context)
    }
    /// One explicit adapter submission. Each attempt costs a step. Cancellation
    /// after submission preserves unknown delivery; never automatically retry.
    pub async fn reply(&self, text: &str) -> Result<OutboundReceipt, AddonError> {
        self.require(Capability::Reply)?;
        if text.len() > self.string_limit {
            return Err(AddonError::StringLimit);
        }
        tokio::task::consume_budget().await;
        self.charge(0)?;
        let mut lease = self.lease.clone();
        tokio::select! {biased;
            _=closed(&mut lease)=>Err(AddonError::Outbound(OutboundError::DeliveryUnknown)),
            _=self.ctx.cancelled()=>Err(AddonError::Outbound(OutboundError::DeliveryUnknown)),
            _=tokio::time::sleep_until(self.meter.deadline)=>Err(AddonError::Outbound(OutboundError::DeliveryUnknown)),
            result=self.ctx.reply(text)=>{if result.is_ok(){self.meter.replies.fetch_add(1,Ordering::Relaxed);}result.map_err(AddonError::from)},
        }
    }
    pub async fn cancelled(&self) {
        let mut lease = self.lease.clone();
        tokio::select! {_=closed(&mut lease)=>{},_=self.ctx.cancelled()=>{},_=tokio::time::sleep_until(self.meter.deadline)=>{}}
    }
}
async fn closed(receiver: &mut watch::Receiver<bool>) {
    loop {
        if *receiver.borrow_and_update() || receiver.changed().await.is_err() {
            return;
        }
    }
}
