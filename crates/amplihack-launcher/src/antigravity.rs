//! Google Antigravity (`agy` CLI) launcher.
//!
//! Modeled on `hermes.rs`/`zcode.rs`. Contracts verified against the live CLI
//! 1.4.9 (2026-10-28):
//! - Headless one-shot: `agy --print "<prompt>"` (also `-p` and `--prompt`);
//!   the flag REQUIRES a value (`flag needs an argument: -print` with none)
//! - `--output-format` takes text|json|stream-json for print mode;
//!   `--input-format stream-json` reads NDJSON from stdin but is a different
//!   protocol (requires `--output-format stream-json`), so plain-prompt stdin
//!   stays disabled until a dedicated builder exists
//! - Resume: `--continue`/`-c` (most recent) and `--conversation <id>` (by
//!   ID); there is no `--resume` flag
//! - `--model <name>` (see `agy models`), `--add-dir <dir>` (repeatable),
//!   `--mode` (accept-edits|auto|plan), `--effort`, `--sandbox`
//! - Config: `~/.config/antigravity/`; `--version` prints e.g. `1.4.9`
//! - Subcommands: agent(s), changelog, help, install, mcp, mic-serve,
//!   models, plugin(s), remote-control, update
//!
//! Naming caveat (observed on this host): the Antigravity IDE ships a
//! desktop `antigravity` binary (snap/Electron). Every flag spelling —
//! including `--version` and `--help` — launches the GUI and never returns,
//! so `antigravity` must NEVER be a candidate or Command target; the agent
//! CLI executable is `agy` and that is the only name amplihack uses.
//!
//! `--dangerously-skip-permissions` exists on `agy`, but like every non-Claude
//! tool amplihack never injects a bypass flag; it is the user's explicit
//! trailing arg.
//!
//! This skeleton is ARGV-ONLY for prompt delivery; the stream-json stdin
//! shape is a different protocol and stays unimplemented.

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

/// Antigravity CLI (`agy`) detection result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AntigravityInfo {
    pub installed: bool,
    pub version: Option<String>,
    pub path: Option<PathBuf>,
}

/// Check if the Antigravity CLI is installed and get version info.
///
/// Probes `agy --version` (verified: prints the version to stdout and exits
/// immediately — unlike the `antigravity` IDE binary, whose every invocation
/// opens a GUI window and never returns).
pub fn check_antigravity() -> AntigravityInfo {
    let child = Command::new("agy")
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    match child.and_then(|c| wait_with_timeout(c, VERSION_CHECK_TIMEOUT)) {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let path = which_binary("agy");
            info!(version = %version, "antigravity (agy) detected");
            AntigravityInfo {
                installed: true,
                version: Some(version),
                path,
            }
        }
        _ => {
            debug!("agy not found or timed out");
            AntigravityInfo {
                installed: false,
                version: None,
                path: None,
            }
        }
    }
}

/// Automatic installation is deliberately UNSUPPORTED in this draft.
///
/// The documented install is a shell script piped to bash
/// (`curl -fsSL https://antigravity.google/cli/install.sh | bash`); executing
/// a piped remote script on the user's behalf is not amplihack's call. Point
/// at the official installer instead.
pub fn install_antigravity() -> Result<()> {
    anyhow::bail!(
        "Automatic Antigravity CLI installation is not supported yet. Install it \
         per https://github.com/google-antigravity/antigravity-cli (e.g. \
         curl -fsSL https://antigravity.google/cli/install.sh | bash) and ensure \
         `agy` is on PATH. Note: the `antigravity` desktop binary is the IDE, \
         not the agent CLI."
    );
}

/// Build the headless one-shot command: `agy --print "<prompt>"`.
///
/// The prompt is delivered as a single argv element immediately after
/// `--print` (appended by the shared delivery helper — see
/// `add_prompt_prefix_args`).
pub fn build_antigravity_command(
    prompt: &str,
    project_path: &Path,
    extra_args: &[String],
) -> Command {
    let delivered =
        build_antigravity_command_with_prompt_delivery(prompt, project_path, extra_args)
            .expect("invalid Antigravity invocation; use the fallible delivery-aware builder");
    assert!(
        delivered.stdin_payload.is_none(),
        "agy --print is argv-only in this draft; stdin delivery is not implemented"
    );
    delivered.command
}

/// Delivery-aware builder for the headless one-shot invocation.
///
/// Intended spelling (argv delivery): `agy [extra_args] --print "<prompt>"`.
/// Stdin delivery is DISABLED in the FlagSet until a builder for the
/// stream-json protocol exists.
pub fn build_antigravity_command_with_prompt_delivery(
    prompt: &str,
    project_path: &Path,
    extra_args: &[String],
) -> std::io::Result<DeliveredCommand> {
    build_tool_command_from_env(AgentBinary::Antigravity, project_path, extra_args, prompt)
}

/// Ensure the Antigravity CLI is installed, then return a ready headless
/// command.
pub fn ensure_and_build(
    prompt: &str,
    project_path: &Path,
    extra_args: &[String],
    auto_install: bool,
) -> Result<Command> {
    let info = check_antigravity();
    if !info.installed {
        if auto_install {
            // install_antigravity() always bails in this draft — see its docs.
            install_antigravity()?;
        } else {
            anyhow::bail!(
                "Antigravity CLI (agy) is not installed. See \
                 https://github.com/google-antigravity/antigravity-cli"
            );
        }
    }
    let delivered =
        build_antigravity_command_with_prompt_delivery(prompt, project_path, extra_args)?;
    anyhow::ensure!(
        delivered.stdin_payload.is_none(),
        "agy --print is argv-only in this draft"
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
    fn antigravity_info_serializes() {
        let info = AntigravityInfo {
            installed: true,
            version: Some("1.4.9".into()),
            path: Some(PathBuf::from("/usr/bin/agy")),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["installed"], true);
        assert_eq!(json["version"], "1.4.9");
    }

    /// The composed one-shot argv is exactly
    /// `agy [extra_args] --print "<prompt>"`, with the prompt as one argv
    /// element (spaces and leading hyphens included) and nothing on stdin.
    #[test]
    fn build_antigravity_command_sets_env_and_argv() {
        let cmd = build_antigravity_command(
            "--leading-hyphen prompt with spaces",
            Path::new("/tmp"),
            &["--no-color".to_string()],
        );
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs.iter().any(
            |(k, v)| *k == "AMPLIHACK_AGENT_BINARY" && v == &Some(std::ffi::OsStr::new("agy"))
        ));
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "--no-color",
                "--print",
                "--leading-hyphen prompt with spaces"
            ]
        );
    }
}
