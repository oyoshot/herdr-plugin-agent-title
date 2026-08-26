use rusqlite::Connection;
use serde_json::Value;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SOURCE: &str = "user:agent-session-title";
const MAX_CHARS: usize = 80;

fn sanitize(value: Option<&str>) -> Option<String> {
    let cleaned = value?
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.is_empty() {
        return None;
    }
    if cleaned.chars().count() <= MAX_CHARS {
        return Some(cleaned);
    }
    Some(format!(
        "{}…",
        cleaned
            .chars()
            .take(MAX_CHARS - 1)
            .collect::<String>()
            .trim_end()
    ))
}

fn codex_home() -> PathBuf {
    if let Some(path) = env::var_os("CODEX_HOME") {
        return path.into();
    }
    let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"))
        .join("codex")
}

fn codex_title(session_id: Option<&str>, prompt: Option<&str>) -> Option<String> {
    let session_id = session_id?;
    let pattern = format!("{}/state_*.sqlite", codex_home().display());
    let mut databases: Vec<PathBuf> = glob::glob(&pattern).ok()?.flatten().collect();
    databases.sort_by_key(|path| fs::metadata(path).and_then(|m| m.modified()).ok());
    databases.reverse();

    for database in databases {
        if let Ok(title) = query_codex_title(&database, session_id) {
            if title.is_some() {
                return title;
            }
        }
    }
    sanitize(prompt)
}

fn query_codex_title(path: &Path, session_id: &str) -> rusqlite::Result<Option<String>> {
    let connection = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let columns: Vec<String> = connection
        .prepare("PRAGMA table_info(threads)")?
        .query_map([], |row| row.get(1))?
        .filter_map(Result::ok)
        .collect();
    let wanted: Vec<&str> = ["name", "title", "first_user_message"]
        .into_iter()
        .filter(|name| columns.iter().any(|column| column == name))
        .collect();
    if !columns.iter().any(|column| column == "id") || wanted.is_empty() {
        return Ok(None);
    }
    let sql = format!("SELECT {} FROM threads WHERE id = ?1", wanted.join(", "));
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query([session_id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    for index in 0..wanted.len() {
        if let Some(title) = sanitize(row.get::<_, Option<String>>(index)?.as_deref()) {
            return Ok(Some(title));
        }
    }
    Ok(None)
}

fn claude_title(input: &Value) -> Option<String> {
    if let Some(title) = sanitize(input.get("session_name").and_then(Value::as_str)) {
        return Some(title);
    }
    let path = input.get("transcript_path").and_then(Value::as_str)?;
    let file = fs::File::open(path).ok()?;
    let mut custom = None;
    let mut ai = None;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if !line.contains("\"custom-title\"") && !line.contains("\"ai-title\"") {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match record.get("type").and_then(Value::as_str) {
            Some("custom-title") => {
                custom = sanitize(record.get("customTitle").and_then(Value::as_str)).or(custom)
            }
            Some("ai-title") => ai = sanitize(record.get("aiTitle").and_then(Value::as_str)).or(ai),
            _ => {}
        }
    }
    custom
        .or(ai)
        .or_else(|| sanitize(input.get("prompt").and_then(Value::as_str)))
}

fn report(agent: &str, title: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let Some(pane_id) = env::var_os("HERDR_PANE_ID") else {
        return Ok(());
    };
    if env::var("HERDR_ENV").as_deref() != Ok("1") {
        return Ok(());
    }
    let herdr = env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let mut command = Command::new(herdr);
    command
        .args(["pane", "report-metadata"])
        .arg(pane_id)
        .args(["--source", SOURCE, "--agent", agent]);
    if let Some(title) = title {
        command.args([
            "--title",
            title,
            "--display-agent",
            title,
            "--token",
            &format!("task={title}"),
        ]);
    } else {
        command.args([
            "--clear-title",
            "--clear-display-agent",
            "--clear-token",
            "task",
        ]);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(())
}

pub fn handle(agent: &str, input: &Value) -> Result<(), Box<dyn std::error::Error>> {
    let event = input.get("hook_event_name").and_then(Value::as_str);
    let title = match agent {
        "codex" => codex_title(
            input.get("session_id").and_then(Value::as_str),
            input.get("prompt").and_then(Value::as_str),
        ),
        "claude" => {
            claude_title(input).or_else(|| sanitize(input.get("prompt").and_then(Value::as_str)))
        }
        _ => return Err("unsupported agent".into()),
    };
    let result = if title.is_some() || event == Some("SessionStart") {
        report(agent, title.as_deref())
    } else {
        Ok(())
    };
    // Stop hooks require a JSON object even if publishing metadata failed.
    if event == Some("Stop") {
        println!("{{}}");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_and_truncates_unicode_by_character() {
        assert_eq!(sanitize(Some(" a\n b ")).as_deref(), Some("a b"));
        let title = sanitize(Some(&"界".repeat(100))).unwrap();
        assert_eq!(title.chars().count(), 80);
        assert!(title.ends_with('…'));
    }
}
