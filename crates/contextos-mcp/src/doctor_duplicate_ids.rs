//! The `Duplicate id detection` doctor check: two or more record notes sharing an `id` among the records of one base.
//! An id is only unique per base (`base_record_create` refuses a repeat among the base's records at write time), so
//! this reports the same collisions after the fact for a hand-authored or otherwise out-of-band note. Read-only, and
//! never auto-fixable: which of two colliding notes keeps the id, and what the other's corrected id should be, is an
//! operator judgement call.

use std::collections::{BTreeMap, BTreeSet};

use contextos_core::{VaultRoot, VaultSet};
use contextos_fs::Filesystem;

use super::{DoctorCheck, DoctorError};
use crate::doctor_bases::VaultBases;
use crate::doctor_kind::DoctorKind;

const MAX_REPORTED: usize = 10;

pub(super) fn duplicate_id_check(
    root: &VaultRoot,
    roots: &VaultSet,
    filesystem: &Filesystem,
) -> Result<DoctorCheck, DoctorError> {
    let bases = VaultBases::load(root, roots, filesystem)?;
    let mut checked = BTreeSet::new();
    let mut duplicates: Vec<(String, String, Vec<&str>)> = Vec::new();
    for base in bases.iter() {
        let mut by_id: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for record in base.records() {
            if let Some(id) = record.id.as_deref() {
                checked.insert(record.path.as_str());
                by_id.entry(id).or_default().push(record.path.as_str());
            }
        }
        duplicates.extend(
            by_id
                .into_iter()
                .filter(|(_, paths)| paths.len() > 1)
                .map(|(id, paths)| (base.path.clone(), id.to_owned(), paths)),
        );
    }
    duplicates.sort();
    let unassessed: Vec<String> = bases
        .iter()
        .filter_map(|base| {
            base.unreadable_reason()
                .map(|reason| format!("{}: {reason}", base.path))
        })
        .collect();

    if duplicates.is_empty() && !unassessed.is_empty() {
        return Ok(DoctorCheck::fail(
            DoctorKind::DuplicateIds,
            format!(
                "{} base(s) could not be checked for duplicate ids: {}",
                unassessed.len(),
                unassessed.join("; ")
            ),
            "Correct each base's definition so its filters can be read, then rerun the doctor.",
        ));
    }
    if duplicates.is_empty() {
        return Ok(DoctorCheck::pass(
            DoctorKind::DuplicateIds,
            format!(
                "{} records checked across {} bases, no duplicate ids",
                checked.len(),
                bases.iter().count()
            ),
        ));
    }
    let mut listed = duplicates
        .iter()
        .take(MAX_REPORTED)
        .map(|(base, id, paths)| format!("{id} in {base}: {}", paths.join(", ")))
        .collect::<Vec<_>>()
        .join("; ");
    if duplicates.len() > MAX_REPORTED {
        use std::fmt::Write as _;
        let _ = write!(listed, ", and {} more", duplicates.len() - MAX_REPORTED);
    }
    if !unassessed.is_empty() {
        use std::fmt::Write as _;
        let _ = write!(
            listed,
            "; {} base(s) could not be checked: {}",
            unassessed.len(),
            unassessed.join("; ")
        );
    }
    Ok(DoctorCheck::fail(
        DoctorKind::DuplicateIds,
        format!(
            "{} id(s) are claimed by more than one file within the same base: {listed}",
            duplicates.len()
        ),
        "Decide which file keeps each duplicated id and assign the other(s) a fresh, unused id; \
         then rerun the doctor.",
    ))
}

#[cfg(test)]
#[path = "doctor_duplicate_ids_test.rs"]
mod tests;
