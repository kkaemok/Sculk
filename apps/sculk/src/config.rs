use sculk_bot::{Decision, MemoryPermissions, PermissionId, RoomId, SenderId};
use sculk_script::Limits;
use serde::Deserialize;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const CONFIG_BYTES: u64 = 65_536;

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Configuration {
    pub bot: BotSettings,
    pub limits: LimitsSettings,
    pub storage: StorageSettings,
    pub live: LiveSettings,
    pub permissions: Vec<Permission>,
    pub rooms: Vec<RoomSettings>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BotSettings {
    pub scripts: PathBuf,
    pub default_prefixes: Vec<String>,
    pub case_sensitive: bool,
    pub offline_room: i64,
    pub offline_sender: i64,
}

impl Default for BotSettings {
    fn default() -> Self {
        Self {
            scripts: "scripts".into(),
            default_prefixes: vec!["!".into(), "/".into()],
            case_sensitive: true,
            offline_room: 1,
            offline_sender: 2,
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsSettings {
    pub event_capacity: usize,
    pub handler_ms: u64,
}
impl Default for LimitsSettings {
    fn default() -> Self {
        Self {
            event_capacity: 16,
            handler_ms: 100,
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageSettings {
    pub enabled: bool,
    pub path: PathBuf,
    pub commit_every: usize,
}
impl Default for StorageSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            path: "data/sculk.db".into(),
            commit_every: 1,
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LiveSettings {
    #[cfg_attr(not(all(unix, feature = "live-validation")), allow(dead_code))]
    pub checkpoint_dir: Option<PathBuf>,
    #[cfg_attr(not(all(unix, feature = "live-validation")), allow(dead_code))]
    pub enable_write: bool,
    pub write_interval_seconds: u64,
}

impl Default for LiveSettings {
    fn default() -> Self {
        Self {
            checkpoint_dir: None,
            enable_write: false,
            write_interval_seconds: 2,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permission {
    pub name: String,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub sender: i64,
    pub room: i64,
    pub grant: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomSettings {
    pub id: i64,
    #[serde(default)]
    pub prefixes: Vec<String>,
}

impl Configuration {
    pub fn read(path: &Path) -> crate::Result<Self> {
        let file = File::open(path).map_err(|_| "cannot open Sculk configuration")?;
        let mut bytes = Vec::new();
        file.take(CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > CONFIG_BYTES {
            return Err("configuration exceeds 64 KiB".into());
        }
        let text = std::str::from_utf8(&bytes)?;
        let config: Self = toml::from_str(text).map_err(|_| "invalid Sculk TOML configuration")?;
        if !(1..=128).contains(&config.limits.event_capacity)
            || !(1..=30_000).contains(&config.limits.handler_ms)
            || config.permissions.len() > 128
            || config.rooms.len() > 128
        {
            return Err("configuration limit exceeded".into());
        }
        if !(1..=256).contains(&config.storage.commit_every) {
            return Err("storage.commit_every must be within 1..256".into());
        }
        if !(1..=60).contains(&config.live.write_interval_seconds) {
            return Err("write_interval_seconds must be within 1..60".into());
        }
        let default_room = RoomId::new(config.bot.offline_room)?;
        SenderId::new(config.bot.offline_sender)?;
        let mut rooms = std::collections::HashSet::new();
        for room in &config.rooms {
            if !rooms.insert(RoomId::new(room.id)?) {
                return Err("duplicate room configuration".into());
            }
        }
        if !rooms.is_empty() && !rooms.contains(&default_room) {
            return Err("offline_room must appear in the configured room list".into());
        }
        Ok(config)
    }

    pub fn script_limits(&self) -> Limits {
        Limits {
            handler_time: Duration::from_millis(self.limits.handler_ms),
            ..Limits::default()
        }
    }

    pub fn install_permissions(&self, bot: &mut sculk_bot::Bot) -> crate::Result<()> {
        let mut store = MemoryPermissions::default();
        for setting in &self.permissions {
            let permission = PermissionId::new(&setting.name)?;
            bot.permission(permission.clone(), setting.default)?;
            let mut seen = std::collections::HashSet::new();
            for rule in &setting.rules {
                let (sender, room) = (SenderId::new(rule.sender)?, RoomId::new(rule.room)?);
                if !seen.insert((sender, room)) {
                    return Err("duplicate permission rule".into());
                }
                store.set(
                    permission.clone(),
                    sender,
                    Some(room),
                    if rule.grant {
                        Decision::Grant
                    } else {
                        Decision::Deny
                    },
                )?;
            }
        }
        bot.permission_store(Arc::new(store));
        Ok(())
    }
}
