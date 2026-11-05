use tempfile::tempdir;

use crate::Config;
use crate::doctor::{DoctorCheck, DoctorReport, DoctorStatus};

/// As `Config::try_from`'s own conversion, with every vault's state directory inside its own root, so a throwaway vault
/// never leaves state under the operator's home directory.
fn scoped_config(paths: Vec<std::path::PathBuf>) -> Result<Config, Box<dyn std::error::Error>> {
    let mut config = Config::try_from(paths)?;
    for vault in &mut config.vaults {
        vault.state_directory = Some(std::path::PathBuf::from(".contextos"));
    }
    Ok(config)
}

fn fence_check(manifest: Option<&str>) -> Result<DoctorCheck, Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir_all(vault.join(".contextos"))?;
    if let Some(manifest) = manifest {
        std::fs::write(vault.join(".contextos/fences.toml"), manifest)?;
    }
    let mut config = scoped_config(vec![vault])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].git.enabled = false;
    let report = DoctorReport::try_from(&config)?;
    report
        .checks
        .into_iter()
        .find(|check| check.subject == "Fence vocabulary")
        .ok_or_else(|| "no Fence vocabulary check reported".into())
}

#[test]
fn a_vault_without_a_fence_manifest_uses_the_built_in_vocabulary() -> Result<(), Box<dyn std::error::Error>> {
    let check = fence_check(None)?;
    assert_eq!(check.status, DoctorStatus::Pass);
    assert!(check.message.contains("built-in"), "{}", check.message);
    Ok(())
}

#[test]
fn a_usable_manifest_passes_and_names_the_standard_it_encodes() -> Result<(), Box<dyn std::error::Error>> {
    let check = fence_check(Some(contextos_markdown::DEFAULT_FENCES_TOML))?;
    assert_eq!(check.status, DoctorStatus::Pass);
    assert!(
        check.message.contains("markdown-fence-conventions"),
        "{}",
        check.message
    );
    Ok(())
}

#[test]
fn a_manifest_that_cannot_be_used_fails_and_says_why_without_being_repaired() -> Result<(), Box<dyn std::error::Error>>
{
    for (manifest, why) in [
        ("schema = 9\n", "newer"),
        ("schema = 1\n[fences.note]\nrole = \"loud\"\n", "valid"),
    ] {
        let check = fence_check(Some(manifest))?;
        assert_eq!(check.status, DoctorStatus::Fail, "{manifest}");
        assert!(!check.auto_fixable, "{manifest}");
        assert!(check.message.contains(why), "{manifest}: {}", check.message);
        let action = check.action.as_deref().unwrap_or_default();
        assert!(action.contains(".contextos/fences.toml"), "{action}");
    }
    Ok(())
}
