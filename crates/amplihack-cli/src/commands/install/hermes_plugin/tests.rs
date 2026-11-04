use super::*;
use crate::test_support::{EnvGuard, env_lock};

/// Minimal synthetic stand-in for a bundled framework source: one
/// canonical skill plus one command and one persona instruction input.
pub(super) fn fake_source(dir: &Path) -> PathBuf {
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

pub(super) fn in_temp_home<T>(f: impl FnOnce(&Path) -> T) -> T {
    let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir(&home).unwrap();
    let _guard = EnvGuard::set([("HOME", home.to_str().unwrap())]);
    let _profile = EnvGuard::unset(["HERMES_HOME"]);
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
