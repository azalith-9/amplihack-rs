//! ZCode install adapter — launch-only `~/.zcode/AGENTS.md` staging.
//!
//! Modeled on `codex_plugin` (which has no AGENTS.md-style patching of its
//! own, so the marker-block contract below is the amplihack-owned
//! equivalent). Four principles carry over unchanged:
//! 1. Preserve unrelated user config — only the owned marker block inside
//!    `~/.zcode/AGENTS.md` is written; everything outside
//!    `<!-- amplihack:begin -->` / `<!-- amplihack:end -->` is byte-preserved,
//!    and `~/.zcode/cli/config.json` is never touched.
//! 2. Track the owned patch in an ownership manifest
//!    (`~/.amplihack/zcode/ownership.json`) so uninstall removes exactly
//!    what install created.
//! 3. Idempotent — the desired block is compared against the installed one;
//!    an unchanged repeat install is a no-op.
//! 4. Trust is the user's — never run `zcode hooks trust` on their behalf.
//!
//! LAUNCH-ONLY COMPATIBILITY: this module patches AGENTS.md only. Hook
//! registration (and skills/MCP entries in `~/.zcode/cli/config.json`) is
//! deliberately NOT wired. Verified against the installed CLI 0.17.0 bundle
//! (2026-10-26): the hook-event enum is exactly SessionStart,
//! UserPromptSubmit, PreToolUse, PermissionRequest, PostToolUse,
//! PostToolUseFailure, Stop — no PreCompact and no SessionEnd hook (the
//! bundle's "SessionEnded" strings are internal app events, not hooks).
//! `hookSpecificOutput`/`permissionDecision` strings exist in the bundle,
//! but that is schema evidence, not tested runtime parity with ZCode's
//! actual hook runner — both directions of every payload must be
//! live-tested first (HANDOFF.md stage 5). Missing hooks mean launch-only
//! compatibility — do not describe the result as preserving all quality
//! gates.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const BEGIN_MARKER: &str = "<!-- amplihack:begin -->";
const END_MARKER: &str = "<!-- amplihack:end -->";

/// Ownership manifest root: `~/.amplihack/zcode/` (amplihack-private).
fn root() -> Result<PathBuf> {
    Ok(super::paths::home_dir()?.join(".amplihack/zcode"))
}

/// ZCode config home. The bundle references `ZCODE_HOME` (scope unconfirmed
/// — observed usage is telemetry-scoped); HOME-relative `~/.zcode` is the
/// default and what this module stages into.
fn zcode_home() -> Result<PathBuf> {
    Ok(super::paths::home_dir()?.join(".zcode"))
}

#[derive(Serialize, Deserialize)]
struct Ownership {
    schema_version: u32,
    zcode_home: PathBuf,
    agents_md_path: PathBuf,
    agents_md_block_digest: String,
}

/// Launch-time entry point called from bootstrap. Patches the amplihack
/// block into `~/.zcode/AGENTS.md` if needed; a strict no-op when current.
pub fn ensure_assets_staged() -> Result<()> {
    let source = super::find_bundled_framework_root()
        .context("bundled source required to stage ZCode global rules")?;
    install(&source.root)?;
    Ok(())
}

/// Install or refresh the amplihack block in `~/.zcode/AGENTS.md`. Returns
/// true if anything changed.
pub(super) fn install(source: &Path) -> Result<bool> {
    let home = zcode_home()?;
    super::connector_paths::ensure_unlinked(&home.join("AGENTS.md"))?;
    super::connector_paths::ensure_unlinked(&ownership_path()?)?;
    fs::create_dir_all(&home).with_context(|| format!("creating {}", home.display()))?;
    let desired = owned_block(&agent_context_source(source)?);

    let agents_md = home.join("AGENTS.md");
    let existing = if path_present(&agents_md)? {
        let bytes = fs::read(&agents_md)?;
        String::from_utf8(bytes).map_err(|_| {
            anyhow::anyhow!(
                "{} is not valid UTF-8; refusing to patch it",
                agents_md.display()
            )
        })?
    } else {
        String::new()
    };

    let next = match split_block(&existing)? {
        // Replace only the owned block; everything outside the markers is
        // preserved byte-for-byte (modulo the surrounding blank-line seam).
        Some((begin, end)) => {
            if existing[begin..end] == desired {
                let digest = format!("{:x}", Sha256::digest(desired.as_bytes()));
                if let Some(record) = read_ownership()?
                    && record.schema_version == 1
                    && record.zcode_home == home
                    && record.agents_md_path == agents_md
                    && record.agents_md_block_digest == digest
                {
                    return Ok(false);
                }
                write_ownership(&home, &agents_md, &desired)?;
                return Ok(true);
            }
            join_seam(existing[..begin].trim_end(), &desired, &existing[end..])
        }
        None => {
            if existing.trim().is_empty() {
                // Absent or whitespace-only file: the block is the whole file.
                format!("{desired}\n")
            } else {
                join_seam(existing.trim_end(), &desired, "")
            }
        }
    };
    atomic_write(&agents_md, next.as_bytes())?;
    write_ownership(&home, &agents_md, &desired)?;
    println!("  ✅ Patched amplihack block into {}", agents_md.display());
    Ok(true)
}

