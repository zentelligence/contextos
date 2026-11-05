use super::*;
use crate::io::{StoreFailure, StoreFailureKind};

#[test]
fn each_read_outcome_has_its_classification() {
    let failure = StoreFailure {
        kind: StoreFailureKind::Unavailable,
        detail: "gone".to_owned(),
    };
    let limits = ReadLimits {
        max_entries: 1,
        max_bytes: 1,
    };
    let cases: Vec<(ReadCheck<()>, ReleaseClassification)> = vec![
        (ReadCheck::Removed, ReleaseClassification::Removed),
        (
            ReadCheck::Incomplete(vec![IncompleteFinding::ManifestMissing]),
            ReleaseClassification::Incomplete,
        ),
        (ReadCheck::Damaged(Vec::new()), ReleaseClassification::Damaged),
        (
            ReadCheck::Unverifiable(UnverifiableReason::StoreUnavailable(failure)),
            ReleaseClassification::Unverifiable,
        ),
        (
            ReadCheck::Unverifiable(UnverifiableReason::LimitExceeded(limits)),
            ReleaseClassification::Unverifiable,
        ),
        (
            ReadCheck::Unverifiable(UnverifiableReason::ChangedWhileRead),
            ReleaseClassification::Unverifiable,
        ),
    ];
    for (check, expected) in cases {
        assert_eq!(ReleaseClassification::from(&check), expected);
    }
}

fn path(raw: &str) -> ReleaseFilePath {
    ReleaseFilePath::try_from(raw).unwrap_or_else(|error| crate::test_support::fail(error))
}

#[test]
fn an_incomplete_finding_says_what_is_absent_or_unexpected_in_store_relative_terms() {
    assert_eq!(
        IncompleteFinding::ManifestMissing.to_string(),
        "manifest.toml is missing"
    );
    assert_eq!(
        IncompleteFinding::ListedFileAbsent(path("lib/a.rhai")).to_string(),
        "lib/a.rhai is listed but absent"
    );
    assert_eq!(
        IncompleteFinding::UnlistedEntry("extra.txt".to_owned()).to_string(),
        "extra.txt is present but not listed"
    );
}

#[test]
fn a_damage_finding_says_what_is_wrong_without_a_host_path() {
    let key = |name: &str, version: &str| crate::test_support::key(name, version);
    let cases = [
        (
            DamageFinding::ManifestInvalid(ManifestRejection {
                code: "packages/invalid-manifest",
                message: "missing field `name`".to_owned(),
            }),
            "manifest.toml is invalid: missing field `name`",
        ),
        (
            DamageFinding::ManifestForAnotherRelease {
                folder: key("acme", "1.0.0"),
                manifest: key("other", "2.0.0"),
            },
            "manifest.toml names other 2.0.0, not acme 1.0.0",
        ),
        (
            DamageFinding::FileHashMismatch(path("sync.rhai")),
            "sync.rhai does not hash to what the manifest lists",
        ),
        (
            DamageFinding::DigestMismatch {
                declared: crate::test_support::digest('a'),
                computed: crate::test_support::digest('b'),
            },
            "the files do not hash to the digest the manifest declares",
        ),
        (
            DamageFinding::LinkInRelease("lib/link".to_owned()),
            "lib/link is a link, which a release never holds",
        ),
        (
            DamageFinding::NotARegularFile("pipe".to_owned()),
            "pipe is neither a file nor a folder",
        ),
    ];
    for (finding, expected) in cases {
        assert_eq!(finding.to_string(), expected);
    }
}

#[test]
fn an_unverifiable_reason_says_why_the_release_could_not_be_checked() {
    let failure = StoreFailure {
        kind: StoreFailureKind::Unavailable,
        detail: "acme/1.0.0: store offline".to_owned(),
    };
    assert_eq!(
        UnverifiableReason::ChangedWhileRead.to_string(),
        "the release changed while it was read"
    );
    assert_eq!(
        UnverifiableReason::StoreUnavailable(failure).to_string(),
        "the store could not be read: acme/1.0.0: store offline"
    );
    assert_eq!(
        UnverifiableReason::LimitExceeded(ReadLimits {
            max_entries: 10,
            max_bytes: 2048
        })
        .to_string(),
        "the release holds more than 10 entries or 2048 bytes"
    );
}
