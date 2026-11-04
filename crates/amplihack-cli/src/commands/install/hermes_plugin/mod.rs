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

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Amplihack-owned namespace under the Hermes skills directory. Everything
/// inside it is ours; nothing outside it is.
const SKILLS_NAMESPACE: &str = "amplihack";

/// Ownership manifest root: `~/.amplihack/hermes/` (amplihack-private).
fn root() -> Result<PathBuf> {
    Ok(super::paths::home_dir()?.join(".amplihack/hermes"))
}

/// Hermes config home. Profiles relocate it via `HERMES_HOME` (verified in
/// the installed source); HOME-relative `~/.hermes` is the default and what
/// this module stages into — it never sets the override.
fn hermes_home() -> Result<PathBuf> {
    Ok(super::paths::home_dir()?.join(".hermes"))
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
        record.skills_dir.starts_with(&hermes),
        "Hermes ownership manifest points outside the Hermes home; preserve it and reconcile manually"
    );
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

/// Build the portable skill set into `bundle` from the bundled framework
/// source. The inputs are exactly the codex_plugin package inputs — canonical
/// bundle skills plus generated instruction skills; the Codex-specific
/// plugin.json/hooks/bin artifacts are not part of a Hermes skill set. Never
/// the repo's contributor AGENTS.md or skills.
fn build_bundle(source: &Path, bundle: &Path) -> Result<()> {
    let canonical = source.join("amplifier-bundle/skills");
    ensure!(
        fs::symlink_metadata(&canonical)
            .map(|m| m.is_dir() && !m.file_type().is_symlink())
            .unwrap_or(false),
        "amplihack bundle source is missing canonical skills at {}; a valid \
         amplihack-rs checkout must ship `amplifier-bundle/skills`",
        canonical.display()
    );
    validate_skill_names(&canonical, &mut std::collections::BTreeSet::new(), 0)?;
    copy_tree(&canonical, bundle, &canonical, 0)?;
    instruction_skills(
        &source.join("docs/claude/commands/amplihack"),
        bundle,
        "command",
    )?;
    instruction_skills(&source.join("amplifier-bundle/agents"), bundle, "persona")?;
    Ok(())
}

fn ownership_path() -> Result<PathBuf> {
    Ok(root()?.join("ownership.json"))
}

fn read_ownership() -> Result<Option<Ownership>> {
    let path = ownership_path()?;
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

/// Number of skill directories (directories containing a SKILL.md) under
/// `dir`, at any depth.
fn count_skills(dir: &Path) -> Result<usize> {
    fn walk(dir: &Path, count: &mut usize) -> Result<()> {
        if dir.join("SKILL.md").is_file() {
            *count += 1;
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                walk(&entry.path(), count)?;
            }
        }
        Ok(())
    }
    let mut count = 0;
    walk(dir, &mut count)?;
    Ok(count)
}

