use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::*;

/// Prefixes of the services whose codes reach a completed write as warnings.
const SERVICE_PREFIXES: [&str; 8] = [
    "git",
    "index",
    "search",
    "projection",
    "log",
    "write",
    "embedding",
    "vector",
];

#[test]
fn failures_and_notices_are_told_apart() {
    for failure in [
        "git/commit",
        "git/stage",
        "git/schedule",
        "index/corrupt",
        "index/storage",
        "index/locked",
        "projection/write-failed",
        "projection/lock-failed",
        "events/write-failed",
    ] {
        assert_eq!(
            WarningClass::try_from(failure).ok(),
            Some(WarningClass::Failure),
            "{failure}"
        );
    }
    for notice in ["projection/hand-edited-replaced", "write/line-endings-converted"] {
        assert_eq!(
            WarningClass::try_from(notice).ok(),
            Some(WarningClass::Notice),
            "{notice}"
        );
    }
}

#[test]
fn an_unclassified_code_is_reported_as_such() {
    assert_eq!(
        WarningClass::try_from("git/brand-new").err().map(|error| error.code()),
        Some("events/unclassified-warning")
    );
}

#[test]
fn the_events_consumers_own_failure_code_is_the_constant_the_table_uses() {
    assert_eq!(WRITE_FAILED_CODE, "events/write-failed");
    assert_eq!(
        WarningClass::try_from(WRITE_FAILED_CODE).ok(),
        Some(WarningClass::Failure)
    );
}

#[test]
fn the_service_of_a_code_is_its_prefix() {
    assert_eq!(service_name("git/commit"), "git");
    assert_eq!(service_name("projection/write-failed"), "projection");
    assert_eq!(service_name("nothing"), "nothing");
}

#[test]
fn no_code_is_classified_twice() {
    let mut seen = BTreeSet::new();
    for (code, _) in CLASSIFIED {
        assert!(seen.insert(*code), "{code} is classified twice");
    }
}

fn source_files(directory: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            source_files(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with("_test.rs"))
        {
            found.push(path);
        }
    }
}

/// Every string literal in the workspace's production sources shaped like a service warning code.
///
/// Warning producers live in crates this one cannot depend on (Git, index, search, projection, the server), and each
/// builds its codes from its own error type, so the one place they can all be seen together is their source. The scan
/// reads every non-test source file, so a code added anywhere in the workspace without a classification fails here.
fn codes_in_workspace_source() -> BTreeSet<String> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let this_table = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("warning_codes.rs");
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(&crates) {
        for entry in entries.flatten() {
            source_files(&entry.path().join("src"), &mut files);
        }
    }
    assert!(
        files.len() > 50,
        "the scan found only {} source files; is the workspace layout intact?",
        files.len()
    );
    let mut codes = BTreeSet::new();
    for file in files.iter().filter(|file| **file != this_table) {
        let Ok(text) = fs::read_to_string(file) else { continue };
        for line in text.lines().filter(|line| !line.trim_start().starts_with("//")) {
            for literal in line.split('"').skip(1).step_by(2) {
                let is_code = literal.split_once('/').is_some_and(|(service, rest)| {
                    SERVICE_PREFIXES.contains(&service)
                        && rest.starts_with(|first: char| first.is_ascii_lowercase())
                        && rest.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                });
                if is_code {
                    codes.insert(literal.to_owned());
                }
            }
        }
    }
    codes
}

#[test]
fn every_warning_code_in_the_workspace_source_is_classified() {
    let unclassified: Vec<String> = codes_in_workspace_source()
        .into_iter()
        .filter(|code| WarningClass::try_from(code.as_str()).is_err())
        .collect();

    assert!(
        unclassified.is_empty(),
        "classify these as a failure or a notice in warning_codes.rs: {unclassified:?}"
    );
}

#[test]
fn no_classified_code_is_left_over_from_one_that_no_longer_exists() {
    let in_source = codes_in_workspace_source();
    let stale: Vec<&str> = CLASSIFIED
        .iter()
        .map(|(code, _)| *code)
        .filter(|code| SERVICE_PREFIXES.contains(&service_name(code)) && !in_source.contains(*code))
        .collect();

    assert!(stale.is_empty(), "remove these from warning_codes.rs: {stale:?}");
}
