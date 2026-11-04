//! Hermes install adapter — launch-only skill staging into `~/.hermes`.
//!
//! Modeled on `codex_plugin` (mirrored, not reused: its transaction and
//! recovery machinery is bound to the native Codex plugin system). Four
//! principles carry over unchanged:
//! 1. Preserve unrelated user config — only the amplihack namespace under
//!    `~/.hermes/skills/amplihack/` is ever written; `~/.hermes/config.yaml`
//!    and every other user file is untouched.
//! 2. Track every owned path in an ownership manifest
//!    (`~/.amplihack/hermes/ownership.json`) so uninstall removes exactly
//!    what install created.
//! 3. Idempotent — the staged tree is digested with framed sha2 (same scheme
//!    as codex_plugin); an unchanged repeat install is a no-op.
//! 4. Consent is the user's — never set Hermes `hooks_auto_accept`, never
//!    touch `~/.hermes/shell-hooks-allowlist.json`.
//!
//! LAUNCH-ONLY COMPATIBILITY: this module stages skills only. Hook
//! registration is deliberately NOT wired. Hermes's blocking hook response
//! shape (`{"action": "block"|"modify", ...}`) is a different contract from
//! the Claude-style `hookSpecificOutput` that `amplihack-hooks` emits, so a
//! direct `command: "<amplihack-hooks> ..."` entry would not work
//! end-to-end; hook wiring is gated on the translation bridge (HANDOFF.md
//! stage 5), and the `hooks:` merge into `~/.hermes/config.yaml` lands with
//! it. Missing hooks mean launch-only compatibility — do not describe the
//! result as preserving all quality gates.
//!
//! Runtime parity of skill DISCOVERY inside the nested namespace directory
//! (`skills/amplihack/<skill>/SKILL.md`) is UNVERIFIED: the SKILL.md format
//! is documented Claude-compatible, but live Hermes discovery behavior has
//! not been exercised. The namespace directory is still the right call — it
//! is what makes uninstall exact.
//!
//! Verified hook facts for stage 5 (installed v0.22.5, hooks docs shipped
//! in-tree): event names `pre_tool_call`, `post_tool_call`, `pre_llm_call`,
//! `on_session_start`, `on_session_finalize` all exist. The blocking shapes
//! are `{"action": "block", "message": ...}` / `{"action": "modify",
//! "args": {...}}`; Hermes also normalizes Claude-style
//! `{"decision": ..., "tool_input": ...}`, but NOT
//! `hookSpecificOutput.permissionDecision` — the bridge is still required.
//! `post_llm_call` is an observer (return ignored) so it cannot host
//! Stop-hook semantics; the directive-shaped near-miss is `pre_verify`
//! (accepts Claude's Stop shape, but fires at the edited-code verify gate).
//! `on_session_finalize` is the genuine teardown event (fires via
//! `finalize_session`) — the correct SessionEnd candidate;
//! `on_session_end` fires per turn. `post_compaction` exists as the closest
//! PreCompact analogue but fires after the fact.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

mod resources;
use resources::{build_bundle, copy_tree, count_skills, current_digest, digest};

/// Amplihack-owned namespace under the Hermes skills directory. Everything
/// inside it is ours; nothing outside it is.
const SKILLS_NAMESPACE: &str = "amplihack";

/// Ownership manifest root: `~/.amplihack/hermes/` (amplihack-private).
fn root() -> Result<PathBuf> {
    Ok(super::paths::home_dir()?.join(".amplihack/hermes"))
}

/// Hermes config home. Profiles relocate it via `HERMES_HOME` (verified in
/// the installed source); HOME-relative `~/.hermes` is the default and what
/// this module supports. Other profile homes are rejected before staging.
fn hermes_home() -> Result<PathBuf> {
    let default = super::paths::home_dir()?.join(".hermes");
    if let Some(active) = std::env::var_os("HERMES_HOME") {
        ensure!(
            Path::new(&active) == default,
            "custom HERMES_HOME profiles are not supported by the Hermes connector; \
             unset HERMES_HOME or use the default ~/.hermes home"
        );
    }
    Ok(default)
}

#[derive(Serialize, Deserialize)]
struct Ownership {
    schema_version: u32,
    hermes_home: PathBuf,
    skills_dir: PathBuf,
    skills_digest: String,
}

/// Launch-time entry point called from bootstrap. Stages the amplihack skill
/// set if the bundled source differs from what is installed; a strict no-op
/// when the installed tree still matches.
pub fn ensure_assets_staged() -> Result<()> {
    let source = super::find_bundled_framework_root()
        .context("bundled source required to stage Hermes skills")?;
    install(&source.root)?;
    Ok(())
}

