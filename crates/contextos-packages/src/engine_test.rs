use proptest::prelude::*;
use sha2::{Digest, Sha256};

use super::*;
use crate::path::ReleaseFilePath;
use crate::test_support::Must;

fn files(entries: &[(&str, &[u8])]) -> ReleaseFiles {
    ReleaseFiles(
        entries
            .iter()
            .map(|(path, bytes)| (ReleaseFilePath::try_from(*path).must("path"), bytes.to_vec()))
            .collect(),
    )
}

/// The digest of `files` computed by the algorithm every release installed before this crate existed carries, written
/// out here independently: each file in component-by-component path order, its `/`-separated path, a zero byte, its
/// bytes, and a zero byte, hashed as one SHA-256.
fn independent_digest(files: &ReleaseFiles) -> String {
    let mut ordered: Vec<(Vec<&str>, &Vec<u8>)> = files
        .0
        .iter()
        .map(|(path, bytes)| (path.segments().iter().map(String::as_str).collect(), bytes))
        .collect();
    ordered.sort();
    let mut hasher = Sha256::new();
    for (segments, bytes) in ordered {
        hasher.update(segments.join("/").as_bytes());
        hasher.update([0u8]);
        hasher.update(bytes);
        hasher.update([0u8]);
    }
    let hash = hasher.finalize();
    hash.iter()
        .fold(String::from("sha256:"), |written, byte| format!("{written}{byte:02x}"))
}

#[test]
fn the_digest_equals_the_one_bundles_and_installed_releases_already_carry() {
    // Pinned from the extraction digest this crate replaces, with names chosen so that the component order
    // (`a/b` before `a-b`) differs from plain text order (`a-b` before `a/b`).
    let release = files(&[("z", b""), ("a.txt", b"three"), ("a-b", b"two"), ("a/b", b"one")]);
    assert_eq!(
        StoreRules.digest(&release).as_str(),
        "sha256:a65f2f4501ef05543e08ab54155f5711c1e5291d0ccf681f96a840bb837fd196"
    );
}

#[test]
fn a_release_with_no_files_digests_to_the_empty_hash() {
    assert_eq!(
        StoreRules.digest(&ReleaseFiles::default()).as_str(),
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn the_path_and_the_bytes_are_both_part_of_the_digest() {
    let base = StoreRules.digest(&files(&[("a", b"x")]));
    assert_ne!(base, StoreRules.digest(&files(&[("b", b"x")])));
    assert_ne!(base, StoreRules.digest(&files(&[("a", b"y")])));
    assert_ne!(
        StoreRules.digest(&files(&[("a", b"xb\0"), ("b", b"")])),
        StoreRules.digest(&files(&[("a", b"x"), ("b", b"b\0")]))
    );
}

proptest! {
    #[test]
    fn the_digest_matches_the_independent_algorithm_for_any_files(
        entries in proptest::collection::btree_map("[a-c]{1,2}(/[a-c.-]{1,2}){0,2}", proptest::collection::vec(any::<u8>(), 0..20), 0..8)
    ) {
        let mut release = ReleaseFiles::default();
        for (path, bytes) in entries {
            if let Ok(path) = ReleaseFilePath::try_from(path.as_str()) {
                release.0.insert(path, bytes);
            }
        }
        let digest = StoreRules.digest(&release);
        prop_assert_eq!(digest.as_str(), independent_digest(&release));
    }
}
