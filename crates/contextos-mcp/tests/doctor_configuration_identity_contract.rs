//! The `configuration` check names this configuration's projection identity, as the first 12 hexadecimal characters
//! of its SHA-256, when projection is enabled, so an operator can match a foreign-owner finding to a configuration.

mod support;
use sha2::{Digest, Sha256};
use support::projection_doctor::{REVIEWER, checks_of_kind, doctor};
use support::projection_world::{BoxError, ProjectionWorld, agent};

/// The 12-character identity the operator would see for the configuration file at `path`.
fn short_identity(path: &std::path::Path) -> String {
    // As the server resolves it: links are followed as far as the path exists, and the rest is kept.
    let existing = path.ancestors().find(|ancestor| ancestor.exists()).unwrap_or(path);
    let resolved = dunce::canonicalize(existing).map_or_else(
        |_| path.to_path_buf(),
        |base| {
            let rest = path.strip_prefix(existing).unwrap_or(path);
            rest.components().fold(base, |resolved, part| resolved.join(part))
        },
    );
    let digest = Sha256::digest(resolved.as_os_str().as_encoded_bytes());
    digest.iter().take(6).fold(String::new(), |mut text, byte| {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
        text
    })
}

#[tokio::test]
async fn the_configuration_check_names_the_projection_identity_when_projection_is_enabled() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    let server = world.projecting_server_for("work.toml")?;
    let file = world.home().join(".config/contextos/work.toml");

    let report = doctor(&server).await?;

    let configuration = checks_of_kind(&report, "configuration");
    assert_eq!(configuration.len(), 1, "{report}");
    let message = configuration[0]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(&short_identity(&file)),
        "the configuration check does not carry the projection identity {}: {message}",
        short_identity(&file)
    );
    Ok(())
}

#[tokio::test]
async fn two_configurations_show_different_identities_in_their_configuration_checks() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let one = doctor(&world.projecting_server_for("one.toml")?).await?;
    let two = doctor(&world.projecting_server_for("two.toml")?).await?;

    assert_ne!(
        checks_of_kind(&one, "configuration")[0]["message"],
        checks_of_kind(&two, "configuration")[0]["message"]
    );
    Ok(())
}

#[tokio::test]
async fn the_configuration_check_names_no_identity_when_projection_is_off() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    let server = contextos_mcp::ContextOsServer::try_from(world.config(contextos_mcp::ConfigLocation::Named, None)?)?;

    let report = doctor(&server).await?;

    let message = checks_of_kind(&report, "configuration")[0]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(!message.to_lowercase().contains("projection"), "{message}");
    Ok(())
}
