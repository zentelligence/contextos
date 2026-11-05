//! The doctor's event checks: whether each event kind definition in the vault's kind registry can be used, whether any
//! kind is defined twice, and whether every event note has a time to be swept by. All three only report: a
//! definition or an event note is the operator's to correct, never something `doctor_resolve` rewrites.
//!
//! A vault that declares no kind registry and holds no event notes gets none of these checks, so the report of a
//! vault with no events is unchanged.

use contextos_core::{VaultPath, VaultPathInput, VaultRoot, VaultSet};
use contextos_events::Verdict;
use contextos_fs::{EntryKind, Filesystem, FsError, ListDirectoryRequest, ReadTextRequest, SearchFilesRequest};
use contextos_obsidian::FrontmatterDocument;

use crate::doctor::{DoctorCheck, DoctorError};
use crate::doctor_kind::DoctorKind;
use crate::event_kinds::{self, KindProblem};
use crate::events_folder::{EVENTS_BASE, EVENTS_FOLDER, EVENTS_INDEX, is_markdown};
use crate::events_record;
use crate::tools::base_record::read_sidecar;
use crate::tools::base_sidecar;
use crate::tools::fs::child_path;

/// The most file names a finding spells out; the count says how many there are in all.
const NAMED_FILES: usize = 10;

/// The event checks for one vault, empty when the vault has neither a kind registry nor event notes.
pub(crate) fn event_checks(
    root: &VaultRoot,
    roots: &VaultSet,
    filesystem: &Filesystem,
) -> Result<Vec<DoctorCheck>, DoctorError> {
    let vault = VaultPath::try_from(VaultPathInput {
        roots,
        raw: &format!("{}://.", root.name()),
    })?;
    let mut checks = Vec::new();
    match child_path(&vault, roots, EVENTS_BASE) {
        Ok(base) => checks.extend(kind_registry_checks(filesystem, roots, &vault, &base)),
        Err(error) => checks.push(DoctorCheck::fail(
            DoctorKind::EventKindInvalid,
            format!(
                "{EVENTS_BASE} cannot be addressed ({}), so its event kind registry was not read",
                crate::tool_error::ToolFailure::from(error).code
            ),
            "Make memory/events.base an ordinary file inside the vault, not a link out of it, and rerun the doctor.",
        )),
    }
    if let Some(unreadable) = unreadable_events(filesystem, roots, &vault) {
        checks.push(unreadable_check(&unreadable));
    }
    Ok(checks)
}

/// The checks of the vault's kind registry: whether each definition is usable, whether any kind is defined twice,
/// whether every definition was read, and whether any other base claims a registry. Empty when the vault's events
/// base names none and no other base does.
fn kind_registry_checks(
    filesystem: &Filesystem,
    roots: &VaultSet,
    vault: &VaultPath,
    base: &VaultPath,
) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    let misplaced = misplaced_registries(filesystem, roots, vault);
    if let Ok(Some(folder)) = events_record::kind_registry_folder(filesystem, roots, base) {
        let series = base_sidecar::sidecar_path(roots, base)
            .and_then(|sidecar| read_sidecar(filesystem, &sidecar))
            .map(|state| events_record::sidecar_series(&state))
            .unwrap_or_default();
        let loaded = event_kinds::load(filesystem, roots, vault, &folder, &series);
        let (duplicates, invalid): (Vec<_>, Vec<_>) = loaded
            .unusable
            .iter()
            .partition(|unusable| matches!(unusable.problem, KindProblem::Duplicate { .. }));
        checks.push(finding(
            DoctorKind::EventKindInvalid,
            &invalid,
            &format!("{} event kinds declared in {folder}", loaded.registry.len()),
            "Correct the definition the message names so it follows templates/registry-event-kind.md; until then that \
             kind cannot be recorded.",
        ));
        checks.push(finding(
            DoctorKind::EventKindDuplicate,
            &duplicates,
            "no event kind is defined twice",
            "Keep one definition of each kind and delete the other; until then neither can be recorded.",
        ));
        checks.push(if loaded.truncated {
            DoctorCheck::fail(
                DoctorKind::EventKindTruncated,
                format!("{folder} holds more event kind definitions, or goes deeper, than are read, so some kinds cannot be recorded"),
                "Move definitions out of the folder, or flatten it, until the registry is within the limits the server reads.",
            )
        } else {
            DoctorCheck::pass(DoctorKind::EventKindTruncated, "every event kind definition was read")
        });
        checks.push(misplaced_check(&misplaced));
    } else if !misplaced.is_empty() {
        checks.push(misplaced_check(&misplaced));
    }
    checks
}

fn misplaced_check(misplaced: &[String]) -> DoctorCheck {
    if misplaced.is_empty() {
        return DoctorCheck::pass(
            DoctorKind::EventKindMisplaced,
            "no base other than memory/events.base declares an event kind registry",
        );
    }
    DoctorCheck::fail(
        DoctorKind::EventKindMisplaced,
        format!(
            "{} declare(s) event_kinds, which only {EVENTS_BASE}'s sidecar may; it has no effect there",
            misplaced.join(", ")
        ),
        "Remove event_kinds from each sidecar the message names, and declare the registry on memory/events.md.",
    )
}

