use super::*;
use crate::test_support::{EnvGuard, env_lock};

pub(super) fn fake_source(dir: &Path, content: &str) -> PathBuf {
    let source = dir.join("repo");
    fs::create_dir_all(source.join("amplifier-bundle")).unwrap();
    fs::write(source.join("amplifier-bundle/CLAUDE.md"), content).unwrap();
    source
}

pub(super) fn in_temp_home<T>(f: impl FnOnce(&Path) -> T) -> T {
    let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir(&home).unwrap();
    let _guard = EnvGuard::set([("HOME", home.to_str().unwrap())]);
    f(dir.path())
}

fn agents_md(dir: &Path) -> PathBuf {
    dir.join("home/.zcode/AGENTS.md")
}

#[test]
fn install_patches_agents_md_and_repeat_install_is_a_noop() {
    in_temp_home(|dir| {
        let source = fake_source(dir, "agent context body\n");
        assert!(install(&source).unwrap());
        let expected = format!("{BEGIN_MARKER}\nagent context body\n{END_MARKER}\n");
        assert_eq!(fs::read_to_string(agents_md(dir)).unwrap(), expected);
        // Ownership manifest recorded under amplihack-private root.
        let manifest = dir.join("home/.amplihack/zcode/ownership.json");
        let record: Ownership = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        assert_eq!(record.schema_version, 1);
        assert_eq!(record.agents_md_path, agents_md(dir));
        // Second install with an unchanged source: no-op, no rewrite.
        let mtime = fs::metadata(agents_md(dir)).unwrap().modified().unwrap();
        let manifest_bytes = fs::read(&manifest).unwrap();
        assert!(!install(&source).unwrap());
        assert_eq!(
            fs::metadata(agents_md(dir)).unwrap().modified().unwrap(),
            mtime
        );
        assert_eq!(fs::read(&manifest).unwrap(), manifest_bytes);
    });
}

#[test]
fn install_preserves_user_content_and_replaces_a_stale_block() {
    in_temp_home(|dir| {
        let path = agents_md(dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "my hand-written rules\n").unwrap();

        let source = fake_source(dir, "rules v1\n");
        install(&source).unwrap();
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .starts_with("my hand-written rules\n")
        );

        // A changed source updates only the owned block.
        let updated = fake_source(dir, "rules v2\n");
        assert!(install(&updated).unwrap());
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("my hand-written rules\n"));
        assert!(text.contains("rules v2\n"));
        assert!(!text.contains("rules v1"));
        // And the update is itself stable.
        assert!(!install(&updated).unwrap());
    });
}

#[test]
fn uninstall_removes_the_owned_block_and_is_idempotent() {
    in_temp_home(|dir| {
        let path = agents_md(dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "my rules\n").unwrap();
        install(&fake_source(dir, "block\n")).unwrap();

        uninstall().unwrap();
        // The pre-patch user content survives byte-for-byte…
        assert_eq!(fs::read_to_string(&path).unwrap(), "my rules\n");
        // …the manifest and staging root are gone…
        assert!(!dir.join("home/.amplihack/zcode").exists());
        // …and a second uninstall succeeds with nothing to do.
        uninstall().unwrap();
    });
}

#[test]
fn uninstall_removes_an_agents_md_amplihack_created() {
    in_temp_home(|dir| {
        install(&fake_source(dir, "block\n")).unwrap();
        assert!(agents_md(dir).is_file());
        uninstall().unwrap();
        assert!(!agents_md(dir).exists());
        assert!(!dir.join("home/.amplihack/zcode").exists());
    });
}

#[test]
fn half_a_marker_block_is_refused_with_the_file_preserved() {
    in_temp_home(|dir| {
        let path = agents_md(dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let broken = "user text\n".to_string() + BEGIN_MARKER + "\nno end marker\n";
        fs::write(&path, &broken).unwrap();
        let error = format!("{:#}", install(&fake_source(dir, "block\n")).unwrap_err());
        assert!(error.contains("fix or remove the marker"));
        assert_eq!(fs::read_to_string(&path).unwrap(), broken);
    });
}
