use serde_json::{json, Map, Value};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const MARKER: &str = "herdr-plugin-agent-title-managed";
const EVENTS: [&str; 3] = ["SessionStart", "UserPromptSubmit", "Stop"];

fn homes() -> (PathBuf, PathBuf) {
    let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let codex = env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| config.join("codex"));
    let claude = env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| config.join("claude"));
    (codex, claude)
}

fn stable_binary() -> PathBuf {
    let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"))
        .join("herdr/bin/herdr-plugin-agent-title")
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn managed(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|command| {
                        command.contains(MARKER)
                            || command.contains("/herdr/scripts/herdr-session-title ")
                    })
            })
        })
}

fn update(
    path: &Path,
    agent: &str,
    executable: Option<&Path>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut root = if path.exists() {
        serde_json::from_slice::<Value>(&fs::read(path)?)?
    } else {
        json!({})
    };
    let original = root.clone();
    let object = root
        .as_object_mut()
        .ok_or("settings root must be an object")?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("hooks must be an object")?;
    let command =
        executable.map(|binary| format!("{} hook {} # {}", shell_quote(binary), agent, MARKER));
    for event in EVENTS {
        let entries = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("hook event must be an array")?;
        entries.retain(|entry| !managed(entry));
        if let Some(command) = &command {
            entries.push(json!({
                "hooks": [{"type": "command", "command": command, "timeout": 5}]
            }));
        }
    }

    let changed = root != original;
    if changed {
        atomic_write(path, &root)?;
    }
    Ok(changed)
}

fn atomic_write(path: &Path, value: &Value) -> Result<(), Box<dyn std::error::Error>> {
    let parent = path.parent().ok_or("settings path has no parent")?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile_path(
        parent,
        path.file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("settings"),
    );
    let mut suffix = 0;
    while temporary.exists() {
        suffix += 1;
        temporary = parent.join(format!(
            ".herdr-agent-title.{}.{}.tmp",
            std::process::id(),
            suffix
        ));
    }
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn tempfile_path(parent: &Path, _name: &str) -> PathBuf {
    parent.join(format!(".herdr-agent-title.{}.tmp", std::process::id()))
}

pub fn install() -> Result<(), Box<dyn std::error::Error>> {
    let source = env::current_exe()?.canonicalize()?;
    let executable = stable_binary();
    let parent = executable.parent().ok_or("binary path has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".herdr-plugin-agent-title.{}.tmp",
        std::process::id()
    ));
    fs::copy(source, &temporary)?;
    fs::rename(temporary, &executable)?;
    let (codex, claude) = homes();
    update(&codex.join("hooks.json"), "codex", Some(&executable))?;
    update(&claude.join("settings.json"), "claude", Some(&executable))?;
    println!("installed Codex and Claude title hooks");
    Ok(())
}

pub fn uninstall() -> Result<(), Box<dyn std::error::Error>> {
    let (codex, claude) = homes();
    update(&codex.join("hooks.json"), "codex", None)?;
    update(&claude.join("settings.json"), "claude", None)?;
    if stable_binary().exists() {
        fs::remove_file(stable_binary())?;
    }
    println!("removed Codex and Claude title hooks");
    Ok(())
}

pub fn doctor() -> Result<(), Box<dyn std::error::Error>> {
    let (codex, claude) = homes();
    let checks: Map<String, Value> = [
        ("herdr".into(), json!(which("herdr"))),
        (
            "codex_hooks".into(),
            json!(codex.join("hooks.json").exists()),
        ),
        (
            "claude_settings".into(),
            json!(claude.join("settings.json").exists()),
        ),
        (
            "codex_state_databases".into(),
            json!(glob::glob(&format!("{}/state_*.sqlite", codex.display()))?.count()),
        ),
    ]
    .into_iter()
    .collect();
    println!("{}", serde_json::to_string_pretty(&checks)?);
    Ok(())
}

fn which(name: &str) -> bool {
    env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|path| path.join(name).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_only_managed_hooks() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        fs::write(&path, r#"{"hooks":{"Stop":[{"hooks":[{"command":"keep"}]},{"hooks":[{"command":"old # herdr-plugin-agent-title-managed"}]}]}}"#).unwrap();
        update(&path, "codex", Some(Path::new("/tmp/new binary"))).unwrap();
        let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let stop = value["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "keep");
        assert!(stop[1]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("'/tmp/new binary' hook codex"));
    }

    #[test]
    fn replaces_legacy_python_hook() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        fs::write(
            &path,
            r#"{"hooks":{"Stop":[{"hooks":[{"command":"python3 /home/me/.config/herdr/scripts/herdr-session-title codex"}]}]}}"#,
        )
        .unwrap();
        update(&path, "codex", Some(Path::new("/tmp/plugin"))).unwrap();
        let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let stop = value["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        assert!(stop[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(MARKER));
    }
}
