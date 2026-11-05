use contextos_core::ContentHash;

use crate::{Candidate, ConfigurationId, FindingKind, ManifestEntry, Rejection};

/// Why the planner is running, which decides what it may leave alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// A write, move, or delete just completed. A hand-edited entry whose source has not changed is left for `doctor`.
    Event,
    /// The server is starting. A hand-edited entry whose source has not changed is left for `doctor`.
    Refresh,
    /// `doctor` is looking: findings and the actions that would be taken are computed, and none is performed.
    Assess,
    /// `doctor_resolve` is repairing: every auto-fixable action is performed.
    Resolve,
}

impl Mode {
    /// Whether the planned action is performed rather than only reported.
    #[must_use]
    pub const fn performs(self) -> bool {
        !matches!(self, Self::Assess)
    }
}

/// The target folder's current state for one slug.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetState {
    /// Nothing is in the target at the slug.
    Absent,
    /// An entry is there, and this is the hash of its bytes (or, for a skill, of its folder).
    Present(ContentHash),
}

/// Everything the planner needs to decide what to do for one slug in one target.
#[derive(Clone, Copy, Debug)]
pub struct PlanInput<'a> {
    /// What each configured vault says about the slug, in configuration order. Empty when no vault is configured.
    pub candidates: &'a [Candidate],
    /// What the target holds.
    pub target: &'a TargetState,
    /// What the ownership manifest records.
    pub entry: Option<&'a ManifestEntry>,
    /// The configuration doing the planning.
    pub configuration: &'a ConfigurationId,
    /// The hash of what the winning definition renders, when the winner is valid and could be rendered.
    pub projected: Option<&'a ContentHash>,
    /// Why the planner is running.
    pub mode: Mode,
}

/// What a write or an adoption records in the ownership manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Recording {
    /// The source vault's name.
    pub vault: String,
    /// The source path, relative to its vault.
    pub source_path: String,
    /// The hash of the source definition.
    pub source_hash: ContentHash,
    /// The hash of what the target holds afterwards.
    pub projected_hash: ContentHash,
}

/// The one change the planner asks for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    /// Write the winning definition's projection and record it.
    Write(Recording),
    /// Record the entry already in the target as ours, without writing.
    Adopt(Recording),
    /// Remove the entry and its record.
    Withdraw,
}

/// A planner finding, before the host and slug it concerns are attached.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanFinding {
    /// What is wrong.
    pub kind: FindingKind,
    /// The source vault concerned.
    pub vault: Option<String>,
    /// For a shadowed definition, the vault whose definition is shadowed.
    pub other_vault: Option<String>,
    /// For an invalid definition, what failed.
    pub rejection: Option<Rejection>,
}

/// The planner's decision for one slug in one target.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Plan {
    /// What to change, if anything.
    pub action: Option<Action>,
    /// What `doctor` should say.
    pub findings: Vec<PlanFinding>,
}

/// The winning definition, as the planner needs to see it.
struct Winner<'a> {
    vault: &'a str,
    source_path: &'a str,
    source_hash: &'a ContentHash,
}

impl From<PlanInput<'_>> for Plan {
    fn from(input: PlanInput<'_>) -> Self {
        let first = input
            .candidates
            .iter()
            .position(|candidate| !matches!(candidate, Candidate::Absent));
        let Some(first) = first else {
            return undefined(&input);
        };
        let mut findings = shadowed(input.candidates, first);
        let action = match &input.candidates[first] {
            Candidate::Absent => None,
            Candidate::Invalid { vault, rejection, .. } => {
                findings.insert(
                    0,
                    PlanFinding {
                        kind: FindingKind::ProjectionInvalidDefinition,
                        vault: Some(vault.clone()),
                        other_vault: None,
                        rejection: Some(*rejection),
                    },
                );
                None
            }
            Candidate::Valid {
                vault,
                source_path,
                source_hash,
            } => input.projected.and_then(|projected| {
                let winner = Winner {
                    vault,
                    source_path,
                    source_hash,
                };
                let (action, mut own) = decide_valid(&input, &winner, projected);
                own.append(&mut findings);
                findings = own;
                action
            }),
        };
        Self { action, findings }
    }
}

