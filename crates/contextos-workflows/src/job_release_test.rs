use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn an_exact_version_is_accepted() -> TestResult {
    for text in ["1.0.0", "0.0.1", "12.34.56"] {
        let version = JobVersion::try_from(text)?;
        assert_eq!(version.as_semver().to_string(), text);
    }
    Ok(())
}

#[test]
fn a_range_is_refused() {
    for text in [
        "^1.0.0",
        "~1.2.3",
        ">=1.0.0",
        "1.*",
        "1.0.0 - 2.0.0",
        "*",
        "=1.0.0",
        ">1.0.0, <2.0.0",
    ] {
        assert!(
            matches!(JobVersion::try_from(text), Err(JobReleaseError::VersionNotExact { .. })),
            "{text}"
        );
    }
}

#[test]
fn a_version_without_all_three_parts_is_refused() {
    for text in ["", "1", "1.0", "v1.0.0", " 1.0.0", "1.0.0 "] {
        assert!(
            matches!(JobVersion::try_from(text), Err(JobReleaseError::VersionNotExact { .. })),
            "{text:?}"
        );
    }
}

#[test]
fn build_metadata_is_refused() {
    assert!(matches!(
        JobVersion::try_from("1.0.0+build.5"),
        Err(JobReleaseError::VersionBuildMetadata { .. })
    ));
}

#[test]
fn a_pre_release_is_refused_because_no_release_can_carry_one() {
    for text in ["1.0.0-rc.1", "1.0.0-alpha", "2.3.4-0"] {
        assert!(
            matches!(
                JobVersion::try_from(text),
                Err(JobReleaseError::VersionPreRelease { .. })
            ),
            "{text}"
        );
    }
}

#[test]
fn a_refusal_names_the_version_text() -> TestResult {
    let error = JobVersion::try_from("^1.0.0").err().ok_or("a refusal was expected")?;
    assert!(error.to_string().contains("^1.0.0"), "{error}");
    Ok(())
}

#[test]
fn a_predecessor_is_a_job_slug() -> TestResult {
    for text in ["nightly-sync", "nightly-sync-v1-0-0", "a", "0", "job-2"] {
        assert_eq!(PredecessorKey::try_from(text)?.as_str(), text);
    }
    Ok(())
}

#[test]
fn a_predecessor_that_is_not_a_slug_is_refused() {
    for text in ["", "Nightly", "a b", "a/b", "..", "a.b", "a_b", "naïve", "a\\b", "C:"] {
        assert!(
            matches!(
                PredecessorKey::try_from(text),
                Err(JobReleaseError::PredecessorNotSlug { .. })
            ),
            "{text:?}"
        );
    }
}

#[test]
fn a_predecessor_that_is_a_windows_device_name_is_refused() {
    for text in ["con", "prn", "aux", "nul", "com1", "lpt9"] {
        assert!(
            matches!(
                PredecessorKey::try_from(text),
                Err(JobReleaseError::PredecessorReserved { .. })
            ),
            "{text}"
        );
    }
}

#[test]
fn every_refusal_has_a_stable_code() -> TestResult {
    assert_eq!(
        JobVersion::try_from("1").err().ok_or("a refusal was expected")?.code(),
        "validation/artefact-invalid"
    );
    assert_eq!(
        PredecessorKey::try_from("")
            .err()
            .ok_or("a refusal was expected")?
            .code(),
        "validation/artefact-invalid"
    );
    Ok(())
}

#[test]
fn the_release_fields_of_a_job_file_are_read_together() -> Result<(), JobReleaseError> {
    let release = JobFileRelease::try_from(JobFileText {
        version: "2.1.0",
        predecessor: Some("nightly-v1-0-0"),
    })?;

    assert_eq!(release.version, JobVersion::try_from("2.1.0")?);
    assert_eq!(release.predecessor, Some(PredecessorKey::try_from("nightly-v1-0-0")?));
    Ok(())
}

#[test]
fn a_job_file_without_a_predecessor_has_none() -> Result<(), JobReleaseError> {
    let release = JobFileRelease::try_from(JobFileText {
        version: "2.1.0",
        predecessor: None,
    })?;

    assert_eq!(release.predecessor, None);
    Ok(())
}

#[test]
fn the_version_is_checked_before_the_predecessor() -> TestResult {
    let error = JobFileRelease::try_from(JobFileText {
        version: "^1",
        predecessor: Some("Bad Key"),
    })
    .err()
    .ok_or("a refusal was expected")?;

    assert!(matches!(error, JobReleaseError::VersionNotExact { .. }));
    Ok(())
}

#[test]
fn a_version_is_made_from_its_three_numbers_and_never_carries_more() -> Result<(), JobReleaseError> {
    let version = JobVersion::from(JobVersionNumbers {
        major: 2,
        minor: 1,
        patch: 7,
    });

    assert_eq!(version, JobVersion::try_from("2.1.7")?);
    assert!(version.as_semver().pre.is_empty() && version.as_semver().build.is_empty());
    Ok(())
}
