//! Enabling a job and returning a whole workflow to its previous release, with what each must leave true.

use super::*;

/// What the key a job's file names as its predecessor holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PredecessorHeld {
    /// The file names none.
    Unnamed,
    /// The file names a key no job holds, as when the predecessor was deleted.
    Absent,
    /// A job holds the key and the job has no link recorded to compare it with.
    Unlinked,
    /// A job of another identity than the one the link recorded holds the key.
    OtherIdentity,
    /// The job the link recorded holds the key and is retired, or was when the link was made, so nothing is retired.
    Settled,
    /// The job the link recorded holds the key and is in use, so the enabling retires it.
    Standing,
}

/// What an enabling of one job is expected to do, worked out from the state and the job's file before it is asked for.
struct Expected {
    key: String,
    busy: bool,
    lifecycle: JobLifecycle,
    release: Option<Version>,
    kind: Option<ReleaseKind>,
    current: Option<Version>,
    previous: Option<Version>,
    /// The `predecessor` the job's file names.
    named: Option<String>,
    /// What the key the file names holds, against the link the job recorded.
    predecessor: PredecessorHeld,
    /// The job is a return successor: its predecessor is on `current`.
    return_successor: bool,
    /// No link and no predecessor, no standing successor, and a schedule given: nothing but its release can refuse it.
    plain: bool,
}

impl Run {
    fn expect_enabling(&self, key: &str, scheduled: bool, busy: bool) -> Expected {
        let (current, previous) = self.versions();
        let entry = self.state.job(WORKFLOW, key);
        let lifecycle = self.state.lifecycle_of(WORKFLOW, key);
        let release = entry.and_then(|job| job.release().cloned());
        let kind = release.as_ref().map(|release| self.kind(release));
        let link = entry.and_then(|job| job.link().cloned());
        let named = self.files.get(key).and_then(|file| file.predecessor.clone());
        let held = named.as_deref().and_then(|name| self.state.job(WORKFLOW, name));
        let same_identity = |held: &JobState| {
            link.as_ref()
                .is_some_and(|link| held.identity() == Some(&link.identity))
        };
        let predecessor_release = held
            .filter(|held| same_identity(held))
            .and_then(|held| held.release().cloned());
        let predecessor = match (&named, held, &link) {
            (None, _, _) => PredecessorHeld::Unnamed,
            (Some(_), None, _) => PredecessorHeld::Absent,
            (Some(_), Some(_), None) => PredecessorHeld::Unlinked,
            (Some(_), Some(held), Some(_)) if !same_identity(held) => PredecessorHeld::OtherIdentity,
            (Some(_), Some(held), Some(link)) if link.retired || held.lifecycle() == JobLifecycle::Retired => {
                PredecessorHeld::Settled
            }
            (Some(_), Some(_), Some(_)) => PredecessorHeld::Standing,
        };
        let plain = lifecycle == JobLifecycle::Registered
            && link.is_none()
            && named.is_none()
            && scheduled
            && self.state.standing_successor_of(WORKFLOW, key).is_none()
            && matches!(kind, Some(ReleaseKind::Current | ReleaseKind::Future));
        Expected {
            key: key.to_owned(),
            busy,
            lifecycle,
            release,
            kind,
            predecessor,
            return_successor: predecessor_release.is_some() && predecessor_release == current,
            plain,
            named,
            current,
            previous,
        }
    }

