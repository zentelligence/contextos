//! The `Link integrity` doctor check: every link in the vault's notes resolves to exactly one file. It asks the link
//! graph the same question `query_graph`'s `unresolved` operation does, so the two always agree: the graph is brought
//! up to date with the vault first, links resolve by the one Obsidian rule against every file, and notes in the
//! folders `[vault.links] exclude` names are not checked as link sources, nor links whose target lies in the folders
//! `[vault.links] ignore_targets` names, which are counted apart. Never auto-fixable: giving an ambiguous link
//! a path, and correcting or removing a broken one, is the author's decision.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::Arc;

use contextos_core::{VaultRoot, VaultRootId};
use contextos_search::{UnresolvedKind, UnresolvedLink, VaultSearchService};

use super::DoctorCheck;
use crate::VaultConfig;
use crate::doctor_kind::DoctorKind;
use crate::server::{Purpose, open_vault_search};

/// How many broken links the message lists before it counts the rest.
const MAX_REPORTED: usize = 10;

/// Where the check finds the link graph it asks.
#[derive(Clone, Copy)]
pub(crate) enum LinkSources<'a> {
    /// The services a running server already holds, one slot per configured vault.
    Live(&'a [Option<Arc<VaultSearchService>>]),
    /// A service opened for this assessment from the vault's own configuration, for a caller with none.
    Open,
    /// The check is not made, for a caller that must write nothing, derived state included.
    Skip,
}

/// The check for the vault at `index`.
pub(crate) fn link_integrity_check(
    sources: LinkSources<'_>,
    vault: &VaultConfig,
    root: &VaultRoot,
    index: usize,
) -> DoctorCheck {
    let kind = DoctorKind::LinkIntegrity;
    if matches!(sources, LinkSources::Skip) {
        return DoctorCheck::pass(kind, "not checked by this assessment");
    }
    if !vault.search.graph {
        return DoctorCheck::pass(
            kind,
            "not checked: [vault.search] graph is off, so there is no link graph to ask",
        );
    }
    let service = match sources {
        LinkSources::Live(services) => services.get(index).cloned().flatten(),
        LinkSources::Open | LinkSources::Skip => VaultRootId::try_from(index)
            .ok()
            .and_then(|id| open_vault_search(vault, root, id, Purpose::Resolve).ok())
            .map(Arc::new),
    };
    let Some(service) = service else {
        return DoctorCheck::pass(kind, "not checked: the vault's search state could not be opened");
    };
    match service.link_integrity() {
        Ok(report) if report.unresolved.is_empty() => {
            let mut message = format!("{} links checked", report.checked);
            if report.ignored > 0 {
                let _ = write!(
                    message,
                    ", and {} links into the folders [vault.links] ignore_targets names not checked",
                    report.ignored
                );
            }
            DoctorCheck::pass(kind, message)
        }
        Ok(report) => DoctorCheck::fail(
            kind,
            describe(&report.unresolved, report.ignored),
            "List every broken link with query_graph operation \"unresolved\" (it pages through the whole list), \
             give each ambiguous link a path that names one file, correct or remove each broken one, and rewrite \
             any note whose links cannot be read, then rerun the doctor. Notes in the folders [vault.links] exclude \
             names are not checked.",
        ),
        Err(error) => DoctorCheck::pass(kind, format!("not checked: {error} ({})", error.code())),
    }
}

/// The failure message: the counts, then the first links listed as `source:line -> target (kind)`, then how many links
/// into the folders `[vault.links] ignore_targets` names were left unchecked, when any were.
fn describe(unresolved: &[UnresolvedLink], ignored: usize) -> String {
    let count = |wanted: UnresolvedKind| unresolved.iter().filter(|link| link.kind == wanted).count();
    let notes: BTreeSet<&str> = unresolved.iter().map(|link| link.source.as_str()).collect();
    let mut message = format!(
        "{} not-found link(s), {} ambiguous link(s), and {} unreadable note(s), across {} note(s): ",
        count(UnresolvedKind::NotFound),
        count(UnresolvedKind::Ambiguous),
        count(UnresolvedKind::Unreadable),
        notes.len()
    );
    let listed: Vec<String> = unresolved.iter().take(MAX_REPORTED).map(listing).collect();
    message.push_str(&listed.join("; "));
    if unresolved.len() > MAX_REPORTED {
        let _ = write!(message, ", and {} more", unresolved.len() - MAX_REPORTED);
    }
    if ignored > 0 {
        let _ = write!(
            message,
            ". {ignored} link(s) into the folders [vault.links] ignore_targets names were not checked."
        );
    }
    message
}

fn listing(link: &UnresolvedLink) -> String {
    match (link.line, link.target.as_deref()) {
        (Some(number), Some(target)) => format!("{}:{number} -> {target} ({})", link.source, label(link.kind)),
        _ => format!("{} ({}): {}", link.source, label(link.kind), link.text),
    }
}

const fn label(kind: UnresolvedKind) -> &'static str {
    match kind {
        UnresolvedKind::NotFound => "not-found",
        UnresolvedKind::Ambiguous => "ambiguous",
        UnresolvedKind::Unreadable => "unreadable",
    }
}

#[cfg(test)]
#[path = "doctor_links_test.rs"]
mod tests;
