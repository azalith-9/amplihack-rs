//! Path checks for connector-owned installation destinations.
use anyhow::{Result, ensure};
use std::{fs, path::Path};

/// Refuse symlinks in an owned destination and its parents below the user home.
/// This checks existing entries before mutation; it does not provide protection
/// against a concurrent process swapping directories after validation.
pub(super) fn ensure_unlinked(path: &Path) -> Result<()> {
    let home = super::paths::home_dir()?;
    let relative = path.strip_prefix(&home)?;
    let mut current = home;
    for component in relative.components() {
        ensure!(
            matches!(component, std::path::Component::Normal(_)),
            "connector destination contains non-normal path components"
        );
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) => ensure!(
                !meta.file_type().is_symlink(),
                "refusing symlink connector destination: {}",
                current.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
