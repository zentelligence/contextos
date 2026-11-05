use proptest::prelude::*;

use super::*;
use crate::test_support::{Must, MustFail};

fn name(raw: &str) -> Result<ReleaseName, PackagesError> {
    ReleaseName::try_from(raw)
}

#[test]
fn a_plain_lower_case_name_is_accepted() {
    for raw in ["a", "my-pkg-1", "0", "a-", &"a".repeat(64)] {
        assert_eq!(
            name(raw).map(|n| n.as_str().to_owned()).ok().as_deref(),
            Some(raw),
            "{raw}"
        );
    }
}

#[test]
fn a_name_that_is_not_one_plain_folder_name_is_refused() {
    let too_long = "a".repeat(65);
    for raw in [
        "",
        &too_long,
        "-a",
        "A",
        "Ab",
        "_staging-x",
        "_a",
        "a/b",
        "a\\b",
        "a.b",
        "a b",
        "a:b",
        "..",
        ".",
        "ünï",
        "a\0",
    ] {
        let error = name(raw).must_fail(raw);
        assert_eq!(error.code(), "packages/invalid-name", "{raw:?}");
    }
}

#[test]
fn a_name_windows_reserves_for_a_device_is_refused() {
    for raw in ["con", "prn", "aux", "nul", "com1", "com9", "lpt1", "lpt9"] {
        assert!(name(raw).is_err(), "{raw}");
    }
    for raw in ["com0", "com10", "lpt0", "console", "nulls"] {
        assert!(name(raw).is_ok(), "{raw}");
    }
}

#[test]
fn an_exact_version_is_accepted_and_written_as_its_folder_name() {
    let version = ReleaseVersion::try_from("1.20.3").must("exact version");
    assert_eq!(version.to_string(), "1.20.3");
    assert_eq!(version.as_semver(), &semver::Version::new(1, 20, 3));
}

#[test]
fn anything_but_an_exact_version_is_refused() {
    for raw in [
        "",
        "1",
        "1.2",
        "1.2.3-rc.1",
        "1.2.3+build",
        "^1.2.3",
        ">=1.0.0",
        "v1.2.3",
        "01.2.3",
        " 1.2.3",
        "1.2.3 ",
        "1.2.x",
    ] {
        let error = ReleaseVersion::try_from(raw).must_fail(raw);
        assert_eq!(error.code(), "packages/invalid-version", "{raw:?}");
    }
}

#[test]
fn a_digest_is_sha256_and_64_lower_case_hex_characters() {
    let good = format!("sha256:{}", "0123456789abcdef".repeat(4));
    assert_eq!(ReleaseDigest::try_from(good.as_str()).must("digest").as_str(), good);
    for raw in [
        String::new(),
        "0123456789abcdef".repeat(4),
        format!("sha256:{}", "0123456789abcdef".repeat(4).to_uppercase()),
        format!("sha256:{}", "a".repeat(63)),
        format!("sha256:{}", "a".repeat(65)),
        format!("sha1:{}", "a".repeat(64)),
        format!("sha256:{}g", "a".repeat(63)),
    ] {
        assert!(ReleaseDigest::try_from(raw.as_str()).is_err(), "{raw}");
    }
}

#[test]
fn an_operation_id_is_one_to_32_lower_case_letters_and_digits() {
    for raw in ["a", "0", "abc123", &"z".repeat(32)] {
        assert_eq!(OperationId::try_from(raw).must(raw).as_str(), raw);
    }
    let too_long = "a".repeat(33);
    for raw in ["", &too_long, "A", "a-b", "a_b", "a/b", "a.b", "a b"] {
        let error = OperationId::try_from(raw).must_fail(raw);
        assert_eq!(error.code(), "packages/invalid-path", "{raw:?}");
    }
}

proptest! {
    #[test]
    fn an_accepted_name_is_one_plain_segment_that_can_never_be_a_staging_or_removal_folder(raw in "\\PC{0,70}") {
        if let Ok(accepted) = name(&raw) {
            let text = accepted.as_str();
            prop_assert!(!text.is_empty() && text.len() <= 64);
            prop_assert!(!text.starts_with('_') && !text.starts_with('-'));
            prop_assert!(text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'));
        }
    }

    #[test]
    fn a_name_of_allowed_characters_is_accepted_unless_it_starts_with_a_hyphen_or_is_a_device(
        raw in "[a-z0-9][a-z0-9-]{0,63}"
    ) {
        let device = matches!(raw.as_str(), "con" | "prn" | "aux" | "nul")
            || ((raw.starts_with("com") || raw.starts_with("lpt")) && raw.len() == 4 && raw.as_bytes()[3] != b'0'
                && raw.as_bytes()[3].is_ascii_digit());
        prop_assert_eq!(name(&raw).is_ok(), !device);
    }

    #[test]
    fn a_version_round_trips_through_its_folder_name(major in 0u64..1000, minor in 0u64..1000, patch in 0u64..1000) {
        let text = format!("{major}.{minor}.{patch}");
        let version = ReleaseVersion::try_from(text.as_str()).must("exact");
        prop_assert_eq!(version.to_string(), text);
    }

    #[test]
    fn an_accepted_version_has_no_pre_release_or_build(raw in "\\PC{0,20}") {
        if let Ok(version) = ReleaseVersion::try_from(raw.as_str()) {
            prop_assert!(version.as_semver().pre.is_empty() && version.as_semver().build.is_empty());
            prop_assert_eq!(version.to_string(), raw);
        }
    }
}

#[test]
fn an_operation_identifier_is_made_from_a_number_in_decimal() {
    assert_eq!(OperationId::from(0u64).as_str(), "0");
    assert_eq!(OperationId::from(u64::MAX).as_str(), "18446744073709551615");
    assert!(OperationId::try_from(OperationId::from(u64::MAX).as_str()).is_ok());
}
