//! Hermes Agent (Nous Research `hermes` CLI) launcher.
//!
//! Modeled on `codex.rs`. Contracts verified against the live CLI v0.22.5
//! (2026-10-26):
//! - One-shot headless: `hermes -z "<prompt>"` — prints ONLY the final
//!   response text to stdout, no banner/spinner; **approvals are
//!   auto-bypassed in one-shot mode** (script mode by design). Interactive
//!   `--yolo` exists but is never injected by amplihack.
//! - Streaming one-shot: `hermes chat --oneshot -q "<prompt>"` (JSON
//!   events; flag shapes unverified against this install)
//! - Resume: `--resume/-r <session>` (`latest` supported) and
//!   `--continue/-c [name]`
//! - Model: `-m/--model <name>`; there is NO top-level `-p` flag (profiles
//!   are the `hermes profile` subcommand), so no prompt/profile collision
//! - Config: `~/.hermes/config.yaml`; skills: `~/.hermes/skills/`
//! - Consent gates that remain the user's: `--accept-hooks`,
//!   `HERMES_ACCEPT_HOOKS=1`, `hooks_auto_accept`, and
//!   `~/.hermes/shell-hooks-allowlist.json` — amplihack touches none of them
//!
//! This skeleton is ARGV-ONLY for prompt delivery. Hermes's stdin shape
//! (`hermes chat --oneshot -q -`) is a different command shape from
//! `hermes -z`, so stdin capability stays disabled until a distinct builder
//! exists.

use anyhow::Result;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tracing::{debug, info};

use crate::flag_matrix::AgentBinary;
use crate::prompt_delivery::{DeliveredCommand, build_tool_command_from_env};

/// Maximum time to wait for a version-check subprocess.
const VERSION_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// Hermes binary detection result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HermesInfo {
    pub installed: bool,
    pub version: Option<String>,
    pub path: Option<PathBuf>,
}

/// Check if Hermes is installed and get version info.
pub fn check_hermes() -> HermesInfo {
    let child = Command::new("hermes")
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    match child.and_then(|c| wait_with_timeout(c, VERSION_CHECK_TIMEOUT)) {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let path = which_binary("hermes");
            info!(version = %version, "Hermes detected");
            HermesInfo {
                installed: true,
                version: Some(version),
                path,
            }
        }
        _ => {
            debug!("Hermes not found or timed out");
            HermesInfo {
                installed: false,
                version: None,
                path: None,
            }
        }
    }
}

/// Automatic installation is deliberately UNSUPPORTED in this draft.
///
/// Hermes is Python-distributed and the official package name is unverified
/// (docs suggest `pip install agent-cli`) — a skeleton must not execute a
/// guessed package install. Point at the official install docs instead.
pub fn install_hermes() -> Result<()> {
    anyhow::bail!(
        "Automatic Hermes installation is not supported yet. Install Hermes \
         per https://hermes-agent.nousresearch.com/docs/getting-started/installation \
         and ensure `hermes` is on PATH."
    );
}

/// Build the headless one-shot command: `hermes -z "<prompt>"`.
///
/// The prompt is delivered as a single argv element immediately after `-z`
/// (appended by the shared delivery helper — see `add_prompt_prefix_args`).
pub fn build_hermes_command(prompt: &str, project_path: &Path, extra_args: &[String]) -> Command {
    let delivered = build_hermes_command_with_prompt_delivery(prompt, project_path, extra_args)
        .expect("invalid Hermes invocation; use the fallible delivery-aware builder");
    assert!(
        delivered.stdin_payload.is_none(),
        "Hermes -z is argv-only in this draft; stdin delivery is not implemented"
    );
    delivered.command
}

/// Delivery-aware builder for the headless one-shot invocation.
///
/// Intended spelling (argv delivery): `hermes [extra_args] -z "<prompt>"`.
/// Stdin delivery is DISABLED in the FlagSet until a builder for the
/// distinct `hermes chat --oneshot -q -` shape exists.
pub fn build_hermes_command_with_prompt_delivery(
    prompt: &str,
    project_path: &Path,
    extra_args: &[String],
) -> std::io::Result<DeliveredCommand> {
    build_tool_command_from_env(AgentBinary::Hermes, project_path, extra_args, prompt)
}

/// Ensure Hermes is installed, then return a ready headless command.
pub fn ensure_and_build(
    prompt: &str,
    project_path: &Path,
    extra_args: &[String],
    auto_install: bool,
) -> Result<Command> {
    let info = check_hermes();
    if !info.installed {
        if auto_install {
            // install_hermes() always bails in this draft — see its docs.
            install_hermes()?;
        } else {
            anyhow::bail!(
                "Hermes is not installed. See \
                 https://hermes-agent.nousresearch.com/docs/getting-started/installation"
            );
        }
    }
    let delivered = build_hermes_command_with_prompt_delivery(prompt, project_path, extra_args)?;
    anyhow::ensure!(
        delivered.stdin_payload.is_none(),
        "Hermes one-shot is argv-only in this draft"
    );
    Ok(delivered.command)
}

fn which_binary(name: &str) -> Option<PathBuf> {
    Command::new("which")
        .arg(name)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
}

/// Wait for a child process with a timeout, killing it if it exceeds the
/// limit. Mirrors codex.rs: collect the REAL exit status and pipe output —
/// a failing exit status and non-empty stdout must survive this helper.
fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> io::Result<std::process::Output> {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait()? {
            Some(status) => {
                let stdout = child.stdout.map_or_else(Vec::new, |mut s| {
                    let mut buf = Vec::new();
                    let _ = io::Read::read_to_end(&mut s, &mut buf);
                    buf
                });
                let stderr = child.stderr.map_or_else(Vec::new, |mut s| {
                    let mut buf = Vec::new();
                    let _ = io::Read::read_to_end(&mut s, &mut buf);
                    buf
                });
                return Ok(std::process::Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            None if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(io::ErrorKind::TimedOut, "process timed out"));
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermes_info_serializes() {
        let info = HermesInfo {
            installed: true,
            version: Some("1.0.0".into()),
            path: Some(PathBuf::from("/usr/bin/hermes")),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["installed"], true);
        assert_eq!(json["version"], "1.0.0");
    }

    /// The composed one-shot argv is exactly
    /// `hermes [extra_args] -z "<prompt>"`, with the prompt as one argv
    /// element (spaces and leading hyphens included) and nothing on stdin.
    #[test]
    fn build_hermes_command_sets_env_and_argv() {
        let cmd = build_hermes_command(
            "--leading-hyphen prompt with spaces",
            Path::new("/tmp"),
            &["--verbose".to_string()],
        );
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(
            envs.iter().any(|(k, v)| *k == "AMPLIHACK_AGENT_BINARY"
                && v == &Some(std::ffi::OsStr::new("hermes")))
        );
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec!["--verbose", "-z", "--leading-hyphen prompt with spaces"]
        );
    }
}
