mod config;
mod storage;

use config::Configuration;
use sculk_bot::{Bot, BoxFuture, Event, Message, Outbound, OutboundError, OutboundReceipt, RoomId};
use sculk_protocol::{Command, Packet};
use sculk_script::{CompiledScripts, Compiler, GlobalCounters};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use storage::Storage;
use tokio::sync::watch;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const HELP: &str = "\
sculk [--config PATH] init
sculk [--config PATH] doctor | status
sculk [--config PATH] script check | list
sculk [--config PATH] run --offline
sculk auth login PRIVATE_JSON [owner validation options]
sculk auth status
sculk rooms list | info PRIVATE_JSON
The offline console supports :reload, :list, :room ID, :status and :quit.
Build with --features live-validation to use the interactive owner probe.";

const STARTER_CONFIG: &str = "\
[bot]
scripts = \"scripts\"
default_prefixes = [\"!\", \"/\"]

[limits]
event_capacity = 16
handler_ms = 100

[storage]
enabled = false
path = \"data/sculk.db\"
";

const STARTER_SCRIPT: &str = include_str!("../../../examples/openchat-bot/scripts/chat.sculk");

fn main() -> ExitCode {
    match cli(std::env::args().skip(1).collect()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("sculk: {error}");
            ExitCode::FAILURE
        }
    }
}

fn cli(args: Vec<String>) -> Result<ExitCode> {
    let mut tail = args.as_slice();
    let mut path = Path::new("sculk.toml");
    if let [flag, value, rest @ ..] = tail {
        if flag == "--config" {
            path = Path::new(value);
            tail = rest;
        }
    }
    let names = tail.iter().map(String::as_str).collect::<Vec<_>>();
    match names.as_slice() {
        [] | ["--help" | "-h"] => println!("{HELP}"),
        ["init"] => init(path)?,
        ["doctor" | "status"] => {
            let host = Host::load(path)?;
            let (_, program) = host.compile(Arc::default())?;
            println!("Offline configuration valid. Handlers: {}. SQLite: {}. Target: {} {}.",
                program.handlers().len(), if host.config.storage.enabled { "enabled" } else { "disabled" },
                std::env::consts::OS, std::env::consts::ARCH);
        }
        ["script", "check"] => {
            let host = Host::load(path)?;
            let (_, program) = host.compile(Arc::default())?;
            println!("Script check passed. Handlers: {}.", program.handlers().len());
        }
        ["script", "list"] => list(&Host::load(path)?.script_paths()?),
        ["run", "--offline"] => Host::load(path)?.run_offline()?,
        ["run"] => return Err("continuous live operation requires owner verification of the KakaoTalk profile and transport".into()),
        ["script", "reload"] => return Err("enter :reload in the running offline console".into()),
        ["auth", "status"] => println!("The owner probe keeps credentials in memory for its current run."),
        ["auth", "login", private_config, options @ ..] => {
            return owner_probe(std::iter::once(*private_config).chain(options.iter().copied()), None);
        }
        ["rooms", "list", private_config] => return owner_probe([*private_config], Some("--read-only")),
        ["rooms", "info", private_config] => return owner_probe([*private_config], Some("--room-info")),
        _ => return Err("unknown options. Run sculk --help".into()),
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(feature = "live-validation")]
fn owner_probe<'a>(
    args: impl IntoIterator<Item = &'a str>,
    option: Option<&'a str>,
) -> Result<ExitCode> {
    let args = ["--live"]
        .into_iter()
        .chain(args)
        .chain(option)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Ok(sculk_auth_client::run_cli(&args))
}

#[cfg(not(feature = "live-validation"))]
fn owner_probe<'a>(_: impl IntoIterator<Item = &'a str>, _: Option<&str>) -> Result<ExitCode> {
    Err("build with --features live-validation to use the interactive owner probe".into())
}

fn init(path: &Path) -> Result<()> {
    if path.try_exists()? {
        return Err("configuration already exists".into());
    }
    let script_dir = config_directory(path).join("scripts");
    fs::create_dir_all(&script_dir)?;
    match private_new(&script_dir.join("chat.sculk")) {
        Ok(mut file) => file.write_all(STARTER_SCRIPT.as_bytes())?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    private_new(path)?.write_all(STARTER_CONFIG.as_bytes())?;
    println!("Created Sculk configuration. Start with sculk run --offline.");
    Ok(())
}

fn private_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn config_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

struct Host<'a> {
    config: Configuration,
    directory: &'a Path,
}

impl<'a> Host<'a> {
    fn load(path: &'a Path) -> Result<Self> {
        Ok(Self {
            config: Configuration::read(path)?,
            directory: config_directory(path),
        })
    }

