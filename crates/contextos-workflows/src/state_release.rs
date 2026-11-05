//! The rules a job state applies about releases: whether a release is current, previous, or future, a job's identity, the link between a successor and
//! its predecessor, and the enabling that moves `current` and `previous` and retires a predecessor.
//!
//! A child of [`crate::state`], so it reaches a job's stored fields directly. Every lifecycle change it makes still
//! passes the transition table.

use semver::Version;

use std::collections::BTreeSet;

use thiserror::Error;

use super::PackageJobsState;
use crate::job_release::PredecessorKey;
use crate::lifecycle::JobLifecycle;
use crate::release_state::{IdentityIssuer, JobIdentity, JobLink, ReleaseKind, ReleaseKinds, ReleaseStateError};

/// Why a successor's link to its predecessor is refused. Each is a failure of the successor's `predecessor` key.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum LinkRefusal {
    /// The successor names itself.
    #[error("job {job} names itself as its predecessor")]
    PredecessorIsItself { job: String },
    /// The workflow has no job of that key, and no link was recorded while there was one.
    #[error("the predecessor {predecessor} is not a job of this workflow")]
    PredecessorMissing { predecessor: String },
    /// The predecessor has not been through activation: only an `enabled`, `suspended`, or `retired` job is succeeded.
    #[error(
        "the predecessor {predecessor} is {lifecycle}; only an enabled, suspended, or retired job can be succeeded"
    )]
    PredecessorStage {
        predecessor: String,
        lifecycle: JobLifecycle,
    },
    /// The predecessor was retired without ever being enabled, which ends its chain.
    #[error(
        "the predecessor {predecessor} was retired but never enabled, so it cannot be succeeded; name the job before it instead"
    )]
    PredecessorNeverEnabled { predecessor: String },
    /// The predecessor's entry holds no identity, so a link could not tell it from a job that later takes its key.
    #[error("the predecessor {predecessor} has no recorded identity")]
    PredecessorUnidentified { predecessor: String },
    /// Another job that has not been retired already succeeds the predecessor.
    #[error("the predecessor {predecessor} already has the successor {successor}")]
    AnotherSuccessor { predecessor: String, successor: String },
    /// The file names a key no job holds, while the job the link recorded still exists under another key.
    #[error(
        "the predecessor {predecessor} is not a job of this workflow, and the job this one succeeded is still {recorded}"
    )]
    PredecessorRenamed { predecessor: String, recorded: String },
    /// The predecessor's key is now held by a job of another identity than the one the link recorded.
    #[error("the predecessor {predecessor} is now a job of another identity than the one this job succeeded")]
    IdentityChanged {
        predecessor: String,
        recorded: JobIdentity,
        found: Option<JobIdentity>,
    },
}

/// The other job of a chain that keeps a job from running live by hand.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChainHold {
    /// The key of the other job.
    pub job: String,
    /// How the other job holds the chain.
    pub relation: ChainRelation,
}

/// How the other job of a chain holds it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChainRelation {
    /// The other job is the job's standing successor: it has taken, or is to take, the job's place.
    Successor,
    /// The other job is the job's predecessor, and is still in use in the given state.
    Predecessor(JobLifecycle),
}

impl std::fmt::Display for ChainHold {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.relation {
            ChainRelation::Successor => write!(formatter, "it has the standing successor {}", self.job),
            ChainRelation::Predecessor(lifecycle) => {
                write!(formatter, "its predecessor {} is {lifecycle}", self.job)
            }
        }
    }
}

impl LinkRefusal {
    /// The stable machine-readable code a validation failure for it carries.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        "validation/artefact-invalid"
    }
}

impl ReleaseKinds for PackageJobsState {
    fn release_kind(&self, package: &str, workflow: &str, release: &Version) -> ReleaseKind {
        self.packages
            .get(package)
            .map_or(ReleaseKind::Future, |releases| releases.kind_of(workflow, release))
    }
}

impl PackageJobsState {
    /// Every identity this state holds or refers to: a job's own, the predecessor a link names, and the successor a
    /// retirement names. A deleted job's identity lives on in the records that refer to it, so it is never reissued.
    #[must_use]
    pub fn identities_held(&self) -> BTreeSet<&JobIdentity> {
        let mut held = BTreeSet::new();
        for job in self.workflows.values().flat_map(|workflow| workflow.jobs.values()) {
            held.extend(job.identity.iter());
            held.extend(job.link.iter().map(|link| &link.identity));
            held.extend(job.retirement.iter().map(|retirement| &retirement.replaced_by.identity));
        }
        held
    }

