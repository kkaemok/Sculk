mod outbound;
mod state;
#[cfg(test)]
mod tests;

use crate::{Host, Result};
use outbound::{OneWrite, Outcome, ReadOnly};
use sculk_auth_client::{permit_service, signal_channel, Prepared, Progress, Stage};
use sculk_bot::{
    Bot, Connection, Event, Failure, Gap, HandlerError, Message, MessageId, OutboundError,
    SessionOutbound, SessionState, StopReason, Synchronization,
};
use sculk_client::{
    auth, connect,
    managed::{self, Room},
    messaging::{Event as Incoming, ReceiveError},
    recovery::{AttemptError, RetryPolicy},
    session::{RequestError, Session},
    synchronization::{self, Cursor, Observation},
};
use state::State;
use std::{
    collections::VecDeque,
    future::Future,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::Duration,
};
use tokio::{
    signal::unix::{signal, Signal, SignalKind},
    sync::watch,
    time::Instant,
};

const HEARTBEAT: Duration = Duration::from_secs(30);
const VALIDATION_WINDOW: Duration = Duration::from_secs(90);
const PAGE_SIZE: u16 = 32;

#[derive(Default)]
struct Options {
    trigger: Option<PathBuf>,
    report: Option<PathBuf>,
    refresh: bool,
}

impl Options {
    fn parse(args: &[&str], service: bool) -> Result<Self> {
        let mut options = Self::default();
        let mut args = args.iter();
        while let Some(argument) = args.next() {
            match *argument {
                "--validate-command" if !service && options.trigger.is_none() => {
                    options.trigger =
                        Some(args.next().ok_or("missing private trigger file")?.into());
                }
                "--report" if options.report.is_none() => {
                    options.report = Some(args.next().ok_or("missing report path")?.into());
                }
                "--refresh" if !options.refresh => options.refresh = true,
                _ => return Err("invalid live runtime options".into()),
            }
        }
        Ok(options)
    }
}

pub(super) fn launch(
    host_path: &Path,
    service: bool,
    auth_path: &Path,
    args: &[&str],
) -> Result<ExitCode> {
    let options = Options::parse(args, service)?;
    let progress = Progress::default();
    let result = Host::load(host_path)
        .and_then(|host| execute(&host, service, auth_path, &options, &progress));
    sculk_auth_client::report(
        options.report.as_deref(),
        if service {
            "live-service"
        } else if options.trigger.is_some() {
            "bot-validation"
        } else {
            "live-runtime"
        },
        &progress,
        result.as_ref().err(),
        result
            .as_ref()
            .err()
            .and_then(|error| error.downcast_ref::<RuntimeError>())
            .map(RuntimeError::diagnostic),
    )?;
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!(
                "live/stopped stage={} reason={error}",
                progress.stage.get().label()
            );
            Ok(ExitCode::from(78))
        }
    }
}

fn execute(
    host: &Host<'_>,
    service: bool,
    auth_path: &Path,
    options: &Options,
    progress: &Progress,
) -> Result<()> {
    let prepared = Prepared::load(auth_path, progress, true)?;
    let room = prepared.room().ok_or("authorized room is required")?;
    let trigger = options
        .trigger
        .as_ref()
        .map(|path| {
            auth::read_private_file(path, 16_384)
                .map_err(|_| "approved private trigger file unavailable")
        })
        .transpose()?;
    let trigger = trigger
        .as_ref()
        .map(|bytes| {
            let text = std::str::from_utf8(bytes).map_err(|_| "invalid UTF-8 trigger")?;
            let text = text.strip_suffix('\n').unwrap_or(text);
            if text.is_empty() || text.contains(['\n', '\r', '\0']) {
                return Err("validation trigger must be one nonempty command line");
            }
            Ok(text)
        })
        .transpose()?;
    let live = &host.config.live;
    let writes = live.enable_write || trigger.is_some();
    let state = if trigger.is_some() {
        State::probe(room)
    } else {
        prepared.require_validated_profile()?;
        let directory = host.directory.join(
            live.checkpoint_dir
                .as_ref()
                .ok_or("live.checkpoint_dir is required")?,
        );
        let storage = &host.config.storage;
        let storage_path = storage.enabled.then(|| host.directory.join(&storage.path));
        progress.stage.set(Stage::Checkpoint);
        State::open(room, &directory, writes, storage_path.as_deref())?
    };
    progress.stage.set(Stage::Configuration);
    let (bot, _) = host.compile(state.counters.clone())?;
    let approval = if service {
        permit_service(&prepared, writes, progress)?
    } else {
        prepared.permit_terminal(progress, true, writes)?
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let (shutdown_signal, cancel) = signal_channel()?;
        let reload = if trigger.is_none() {
            Some(Reload {
                signal: signal(SignalKind::hangup())?,
                config: host.config.clone(),
                directory: host.directory.to_path_buf(),
            })
        } else {
            None
        };
        let result = async {
            let authorized = prepared
                .authorize(approval, options.refresh, progress, cancel.clone())
                .await?;
            let manager = Room::new(room, 512, managed::Budget::default(), &state.staged)?;
            let mut runner = Runner::new(
                bot,
                manager,
                state,
                progress,
                cancel.clone(),
                trigger,
                writes.then(|| Duration::from_secs(live.write_interval_seconds)),
            );
            runner.reload = reload;
            runner
                .run(|| authorized.connect(progress, cancel.clone()))
                .await
        }
        .await;
        shutdown_signal.abort();
        result
    })
}

