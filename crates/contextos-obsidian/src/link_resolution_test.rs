use std::collections::BTreeSet;
use std::convert::Infallible;

use super::*;
use crate::VaultFiles;

/// A vault that answers existence and name questions from a fixed set of files, as the real ones do.
struct Vault(BTreeSet<String>);

impl Vault {
    fn of(paths: &[&str]) -> Self {
        Self(paths.iter().map(|path| (*path).to_owned()).collect())
    }
}

impl LinkTargets for Vault {
    type Error = Infallible;

    fn is_file(&self, path: &str) -> Result<bool, Self::Error> {
        Ok(self.0.contains(path))
    }

    fn files_ending_with(&self, suffix: &str) -> Result<Vec<String>, Self::Error> {
        let suffix = suffix.to_lowercase();
        Ok(self
            .0
            .iter()
            .filter(|path| {
                let path = path.to_lowercase();
                path == suffix || path.ends_with(&format!("/{suffix}"))
            })
            .cloned()
            .collect())
    }
}

const DECISION_BASES: [&str; 3] = [
    "memory/decisions.base",
    "work/offers/alpha/decisions.base",
    "work/offers/beta/decisions.base",
];

#[test]
fn a_bare_name_resolves_to_the_nearest_ancestor_that_holds_it() {
    let vault = Vault::of(&DECISION_BASES);

    let resolution = resolve_link(&vault, "work/offers/alpha/decisions/d-001.md", "decisions.base");

    assert_eq!(
        resolution,
        Ok(LinkResolution::Found("work/offers/alpha/decisions.base".to_owned()))
    );
}

#[test]
fn a_bare_name_no_ancestor_holds_is_ambiguous_when_several_files_share_it() {
    let vault = Vault::of(&DECISION_BASES);

    let resolution = resolve_link(&vault, "elsewhere/note.md", "decisions.base");

    assert_eq!(
        resolution,
        Ok(LinkResolution::Ambiguous {
            candidates: DECISION_BASES.map(str::to_owned).to_vec()
        })
    );
}

#[test]
fn a_bare_name_no_ancestor_holds_is_found_when_exactly_one_file_has_it() {
    let vault = Vault::of(&["work/offers/alpha/registry.base"]);

    let resolution = resolve_link(&vault, "elsewhere/note.md", "registry.base");

    assert_eq!(
        resolution,
        Ok(LinkResolution::Found("work/offers/alpha/registry.base".to_owned()))
    );
}

#[test]
fn a_rooted_target_is_exact_and_never_looked_up_by_name() {
    let vault = Vault::of(&DECISION_BASES);

    assert_eq!(
        resolve_link(&vault, "work/offers/alpha/d.md", "/memory/decisions.base"),
        Ok(LinkResolution::Found("memory/decisions.base".to_owned()))
    );
    assert_eq!(
        resolve_link(&vault, "work/offers/alpha/d.md", "/decisions.base"),
        Ok(LinkResolution::NotFound)
    );
}

#[test]
fn a_target_no_file_answers_to_is_not_found() {
    let vault = Vault::of(&DECISION_BASES);

    assert_eq!(
        resolve_link(&vault, "work/note.md", "missing.base"),
        Ok(LinkResolution::NotFound)
    );
}

#[test]
fn a_folder_link_resolves_to_the_index_note_inside_it() {
    let vault = Vault::of(&["memory/raw/2026/index.md", "memory/raw/2026/a.md", "other/index.md"]);

    assert_eq!(
        resolve_link(&vault, "memory/notes/n.md", "../raw/2026/"),
        Ok(LinkResolution::Found("memory/raw/2026/index.md".to_owned()))
    );
    assert_eq!(
        resolve_link(&vault, "memory/notes/n.md", "/memory/raw/2026/"),
        Ok(LinkResolution::Found("memory/raw/2026/index.md".to_owned()))
    );
    assert_eq!(
        resolve_link(&vault, "memory/raw/n.md", "2026/"),
        Ok(LinkResolution::Found("memory/raw/2026/index.md".to_owned()))
    );
}

#[test]
fn a_folder_link_to_a_folder_without_an_index_note_is_not_found() {
    let vault = Vault::of(&["memory/raw/2026/a.md", "memory/raw.md"]);

    assert_eq!(
        resolve_link(&vault, "memory/n.md", "raw/2026/"),
        Ok(LinkResolution::NotFound)
    );
    assert_eq!(
        resolve_link(&vault, "memory/n.md", "raw/"),
        Ok(LinkResolution::NotFound)
    );
}

#[test]
fn a_target_without_a_trailing_slash_never_names_a_folder() {
    let vault = Vault::of(&["memory/raw/2026/index.md"]);

    assert_eq!(
        resolve_link(&vault, "memory/n.md", "raw/2026"),
        Ok(LinkResolution::NotFound)
    );
}

#[test]
fn the_resolver_answers_from_a_vault_file_set_exactly_as_from_any_other_vault() {
    let vault: VaultFiles = ["a/note.md", "a/b/photo.png", "c/photo.png", "a/b/board.canvas"]
        .into_iter()
        .map(str::to_owned)
        .collect();

    assert_eq!(
        resolve_link(&vault, "a/b/n.md", "photo.png"),
        Ok(LinkResolution::Found("a/b/photo.png".to_owned()))
    );
    assert_eq!(
        resolve_link(&vault, "z/n.md", "photo.png"),
        Ok(LinkResolution::Ambiguous {
            candidates: vec!["a/b/photo.png".to_owned(), "c/photo.png".to_owned()]
        })
    );
    assert_eq!(
        resolve_link(&vault, "z/n.md", "board.canvas"),
        Ok(LinkResolution::Found("a/b/board.canvas".to_owned()))
    );
}

#[test]
fn a_name_several_files_answer_to_is_settled_by_the_records_folder_of_the_declared_base() {
    let vault = Vault::of(&["people/ann.md", "archive/ann.md", "daily/today.md"]);
    let folder = RecordsFolder {
        path: "people",
        includes_subfolders: true,
    };

    assert_eq!(
        resolve_link_narrowed(&vault, "daily/today.md", "ann", Some(folder)),
        Ok(LinkResolution::Found("people/ann.md".to_owned()))
    );
    assert_eq!(
        resolve_link(&vault, "daily/today.md", "ann"),
        Ok(LinkResolution::Ambiguous {
            candidates: vec!["archive/ann.md".to_owned(), "people/ann.md".to_owned()]
        })
    );
}
