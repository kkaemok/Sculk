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
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
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