#[derive(Debug, thiserror::Error)]
pub(super) enum RuntimeError {
    #[error("credentials expired or the account requires explicit intervention")]
    Intervention,
    #[error("selected-room continuity could not be verified")]
    Continuity,
    #[error("selected-room history is no longer available at the checkpoint")]
    HistoryUnavailable,
    #[error("room history did not cover the last recorded write attempt")]
    UnreconciledWrite,
    #[error("the room connection repeatedly stopped during recovery")]
    RecoveryLimit,
    #[error("script dispatch did not complete successfully")]
    Dispatch,
    #[error("the peer rejected the script WRITE with status {0}")]
    Rejected(i64),
    #[error("script WRITE delivery is unknown")]
    UnknownDelivery,
    #[error("validation did not observe the authorized command")]
    ValidationTimeout,
    #[error("command validation was cancelled before completion")]
    ValidationCancelled,
    #[error("validation did not dispatch a command with one acknowledged WRITE")]
    ValidationCommand,
    #[error("selected-room connection was interrupted during command validation")]
    ValidationInterrupted,
    #[error("live checkpoint or counter storage operation failed")]
    Storage,
    #[error("shutdown did not confirm a clean transport close")]
    Shutdown,
}

impl RuntimeError {
    fn diagnostic(&self) -> (&'static str, Option<i64>) {
        match self {
            Self::Intervention => ("reauthorization-required", None),
            Self::Continuity => ("room-continuity-unverified", None),
            Self::HistoryUnavailable => ("history-unavailable", None),
            Self::UnreconciledWrite => ("unreconciled-write-boundary", None),
            Self::RecoveryLimit => ("recovery-limit", None),
            Self::Dispatch => ("script-dispatch-failed", None),
            Self::Rejected(status) => ("write-rejected", Some(*status)),
            Self::UnknownDelivery => ("write-delivery-unknown", None),
            Self::ValidationTimeout => ("bot-validation-timeout", None),
            Self::ValidationCancelled => ("bot-validation-cancelled", None),
            Self::ValidationCommand => ("bot-validation-incomplete", None),
            Self::ValidationInterrupted => ("bot-validation-interrupted", None),
            Self::Storage => ("checkpoint-storage-failed", None),
            Self::Shutdown => ("shutdown-unconfirmed", None),
        }
    }
}

enum Next {
    Stop,
    Reconnect,
    Message(Message),
}

#[derive(Default)]
struct Health {
    dispatched: u64,
    reconciled: u64,
    suppressed: u64,
    writes: u64,
}

struct Runner<'a> {
    bot: Bot,
    room: Room,
    state: State,
    progress: &'a Progress,
    cancel: watch::Receiver<bool>,
    trigger: Option<&'a str>,
    interval: Option<Duration>,
    echoes: VecDeque<MessageId>,
    health: Health,
    reload: Option<Reload>,
}

struct Reload {
    signal: Signal,
    config: crate::config::Configuration,
    directory: PathBuf,
}