/// Install or refresh amplihack skills for Hermes. Returns true if anything
/// changed.
pub(super) fn install(source: &Path) -> Result<bool> {
    let hermes = hermes_home()?;
    let root = root()?;
    super::connector_paths::ensure_unlinked(&hermes.join("skills").join(SKILLS_NAMESPACE))?;
    super::connector_paths::ensure_unlinked(&root.join("ownership.json"))?;
    fs::create_dir_all(&hermes).with_context(|| format!("creating {}", hermes.display()))?;
    fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    ensure!(
        !fs::symlink_metadata(&root)?.file_type().is_symlink(),
        "refusing symlink Hermes staging root"
    );

    // The bundle is rebuilt before every comparison so the digest reflects
    // the CURRENT bundled source (the codex transaction stages freshly the
    // same way). The build is deterministic, so an unchanged source yields an
    // unchanged digest.
    let bundle = root.join("bundle");
    if path_present(&bundle)? {
        fs::remove_dir_all(&bundle).with_context(|| format!("clearing {}", bundle.display()))?;
    }
    build_bundle(source, &bundle)?;
    let skills_digest = digest(&bundle)?;

    let skills_dir = hermes.join("skills").join(SKILLS_NAMESPACE);
    if let Some(record) = read_ownership()?
        && record.schema_version == 1
        && current_digest(&record.skills_digest)
        && record.hermes_home == hermes
        && record.skills_dir == skills_dir
        && record.skills_digest == skills_digest
        && path_present(&skills_dir)?
        && digest(&skills_dir)? == skills_digest
    {
        return Ok(false);
    }

    // Replace the owned namespace wholesale. It is amplihack-private, so a
    // wholesale replace is the documented contract — anything a user placed
    // inside `skills/amplihack/` is ours to manage.
    // ponytail: not a durable transaction like codex_plugin's — a crash
    // mid-copy leaves a partial namespace, and the next install's digest
    // mismatch repair is the recovery path.
    if path_present(&skills_dir)? {
        fs::remove_dir_all(&skills_dir)
            .with_context(|| format!("clearing {}", skills_dir.display()))?;
    }
    copy_tree(&bundle, &skills_dir, &bundle, 0)?;
    write_ownership(&hermes, &skills_dir, &skills_digest)?;
    println!(
        "  ✅ Staged {} amplihack skills for Hermes at {}",
        count_skills(&skills_dir)?,
        skills_dir.display()
    );
    Ok(true)
}

/// Remove exactly the paths amplihack owns. A missing file is already-removed,
/// not an error: a partial manual cleanup must not wedge uninstall.
/// A missing manifest means amplihack owns nothing here.
pub(super) fn uninstall() -> Result<()> {
    let Some(record) = read_ownership()? else {
        return Ok(());
    };
    ensure!(
        record.schema_version == 1,
        "unknown Hermes ownership manifest schema; preserve it and reconcile manually"
    );
    let hermes = hermes_home()?;
    ensure!(
        record.hermes_home == hermes,
        "Hermes ownership manifest names a different home; preserve it and reconcile manually"
    );
    ensure!(
        record.skills_dir == hermes.join("skills").join(SKILLS_NAMESPACE),
        "Hermes ownership manifest does not name the owned skill namespace; preserve it and reconcile manually"
    );
    super::connector_paths::ensure_unlinked(&record.skills_dir)?;
    super::connector_paths::ensure_unlinked(&root()?.join("bundle"))?;
    if path_present(&record.skills_dir)? {
        fs::remove_dir_all(&record.skills_dir)
            .with_context(|| format!("removing {}", record.skills_dir.display()))?;
        println!(
            "  🗑️  Removed amplihack skills at {}",
            record.skills_dir.display()
        );
    }
    // Drop the amplihack-private staging artifacts (the build bundle), then
    // the manifest, then the staging root only if nothing foreign moved into
    // it.
    let root = root()?;
    let bundle = root.join("bundle");
    if path_present(&bundle)? {
        fs::remove_dir_all(&bundle).with_context(|| format!("removing {}", bundle.display()))?;
    }
    fs::remove_file(root.join("ownership.json"))
        .with_context(|| format!("removing {}", root.join("ownership.json").display()))?;
    let _ = fs::remove_dir(&root);
    Ok(())
}

fn ownership_path() -> Result<PathBuf> {
    Ok(root()?.join("ownership.json"))
}

fn read_ownership() -> Result<Option<Ownership>> {
    let path = ownership_path()?;
    super::connector_paths::ensure_unlinked(&path)?;
    match fs::symlink_metadata(&path) {
        Ok(meta) => {
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "refusing nonregular Hermes ownership manifest"
            );
            Ok(Some(serde_json::from_slice(&fs::read(&path)?).context(
                "malformed Hermes ownership manifest; repair or remove \
                 ~/.amplihack/hermes manually before retrying",
            )?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn write_ownership(hermes_home: &Path, skills_dir: &Path, digest: &str) -> Result<()> {
    let record = Ownership {
        schema_version: 1,
        hermes_home: hermes_home.to_path_buf(),
        skills_dir: skills_dir.to_path_buf(),
        skills_digest: digest.to_string(),
    };
    let path = ownership_path()?;
    let mut body = serde_json::to_string_pretty(&record)?;
    body.push('\n');
    atomic_write(&path, body.as_bytes())
}

fn atomic_write(path: &Path, body: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("manifest destination has no parent")?;
    fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(body)?;
    staged.as_file().sync_all()?;
    staged
        .persist(path)
        .with_context(|| format!("failed atomic write to {}", path.display()))?;
    Ok(())
}

/// `Path::exists` follows symlinks and reports a dangling symlink as absent.
/// Ownership checks must observe the directory entry itself.
fn path_present(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_safety;
