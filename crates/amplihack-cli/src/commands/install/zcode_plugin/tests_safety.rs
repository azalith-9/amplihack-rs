use super::tests::{fake_source, in_temp_home};
use super::*;

#[test]
fn unchanged_install_repairs_missing_or_stale_ownership() {
    in_temp_home(|dir| {
        let source = fake_source(dir, "instructions");
        install(&source).unwrap();
        let manifest = ownership_path().unwrap();
        let agents = dir.join("home/.zcode/AGENTS.md");
        let content = fs::read(&agents).unwrap();
        fs::remove_file(&manifest).unwrap();
        assert!(install(&source).unwrap());
        assert!(manifest.exists());
        assert_eq!(fs::read(&agents).unwrap(), content);
        let mut record = read_ownership().unwrap().unwrap();
        record.agents_md_block_digest = "stale".into();
        fs::write(&manifest, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(install(&source).unwrap());
        assert!(!install(&source).unwrap());
        uninstall().unwrap();
        assert!(!agents.exists());
    });
}

#[test]
fn uninstall_rejects_other_files_and_traversal() {
    in_temp_home(|dir| {
        install(&fake_source(dir, "instructions")).unwrap();
        let home = dir.join("home/.zcode");
        let foreign = home.join("user.md");
        let content = format!("{BEGIN_MARKER}\nuser rules\n{END_MARKER}");
        fs::write(&foreign, &content).unwrap();
        let path = ownership_path().unwrap();
        let original = fs::read(&path).unwrap();
        for target in [foreign.clone(), home.join("child/../user.md")] {
            let mut record: Ownership = serde_json::from_slice(&original).unwrap();
            record.agents_md_path = target;
            fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
            assert!(uninstall().is_err());
            assert_eq!(fs::read_to_string(&foreign).unwrap(), content);
            assert!(path.exists());
        }
    });
}

#[cfg(unix)]
#[test]
fn symlinked_agents_file_is_preserved() {
    in_temp_home(|dir| {
        let source = fake_source(dir, "instructions");
        install(&source).unwrap();
        let agents = dir.join("home/.zcode/AGENTS.md");
        let foreign = dir.join("user.md");
        fs::rename(&agents, &foreign).unwrap();
        std::os::unix::fs::symlink(&foreign, &agents).unwrap();
        assert!(install(&source).is_err());
        assert!(uninstall().is_err());
        assert!(
            fs::symlink_metadata(&agents)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(foreign.exists());
    });
}
