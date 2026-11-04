use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

/// Build the portable skill set into `bundle` from the bundled framework
/// source. The inputs are exactly the codex_plugin package inputs — canonical
/// bundle skills plus generated instruction skills; the Codex-specific
/// plugin.json/hooks/bin artifacts are not part of a Hermes skill set. Never
/// the repo's contributor AGENTS.md or skills.
pub(super) fn build_bundle(source: &Path, bundle: &Path) -> Result<()> {
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

/// Number of skill directories (directories containing a SKILL.md) under
/// `dir`, at any depth.
pub(super) fn count_skills(dir: &Path) -> Result<usize> {
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
pub(super) fn copy_tree(
    source: &Path,
    destination: &Path,
    boundary: &Path,
    depth: usize,
) -> Result<()> {
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
pub(super) fn digest(root: &Path) -> Result<String> {
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
pub(super) fn current_digest(value: &str) -> bool {
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