    /// The job's identity, given by `issuer` when its key first enters state and kept from then on.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseStateError::IdentityReused`] when the issuer gives an identity the state already holds, and
    /// records nothing.
    pub fn ensure_identity(
        &mut self,
        workflow_slug: &str,
        job_slug: &str,
        issuer: &dyn IdentityIssuer,
    ) -> Result<&JobIdentity, ReleaseStateError> {
        self.ensure_identity_avoiding(workflow_slug, job_slug, issuer, &BTreeSet::new())
    }

    /// As [`Self::ensure_identity`], and the issued identity must also not be one in `elsewhere`, which a host state
    /// uses for the identities of its other packages.
    pub(crate) fn ensure_identity_avoiding(
        &mut self,
        workflow_slug: &str,
        job_slug: &str,
        issuer: &dyn IdentityIssuer,
        elsewhere: &BTreeSet<JobIdentity>,
    ) -> Result<&JobIdentity, ReleaseStateError> {
        let fresh = if self
            .job(workflow_slug, job_slug)
            .is_some_and(|job| job.identity.is_some())
        {
            None
        } else {
            let candidate = issuer.issue();
            if elsewhere.contains(&candidate) || self.identities_held().contains(&candidate) {
                return Err(ReleaseStateError::IdentityReused {
                    identity: candidate.as_str().to_owned(),
                    job: format!("{workflow_slug}/{job_slug}"),
                });
            }
            Some(candidate)
        };
        // The slot is only empty when `fresh` holds the identity to put in it.
        Ok(self
            .job_entry(workflow_slug, job_slug)
            .identity
            .get_or_insert_with(|| fresh.unwrap_or_else(|| issuer.issue())))
    }
}

impl PackageJobsState {
    /// The schedule of the predecessor a successor was linked to, which the successor takes when it is enabled with none
    /// given: the stored schedule of the job of the same workflow whose identity the successor's link records.
    #[must_use]
    pub fn predecessor_schedule(&self, workflow_slug: &str, job_slug: &str) -> Option<&str> {
        let link = self.job(workflow_slug, job_slug)?.link.as_ref()?;
        self.workflows
            .get(workflow_slug)?
            .jobs
            .iter()
            .find(|(key, job)| key.as_str() != job_slug && job.identity.as_ref() == Some(&link.identity))
            .and_then(|(_, job)| job.schedule.as_deref())
    }

    /// The key of the job that stands as `job_slug`'s successor, when there is one: the job its retirement record
    /// names, since that job was enabled and took its place for good, or else a job that has not been retired and whose
    /// link names `job_slug`'s identity.
    #[must_use]
    pub fn standing_successor_of(&self, workflow_slug: &str, job_slug: &str) -> Option<&str> {
        let entry = self.job(workflow_slug, job_slug)?;
        if let Some(retirement) = &entry.retirement {
            return Some(retirement.replaced_by.job.as_str());
        }
        let identity = entry.identity.as_ref()?;
        self.workflows
            .get(workflow_slug)?
            .jobs
            .iter()
            .find(|(key, job)| {
                key.as_str() != job_slug
                    && job.lifecycle != JobLifecycle::Retired
                    && job.link.as_ref().is_some_and(|link| &link.identity == identity)
            })
            .map(|(key, _)| key.as_str())
    }

    /// The job that keeps `job_slug` from running live by hand: its standing successor, or its predecessor while that is
    /// `enabled` or `suspended`. A chain has one job in use at a time, and a successor is tried by dry runs.
    #[must_use]
    pub fn chain_hold_on_live_run(&self, workflow_slug: &str, job_slug: &str) -> Option<ChainHold> {
        if let Some(successor) = self.standing_successor_of(workflow_slug, job_slug) {
            return Some(ChainHold {
                job: successor.to_owned(),
                relation: ChainRelation::Successor,
            });
        }
        let link = self.job(workflow_slug, job_slug)?.link.as_ref()?;
        self.workflows
            .get(workflow_slug)?
            .jobs
            .iter()
            .find(|(key, job)| {
                key.as_str() != job_slug
                    && job.identity.as_ref() == Some(&link.identity)
                    && matches!(job.lifecycle, JobLifecycle::Enabled | JobLifecycle::Suspended)
            })
            .map(|(key, job)| ChainHold {
                job: key.clone(),
                relation: ChainRelation::Predecessor(job.lifecycle),
            })
    }

