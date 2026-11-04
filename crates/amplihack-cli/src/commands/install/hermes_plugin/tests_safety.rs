use super::tests::{fake_source, in_temp_home};
use super::*;
use crate::test_support::EnvGuard;

#[test]
fn uninstall_rejects_unrelated_or_traversing_manifest_targets() {
    in_temp_home(|dir| {
        install(&fake_source(dir)).unwrap();
        let home = dir.join("home/.hermes");
        let unrelated = home.join("skills/my-own");
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(unrelated.join("keep"), "user data").unwrap();
        let path = ownership_path().unwrap();
        let original = fs::read(&path).unwrap();
        for target in [
            home.clone(),
            unrelated.clone(),
            home.join("skills/amplihack/../my-own"),
        ] {
            let mut record: Ownership = serde_json::from_slice(&original).unwrap();
            record.skills_dir = target;
            fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
            assert!(uninstall().is_err());
            assert_eq!(
                fs::read_to_string(unrelated.join("keep")).unwrap(),
                "user data"
            );
            assert!(path.exists());
        }
    });
}

#[test]
fn custom_profile_is_rejected_before_staging() {
    in_temp_home(|dir| {
        let profile = dir.join("profile");
        let _env = EnvGuard::set([("HERMES_HOME", profile.to_str().unwrap())]);
        let error = install(&fake_source(dir)).unwrap_err().to_string();
        assert!(error.contains("custom HERMES_HOME"));
        assert!(!dir.join("home/.hermes").exists());
        assert!(!profile.exists());
    });
}

#[test]
fn explicit_default_profile_can_install_and_uninstall() {
    in_temp_home(|dir| {
        let home = dir.join("home/.hermes");
        let _env = EnvGuard::set([("HERMES_HOME", home.to_str().unwrap())]);
        install(&fake_source(dir)).unwrap();
        uninstall().unwrap();
        assert!(!home.join("skills/amplihack").exists());
    });
}

#[cfg(unix)]
#[test]
fn symlinked_skill_parent_cannot_redirect_install_or_uninstall() {
    in_temp_home(|dir| {
        let source = fake_source(dir);
        install(&source).unwrap();
        let skills = dir.join("home/.hermes/skills");
        let outside = dir.join("outside");
        fs::rename(&skills, &outside).unwrap();
        std::os::unix::fs::symlink(&outside, &skills).unwrap();
        assert!(install(&source).is_err());
        assert!(uninstall().is_err());
        assert!(outside.join("amplihack/alpha/SKILL.md").exists());
    });
}
