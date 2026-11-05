//! The ownership manifest on disk: round trip, missing versus malformed, and mismatch rejection.

use std::fs;

use contextos_core::ContentHash;
use contextos_projection::{
    ConfigurationId, ManifestEntry, ManifestLocation, OwnershipManifest, ProjectionError, ProjectionTarget, Slug,
};
use tempfile::{TempDir, tempdir};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture {
    root: TempDir,
    state: TempDir,
    target: ProjectionTarget,
    location: ManifestLocation,
}

fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let state = tempdir()?;
    let target = ProjectionTarget::try_from(root.path())?;
    let location = ManifestLocation::for_target(state.path(), &target)?;
    Ok(Fixture {
        root,
        state,
        target,
        location,
    })
}

fn entry(seed: u8) -> Result<ManifestEntry, ProjectionError> {
    ManifestEntry::new(
        "harbour",
        "registry/agents/reviewer.md",
        ContentHash::from([seed; 32]),
        ContentHash::from([seed.wrapping_add(1); 32]),
        ConfigurationId::try_from("claude")?,
    )
}

fn malformed(result: &Result<OwnershipManifest, ProjectionError>) -> bool {
    matches!(*result, Err(ProjectionError::ManifestMalformed { .. }))
}

fn written_document(fixture: &Fixture) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let mut manifest = OwnershipManifest::empty();
    manifest.insert(Slug::try_from("reviewer")?, entry(1)?);
    manifest.write(&fixture.location, &fixture.target)?;
    let path: &std::path::Path = (&fixture.location).into();
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn rewrite(fixture: &Fixture, document: &serde_json::Value) -> TestResult {
    let path: &std::path::Path = (&fixture.location).into();
    fs::write(path, serde_json::to_vec(document)?)?;
    Ok(())
}

#[test]
fn a_missing_manifest_reads_as_empty() -> TestResult {
    let fixture = fixture()?;
    let manifest = OwnershipManifest::read(&fixture.location, &fixture.target)?;
    assert!(manifest.is_empty());
    assert_eq!(manifest.len(), 0);
    Ok(())
}

#[test]
fn a_manifest_round_trips_every_field_in_slug_order() -> TestResult {
    let fixture = fixture()?;
    let mut manifest = OwnershipManifest::empty();
    manifest.insert(Slug::try_from("zeta")?, entry(9)?);
    manifest.insert(Slug::try_from("alpha")?, entry(1)?);

    manifest.write(&fixture.location, &fixture.target)?;
    let read = OwnershipManifest::read(&fixture.location, &fixture.target)?;

    assert_eq!(read, manifest);
    let alpha = read.get(&Slug::try_from("alpha")?).ok_or("alpha missing")?;
    assert_eq!(alpha.source_vault(), "harbour");
    assert_eq!(alpha.source_path(), "registry/agents/reviewer.md");
    assert_eq!(alpha.source_hash(), &ContentHash::from([1_u8; 32]));
    assert_eq!(alpha.projected_hash(), &ContentHash::from([2_u8; 32]));
    assert_eq!(<&str>::from(alpha.configuration()), "claude");
    let order: Vec<&str> = read.iter().map(|(slug, _)| <&str>::from(slug)).collect();
    assert_eq!(order, vec!["alpha", "zeta"]);
    Ok(())
}

#[test]
fn the_manifest_is_stored_under_the_stateroot_not_in_the_target() -> TestResult {
    let fixture = fixture()?;
    written_document(&fixture)?;
    let path: &std::path::Path = (&fixture.location).into();
    assert!(path.starts_with(fixture.state.path().join("projection")));
    assert_eq!(fs::read_dir(fixture.root.path())?.count(), 0);
    Ok(())
}

