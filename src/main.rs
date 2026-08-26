mod hooks;
mod title;

use std::env;
use std::io;
use std::process::ExitCode;

fn usage() {
    eprintln!(
        "usage: herdr-plugin-agent-title <install-hooks|uninstall-hooks|doctor|hook codex|hook claude>"
    );
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    match (args.next().as_deref(), args.next().as_deref(), args.next()) {
        (Some("install-hooks"), None, None) => hooks::install(),
        (Some("uninstall-hooks"), None, None) => hooks::uninstall(),
        (Some("doctor"), None, None) => hooks::doctor(),
        (Some("hook"), Some(agent @ ("codex" | "claude")), None) => {
            let input: serde_json::Value = serde_json::from_reader(io::stdin())?;
            title::handle(agent, &input)
        }
        _ => {
            usage();
            Err("invalid arguments".into())
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if env::var_os("HERDR_AGENT_TITLE_DEBUG").is_some() {
                eprintln!("herdr-plugin-agent-title: {error}");
            }
            ExitCode::FAILURE
        }
    }
}