    /// Enables the job `key`, asking it what its file names, and checks how the enabling moved the releases.
    pub(super) fn enable(
        &mut self,
        key: &str,
        schedule: Option<JobSchedule>,
        busy: bool,
        now: time::OffsetDateTime,
    ) -> Result<Moved, TestCaseError> {
        let expected = self.expect_enabling(key, schedule.is_some(), busy);
        let before = self.state.clone();
        let fingerprint = before
            .job(WORKFLOW, key)
            .and_then(|job| job.fingerprint().cloned())
            .unwrap_or_else(|| fingerprint_for(&release_for_missing()));
        let predecessor = expected
            .named
            .as_deref()
            .and_then(|name| PredecessorKey::try_from(name).ok());
        let result = self.state.enable(
            scope(key),
            EnableRequest {
                schedule,
                version: fingerprint,
                predecessor: predecessor.as_ref(),
                installed: &self.installed,
                predecessor_running: busy,
            },
            now,
        );
        match result {
            Err(error) => {
                prop_assert_eq!(&self.state, &before, "a refused enabling writes nothing");
                if matches!(error, TransitionError::PredecessorBusy { .. }) {
                    prop_assert!(
                        expected.busy && (expected.predecessor == PredecessorHeld::Standing),
                        "the busy refusal is for a predecessor that would be retired: {error}"
                    );
                }
                prop_assert!(
                    !expected.plain,
                    "a registered job on a current or future release enabled with a schedule was refused: {error}"
                );
                Ok(Moved::Not)
            }
            Ok(enabled) => self.check_enabled(&expected, &enabled),
        }
    }

    /// What an enabling that went ahead must have done: retired the predecessor it replaces, and moved `current` and
    /// `previous` as its release says.
    fn check_enabled(&self, expected: &Expected, enabled: &Enabled) -> Result<Moved, TestCaseError> {
        let key = expected.key.as_str();
        let (Some(release), Some(kind)) = (expected.release.clone(), expected.kind) else {
            return Err(failure("an enabling went ahead for a job that records no release"));
        };
        prop_assert_eq!(expected.lifecycle, JobLifecycle::Registered);
        prop_assert!(
            expected.predecessor != PredecessorHeld::OtherIdentity,
            "an enabling went ahead over a predecessor key held by another job"
        );
        prop_assert!(
            !(expected.busy && (expected.predecessor == PredecessorHeld::Standing)),
            "an enabling retired a predecessor whose run claim is held"
        );
        prop_assert_eq!(self.state.lifecycle_of(WORKFLOW, key), JobLifecycle::Enabled);
        // The predecessor is retired by its successor's enabling, and only then.
        match (
            &enabled.retired,
            (expected.predecessor == PredecessorHeld::Standing),
            expected.named.as_deref(),
        ) {
            (Some(retired), true, Some(name)) => {
                prop_assert_eq!(retired, name);
                let predecessor = self.state.job(WORKFLOW, name);
                prop_assert_eq!(predecessor.map(JobState::lifecycle), Some(JobLifecycle::Retired));
                prop_assert_eq!(
                    predecessor
                        .and_then(|job| job.retirement())
                        .map(|retirement| retirement.replaced_by.job.as_str()),
                    Some(key)
                );
            }
            (None, false, _) => {}
            other => {
                return Err(failure(format!(
                    "retired {other:?} against would_retire {}",
                    (expected.predecessor == PredecessorHeld::Standing)
                )));
            }
        }
        let after = self.versions();
        let (current, previous) = (&expected.current, &expected.previous);
        let moved = match kind {
            ReleaseKind::Future => {
                prop_assert_eq!(
                    &after.0,
                    &Some(release.clone()),
                    "an enabling on a future release makes it current"
                );
                prop_assert_eq!(
                    &after.1,
                    &current.clone().filter(|former| *former != release),
                    "previous becomes the former current"
                );
                Moved::ByFuture
            }
            ReleaseKind::Previous => {
                prop_assert!(
                    expected.return_successor,
                    "only a return successor is enabled onto previous"
                );
                prop_assert_eq!(previous.as_ref(), Some(&release));
                prop_assert_eq!(
                    &after.0,
                    &Some(release.clone()),
                    "an enabling on previous makes it current"
                );
                prop_assert_eq!(&after.1, current, "an enabling on previous swaps it with current");
                Moved::BySwap
            }
            ReleaseKind::Current => {
                prop_assert_eq!(
                    &after,
                    &(current.clone(), previous.clone()),
                    "an enabling on current moves nothing"
                );
                Moved::Not
            }
            ReleaseKind::Retired => {
                return Err(failure(format!(
                    "an enabling went ahead on the retired release {release}"
                )));
            }
        };
        prop_assert_eq!(
            enabled.moved.is_some(),
            moved != Moved::Not,
            "the report says whether the releases moved"
        );
        Ok(moved)
    }