/// Remove exactly the owned marker block from `~/.zcode/AGENTS.md`,
/// restoring the surrounding user content. A file left empty by the removal
/// is deleted (restoring the pre-patch absent-file state). Missing files or a
/// missing block are already-removed, not errors; a missing manifest means
/// amplihack owns nothing here.
pub(super) fn uninstall() -> Result<()> {
    let Some(record) = read_ownership()? else {
        return Ok(());
    };
    ensure!(
        record.schema_version == 1,
        "unknown ZCode ownership manifest schema; preserve it and reconcile manually"
    );
    let home = zcode_home()?;
    ensure!(
        record.zcode_home == home,
        "ZCode ownership manifest names a different home; preserve it and reconcile manually"
    );
    ensure!(
        record.agents_md_path == home.join("AGENTS.md"),
        "ZCode ownership manifest does not name the owned AGENTS.md; preserve it and reconcile manually"
    );
    super::connector_paths::ensure_unlinked(&record.agents_md_path)?;
    if path_present(&record.agents_md_path)? {
        let bytes = fs::read(&record.agents_md_path)?;
        let text = String::from_utf8(bytes).map_err(|_| {
            anyhow::anyhow!(
                "{} is not valid UTF-8; refusing to edit it",
                record.agents_md_path.display()
            )
        })?;
        if let Some((begin, end)) = split_block(&text)? {
            let before = text[..begin].trim_end();
            let after = text[end..].trim_start_matches(['\r', '\n']);
            if before.is_empty() && after.is_empty() {
                fs::remove_file(&record.agents_md_path)
                    .with_context(|| format!("removing {}", record.agents_md_path.display()))?;
            } else {
                let mut next = before.to_string();
                if !next.is_empty() && !after.is_empty() {
                    next.push_str("\n\n");
                }
                if !after.is_empty() {
                    next.push_str(after);
                } else {
                    next.push('\n');
                }
                atomic_write(&record.agents_md_path, next.as_bytes())?;
            }
            println!(
                "  🗑️  Removed amplihack block from {}",
                record.agents_md_path.display()
            );
        }
    }
    // Drop the manifest last, then the staging root only if nothing foreign
    // moved into it.
    let root = root()?;
    fs::remove_file(root.join("ownership.json"))
        .with_context(|| format!("removing {}", root.join("ownership.json").display()))?;
    let _ = fs::remove_dir(&root);
    Ok(())
}

/// The exact block text install owns, for the given agent-context content.
fn owned_block(content: &str) -> String {
    format!("{BEGIN_MARKER}\n{}\n{END_MARKER}", content.trim())
}

/// Splice a replacement block between the user content before and after it,
/// with the same blank-line seam convention on both sides.
fn join_seam(before: &str, block: &str, raw_after: &str) -> String {
    let after = raw_after.trim_start_matches(['\r', '\n']);
    let mut next = String::new();
    if !before.is_empty() {
        next.push_str(before);
        next.push_str("\n\n");
    }
    next.push_str(block);
    if after.is_empty() {
        next.push('\n');
    } else {
        next.push_str("\n\n");
        next.push_str(after);
    }
    next
}

/// Split `text` around the owned marker block, returning the inclusive start
/// of the begin marker and the exclusive end of the end marker. `None` when
/// no block is present. Exactly one marker is corruption — bail with the file
/// preserved rather than guessing.
fn split_block(text: &str) -> Result<Option<(usize, usize)>> {
    let begin = text.find(BEGIN_MARKER);
    let end = text.find(END_MARKER);
    match (begin, end) {
        (None, None) => Ok(None),
        (Some(b), Some(e)) => {
            ensure!(
                b < e,
                "inverted amplihack markers in AGENTS.md; fix or remove them manually"
            );
            Ok(Some((b, e + END_MARKER.len())))
        }
        (Some(_), None) => bail!(
            "{} marker without {} in AGENTS.md; fix or remove the marker manually",
            BEGIN_MARKER,
            END_MARKER
        ),
        (None, Some(_)) => bail!(
            "{} marker without {} in AGENTS.md; fix or remove the marker manually",
            END_MARKER,
            BEGIN_MARKER
        ),
    }
}

/// Amplihack's canonical agent-context document, resolved the same way the
/// Claude install stages `$AMPLIHACK_HOME/CLAUDE.md` (bundle layout
/// preferred, legacy repo-root fallback). Deliberately NOT the repo's
/// contributor-facing AGENTS.md.
fn agent_context_source(source: &Path) -> Result<String> {
    let bundle_candidate = source.join("amplifier-bundle/CLAUDE.md");
    let legacy_candidate = source.join("CLAUDE.md");
    let chosen = if bundle_candidate.is_file() {
        bundle_candidate
    } else if legacy_candidate.is_file() {
        legacy_candidate
    } else {
        bail!(
            "CLAUDE.md not found in source repo. Searched: {} and {}. A valid \
             amplihack-rs checkout must ship CLAUDE.md inside \
             `amplifier-bundle/CLAUDE.md` (preferred, bundle layout) or at \
             the repo root (legacy layout).",
            bundle_candidate.display(),
            legacy_candidate.display()
        )
    };
    fs::read_to_string(&chosen).with_context(|| format!("reading {}", chosen.display()))
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
                "refusing nonregular ZCode ownership manifest"
            );
            Ok(Some(serde_json::from_slice(&fs::read(&path)?).context(
                "malformed ZCode ownership manifest; repair or remove \
                 ~/.amplihack/zcode manually before retrying",
            )?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn write_ownership(zcode_home: &Path, agents_md: &Path, block: &str) -> Result<()> {
    let record = Ownership {
        schema_version: 1,
        zcode_home: zcode_home.to_path_buf(),
        agents_md_path: agents_md.to_path_buf(),
        agents_md_block_digest: format!("{:x}", Sha256::digest(block.as_bytes())),
    };
    let path = ownership_path()?;
    let mut body = serde_json::to_string_pretty(&record)?;
    body.push('\n');
    atomic_write(&path, body.as_bytes())
}

fn atomic_write(path: &Path, body: &[u8]) -> Result<()> {
    let parent = path.parent().context("write destination has no parent")?;
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