async fn hangup(reload: &mut Option<Reload>) {
    if let Some(reload) = reload {
        if reload.signal.recv().await.is_some() {
            return;
        }
    }
    std::future::pending().await
}

impl<'a> Runner<'a> {
    fn new(
        bot: Bot,
        room: Room,
        state: State,
        progress: &'a Progress,
        cancel: watch::Receiver<bool>,
        trigger: Option<&'a str>,
        interval: Option<Duration>,
    ) -> Self {
        Self {
            bot,
            room,
            state,
            progress,
            cancel,
            trigger,
            interval,
            echoes: VecDeque::new(),
            health: Health::default(),
            reload: None,
        }
    }

    fn stopping(&self) -> bool {
        *self.cancel.borrow() || self.cancel.has_changed().is_err()
    }

    fn health(&self, status: &str) {
        println!("live/{status} generation={} dispatched={} history-checked={} suppressed={} acknowledged-writes={}",
            self.room.generation(), self.health.dispatched, self.health.reconciled, self.health.suppressed, self.health.writes);
    }

    async fn run<F, Fut>(mut self, connector: F) -> Result<()>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = std::result::Result<Session, connect::Error>>,
    {
        let result = self.drive(connector).await;
        let result = if result.is_ok() && self.trigger.is_some() && self.health.dispatched == 0 {
            Err(RuntimeError::ValidationCancelled.into())
        } else {
            result
        };
        let failed_stage = self.progress.stage.get();
        self.progress.stage.set(Stage::Shutdown);
        let closed =
            tokio::time::timeout(Duration::from_secs(6), self.room.shutdown_checked()).await;
        let closed = matches!(closed, Ok(Ok(())));
        if closed {
            self.progress.completed(Stage::Shutdown);
        }
        self.health(if closed {
            "closed"
        } else {
            "close-unconfirmed"
        });
        if result.is_err() {
            self.progress.stage.set(failed_stage);
            return result;
        }
        if !closed {
            return Err(RuntimeError::Shutdown.into());
        }
        self.progress.stage.set(Stage::Complete);
        Ok(())
    }

    async fn drive<F, Fut>(&mut self, mut connector: F) -> Result<()>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = std::result::Result<Session, connect::Error>>,
    {
        let mut reconnects = 0;
        let mut stable_since = Instant::now();
        loop {
            if self.stopping() {
                return Ok(());
            }
            if self.room.generation() != 0 {
                if self.trigger.is_some() {
                    return Err(RuntimeError::ValidationInterrupted.into());
                }
                if stable_since.elapsed() >= Duration::from_secs(60) {
                    reconnects = 0;
                }
                reconnects += 1;
                if reconnects > 5 {
                    return Err(RuntimeError::RecoveryLimit.into());
                }
                self.progress.stage.set(Stage::Recovery);
                self.health("reconnecting");
                // Drop any still-active peer before reconnecting. Expired
                // sessions are latched by the manager even when retired.
                if self.room.retire().await.is_err() {
                    self.health("previous-close-unconfirmed");
                }
                let delay = Duration::from_secs((1 << (reconnects - 1)).min(8));
                let mut cancel = self.cancel.clone();
                tokio::select! { biased;
                    _ = wait_cancel(&mut cancel) => return Ok(()),
                    _ = tokio::time::sleep(delay) => {},
                }
            }
            let policy = RetryPolicy {
                attempts: 3,
                initial: Duration::from_secs(1),
                maximum: Duration::from_secs(8),
            };
            if let Err(error) = self
                .room
                .reconnect(
                    policy,
                    Duration::from_secs(65),
                    self.cancel.clone(),
                    &mut connector,
                )
                .await
            {
                return match error {
                    managed::Error::Connect(AttemptError::Cancelled) => Ok(()),
                    managed::Error::Connect(
                        AttemptError::Transient(error) | AttemptError::Permanent(error),
                    ) => Err(error.into()),
                    managed::Error::Intervention => Err(RuntimeError::Intervention.into()),
                    other => Err(other.into()),
                };
            }
            stable_since = Instant::now();
            if let Err(error) = self.synchronize().await {
                if self.stopping() {
                    return Ok(());
                }
                if error
                    .downcast_ref::<managed::Error>()
                    .is_some_and(recoverable)
                {
                    continue;
                }
                return Err(error);
            }
            let adapter = if let Some(interval) = self.interval {
                Some(Arc::new(SessionOutbound::new(
                    self.room.text_sender()?,
                    self.room.room_id(),
                    interval,
                )?))
            } else {
                None
            };
            self.health("ready");
            self.notify(Event::Connection(Connection::State {
                generation: self.room.generation(),
                state: SessionState::Authenticated,
            }))
            .await?;
            if self.trigger.is_some() {
                println!("live/validation-ready: send the authorized command in the selected room");
            }
            let deadline = self.trigger.map(|_| Instant::now() + VALIDATION_WINDOW);
            let mut heartbeat = Instant::now() + HEARTBEAT;
            loop {
                let message = match self.next(&mut heartbeat, deadline).await? {
                    Next::Stop => return Ok(()),
                    Next::Reconnect => break,
                    Next::Message(message) => message,
                };
                let echo = self.echoes.contains(&message.id);
                let chosen = self
                    .trigger
                    .is_none_or(|trigger| message.text() == Some(trigger));
                if echo || !chosen || message.id.get() <= self.state.attempted.get() {
                    self.acknowledge(&message, false).await?;
                    self.health.suppressed = self.health.suppressed.saturating_add(1);
                    continue;
                }
                if self.dispatch(&message, adapter.clone()).await? {
                    return Ok(());
                }
            }
            self.notify(Event::Connection(Connection::Lost { expired: false }))
                .await?;
        }
    }