#[test]
fn removing_and_replacing_entries_is_reflected_after_a_round_trip() -> TestResult {
    let fixture = fixture()?;
    let slug = Slug::try_from("reviewer")?;
    let mut manifest = OwnershipManifest::empty();
    assert!(manifest.insert(slug.clone(), entry(1)?).is_none());
    assert!(manifest.insert(slug.clone(), entry(5)?).is_some());
    manifest.write(&fixture.location, &fixture.target)?;
    let mut read = OwnershipManifest::read(&fixture.location, &fixture.target)?;
    assert_eq!(read.get(&slug), Some(&entry(5)?));
    assert!(read.remove(&slug).is_some());
    read.write(&fixture.location, &fixture.target)?;
    assert!(OwnershipManifest::read(&fixture.location, &fixture.target)?.is_empty());
    Ok(())
}

#[test]
fn malformed_content_is_reported_and_never_repaired() -> TestResult {
    let fixture = fixture()?;
    let path: &std::path::Path = (&fixture.location).into();
    fs::create_dir_all(path.parent().ok_or("no parent")?)?;
    for content in [
        "",
        "not json",
        "[]",
        "{}",
        "{\"version\":1}",
        "{\"version\":1,\"targetroot\":\"x\"}",
    ] {
        fs::write(path, content)?;
        assert!(
            malformed(&OwnershipManifest::read(&fixture.location, &fixture.target)),
            "{content:?}"
        );
        assert_eq!(
            fs::read_to_string(path)?,
            content,
            "a malformed manifest must be left as found"
        );
    }
    Ok(())
}

#[test]
fn unknown_fields_a_wrong_version_and_a_foreign_target_are_malformed() -> TestResult {
    let fixture = fixture()?;
    let base = written_document(&fixture)?;

    let mut unknown_top = base.clone();
    unknown_top["extra"] = serde_json::json!(true);
    let mut unknown_entry = base.clone();
    unknown_entry["entries"]["reviewer"]["extra"] = serde_json::json!(true);
    let mut wrong_version = base.clone();
    wrong_version["version"] = serde_json::json!(2);
    let mut foreignroot = base.clone();
    foreignroot["targetroot"] = serde_json::json!("/somewhere/else");

    for (name, document) in [
        ("unknown top-level field", unknown_top),
        ("unknown entry field", unknown_entry),
        ("wrong version", wrong_version),
        ("foreign target root", foreignroot),
    ] {
        rewrite(&fixture, &document)?;
        assert!(
            malformed(&OwnershipManifest::read(&fixture.location, &fixture.target)),
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn invalid_entry_values_are_malformed() -> TestResult {
    let fixture = fixture()?;
    let base = written_document(&fixture)?;

    let mut bad_slug = base.clone();
    let value = bad_slug["entries"]["reviewer"].clone();
    bad_slug["entries"] = serde_json::json!({ "Bad Slug": value });
    let cases = [
        ("invalid slug key", bad_slug),
        ("empty vault", with(&base, "source_vault", "")),
        ("empty path", with(&base, "source_path", "")),
        ("short hash", with(&base, "source_hash", "abc")),
        ("non-hex projected hash", with(&base, "projected_hash", &"z".repeat(64))),
        ("empty configuration", with(&base, "configuration", "")),
    ];
    for (name, document) in cases {
        rewrite(&fixture, &document)?;
        assert!(
            malformed(&OwnershipManifest::read(&fixture.location, &fixture.target)),
            "{name}"
        );
    }
    Ok(())
}

fn with(base: &serde_json::Value, field: &str, value: &str) -> serde_json::Value {
    let mut document = base.clone();
    document["entries"]["reviewer"][field] = serde_json::json!(value);
    document
}

#[test]
fn malformed_errors_carry_the_stable_code_and_no_manifest_content() -> TestResult {
    let fixture = fixture()?;
    let path: &std::path::Path = (&fixture.location).into();
    fs::create_dir_all(path.parent().ok_or("no parent")?)?;
    fs::write(path, "{\"version\":1,\"hunter2\":\"secret-token\"}")?;
    let error = OwnershipManifest::read(&fixture.location, &fixture.target)
        .err()
        .ok_or("expected an error")?;
    assert_eq!(error.code(), "projection/manifest-malformed");
    assert!(!error.to_string().contains("secret-token"), "{error}");
    assert!(!error.to_string().contains("hunter2"), "{error}");
    Ok(())
}
