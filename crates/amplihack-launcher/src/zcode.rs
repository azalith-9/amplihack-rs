//! ZCode (Z.ai `zcode` CLI) launcher.
//!
//! Modeled on `codex.rs`. Contracts verified against the installed CLI
//! 0.17.0 (2026-10-26):
//! - Headless: `zcode --prompt "<prompt>"` (also `-p`); `--cwd <dir>` to
//!   relocate; `--resume <sessionId>` / `-c` to continue; `--json` for
//!   machine-readable output (there is NO `--output-format` flag)
//! - `--mode` takes build|edit|plan|yolo|guarded and **defaults to yolo for
//!   `--prompt` runs** — amplihack neither injects nor overrides it; a
//!   stricter mode is the user's explicit extra arg
//! - `--target <text>` sets the session GOAL, not a model; there is no
//!   model-selection argv flag (model lives in the TUI / config)
//! - Config: `~/.zcode/cli/config.json` (hooks, skills, MCP, permissions);
//!   global rules: `~/.zcode/AGENTS.md`
//!
//! Packaging caveat (observed on this host): `zcode` on PATH can be the
//! Electron desktop wrapper — it accepts the same flags and exits 0, but
//! agent output goes to the desktop surface, not stdout. The bare CLI is
//! the node bundle shipped at `/opt/ZCode/resources/glm/zcode.cjs` and also
//! requires a model to be selected in its config. A future output-parsing
//! executor must resolve which entry point it is talking to.
//!
//! This skeleton is ARGV-ONLY for prompt delivery; no stdin mode exists in
//! the CLI surface.

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

/// ZCode binary detection result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ZcodeInfo {
    pub installed: bool,
    pub version: Option<String>,
    pub path: Option<PathBuf>,
}

/// Check if zcode is installed and get version info.
pub fn check_zcode() -> ZcodeInfo {
    let child = Command::new("zcode")
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    match child.and_then(|c| wait_with_timeout(c, VERSION_CHECK_TIMEOUT)) {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let path = which_binary("zcode");
            info!(version = %version, "zcode detected");
            ZcodeInfo {
                installed: true,
                version: Some(version),
                path,
            }
        }
        _ => {
            debug!("zcode not found or timed out");
            ZcodeInfo {
                installed: false,
                version: None,
                path: None,
            }
        }
    }
}

/// Automatic installation is deliberately UNSUPPORTED in this draft.
///
/// ZCode's CLI ships inside the Z.ai desktop app repo; a standalone
/// distribution channel is unconfirmed. Point at the official repo instead of
/// guessing a package name.
pub fn install_zcode() -> Result<()> {
    anyhow::bail!(
        "Automatic zcode installation is not supported yet. Install ZCode from \
         https://github.com/zai-org/ZCode (apps/zcode-cli) and ensure `zcode` \
         is on PATH."
    );
}

/// Build the headless command: `zcode --prompt "<prompt>"`.
///
/// The prompt is delivered as a single argv element immediately after
/// `--prompt` (appended by the shared delivery helper — see
/// `add_prompt_prefix_args`). The caller sets the working directory
/// (`--cwd` is only needed when it differs from the process cwd).
/// No output-format or permission-bypass flags are injected. Autonomous
/// execution is not implemented; see the module docs.
pub fn build_zcode_command(prompt: &str, project_path: &Path, extra_args: &[String]) -> Command {
    let delivered = build_zcode_command_with_prompt_delivery(prompt, project_path, extra_args)
        .expect("invalid zcode invocation; use the fallible delivery-aware builder");
    assert!(
        delivered.stdin_payload.is_none(),
        "zcode headless is argv-only in this draft"
    );
    delivered.command
}

/// Delivery-aware builder for the headless invocation.
///
/// Intended spelling (argv delivery): `zcode [extra_args] --prompt "<prompt>"`.
/// Stdin delivery is not documented for zcode — the FlagSet marks it
/// unsupported, so `deliver()` degrades to argv.
pub fn build_zcode_command_with_prompt_delivery(
    prompt: &str,
    project_path: &Path,
    extra_args: &[String],
) -> std::io::Result<DeliveredCommand> {
    build_tool_command_from_env(AgentBinary::Zcode, project_path, extra_args, prompt)
}

/// Ensure zcode is installed, then return a ready headless command.
pub fn ensure_and_build(
    prompt: &str,
    project_path: &Path,
    extra_args: &[String],
    auto_install: bool,
) -> Result<Command> {
    let info = check_zcode();
    if !info.installed {
        if auto_install {
            // install_zcode() always bails in this draft — see its docs.
            install_zcode()?;
        } else {
            anyhow::bail!(
                "zcode is not installed. Install it from \
                 https://github.com/zai-org/ZCode (apps/zcode-cli)."
            );
        }
    }
    let delivered = build_zcode_command_with_prompt_delivery(prompt, project_path, extra_args)?;
    anyhow::ensure!(
        delivered.stdin_payload.is_none(),
        "zcode headless is argv-only in this draft"
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
    fn zcode_info_serializes() {
        let info = ZcodeInfo {
            installed: true,
            version: Some("1.0.0".into()),
            path: Some(PathBuf::from("/usr/bin/zcode")),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["installed"], true);
        assert_eq!(json["version"], "1.0.0");
    }

    /// The composed headless argv is exactly
    /// `zcode [extra_args] --prompt "<prompt>"`, with the prompt as one argv
    /// element (spaces and leading hyphens included) and nothing on stdin.
    #[test]
    fn build_zcode_command_sets_env_and_argv() {
        let cmd = build_zcode_command(
            "--leading-hyphen prompt with spaces",
            Path::new("/tmp"),
            &["--no-color".to_string()],
        );
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(
            envs.iter().any(|(k, v)| *k == "AMPLIHACK_AGENT_BINARY"
                && v == &Some(std::ffi::OsStr::new("zcode")))
        );
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "--no-color",
                "--prompt",
                "--leading-hyphen prompt with spaces"
            ]
        );
    }
}