    /// The key of the job of `workflow_slug`, other than `job_slug`, that holds `identity`: the job a link recorded, which
    /// is still there when it was only renamed and gone when it was deleted.
    #[must_use]
    pub fn holder_of_identity(&self, workflow_slug: &str, job_slug: &str, identity: &JobIdentity) -> Option<&str> {
        self.workflows
            .get(workflow_slug)?
            .jobs
            .iter()
            .find(|(key, job)| key.as_str() != job_slug && job.identity.as_ref() == Some(identity))
            .map(|(key, _)| key.as_str())
    }

    /// Checks the link `job_slug` would have to `predecessor`, as a validation of its job file does: the predecessor is
    /// a job of the same workflow that is `enabled`, `suspended`, or `retired`, has an identity, and has no other
    /// standing successor. Once a link is recorded the predecessor's identity is held to what was recorded, and a
    /// predecessor deleted since stands.
    ///
    /// # Errors
    ///
    /// Returns the [`LinkRefusal`] that says which of those does not hold.
    pub fn check_link(
        &self,
        workflow_slug: &str,
        job_slug: &str,
        predecessor: &PredecessorKey,
    ) -> Result<JobLink, LinkRefusal> {
        let name = predecessor.as_str();
        if name == job_slug {
            return Err(LinkRefusal::PredecessorIsItself {
                job: job_slug.to_owned(),
            });
        }
        let recorded = self.job(workflow_slug, job_slug).and_then(|job| job.link.as_ref());
        let held = self.job(workflow_slug, name);
        let link = match (recorded, held) {
            (Some(link), None) => {
                // A key no job holds stands only when the job the link recorded was deleted. While a job still holds
                // that identity, the file now names a key it does not answer to.
                if let Some(holder) = self.holder_of_identity(workflow_slug, job_slug, &link.identity) {
                    return Err(LinkRefusal::PredecessorRenamed {
                        predecessor: name.to_owned(),
                        recorded: holder.to_owned(),
                    });
                }
                return Ok(link.clone());
            }
            (None, None) => {
                return Err(LinkRefusal::PredecessorMissing {
                    predecessor: name.to_owned(),
                });
            }
            (Some(link), Some(held)) => {
                if held.identity.as_ref() != Some(&link.identity) {
                    return Err(LinkRefusal::IdentityChanged {
                        predecessor: name.to_owned(),
                        recorded: link.identity.clone(),
                        found: held.identity.clone(),
                    });
                }
                link.clone()
            }
            (None, Some(held)) => {
                if !matches!(
                    held.lifecycle,
                    JobLifecycle::Enabled | JobLifecycle::Suspended | JobLifecycle::Retired
                ) {
                    return Err(LinkRefusal::PredecessorStage {
                        predecessor: name.to_owned(),
                        lifecycle: held.lifecycle,
                    });
                }
                if held.lifecycle == JobLifecycle::Retired && !held.ever_enabled() {
                    return Err(LinkRefusal::PredecessorNeverEnabled {
                        predecessor: name.to_owned(),
                    });
                }
                let identity = held
                    .identity
                    .clone()
                    .ok_or_else(|| LinkRefusal::PredecessorUnidentified {
                        predecessor: name.to_owned(),
                    })?;
                JobLink {
                    identity,
                    retired: held.lifecycle == JobLifecycle::Retired,
                    predecessor: Some(name.to_owned()),
                }
            }
        };
        match self.standing_successor_of(workflow_slug, name) {
            Some(other) if other != job_slug => Err(LinkRefusal::AnotherSuccessor {
                predecessor: name.to_owned(),
                successor: other.to_owned(),
            }),
            _ => Ok(link),
        }
    }

    /// Records the link `job_slug` has to `predecessor` at its first passing validation, after checking it. A link
    /// already recorded is checked again and kept as it was: what was true at the first passing validation stands.
    ///
    /// # Errors
    ///
    /// Returns the [`LinkRefusal`] [`Self::check_link`] gives, and records nothing.
    pub fn record_link(
        &mut self,
        workflow_slug: &str,
        job_slug: &str,
        predecessor: &PredecessorKey,
    ) -> Result<&JobLink, LinkRefusal> {
        let checked = self.check_link(workflow_slug, job_slug, predecessor)?;
        Ok(self.job_entry(workflow_slug, job_slug).link.get_or_insert(checked))
    }
}