/// The sidecars, other than the events base's own, that declare `event_kinds`, by vault-relative path.
fn misplaced_registries(filesystem: &Filesystem, roots: &VaultSet, vault: &VaultPath) -> Vec<String> {
    let Ok(bases) = filesystem.search_files(&SearchFilesRequest {
        path: vault.clone(),
        pattern: "**/*.base".to_owned(),
        exclude_patterns: Vec::new(),
        max_results: usize::MAX,
    }) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for base in bases.into_iter().filter(|base| base != EVENTS_BASE) {
        let Ok(base_path) = child_path(vault, roots, &base) else {
            continue;
        };
        let Ok(Some(sidecar)) = base_sidecar::sidecar_path(roots, &base_path).and_then(|path| {
            let relative = path.portable_relative();
            Ok(contextos_core::ReadsVault::read_optional_text(filesystem, &path)?.map(|file| (relative, file.content)))
        }) else {
            continue;
        };
        let declares = FrontmatterDocument::try_from(sidecar.1.as_str()).is_ok_and(|document| {
            document
                .frontmatter()
                .get("event_kinds")
                .is_some_and(|value| !value.is_null())
        });
        if declares {
            found.push(sidecar.0);
        }
    }
    found.sort();
    found
}

/// A pass naming how many things are fine, or a failure naming the files that are not.
fn finding(kind: DoctorKind, problems: &[&event_kinds::UnusableKind], passing: &str, action: &str) -> DoctorCheck {
    if problems.is_empty() {
        return DoctorCheck::pass(kind, passing);
    }
    let named = problems
        .iter()
        .take(NAMED_FILES)
        .map(|unusable| format!("{} ({})", unusable.path, unusable.problem.describe()))
        .collect::<Vec<_>>()
        .join("; ");
    let more = problems.len().saturating_sub(NAMED_FILES);
    let tail = if more > 0 {
        format!("; and {more} more")
    } else {
        String::new()
    };
    DoctorCheck::fail(
        kind,
        format!("{} definition(s) cannot be used: {named}{tail}", problems.len()),
        action,
    )
}

fn unreadable_check(unreadable: &[String]) -> DoctorCheck {
    if unreadable.is_empty() {
        return DoctorCheck::pass(
            DoctorKind::EventUnreadable,
            "every note under the events folder is an event with a readable time",
        );
    }
    let named = unreadable
        .iter()
        .take(NAMED_FILES)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let more = unreadable.len().saturating_sub(NAMED_FILES);
    let tail = if more > 0 {
        format!(", and {more} more")
    } else {
        String::new()
    };
    DoctorCheck::fail(
        DoctorKind::EventUnreadable,
        format!(
            "{} note(s) under the events folder are not events with a readable `at` (`type: event`, `at` in RFC 3339) and are never swept or deleted: {named}{tail}",
            unreadable.len()
        ),
        "Give each event an `at` in RFC 3339, move any note that is not an event out of the events folder, or delete it; the retention sweep keeps it until then.",
    )
}

/// The notes that are not events with a readable `at`, or `None` when the vault has no events folder.
fn unreadable_events(filesystem: &Filesystem, roots: &VaultSet, vault: &VaultPath) -> Option<Vec<String>> {
    let folder = child_path(vault, roots, EVENTS_FOLDER).ok()?;
    filesystem
        .list_directory(&ListDirectoryRequest { path: folder.clone() })
        .ok()?;
    let mut found = Vec::new();
    walk(filesystem, roots, vault, EVENTS_FOLDER, &mut |relative, text| {
        // Only a note that says it is an event, with a time the sweep can read, is one the sweep judges.
        let readable = FrontmatterDocument::try_from(text).is_ok_and(|document| {
            document.frontmatter().get("type").and_then(serde_json::Value::as_str) == Some("event")
                && Verdict::time_of(document.frontmatter()).is_some()
        });
        if !readable {
            found.push(relative.to_owned());
        }
    });
    found.sort();
    Some(found)
}

/// Calls `visit` with the path and text of every Markdown note in `relative` and below, but the folder's own index.
fn walk(
    filesystem: &Filesystem,
    roots: &VaultSet,
    vault: &VaultPath,
    relative: &str,
    visit: &mut dyn FnMut(&str, &str),
) {
    let Ok(path) = child_path(vault, roots, relative) else {
        return;
    };
    let Ok(listing) = filesystem.list_directory(&ListDirectoryRequest { path }) else {
        return;
    };
    for entry in listing.entries {
        let child = format!("{relative}/{}", entry.name);
        match entry.kind {
            EntryKind::Directory => walk(filesystem, roots, vault, &child, visit),
            EntryKind::File if child != EVENTS_INDEX && is_markdown(&entry.name) => {
                // A path that does not come back as the entry asked for is a link: it is reported as having no
                // readable time, and never read through, as the sweep never follows one either.
                let note = match child_path(vault, roots, &child) {
                    Ok(note) if note.portable_relative() == child => note,
                    Ok(_) | Err(_) => {
                        visit(&child, "");
                        continue;
                    }
                };
                match filesystem.read_text(&ReadTextRequest {
                    path: note,
                    limit: None,
                }) {
                    Ok(file) => visit(&child, &file.content),
                    Err(FsError::Binary { .. }) => visit(&child, ""),
                    Err(_) => {}
                }
            }
            EntryKind::File => {}
        }
    }
}