    async fn synchronize(&mut self) -> Result<()> {
        self.progress.stage.set(Stage::RoomSelection);
        // This is a room-bound CHATINFO request, requiring its exact echoed ID.
        let through = self.room.watermark(self.cancel.clone()).await?;
        self.progress.completed(Stage::RoomSelection);
        self.progress.stage.set(Stage::Synchronization);
        let value = through.map_or(0, MessageId::get);
        let current = self.room.cursor().get();
        if value < current {
            return Err(RuntimeError::Continuity.into());
        }
        if value < self.state.attempted.get() {
            return Err(RuntimeError::UnreconciledWrite.into());
        }
        if value == current || !self.state.initialized {
            self.room.tracker_mut().align_watermark(
                through,
                !self.state.initialized,
                &mut self.state.staged,
            )?;
            self.persist(false).await?;
        } else if let Some(through) = through {
            while self.room.cursor().get() < value {
                let page = self
                    .room
                    .synchronize(through, PAGE_SIZE, self.cancel.clone())
                    .await?;
                if page.messages().is_empty() {
                    return Err(RuntimeError::Continuity.into());
                }
                self.room.tracker_mut().begin_page(&page)?;
                if self.room.tracker().gap() == Gap::UnavailableHistory {
                    return Err(RuntimeError::HistoryUnavailable.into());
                }
                for index in 0..page.messages().len() {
                    // Historical processing validates and classifies the page.
                    // No handler or outbound effect is invoked for these logs.
                    self.room.tracker_mut().acknowledge_history(
                        &page,
                        index,
                        &mut self.state.staged,
                    )?;
                    self.health.reconciled = self.health.reconciled.saturating_add(1);
                }
                let gap = self.room.tracker_mut().finish_page()?;
                if self.room.cursor().get() == value && gap != Gap::None {
                    return Err(RuntimeError::Continuity.into());
                }
                self.persist(false).await?;
            }
        }
        self.progress.completed(Stage::Synchronization);
        self.notify(Event::Synchronization(Synchronization {
            room: self.room.room_id(),
            cursor: self.state.committed,
            gap: self.room.tracker().gap(),
        }))
        .await?;
        Ok(())
    }

    async fn reconcile(&mut self) -> Result<Option<Next>> {
        match self.synchronize().await {
            Ok(()) => Ok(None),
            Err(_) if self.stopping() => Ok(Some(Next::Stop)),
            Err(error)
                if error
                    .downcast_ref::<managed::Error>()
                    .is_some_and(recoverable) =>
            {
                Ok(Some(Next::Reconnect))
            }
            Err(error) => Err(error),
        }
    }

