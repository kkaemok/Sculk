use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Command, Output, Stdio},
};

fn command(dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sculk"));
    cmd.current_dir(dir);
    cmd
}

fn run(dir: &Path, input: &[u8]) -> Output {
    let mut child = command(dir)
        .args(["run", "--offline"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let write = child.stdin.take().unwrap().write_all(input);
    let output = child.wait_with_output().unwrap();
    if let Err(error) = write {
        assert!(
            !output.status.success() && error.kind() == std::io::ErrorKind::BrokenPipe,
            "{error}: {output:?}"
        );
    }
    output
}

#[test]
fn installs_runs_both_extensions_and_reloads_with_rollback() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    assert!(command(root).arg("init").status().unwrap().success());
    fs::write(
        root.join("scripts/native.sc"),
        "command more:\n    trigger:\n        reply \"More\"\n",
    )
    .unwrap();
    assert!(command(root)
        .args(["script", "check"])
        .status()
        .unwrap()
        .success());
    let script = root.join("scripts/chat.sculk");
    let mut child = command(root)
        .args(["run", "--offline"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut read = || {
        let mut line = String::new();
        assert!(output.read_line(&mut line).unwrap() > 0);
        line
    };
    assert!(read().contains("offline mode"));
    input.write_all(b"!ping\n/more\n").unwrap();
    assert_eq!(read().trim(), "Bot (mock): Pong!");
    assert_eq!(read().trim(), "Bot (mock): More");

    fs::write(&script, "not a declaration").unwrap();
    input.write_all(b":reload\n!ping\n").unwrap();
    assert_eq!(read().trim(), "Bot (mock): Pong!");

    fs::write(
        &script,
        "command ping:\n    trigger:\n        reply \"Updated\"\n",
    )
    .unwrap();
    input.write_all(b":reload\n!ping\n:quit\n").unwrap();
    assert_eq!(read().trim(), "Scripts reloaded.");
    assert_eq!(read().trim(), "Bot (mock): Updated");
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[test]
#[cfg(unix)]
fn sqlite_survives_restart_and_refuses_corruption() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    assert!(command(root).arg("init").status().unwrap().success());
    let config = root.join("sculk.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("enabled = false", "enabled = true"),
    )
    .unwrap();
    for counter in 1..=2 {
        let output = run(root, b"!count\n");
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8(output.stdout)
            .unwrap()
            .contains(&format!("Counter: {counter}")));
    }
    let state = root.join("data/sculk.db");
    fs::write(state, "not sqlite").unwrap();
    assert!(!run(root, b"!count\n").status.success());
}

#[cfg(unix)]
#[test]
fn sqlite_migrates_legacy_counters_and_restores_exact_typed_state_per_room() {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    assert!(command(root).arg("init").status().unwrap().success());
    fs::write(
        root.join("sculk.toml"),
        "[storage]\nenabled = true\n[[rooms]]\nid = 1\n[[rooms]]\nid = 2\n",
    )
    .unwrap();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(root.join("data"))
        .unwrap();
    let db = root.join("data/sculk.db");
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&db)
        .unwrap();
    let database = rusqlite::Connection::open(&db).unwrap();
    database
        .execute_batch(
            "CREATE TABLE counters (name TEXT PRIMARY KEY NOT NULL, value INTEGER NOT NULL) STRICT;
        INSERT INTO counters VALUES ('fixture', 42);
        PRAGMA user_version=1;",
        )
        .unwrap();
    drop(database);
    fs::write(
        root.join("scripts/state.sc"),
        r#"
command store:
    trigger:
        set {state::text::room::greeting} to "한글 😀"
        set {state::integer-list::values} to integer-list(9223372036854775807)
        set {state::boolean-map::flags} to put(boolean-map(), "valid", true)
command state:
    trigger:
        if get({state::boolean-map::flags}, "valid"):
            set {_greeting} to state-or({state::text::room::greeting}, "missing")
            set {_value} to item({state::integer-list::values}, 0)
            reply "%{_greeting}%:%{_value}%:%{global::fixture}%"
"#,
    )
    .unwrap();
    let written = run(root, b"!store\n");
    assert!(written.status.success(), "{written:?}");
    let restored = run(root, b"!state\n:room 2\n!state\n");
    assert!(restored.status.success(), "{restored:?}");
    let restored = String::from_utf8(restored.stdout).unwrap();
    assert!(
        restored.contains("Bot (mock): 한글 😀:9223372036854775807:42"),
        "{restored}"
    );
    assert!(
        restored.contains("Bot (mock): missing:9223372036854775807:42"),
        "{restored}"
    );
}

#[test]
fn room_prefix_and_permission_rules_share_the_script_command_registry() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    assert!(command(root).arg("init").status().unwrap().success());
    fs::write(
        root.join("sculk.toml"),
        r#"
[[rooms]]
id = 1
prefixes = [">", ">>"]

[[rooms]]
id = 2
prefixes = ["봇아 "]

[[permissions]]
name = "bot.manage"

[[permissions.rules]]
sender = 2
room = 2
grant = true
"#,
    )
    .unwrap();
    fs::write(
        root.join("scripts/chat.sculk"),
        r#"command ping:
    permissions: "bot.manage"
    trigger:
        reply "Allowed"
"#,
    )
    .unwrap();
    let output = run(root, ">>ping\n:room 2\n봇아 ping\n".as_bytes());
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.matches("Bot (mock): Allowed").count(), 1);
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("Permission denied."));
}

