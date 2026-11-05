use tempfile::tempdir;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn target(dir: &Path) -> Result<ProjectionTarget, ProjectionError> {
    ProjectionTarget::try_from(dir)
}

#[test]
fn configuration_ids_accept_visible_text_and_reject_empty_overlong_or_control_characters() {
    assert!(ConfigurationId::try_from("claude").is_ok());
    assert!(ConfigurationId::try_from("codex-main_2.0").is_ok());
    for value in ["", "a\nb", "a\0b", &"x".repeat(129)] {
        assert!(
            matches!(
                ConfigurationId::try_from(value),
                Err(ProjectionError::InvalidConfigurationId { .. })
            ),
            "{value:?}"
        );
    }
}

#[test]
fn the_target_key_is_a_stable_sha256_of_the_resolved_root() -> TestResult {
    let dir = tempdir()?;
    let one = TargetKey::from(&target(dir.path())?);
    let two = TargetKey::from(&target(dir.path())?);
    let other = tempdir()?;
    assert_eq!(one, two);
    assert_ne!(one, TargetKey::from(&target(other.path())?));
    let text = <&str>::from(&one);
    assert_eq!(text.len(), 64);
    assert!(text.bytes().all(|byte| byte.is_ascii_hexdigit()));
    Ok(())
}

#[test]
fn the_location_for_a_target_is_the_state_projection_folder_and_the_key() -> TestResult {
    let state = tempdir()?;
    let root = tempdir()?;
    let target = target(root.path())?;
    let location = ManifestLocation::for_target(state.path(), &target)?;
    let key = TargetKey::from(&target);
    let expected = state
        .path()
        .join("projection")
        .join(format!("{}.json", <&str>::from(&key)));
    assert_eq!(<&Path>::from(&location), expected.as_path());
    Ok(())
}

#[test]
fn a_relative_state_root_or_location_is_rejected() -> TestResult {
    let root = tempdir()?;
    let target = target(root.path())?;
    assert!(matches!(
        ManifestLocation::for_target(Path::new("state"), &target),
        Err(ProjectionError::TargetRootRejected { .. })
    ));
    assert!(matches!(
        ManifestLocation::try_from(Path::new("manifest.json")),
        Err(ProjectionError::TargetRootRejected { .. })
    ));
    Ok(())
}

#[test]
fn an_entry_requires_a_source_vault_and_path() -> TestResult {
    let hash = ContentHash::from([1_u8; 32]);
    let configuration = ConfigurationId::try_from("claude")?;
    for (vault, path) in [("", "registry/agents/a.md"), ("harbour", "")] {
        assert!(
            ManifestEntry::new(vault, path, hash.clone(), hash.clone(), configuration.clone()).is_err(),
            "{vault:?} {path:?}"
        );
    }
    Ok(())
}