    async fn next(&mut self, heartbeat: &mut Instant, deadline: Option<Instant>) -> Result<Next> {
        loop {
            self.progress.stage.set(Stage::MessageReception);
            let mut cancel = self.cancel.clone();
            let event = tokio::select! { biased;
                _ = wait_cancel(&mut cancel) => return Ok(Next::Stop),
                _ = until(deadline) => return Err(RuntimeError::ValidationTimeout.into()),
                _ = hangup(&mut self.reload) => {
                    if let Some(reload) = &self.reload {
                        self.reload_scripts(reload.config.clone(), reload.directory.clone()).await;
                    }
                    continue;
                },
                _ = tokio::time::sleep_until(*heartbeat) => {
                    match self.room.heartbeat(self.cancel.clone()).await {
                        Ok(()) => {
                            *heartbeat = Instant::now() + HEARTBEAT;
                            self.health("healthy");
                            continue;
                        }
                        Err(_) if self.stopping() => return Ok(Next::Stop),
                        Err(error) if recoverable(&error) => return Ok(Next::Reconnect),
                        Err(error) => return Err(error.into()),
                    }
                },
                item = self.room.recv(self.cancel.clone()) => item,
            };
            let message = match event {
                Ok(Incoming::Message(message)) if message.room == self.room.room_id() => message,
                Ok(Incoming::Malformed(_)) => {
                    self.room.tracker_mut().suspect_gap();
                    if let Some(step) = self.reconcile().await? {
                        return Ok(step);
                    }
                    continue;
                }
                Ok(_) => continue,
                Err(managed::Error::Receive(ReceiveError::Cancelled)) => return Ok(Next::Stop),
                Err(managed::Error::Receive(ReceiveError::Expired)) => {
                    return Err(RuntimeError::Intervention.into())
                }
                Err(managed::Error::Receive(ReceiveError::Lagged(count))) => {
                    self.health("lagged");
                    self.notify(Event::Lagged(count)).await?;
                    if let Some(step) = self.reconcile().await? {
                        return Ok(step);
                    }
                    continue;
                }
                Err(error) if recoverable(&error) => return Ok(Next::Reconnect),
                Err(error) => return Err(error.into()),
            };
            let mut observation = self.room.tracker().observe(&message)?;
            if observation == Observation::New
                && (self.room.tracker().gap() != Gap::None
                    || message.previous_id() != Some(self.room.cursor().get()))
            {
                self.room.tracker_mut().suspect_gap();
                if let Some(step) = self.reconcile().await? {
                    return Ok(step);
                }
                observation = self.room.tracker().observe(&message)?;
                if observation == Observation::New
                    && message.previous_id() != Some(self.room.cursor().get())
                {
                    return Err(RuntimeError::Continuity.into());
                }
            }
            if observation != Observation::New {
                self.health.suppressed = self.health.suppressed.saturating_add(1);
                continue;
            }
            return Ok(Next::Message(message));
        }
    }

    async fn reload_scripts(&mut self, config: crate::config::Configuration, directory: PathBuf) {
        let counters = self.state.counters.clone();
        let replacement = tokio::task::spawn_blocking(move || {
            Host {
                config,
                directory: &directory,
            }
            .compile(counters)
            .map(|(bot, _)| bot)
        })
        .await;
        if let Ok(Ok(bot)) = replacement {
            self.bot = bot;
            self.health("scripts-reloaded");
        } else {
            self.health("script-reload-rejected");
        }
    }