#[test]
#[cfg(unix)]
fn opt_in_batches_keep_prior_transactions_and_flush_on_clean_eof() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    assert!(command(root).arg("init").status().unwrap().success());
    let config = root.join("sculk.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("enabled = false", "enabled = true\ncommit_every = 3"),
    )
    .unwrap();

    let mut process = command(root)
        .args(["run", "--offline"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut writer = process.stdin.take().unwrap();
    let mut lines = BufReader::new(process.stdout.take().unwrap()).lines();
    assert!(lines.next().unwrap().unwrap().contains("offline mode"));
    for count in 1..=4 {
        writer.write_all(b"!count\n").unwrap();
        assert_eq!(
            lines.next().unwrap().unwrap(),
            format!("Bot (mock): Counter: {count}")
        );
    }
    process.kill().unwrap();
    assert!(!process.wait().unwrap().success());
    let clean = run(root, b"!count\n");
    assert!(clean.status.success(), "{clean:?}");
    assert!(String::from_utf8_lossy(&clean.stdout).contains("Counter: 4\n"));
    let restored = run(root, b"!count\n");
    assert!(restored.status.success(), "{restored:?}");
    assert!(String::from_utf8_lossy(&restored.stdout).contains("Counter: 5\n"));
}

#[cfg(all(unix, feature = "live-validation"))]
#[test]
fn a_piped_live_command_validation_stops_at_operator_authorization() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    assert!(command(root).arg("init").status().unwrap().success());
    let key = root.join("test.der");
    let auth = root.join("auth.json");
    let trigger = root.join("trigger");
    let report = root.join("stages.json");
    fs::write(
        &key,
        include_bytes!("../../../tests/fixtures/documented-server-public.der"),
    )
    .unwrap();
    fs::write(&trigger, "!ping\n").unwrap();
    fs::write(&auth, format!(
        r#"{{"expected_user_id":42,"email":"fixture@example.invalid",
        "device_file":"{root}/device","server_key_file":"{root}/test.der",
        "server_key_sha256":"c64be379ff4333c91346e7d8685ab11c0ab15cf46183f5316853eb9ae4f1d3ed",
        "connection_mode":"direct","allowed_endpoints":[{{"host":"booking-loco.kakao.com","port":443}}],
        "app_version":"fixture","os_version":"13","api_level":"33","device_name":"fixture","test_room_id":2024}}"#,
        root = root.display()
    )).unwrap();
    for path in [&key, &auth, &trigger] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let output = command(root)
        .args([
            "run",
            "--live",
            auth.to_str().unwrap(),
            "--validate-command",
            trigger.to_str().unwrap(),
            "--report",
            report.to_str().unwrap(),
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(78));
    let report = fs::read_to_string(report).unwrap();
    assert!(report.contains(r#""last_stage": "account-authorization""#));
    assert!(report.contains(r#""https_authentication": false"#));
    assert!(!report.contains("fixture@example.invalid") && !report.contains("!ping"));
    assert!(!root.join("device").exists());
}