/// Canonical resource copying with symlink-escape refusal (mirrors
/// codex_plugin::resources::copy_tree).
fn copy_tree(source: &Path, destination: &Path, boundary: &Path, depth: usize) -> Result<()> {
    ensure!(depth < 32, "Hermes resource nesting exceeds safe depth");
    fs::create_dir_all(destination)?;
    for e in fs::read_dir(source)? {
        let e = e?;
        let target = destination.join(e.file_name());
        let kind = e.file_type()?;
        if kind.is_symlink() {
            let link = fs::read_link(e.path())?;
            ensure!(
                !link.is_absolute(),
                "absolute skill resource symlink is not portable"
            );
            // Keep relative resource links (including intentionally dangling
            // ones) only if lexical resolution stays inside the complete tree.
            let mut resolved = e.path().parent().context("resource parent")?.to_path_buf();
            for c in link.components() {
                match c {
                    std::path::Component::ParentDir => {
                        ensure!(resolved.pop(), "resource symlink escape");
                    }
                    std::path::Component::Normal(n) => resolved.push(n),
                    std::path::Component::CurDir => {}
                    _ => bail!("resource symlink escape"),
                }
            }
            ensure!(
                resolved.starts_with(boundary),
                "skill resource symlink escapes canonical tree"
            );
            if e.path().exists() {
                ensure!(
                    e.path().canonicalize()?.starts_with(boundary),
                    "resolved skill resource escapes canonical tree"
                );
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(link, target)?;
            #[cfg(not(unix))]
            bail!("symlink resource staging requires Unix");
        } else if kind.is_dir() {
            copy_tree(&e.path(), &target, boundary, depth + 1)?;
        } else {
            ensure!(kind.is_file(), "unsupported skill resource type");
            fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

/// Framed tree digest (mirrors codex_plugin::resources::digest with a
/// Hermes-specific domain tag, so trees from different adapters can never be
/// confused for each other).
fn digest(root: &Path) -> Result<String> {
    fn field(hash: &mut Sha256, bytes: &[u8]) {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    fn walk(path: &Path, base: &Path, hash: &mut Sha256) -> Result<()> {
        let mut entries = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            field(
                hash,
                e.path().strip_prefix(base)?.as_os_str().as_encoded_bytes(),
            );
            let kind = e.file_type()?;
            if kind.is_symlink() {
                hash.update(b"L");
                field(
                    hash,
                    fs::read_link(e.path())?.as_os_str().as_encoded_bytes(),
                );
            } else if kind.is_dir() {
                hash.update(b"D");
                walk(&e.path(), base, hash)?;
            } else {
                ensure!(kind.is_file(), "unsupported Hermes digest entry type");
                hash.update(b"F");
                field(hash, &fs::read(e.path())?);
            }
        }
        Ok(())
    }
    let meta = fs::symlink_metadata(root)?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "nonregular Hermes digest root"
    );
    let mut hash = Sha256::new();
    hash.update(b"amplihack-hermes-tree-v1\0");
    walk(root, root, &mut hash)?;
    Ok(format!("v1:{:x}", hash.finalize()))
}

/// A recorded digest must be a current framed identity before it can
/// authorize anything.
fn current_digest(value: &str) -> bool {
    value
        .strip_prefix("v1:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Validate canonical skill frontmatter (mirrors
/// codex_plugin::resources::validate_skill_names).
fn validate_skill_names(
    directory: &Path,
    names: &mut std::collections::BTreeSet<String>,
    depth: usize,
) -> Result<()> {
    ensure!(depth < 32, "skill validation depth exceeds safe limit");
    let skill = directory.join("SKILL.md");
    if skill.is_file() {
        let text = fs::read_to_string(&skill)?;
        let mut lines = text.lines();
        ensure!(
            lines.next() == Some("---"),
            "Hermes skill lacks YAML frontmatter: {}",
            skill.display()
        );
        let mut yaml = Vec::new();
        let mut closed = false;
        for line in lines {
            if line.trim() == "---" {
                closed = true;
                break;
            }
            yaml.push(line);
        }
        ensure!(closed, "Hermes skill frontmatter is unterminated");
        let frontmatter: serde_yaml::Value = serde_yaml::from_str(&yaml.join("\n"))?;
        let name = frontmatter["name"]
            .as_str()
            .context("Hermes skill requires a string name")?;
        ensure!(
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "invalid canonical Hermes skill name"
        );
        ensure!(
            frontmatter["description"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty()),
            "Hermes skill requires a nonempty description"
        );
        ensure!(
            names.insert(name.to_string()),
            "duplicate canonical Hermes skill identity: {name}"
        );
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            validate_skill_names(&entry.path(), names, depth + 1)?;
        }
    }
    Ok(())
}

/// Portable generated instruction skills (mirrors
/// codex_plugin::instructions).
fn portable_instruction(source: &str) -> String {
    let body = if let Some(rest) = source.strip_prefix("---\n") {
        rest.split_once("\n---\n")
            .map(|(_, body)| body)
            .unwrap_or(source)
    } else {
        source
    };
    body.replace(
        "Use GPT-4 to analyze code + comments",
        "Analyze code + comments",
    )
}

fn instruction_skills(source: &Path, output: &Path, prefix: &str) -> Result<()> {
    instruction_skills_under(source, source, output, prefix)
}

fn instruction_skills_under(source: &Path, base: &Path, output: &Path, prefix: &str) -> Result<()> {
    if !source.exists() {
        return Ok(());
    }
    for e in fs::read_dir(source)? {
        let e = e?;
        if e.file_type()?.is_dir() {
            instruction_skills_under(&e.path(), base, output, prefix)?;
            continue;
        }
        if e.path().extension().is_none_or(|x| x != "md") {
            continue;
        }
        let stem = e
            .path()
            .strip_prefix(base)?
            .with_extension("")
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("-");
        ensure!(
            stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "invalid instruction skill name"
        );
        let name = format!("amplihack-{prefix}-{stem}");
        let dir = output.join(&name);
        ensure!(!dir.exists(), "generated Hermes skill collision: {name}");
        fs::create_dir(&dir)?;
        let source_body = fs::read_to_string(e.path())?;
        let body = portable_instruction(&source_body);
        fs::write(
            dir.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: Amplihack {prefix} instructions for {stem}\n---\n\n{body}"
            ),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{EnvGuard, env_lock};

    /// Minimal synthetic stand-in for a bundled framework source: one
    /// canonical skill plus one command and one persona instruction input.
    fn fake_source(dir: &Path) -> PathBuf {
        let source = dir.join("repo");
        fs::create_dir_all(source.join("amplifier-bundle/skills/alpha")).unwrap();
        fs::write(
            source.join("amplifier-bundle/skills/alpha/SKILL.md"),
            "---\nname: alpha\ndescription: alpha skill\n---\nalpha body\n",
        )
        .unwrap();
        fs::create_dir_all(source.join("docs/claude/commands/amplihack")).unwrap();
        fs::write(
            source.join("docs/claude/commands/amplihack/foo.md"),
            "command doc body\n",
        )
        .unwrap();
        fs::create_dir_all(source.join("amplifier-bundle/agents")).unwrap();
        fs::write(
            source.join("amplifier-bundle/agents/bar.md"),
            "persona doc\n",
        )
        .unwrap();
        source
    }

    fn in_temp_home<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir(&home).unwrap();
        let _guard = EnvGuard::set([("HOME", home.to_str().unwrap())]);
        f(dir.path())
    }

    fn mtime(path: &Path) -> std::time::SystemTime {
        fs::metadata(path).unwrap().modified().unwrap()
    }

    #[test]
    fn install_creates_expected_layout_and_repeat_install_is_a_noop() {
        in_temp_home(|dir| {
            let source = fake_source(dir);
            assert!(install(&source).unwrap());
            let hermes = dir.join("home/.hermes");
            let skills = hermes.join("skills/amplihack");
            // Canonical skill staged under the namespace dir.
            assert_eq!(
                fs::read_to_string(skills.join("alpha/SKILL.md")).unwrap(),
                "---\nname: alpha\ndescription: alpha skill\n---\nalpha body\n"
            );
            // Generated instruction skills staged alongside.
            let command_skill = skills.join("amplihack-command-foo/SKILL.md");
            assert!(command_skill.is_file());
            let text = fs::read_to_string(&command_skill).unwrap();
            assert!(text.starts_with("---\nname: amplihack-command-foo\n"));
            assert!(skills.join("amplihack-persona-bar/SKILL.md").is_file());
            // Ownership manifest + build bundle live under amplihack-private root.
            let manifest = dir.join("home/.amplihack/hermes/ownership.json");
            assert!(manifest.is_file());
            let record: Ownership = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
            assert_eq!(record.schema_version, 1);
            assert!(current_digest(&record.skills_digest));

            // Second install with an unchanged source: no-op, no rewrites.
            let skill_mtime = mtime(&skills.join("alpha/SKILL.md"));
            let manifest_bytes = fs::read(&manifest).unwrap();
            assert!(!install(&source).unwrap());
            assert_eq!(mtime(&skills.join("alpha/SKILL.md")), skill_mtime);
            assert_eq!(fs::read(&manifest).unwrap(), manifest_bytes);
        });
    }

    #[test]
    fn install_preserves_unrelated_user_config() {
        in_temp_home(|dir| {
            let hermes = dir.join("home/.hermes");
            fs::create_dir_all(&hermes).unwrap();
            fs::write(hermes.join("config.yaml"), "model: my-own-choice\n").unwrap();
            let own_skill = hermes.join("skills/my-own");
            fs::create_dir_all(&own_skill).unwrap();
            fs::write(own_skill.join("SKILL.md"), "user's own skill\n").unwrap();

            let source = fake_source(dir);
            install(&source).unwrap();
            assert_eq!(
                fs::read_to_string(hermes.join("config.yaml")).unwrap(),
                "model: my-own-choice\n"
            );
            assert_eq!(
                fs::read_to_string(own_skill.join("SKILL.md")).unwrap(),
                "user's own skill\n"
            );
            // Reinstall leaves them intact too.
            install(&source).unwrap();
            assert_eq!(
                fs::read_to_string(hermes.join("config.yaml")).unwrap(),
                "model: my-own-choice\n"
            );
            assert!(own_skill.join("SKILL.md").is_file());
        });
    }

    #[test]
    fn uninstall_removes_exactly_the_owned_files_and_is_idempotent() {
        in_temp_home(|dir| {
            let source = fake_source(dir);
            let hermes = dir.join("home/.hermes");
            fs::create_dir_all(hermes.join("skills/my-own")).unwrap();
            fs::write(hermes.join("skills/my-own/SKILL.md"), "user's own skill\n").unwrap();
            install(&source).unwrap();
            fs::write(hermes.join("config.yaml"), "user config\n").unwrap();

            uninstall().unwrap();
            assert!(!hermes.join("skills/amplihack").exists());
            // Owned manifest and staging root are gone…
            assert!(!dir.join("home/.amplihack/hermes").exists());
            // …and nothing else is.
            assert_eq!(
                fs::read_to_string(hermes.join("config.yaml")).unwrap(),
                "user config\n"
            );
            assert_eq!(
                fs::read_to_string(hermes.join("skills/my-own/SKILL.md")).unwrap(),
                "user's own skill\n"
            );

            // Idempotent: a second uninstall succeeds with nothing to do.
            uninstall().unwrap();

            // …and a fresh install after uninstall works again.
            assert!(install(&source).unwrap());
            assert!(hermes.join("skills/amplihack/alpha/SKILL.md").is_file());
        });
    }

    #[test]
    fn uninstall_treats_missing_owned_files_as_already_removed() {
        in_temp_home(|dir| {
            let source = fake_source(dir);
            install(&source).unwrap();
            // Simulate a partial manual cleanup: the namespace is already gone.
            fs::remove_dir_all(dir.join("home/.hermes/skills/amplihack")).unwrap();
            // Must not error, and must still retire the ownership record.
            uninstall().unwrap();
            assert!(!dir.join("home/.amplihack/hermes").exists());
        });
    }

    #[test]
    fn stale_installed_tree_is_replaced_on_reinstall() {
        in_temp_home(|dir| {
            let source = fake_source(dir);
            install(&source).unwrap();
            let skills = dir.join("home/.hermes/skills/amplihack");
            // Simulate drift (a lost or hand-edited file inside the namespace).
            fs::remove_file(skills.join("alpha/SKILL.md")).unwrap();
            assert!(install(&source).unwrap());
            assert!(skills.join("alpha/SKILL.md").is_file());
        });
    }

    #[test]
    fn missing_canonical_skills_source_bails_loudly() {
        in_temp_home(|dir| {
            let source = dir.join("empty-repo");
            fs::create_dir_all(&source).unwrap();
            let error = format!("{:#}", install(&source).unwrap_err());
            assert!(error.contains("amplifier-bundle/skills"));
            // A failed install must not leave an ownership record behind.
            assert!(!dir.join("home/.amplihack/hermes/ownership.json").exists());
        });
    }
}