/// The plan when no configured vault defines the slug.
///
/// An entry the operator edited by hand is theirs to decide about: events and refreshes keep it, and only `doctor`
/// (assessing, then resolving) removes it. It is reported as both orphaned and hand-edited.
fn undefined(input: &PlanInput<'_>) -> Plan {
    let Some(entry) = input.entry else {
        return Plan::default();
    };
    if entry.configuration() != input.configuration {
        // Another configuration's entry is never changed, whether or not any vault still defines the slug.
        return Plan {
            action: None,
            findings: vec![PlanFinding {
                kind: FindingKind::ProjectionForeignOwner,
                vault: Some(entry.source_vault().to_owned()),
                other_vault: None,
                rejection: None,
            }],
        };
    }
    let hand_edited = matches!(input.target, TargetState::Present(found) if found != entry.projected_hash());
    let finding = |kind| PlanFinding {
        kind,
        vault: Some(entry.source_vault().to_owned()),
        other_vault: None,
        rejection: None,
    };
    let mut findings = vec![finding(FindingKind::ProjectionOrphaned)];
    if hand_edited {
        findings.push(finding(FindingKind::ProjectionHandEdited));
    }
    // With no vault configured nothing is withdrawn behind the operator's back; only `doctor` may clear the
    // entries, so a mistaken or empty configuration cannot wipe the hosts' folders.
    let doctor = matches!(input.mode, Mode::Assess | Mode::Resolve);
    let withdraw = if hand_edited {
        doctor
    } else {
        !input.candidates.is_empty() || doctor
    };
    Plan {
        action: withdraw.then_some(Action::Withdraw),
        findings,
    }
}

/// A finding for every later vault whose definition differs from the winner's. A definition whose bytes could not be
/// taken as a whole is treated as different.
fn shadowed(candidates: &[Candidate], first: usize) -> Vec<PlanFinding> {
    let (winner_vault, winner_hash) = match &candidates[first] {
        Candidate::Valid { vault, source_hash, .. } => (vault, Some(source_hash)),
        Candidate::Invalid { vault, source_hash, .. } => (vault, source_hash.as_ref()),
        Candidate::Absent => return Vec::new(),
    };
    candidates[first + 1..]
        .iter()
        .filter_map(|candidate| {
            let (vault, hash) = match candidate {
                Candidate::Absent => return None,
                Candidate::Valid { vault, source_hash, .. } => (vault, Some(source_hash)),
                Candidate::Invalid { vault, source_hash, .. } => (vault, source_hash.as_ref()),
            };
            (hash.is_none() || hash != winner_hash).then(|| PlanFinding {
                kind: FindingKind::ProjectionShadowed,
                vault: Some(winner_vault.clone()),
                other_vault: Some(vault.clone()),
                rejection: None,
            })
        })
        .collect()
}

fn finding(kind: FindingKind, winner: &Winner<'_>) -> PlanFinding {
    PlanFinding {
        kind,
        vault: Some(winner.vault.to_owned()),
        other_vault: None,
        rejection: None,
    }
}

fn recording(winner: &Winner<'_>, projected: &ContentHash) -> Recording {
    Recording {
        vault: winner.vault.to_owned(),
        source_path: winner.source_path.to_owned(),
        source_hash: winner.source_hash.clone(),
        projected_hash: projected.clone(),
    }
}

/// The plan for a valid winner: ownership first, then whether the target is in step.
fn decide_valid(
    input: &PlanInput<'_>,
    winner: &Winner<'_>,
    projected: &ContentHash,
) -> (Option<Action>, Vec<PlanFinding>) {
    let write = || Some(Action::Write(recording(winner, projected)));
    let adopt = || Some(Action::Adopt(recording(winner, projected)));
    match (input.entry, input.target) {
        (Some(entry), _) if entry.configuration() != input.configuration => {
            (None, vec![finding(FindingKind::ProjectionForeignOwner, winner)])
        }
        (_, TargetState::Absent) => (write(), vec![finding(FindingKind::ProjectionStale, winner)]),
        (None, TargetState::Present(found)) if found == projected => (adopt(), Vec::new()),
        (None, TargetState::Present(_)) => (None, vec![finding(FindingKind::ProjectionUnownedCollision, winner)]),
        (Some(entry), TargetState::Present(found)) => {
            let record_current = entry.source_vault() == winner.vault
                && entry.source_path() == winner.source_path
                && entry.source_hash() == winner.source_hash
                && entry.projected_hash() == projected;
            if found == projected {
                return (if record_current { None } else { adopt() }, Vec::new());
            }
            if found != entry.projected_hash() {
                let source_unchanged =
                    entry.source_vault() == winner.vault && entry.source_hash() == winner.source_hash;
                let hand_edited = vec![finding(FindingKind::ProjectionHandEdited, winner)];
                // A changed render with an unchanged source (a configuration tier change, say) counts as unchanged:
                // the operator's edit is theirs until the vault itself moves on or `doctor_resolve` replaces it.
                return if source_unchanged && matches!(input.mode, Mode::Event | Mode::Refresh) {
                    (None, hand_edited)
                } else {
                    (write(), hand_edited)
                };
            }
            (write(), vec![finding(FindingKind::ProjectionStale, winner)])
        }
    }
}

#[cfg(test)]
#[path = "plan_test.rs"]
mod tests;
