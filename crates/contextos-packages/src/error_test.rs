use super::*;
use crate::io::StoreFailureKind;
use crate::name::ReleaseName;
use crate::name::ReleaseVersion;
use crate::test_support::Must;

fn key() -> ReleaseKey {
    ReleaseKey {
        name: ReleaseName::try_from("report").must("name"),
        version: ReleaseVersion::try_from("1.2.3").must("version"),
    }
}

fn digest(fill: char) -> ReleaseDigest {
    ReleaseDigest::try_from(format!("sha256:{}", fill.to_string().repeat(64)).as_str()).must("digest")
}

#[test]
fn every_error_has_its_documented_code() {
    let failure = StoreFailure {
        kind: StoreFailureKind::Failed,
        detail: "report/1.2.3/a.rhai".to_owned(),
    };
    let cases: Vec<(PackagesError, &str)> = vec![
        (PackagesError::LedgerMissing, "packages/ledger-unreadable"),
        (
            PackagesError::LedgerUnreadable { reason: "x".to_owned() },
            "packages/ledger-unreadable",
        ),
        (PackagesError::LedgerPresent, "packages/ledger-present"),
        (
            PackagesError::DuplicateIdentity {
                release: key(),
                recorded: digest('a'),
                offered: digest('b'),
            },
            "packages/duplicate-identity",
        ),
        (PackagesError::NotFound { release: key() }, "packages/not-found"),
        (
            PackagesError::InvalidName {
                raw: "X".to_owned(),
                reason: "r",
            },
            "packages/invalid-name",
        ),
        (
            PackagesError::InvalidVersion {
                raw: "1".to_owned(),
                reason: "r".to_owned(),
            },
            "packages/invalid-version",
        ),
        (
            PackagesError::InvalidStorePath {
                raw: "..".to_owned(),
                reason: "r",
            },
            "packages/invalid-path",
        ),
        (
            PackagesError::LimitExceeded {
                limits: ReadLimits {
                    max_entries: 1,
                    max_bytes: 2,
                },
            },
            "packages/folder-too-large",
        ),
        (PackagesError::StoreLocked, "packages/store-locked"),
        (
            PackagesError::ReleaseIncomplete { release: key() },
            "packages/release-incomplete",
        ),
        (PackagesError::Store(failure), "packages/store-unavailable"),
    ];
    for (error, code) in cases {
        assert_eq!(error.code(), code, "{error}");
    }
}

#[test]
fn a_manifest_rejection_keeps_the_manifest_models_own_code() {
    for code in [
        "packages/invalid-manifest",
        "packages/invalid-name",
        "packages/invalid-version",
    ] {
        let error = PackagesError::InvalidManifest(ManifestRejection {
            code,
            message: "m".to_owned(),
        });
        assert_eq!(error.code(), code);
        assert_eq!(error.to_string(), "m");
    }
}

#[test]
fn a_store_failure_converts_with_a_busy_lock_as_store_locked() {
    let busy = StoreFailure {
        kind: StoreFailureKind::LockBusy,
        detail: "lock".to_owned(),
    };
    assert!(matches!(PackagesError::from(busy), PackagesError::StoreLocked));
    let other = StoreFailure {
        kind: StoreFailureKind::Unavailable,
        detail: "gone".to_owned(),
    };
    let error = PackagesError::from(other.clone());
    assert!(matches!(&error, PackagesError::Store(failure) if *failure == other));
    assert_eq!(error.code(), "packages/store-unavailable");
}

#[test]
fn messages_name_releases_and_store_relative_paths_only() {
    let error = PackagesError::DuplicateIdentity {
        release: key(),
        recorded: digest('a'),
        offered: digest('b'),
    };
    let message = error.to_string();
    assert!(message.contains("report 1.2.3"), "{message}");
    assert!(message.contains(&format!("sha256:{}", "a".repeat(64))), "{message}");
}