    fn script_paths(&self) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        let dir = self.directory.join(&self.config.bot.scripts);
        for (index, entry) in fs::read_dir(dir)?.enumerate() {
            if index == 4096 {
                return Err("script directory entry limit exceeded".into());
            }
            let entry = entry?;
            let path = entry.path();
            if matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("sculk" | "sc")
            ) {
                if !entry.file_type()?.is_file() {
                    return Err("script entries must be regular files".into());
                }
                paths.push(path);
                if paths.len() > self.config.script_limits().scripts {
                    return Err("script count exceeds configured limits".into());
                }
            }
        }
        paths.sort_unstable();
        if paths.is_empty() {
            return Err("script directory has no .sculk or .sc files".into());
        }
        Ok(paths)
    }

    fn compile(&self, counters: Arc<GlobalCounters>) -> Result<(Bot, CompiledScripts)> {
        let program = Compiler::new(self.config.script_limits())
            .case_sensitive(self.config.bot.case_sensitive)
            .default_prefixes(self.config.bot.default_prefixes.clone())
            .with_globals(counters)
            .load_files(&self.script_paths()?)
            .map_err(|diagnostics| {
                for diagnostic in diagnostics {
                    eprintln!("{diagnostic}");
                }
                "script compilation failed"
            })?;
        let mut bot = Bot::new(sculk_bot::Config {
            case_sensitive: self.config.bot.case_sensitive,
            event_capacity: self.config.limits.event_capacity,
            ..sculk_bot::Config::default()
        })?;
        self.config.install_permissions(&mut bot)?;
        bot.outbound(Arc::new(ConsoleMock));
        let mut bot = program.install_with_errors(bot, Arc::new(|error| eprintln!("{error}")))?;
        for room in &self.config.rooms {
            if !room.prefixes.is_empty() {
                bot.room_prefixes(RoomId::new(room.id)?, &room.prefixes)?;
            }
        }
        Ok((bot, program))
    }

    fn run_offline(&self) -> Result<()> {
        let (mut storage, counters) = if self.config.storage.enabled {
            let (storage, counters) =
                Storage::open(&self.directory.join(&self.config.storage.path))?;
            (Some(storage), Arc::new(counters))
        } else {
            (None, Arc::default())
        };
        let (mut bot, _) = self.compile(counters.clone())?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (_signal, cancel) = watch::channel(false);
        let stdin = io::stdin();
        let interactive = stdin.is_terminal();
        let mut reader = stdin.lock();
        let mut id = 0_i64;
        let (mut completed, mut commands, mut failures) = (0_u64, 0_u64, 0_u64);
        let mut room = self.config.bot.offline_room;
        let mut bytes = Vec::new();
        println!("Sculk scripts loaded in offline mode. Enter a message or :quit.");
        loop {
            if interactive {
                print!("> ");
                io::stdout().flush()?;
            }
            bytes.clear();
            if (&mut reader).take(16_386).read_until(b'\n', &mut bytes)? == 0 {
                break;
            }
            let text = std::str::from_utf8(&bytes)?;
            let text = text.strip_suffix('\n').unwrap_or(text);
            let text = text.strip_suffix('\r').unwrap_or(text);
            if text.len() > 16_384 || text.contains('\0') {
                return Err("offline input exceeded its size or text limit".into());
            }
            match text {
                ":quit" => break,
                ":status" => {
                    println!("Offline events completed={completed}, commands={commands}, failures={failures}.");
                    continue;
                }
                ":reload" => {
                    match self.compile(counters.clone()) {
                        Ok((updated, _)) => {
                            bot = updated;
                            println!("Scripts reloaded.");
                        }
                        Err(error) => eprintln!(
                            "Reload failed. Continuing with the previous scripts. {error}"
                        ),
                    }
                    continue;
                }
                ":list" => {
                    match self.script_paths() {
                        Ok(paths) => list(&paths),
                        Err(error) => eprintln!("{error}"),
                    }
                    continue;
                }
                text if text.starts_with(":room ") => {
                    if let Ok(candidate) = text[6..].parse::<i64>() {
                        if self.config.rooms.iter().any(|room| room.id == candidate) {
                            room = candidate;
                            println!("Switched synthetic room.");
                            continue;
                        }
                    }
                    eprintln!("Choose a configured synthetic room.");
                    continue;
                }
                _ => {}
            }
            id = id
                .checked_add(1)
                .ok_or("offline message sequence exhausted")?;
            let packet = Packet::from_document(
                0,
                Command::new("MSG")?,
                &bson::doc! { "chatId":room, "chatLog": {
                    "logId":id, "authorId":self.config.bot.offline_sender, "type":1,
                    "sendAt":i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())?,
                    "message":text,
                } },
            )?;
            let (sender, source) = sculk_bot::channel(1)?;
            sender.try_send(Event::Message(Message::from_packet(packet)?))?;
            drop(sender);
            let report = runtime.block_on(bot.run_source(source, cancel.clone()));
            completed = completed.saturating_add(report.completed);
            commands = commands.saturating_add(report.commands);
            failures = failures.saturating_add(report.failures);
            if let Some(storage) = &mut storage {
                storage
                    .save(&counters)
                    .map_err(|_| "cannot commit script counters")?;
            }
            if report.stop != sculk_bot::StopReason::Exhausted || report.shutdown.is_err() {
                return Err("offline dispatch stopped before drain".into());
            }
            match report.first_failure {
                Some(sculk_bot::Failure::UnknownCommand) => eprintln!("Unknown command."),
                Some(sculk_bot::Failure::PermissionDenied) => eprintln!("Permission denied."),
                Some(sculk_bot::Failure::Parse(_)) => eprintln!("Invalid command arguments."),
                _ => {}
            }
        }
        Ok(())
    }
}

fn list(paths: &[PathBuf]) {
    for path in paths {
        if let Some(name) = path.file_name() {
            println!("{}", name.to_string_lossy().escape_debug());
        }
    }
}

struct ConsoleMock;
impl Outbound for ConsoleMock {
    fn send<'a>(
        &'a self,
        _: RoomId,
        text: &'a str,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'a, std::result::Result<OutboundReceipt, OutboundError>> {
        Box::pin(async move {
            println!("Bot (mock): {}", text.escape_debug());
            Ok(OutboundReceipt::MockAccepted)
        })
    }
}
