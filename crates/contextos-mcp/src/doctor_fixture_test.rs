use tempfile::tempdir;

use crate::Config;
use crate::doctor::{DoctorCheck, DoctorReport};

/// The doctor check with the given `subject` over a fresh vault holding `files`, each a vault-relative path and its
/// content.
pub(super) fn check_over(subject: &str, files: &[(&str, &str)]) -> Result<DoctorCheck, Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    for (relative, content) in files {
        let path = vault.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
    }
    let mut config = Config::try_from(vec![vault])?;
    for vault in &mut config.vaults {
        vault.state_directory = Some(std::path::PathBuf::from(".contextos"));
    }
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].git.enabled = false;
    let report = DoctorReport::try_from(&config)?;
    Ok(report
        .checks
        .into_iter()
        .find(|check| check.subject == subject)
        .ok_or_else(|| format!("no {subject} check reported"))?)
}