    async fn dispatch(
        &mut self,
        message: &Message,
        adapter: Option<Arc<SessionOutbound>>,
    ) -> Result<bool> {
        let write = if let Some(adapter) = adapter {
            self.progress.stage.set(Stage::Checkpoint);
            self.state
                .fence(Cursor::new(message.id.get())?)
                .await
                .map_err(|_| RuntimeError::Storage)?;
            let outbound = Arc::new(OneWrite::new(adapter));
            self.bot.outbound(outbound.clone());
            Some(outbound)
        } else {
            self.bot.outbound(Arc::new(ReadOnly));
            None
        };
        if self.trigger.is_some() {
            self.progress.completed(Stage::MessageReception);
        }
        self.progress.stage.set(Stage::BotDispatch);
        let report = self.dispatch_event(Event::Message(message.clone())).await?;
        if report.commands == 1 {
            self.progress.completed(Stage::BotDispatch);
        }
        let outcome = write
            .as_ref()
            .map_or(Outcome::Unused, |outbound| outbound.outcome());
        self.bot.outbound(Arc::new(ReadOnly));
        match outcome {
            Outcome::Pending | Outcome::Unknown => {
                self.progress.stage.set(Stage::MessageSend);
                return Err(RuntimeError::UnknownDelivery.into());
            }
            Outcome::Rejected(status) => {
                self.progress.stage.set(Stage::MessageSend);
                return Err(RuntimeError::Rejected(status).into());
            }
            Outcome::Acknowledged(id) => {
                self.progress.completed(Stage::MessageSend);
                self.health.writes = self.health.writes.saturating_add(1);
                if self.echoes.len() == 512 {
                    self.echoes.pop_front();
                }
                self.echoes.push_back(id);
            }
            Outcome::Unused | Outcome::NotSubmitted => {}
        }
        if report.stop == StopReason::Cancelled && self.stopping() {
            return Ok(true);
        }
        let no_effect = matches!(
            report.first_failure,
            Some(
                Failure::UnknownCommand
                    | Failure::PermissionDenied
                    | Failure::Cooldown
                    | Failure::Parse(_)
            ) | Some(Failure::Handler(HandlerError::Outbound(
                OutboundError::Unavailable | OutboundError::NotSent
            ))) | Some(Failure::Command(HandlerError::Outbound(
                OutboundError::Unavailable | OutboundError::NotSent
            )))
        );
        if report.stop != StopReason::Exhausted
            || report.shutdown.is_err()
            || report.completed != 1
            || (report.failures != 0 && !(report.failures == 1 && no_effect))
        {
            return Err(RuntimeError::Dispatch.into());
        }
        if self.trigger.is_some()
            && (report.commands != 1
                || !matches!(outcome, Outcome::Acknowledged(_))
                || report.failures != 0)
        {
            return Err(RuntimeError::ValidationCommand.into());
        }
        self.acknowledge(message, true).await?;
        self.health.dispatched = self.health.dispatched.saturating_add(1);
        Ok(self.trigger.is_some())
    }

    async fn notify(&mut self, event: Event) -> Result<()> {
        self.bot.outbound(Arc::new(ReadOnly));
        self.dispatch_event(event).await?;
        Ok(())
    }

    async fn dispatch_event(&self, event: Event) -> Result<sculk_bot::RunReport> {
        let (sender, source) = sculk_bot::channel(1)?;
        sender.try_send(event).map_err(|_| RuntimeError::Dispatch)?;
        drop(sender);
        Ok(self.bot.run_source(source, self.cancel.clone()).await)
    }

    async fn acknowledge(&mut self, message: &Message, save_counters: bool) -> Result<()> {
        self.room
            .tracker_mut()
            .acknowledge_live(message, &mut self.state.staged)?;
        if self.room.cursor().get() != message.id.get() {
            return Err(RuntimeError::Continuity.into());
        }
        self.persist(save_counters).await
    }

    async fn persist(&mut self, save_counters: bool) -> Result<()> {
        let prior = self.state.committed;
        if prior == self.room.cursor() && !save_counters && self.state.initialized {
            return Ok(());
        }
        let stage = self.progress.stage.replace(Stage::Checkpoint);
        let initialize = !self.state.initialized;
        self.state
            .flush(save_counters)
            .await
            .map_err(|_| RuntimeError::Storage)?;
        if initialize || prior != self.state.committed {
            self.progress.completed(Stage::Checkpoint);
        }
        self.progress.stage.set(stage);
        Ok(())
    }
}

fn recoverable(error: &managed::Error) -> bool {
    matches!(
        error,
        managed::Error::Receive(ReceiveError::Inactive)
            | managed::Error::Sync(synchronization::Error::Session(RequestError::Transport(
                sculk_client::Error::Closed | sculk_client::Error::Timeout
            )))
    )
}

async fn until(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
}

async fn wait_cancel(cancel: &mut watch::Receiver<bool>) {
    while !*cancel.borrow_and_update() {
        if cancel.changed().await.is_err() {
            break;
        }
    }
}
