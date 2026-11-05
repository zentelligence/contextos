use proptest::prelude::*;

use super::*;
use crate::name::ReleaseVersion;
use crate::test_support::{Must, MustFail};

fn file(raw: &str) -> Result<ReleaseFilePath, PackagesError> {
    ReleaseFilePath::try_from(raw)
}

fn key() -> ReleaseKey {
    ReleaseKey {
        name: ReleaseName::try_from("report").must("name"),
        version: ReleaseVersion::try_from("1.2.3").must("version"),
    }
}

fn id(raw: &str) -> OperationId {
    OperationId::try_from(raw).must("id")
}

#[test]
fn a_listed_path_of_plain_segments_is_accepted_and_written_with_slashes() {
    for raw in [
        "a",
        "a/b/c.rhai",
        "templates/index.html",
        "dir.with.dots/x",
        "a-b_c/é.txt",
        "sub/manifest.toml",
    ] {
        let path = file(raw).must(raw);
        assert_eq!(path.to_string(), raw);
        assert_eq!(path.segments().join("/"), raw);
    }
}

#[test]
fn a_listed_path_that_could_escape_or_confuse_a_platform_is_refused() {
    for raw in [
        "",
        "/a",
        "a/",
        "a//b",
        ".",
        "..",
        "a/./b",
        "a/../b",
        "../a",
        "a\\b",
        "c:/a",
        "a:b",
        "a/b:stream",
        "a.",
        "a ",
        "a/b.",
        "a/b ",
        "a\tb",
        "a\nb",
        "a\0b",
        "manifest.toml",
    ] {
        let error = file(raw).must_fail(raw);
        assert_eq!(error.code(), "packages/invalid-path", "{raw:?}");
    }
}

#[test]
fn listed_paths_order_segment_by_segment_not_by_text() {
    let mut paths = ["a-b", "a/b", "a.txt", "a", "z"]
        .map(|raw| file(raw).must(raw))
        .to_vec();
    paths.sort();
    let sorted: Vec<String> = paths.iter().map(ToString::to_string).collect();
    assert_eq!(sorted, ["a", "a/b", "a-b", "a.txt", "z"]);
}

#[test]
fn store_paths_are_built_from_validated_parts() {
    let name = ReleaseName::try_from("report").must("name");
    assert_eq!(StorePath::root().segments().len(), 0);
    assert_eq!(StorePath::from(&name).to_string(), "report");
    let release = StorePath::from(&key());
    assert_eq!(release.to_string(), "report/1.2.3");
    assert_eq!(release.manifest().to_string(), "report/1.2.3/manifest.toml");
    assert_eq!(
        release.within(&file("lib/x.rhai").must("path")).to_string(),
        "report/1.2.3/lib/x.rhai"
    );
    assert_eq!(StorePath::staging_folder(&id("ab1")).to_string(), "_staging-ab1");
    assert_eq!(StorePath::removing_folder(&id("ab1")).to_string(), "_removing-ab1");
    assert_eq!(StorePath::ledger_file().to_string(), "ledger.toml");
}

#[test]
fn a_store_paths_parent_drops_one_segment_and_the_root_has_none() {
    let release = StorePath::from(&key());
    assert_eq!(release.parent().must("parent").to_string(), "report");
    assert_eq!(release.parent().and_then(|p| p.parent()), Some(StorePath::root()));
    assert_eq!(StorePath::root().parent(), None);
    assert_eq!(StorePath::root().to_string(), "");
}

#[test]
fn a_top_level_entry_is_read_by_its_name() {
    assert_eq!(StoreEntryName::from("ledger.toml"), StoreEntryName::Ledger);
    assert_eq!(StoreEntryName::from("_staging-ab1"), StoreEntryName::Staging(id("ab1")));
    assert_eq!(StoreEntryName::from("_removing-9z"), StoreEntryName::Removing(id("9z")));
    assert!(matches!(StoreEntryName::from("report"), StoreEntryName::Package(name) if name.as_str() == "report"));
}

#[test]
fn the_stores_lock_file_and_its_queue_are_read_as_the_lock() {
    for raw in [".lock", ".lock.queue", ".lock.queue.lock"] {
        assert_eq!(StoreEntryName::from(raw), StoreEntryName::Lock);
    }
}

#[test]
fn an_entry_that_reads_as_nothing_the_engine_owns_is_other() {
    for raw in [
        ".lock.other",
        "Report",
        "_staging-",
        "_staging-UP",
        "_removing-a-b",
        "_other",
        "ledger.toml.bak",
        "a b",
        "",
    ] {
        assert_eq!(
            StoreEntryName::from(raw),
            StoreEntryName::Other(raw.to_owned()),
            "{raw:?}"
        );
    }
}

proptest! {
    #[test]
    fn an_accepted_listed_path_has_only_plain_segments(raw in "\\PC{0,30}") {
        if let Ok(path) = file(&raw) {
            prop_assert!(!path.segments().is_empty());
            for segment in path.segments() {
                prop_assert!(!segment.is_empty() && segment != "." && segment != "..");
                prop_assert!(!segment.contains(['\\', ':', '/']) && !segment.chars().any(char::is_control));
                prop_assert!(!segment.ends_with('.') && !segment.ends_with(' '));
            }
            prop_assert_eq!(path.to_string(), raw);
        }
    }

    #[test]
    fn an_entry_name_is_never_both_a_package_and_an_engine_folder(raw in "[ -~]{0,20}") {
        match StoreEntryName::from(raw.as_str()) {
            StoreEntryName::Package(name) => prop_assert!(!name.as_str().starts_with('_')),
            StoreEntryName::Staging(_) => prop_assert!(raw.starts_with(STAGING_PREFIX)),
            StoreEntryName::Removing(_) => prop_assert!(raw.starts_with(REMOVING_PREFIX)),
            StoreEntryName::Ledger => prop_assert_eq!(raw, LEDGER_FILE_NAME),
            StoreEntryName::Lock => prop_assert!(raw.starts_with(LOCK_FILE_NAME)),
            StoreEntryName::Other(other) => prop_assert_eq!(other, raw),
        }
    }
}
