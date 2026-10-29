use crate::{
    command, permissions::Permissions, BoxFuture, Command, Context, DisabledOutbound, Event,
    EventKind, EventSource, MemoryPermissions, Outbound, PermissionId, PermissionStore,
    SourceError,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{mpsc, watch};

/// Immutable startup policy; all handlers run sequentially in admitted FIFO order.
#[derive(Clone, Debug)]
pub struct Config {
    /// Exact UTF-8 prefix, 1..64 bytes. Default `!`.
    pub prefix: String,
    /// False performs ASCII case folding of registered names and lookup tokens.
    pub case_sensitive: bool,
    /// 1..128 queue slots; full queue terminates intake explicitly, never blocks it.
    pub event_capacity: usize,
    /// 1..1 MiB maximum retained packet body per admitted event (default 64 KiB).
    pub max_event_bytes: usize,
    /// Bounded recent `(room, message ID)` window per run, 1..4096.
    pub dedup_capacity: usize,
    /// Maximum adapter shutdown duration, nonzero and at most 30 seconds.
    pub shutdown_timeout: Duration,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            prefix: "!".into(),
            case_sensitive: true,
            event_capacity: 16,
            max_event_bytes: 65536,
            dedup_capacity: 256,
            shutdown_timeout: Duration::from_secs(2),
        }
    }
}
/// Startup-only errors. Failed registrations leave the previous registry intact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RegistrationError {
    #[error("invalid bot configuration or registration")]
    Invalid,
    #[error("duplicate command, alias or permission")]
    Duplicate,
    #[error("bot registration limit exceeded")]
    Limit,
    #[error("command requires an unregistered permission")]
    UnregisteredPermission,
}
/// Expected handler failure; callbacks must yield and must not panic or block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HandlerError {
    #[error("handler failed")]
    Failed,
    #[error(transparent)]
    Outbound(#[from] crate::OutboundError),
}
/// Structured failures delivered to error handlers, without message text or IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Handler(HandlerError),
    Command(HandlerError),
    PermissionDenied,
    UnknownCommand,
    Parse(crate::ParseError),
}
/// Why intake stopped. Partial handler effects are never retried automatically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    Exhausted,
    Cancelled,
    QueueOverflow,
    EventTooLarge,
    Source(SourceError),
}
/// Finite counters and first failure, never an unbounded error/message collection.
#[derive(Debug)]
pub struct RunReport {
    pub stop: StopReason,
    /// Events removed from the source, including an event rejected at admission.
    pub received: u64,
    pub admitted: u64,
    /// Events fully dispatched (including denied/unknown/failed command attempts).
    pub completed: u64,
    pub duplicates: u64,
    /// Command callbacks invoked once; errors still count as invocations.
    pub commands: u64,
    pub failures: u64,
    pub first_failure: Option<Failure>,
    /// Adapter cleanup result. A timeout is an error, not a clean close claim.
    pub shutdown: Result<(), SourceError>,
}
type Handler = Arc<dyn Fn(Context) -> BoxFuture<'static, Result<(), HandlerError>> + Send + Sync>;
fn boxed<F, Fut>(handler: F) -> Handler
where
    F: Fn(Context) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), HandlerError>> + Send + 'static,
{
    Arc::new(move |ctx| Box::pin(handler(ctx)))
}
struct Registered {
    command: Command,
    handler: Handler,
}
/// Startup builder consumed by `run`; no registration mutation or definition
/// parsing in the receive loop. A future script host registers the same callbacks.
pub struct Bot {
    config: Config,
    commands: Vec<Registered>,
    lookup: HashMap<String, usize>,
    handlers: Vec<(EventKind, Handler)>,
    errors: Vec<Handler>,
    permissions: Permissions,
    outbound: Arc<dyn Outbound>,
}
impl Bot {
    /// Validate limits; creates no tasks, timers or connections.
    pub fn new(config: Config) -> Result<Self, RegistrationError> {
        if config.prefix.is_empty()
            || config.prefix.len() > 64
            || config.prefix.contains(char::is_whitespace)
            || config.prefix.contains('\0')
            || !(1..=128).contains(&config.event_capacity)
            || !(1..=1048576).contains(&config.max_event_bytes)
            || !(1..=4096).contains(&config.dedup_capacity)
            || config.shutdown_timeout.is_zero()
            || config.shutdown_timeout > Duration::from_secs(30)
        {
            return Err(RegistrationError::Invalid);
        }
        Ok(Self {
            config,
            commands: Vec::new(),
            lookup: HashMap::new(),
            handlers: Vec::new(),
            errors: Vec::new(),
            permissions: Permissions {
                defaults: HashMap::new(),
                store: Arc::new(MemoryPermissions::default()),
            },
            outbound: Arc::new(DisabledOutbound),
        })
    }
    /// Replace permission lookups before startup. No server roles are consulted.
    pub fn permission_store(&mut self, store: Arc<dyn PermissionStore>) {
        self.permissions.store = store;
    }
    /// Configure an explicit adapter; default reply attempts fail locally.
    pub fn outbound(&mut self, outbound: Arc<dyn Outbound>) {
        self.outbound = outbound;
    }
    /// Register at most 128 permission identifiers with explicit defaults.
    pub fn permission(&mut self, id: PermissionId, default: bool) -> Result<(), RegistrationError> {
        if self.permissions.defaults.contains_key(&id) {
            return Err(RegistrationError::Duplicate);
        }
        if self.permissions.defaults.len() == 128 {
            return Err(RegistrationError::Limit);
        }
        self.permissions.defaults.insert(id, default);
        Ok(())
    }
    /// Convenience command with no required permission/aliases.
    pub fn command<F, Fut>(&mut self, name: &str, handler: F) -> Result<(), RegistrationError>
    where
        F: Fn(Context) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), HandlerError>> + Send + 'static,
    {
        self.register(Command::new(name), handler)
    }
    /// Compile name/aliases once. At most 128 commands and 16 aliases per command.
    pub fn register<F, Fut>(
        &mut self,
        command: Command,
        handler: F,
    ) -> Result<(), RegistrationError>
    where
        F: Fn(Context) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), HandlerError>> + Send + 'static,
    {
        if self.commands.len() == 128
            || command.aliases.len() > 16
            || command.permissions.len() > 128
        {
            return Err(RegistrationError::Limit);
        }
        if command
            .permissions
            .iter()
            .any(|p| !self.permissions.defaults.contains_key(p))
        {
            return Err(RegistrationError::UnregisteredPermission);
        }
        let mut names = HashSet::new();
        for name in std::iter::once(&command.name).chain(&command.aliases) {
            if !command::valid_name(name) {
                return Err(RegistrationError::Invalid);
            }
            let key = command::key(name, self.config.case_sensitive);
            if self.lookup.contains_key(&key) || !names.insert(key) {
                return Err(RegistrationError::Duplicate);
            }
        }
        for name in names {
            self.lookup.insert(name, self.commands.len());
        }
        self.commands.push(Registered {
            command,
            handler: boxed(handler),
        });
        Ok(())
    }
    /// At most 128 general callbacks, called in registration order before commands.
    pub fn on<F, Fut>(&mut self, kind: EventKind, handler: F) -> Result<(), RegistrationError>
    where
        F: Fn(Context) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), HandlerError>> + Send + 'static,
    {
        if self.handlers.len() == 128 {
            return Err(RegistrationError::Limit);
        }
        self.handlers.push((kind, boxed(handler)));
        Ok(())
    }
    /// At most 16 error handlers. Their failures are counted without recursion.
    pub fn on_error<F, Fut>(&mut self, handler: F) -> Result<(), RegistrationError>
    where
        F: Fn(Context) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), HandlerError>> + Send + 'static,
    {
        if self.errors.len() == 16 {
            return Err(RegistrationError::Limit);
        }
        self.errors.push(boxed(handler));
        Ok(())
    }
    /// Concurrent intake and one sequential worker in this same future: no tasks
    /// are spawned. EOF drains; cancellation/overflow/source error interrupts the
    /// current handler and discards queued work with explicit report counters.
    /// Dropping this future drops its owned source and all callback futures.
    pub async fn run<S: EventSource>(
        self,
        mut source: S,
        mut cancel: watch::Receiver<bool>,
    ) -> RunReport {
        let (sender, receiver) = mpsc::channel(self.config.event_capacity);
        let (stop_tx, stop_rx) = watch::channel(false);
        let intake_cancel = stop_rx.clone();
        let mut receive_cancel = stop_rx.clone();
        let (work, mut intake) = {
            let running = async {
                tokio::join!(self.dispatch(receiver, stop_rx), async {
                    let mut received = 0;
                    let mut admitted = 0;
                    let stop = loop {
                        let event = tokio::select! {biased;
                            _=cancelled(&mut receive_cancel)=>break StopReason::Cancelled,
                            result=source.recv(intake_cancel.clone())=>match result {Ok(Some(e))=>e,Ok(None)=>break StopReason::Exhausted,Err(e)=>break StopReason::Source(e)},
                        };
                        received += 1;
                        if retained_bytes(&event) > self.config.max_event_bytes {
                            break StopReason::EventTooLarge;
                        }
                        if sender.try_send(event).is_err() {
                            break StopReason::QueueOverflow;
                        }
                        admitted += 1;
                        // Fairness for an immediately-ready synthetic or buffered source.
                        // This is cooperative scheduling, not a task per received event.
                        tokio::task::yield_now().await;
                    };
                    if stop != StopReason::Exhausted {
                        stop_tx.send_replace(true);
                    }
                    drop(sender);
                    (stop, received, admitted)
                })
            };
            tokio::pin!(running);
            tokio::select! {biased;
                _=cancelled(&mut cancel)=>{stop_tx.send_replace(true);let (work,mut intake)=running.await;intake.0=StopReason::Cancelled;(work,intake)},
                result=&mut running=>result,
            }
        };
        if *cancel.borrow() || cancel.has_changed().is_err() {
            intake.0 = StopReason::Cancelled;
        }
        stop_tx.send_replace(true);
        let shutdown = tokio::time::timeout(self.config.shutdown_timeout, source.shutdown())
            .await
            .unwrap_or(Err(SourceError::Shutdown));
        RunReport {
            stop: intake.0,
            received: intake.1,
            admitted: intake.2,
            completed: work.completed,
            duplicates: work.duplicates,
            commands: work.commands,
            failures: work.failures,
            first_failure: work.first_failure,
            shutdown,
        }
    }
    async fn dispatch(
        &self,
        mut receiver: mpsc::Receiver<Event>,
        mut cancel: watch::Receiver<bool>,
    ) -> Work {
        let mut work = Work::default();
        let mut seen = HashSet::new();
        let mut order = VecDeque::new();
        let permissions = Arc::new(Permissions {
            defaults: self.permissions.defaults.clone(),
            store: self.permissions.store.clone(),
        });
        loop {
            let event = tokio::select! {biased;_=cancelled(&mut cancel)=>break,event=receiver.recv()=>match event{Some(e)=>e,None=>break}};
            if let Event::Message(m) = &event {
                let id = (m.room, m.id);
                if seen.contains(&id) {
                    work.duplicates += 1;
                    continue;
                }
                if seen.len() == self.config.dedup_capacity {
                    if let Some(old) = order.pop_front() {
                        seen.remove(&old);
                    }
                }
                seen.insert(id);
                order.push_back(id);
            }
            let ctx = Context {
                event: Arc::new(event),
                invocation: None,
                failure: None,
                permissions: permissions.clone(),
                outbound: self.outbound.clone(),
                cancel: cancel.clone(),
            };
            let dispatch = self.dispatch_one(ctx, &mut work);
            tokio::select! {biased;_=cancelled(&mut cancel)=>break,_=dispatch=>work.completed+=1}
        }
        work
    }
    async fn dispatch_one(&self, mut ctx: Context, work: &mut Work) {
        for (kind, handler) in &self.handlers {
            if kind.matches(ctx.event()) {
                if let Err(error) = handler(ctx.clone()).await {
                    self.failure(ctx.clone(), Failure::Handler(error), work)
                        .await;
                }
            }
        }
        let Some(text) = ctx
            .message()
            .filter(|m| m.kind == sculk_client::messaging::MessageType::Text)
            .and_then(|m| m.text())
        else {
            return;
        };
        let invocation = match command::parse_invocation(text, &self.config.prefix) {
            Ok(Some(i)) => i,
            Ok(None) => return,
            Err(e) => {
                self.failure(ctx, Failure::Parse(e), work).await;
                return;
            }
        };
        let name = invocation.name(text);
        let index = if self.config.case_sensitive {
            self.lookup.get(name).copied()
        } else {
            self.lookup.get(&name.to_ascii_lowercase()).copied()
        };
        ctx.invocation = Some(Arc::new(invocation));
        let Some(index) = index else {
            self.failure(ctx, Failure::UnknownCommand, work).await;
            return;
        };
        let Registered { command, handler } = &self.commands[index];
        if command.permissions.iter().any(|id| !ctx.has_permission(id)) {
            self.failure(ctx, Failure::PermissionDenied, work).await;
            return;
        }
        work.commands += 1;
        if let Err(e) = handler(ctx.clone()).await {
            self.failure(ctx, Failure::Command(e), work).await;
        }
    }
    async fn failure(&self, mut ctx: Context, failure: Failure, work: &mut Work) {
        work.failures += 1;
        work.first_failure.get_or_insert(failure);
        ctx.failure = Some(failure);
        for handler in &self.errors {
            if handler(ctx.clone()).await.is_err() {
                work.failures += 1;
            }
        }
    }
}
#[derive(Default)]
struct Work {
    completed: u64,
    duplicates: u64,
    commands: u64,
    failures: u64,
    first_failure: Option<Failure>,
}
fn retained_bytes(event: &Event) -> usize {
    match event {
        Event::Message(m) => m.packet().body().len(),
        Event::Unknown(sculk_client::messaging::Event::Message(m)) => m.packet().body().len(),
        Event::Unknown(
            sculk_client::messaging::Event::Unknown(p)
            | sculk_client::messaging::Event::Malformed(p),
        ) => p.body().len(),
        _ => 0,
    }
}
pub(crate) async fn cancelled(receiver: &mut watch::Receiver<bool>) {
    loop {
        if *receiver.borrow_and_update() {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}