    /// The markers after a return from `current` to `previous`: a swap that moves `current` up marks every installed
    /// release strictly between the old and the new `current`, and a swap that moves it down marks none.
    fn markers_after_a_return(
        &self,
        mut markers: BTreeSet<Version>,
        current: &Version,
        previous: &Version,
    ) -> BTreeSet<Version> {
        if previous > current {
            markers.extend(
                self.installed
                    .iter()
                    .filter(|candidate| *candidate > current && *candidate < previous)
                    .cloned(),
            );
        }
        markers
    }

    /// A bulk return: a return successor for every job on current, then each enabled in turn.
    pub(super) fn return_all(&mut self, now: time::OffsetDateTime) -> Result<Moved, TestCaseError> {
        let (Some(current), Some(previous)) = self.versions() else {
            return Ok(Moved::Not);
        };
        let candidates: Vec<String> = self
            .state
            .workflows
            .get(WORKFLOW)
            .map(|workflow| workflow.jobs.iter())
            .into_iter()
            .flatten()
            .filter(|(_, job)| {
                job.release()
                    .is_some_and(|release| selected_for_all(job.lifecycle(), self.kind(release)))
            })
            .map(|(key, _)| key.clone())
            .collect();
        let successors: Vec<String> = candidates
            .iter()
            .map(|key| {
                SuccessorKey::from(SuccessorKeyInput {
                    predecessor: key,
                    to: &previous,
                })
                .as_str()
                .to_owned()
            })
            .collect();
        let unique: BTreeSet<&String> = successors.iter().collect();
        let taken = successors.iter().any(|key| self.state.job(WORKFLOW, key).is_some());
        let mut refused = taken || unique.len() != successors.len();
        for key in &candidates {
            let from = self
                .state
                .job(WORKFLOW, key)
                .and_then(|job| job.release().cloned())
                .unwrap_or_else(|| current.clone());
            let check = SuccessorCheck {
                stage: self.state.lifecycle_of(WORKFLOW, key),
                ever_enabled: self.state.ever_enabled(WORKFLOW, key),
                standing_successor: self.state.standing_successor_of(WORKFLOW, key),
                from: &from,
                from_kind: ReleaseKind::Current,
                to: &previous,
                to_kind: ReleaseKind::Previous,
            };
            if let Err(refusal) = check_successor(&check) {
                prop_assert!(
                    matches!(refusal, SuccessorRefusal::HasSuccessor { .. }),
                    "a return of an enabled or suspended job on current is refused only for its successor: {refusal}"
                );
                refused = true;
            }
        }
        // A bulk request with any refusal writes nothing, so the model writes nothing for it.
        if candidates.is_empty() || refused {
            return Ok(Moved::Not);
        }
        for (key, successor) in candidates.iter().zip(&successors) {
            prop_assert!(
                self.create_job(successor, &previous, Some(key), now)?,
                "a return successor of {key} validates"
            );
        }
        let markers = self.marker_set();
        let mut moved = Moved::Not;
        for (position, successor) in successors.iter().enumerate() {
            let outcome = self.enable(successor, Some(cron()), false, now)?;
            prop_assert_eq!(
                self.state.lifecycle_of(WORKFLOW, successor),
                JobLifecycle::Enabled,
                "{} is enabled in turn",
                successor
            );
            if position == 0 {
                prop_assert_eq!(
                    outcome,
                    Moved::BySwap,
                    "the first enabling of a return swaps current and previous"
                );
                prop_assert_eq!(self.versions(), (Some(previous.clone()), Some(current.clone())));
            } else {
                prop_assert_eq!(outcome, Moved::Not, "a later enabling of the return moves nothing");
                prop_assert_eq!(self.versions(), (Some(previous.clone()), Some(current.clone())));
            }
            moved = Moved::BySwap;
        }
        for key in &candidates {
            prop_assert_eq!(
                self.state.lifecycle_of(WORKFLOW, key),
                JobLifecycle::Retired,
                "{} is retired by its return successor",
                key
            );
        }
        let expected_markers = self.markers_after_a_return(markers, &current, &previous);
        prop_assert_eq!(
            self.marker_set(),
            expected_markers,
            "a return marks exactly the releases it passes over when it moves current up"
        );
        Ok(moved)
    }
}
