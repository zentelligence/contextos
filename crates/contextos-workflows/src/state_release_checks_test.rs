//! What must hold after every generated operation: how releases are kinded and marked, and the chain of successors.

use super::*;

impl Run {
    /// The invariants that hold after every operation.
    pub(super) fn check(&mut self, op: &Op, moved: Moved, before: &Before) -> Result<(), TestCaseError> {
        let (current, previous) = self.versions();
        prop_assert!(
            current.is_none() || current != previous,
            "current and previous are never one release"
        );
        for version in current.iter().chain(&previous) {
            prop_assert!(
                self.installed.contains(version),
                "{version} is current or previous and not installed"
            );
        }
        // `current` and `previous` move only on an allowed enabling.
        if moved == Moved::Not {
            prop_assert_eq!(
                &(current.clone(), previous.clone()),
                &before.versions,
                "current or previous moved without an allowed enabling"
            );
        }
        // `future` is the newest installed release newer than `current` that is neither `previous` nor marked.
        let future = self
            .state
            .package_releases(PACKAGE)
            .and_then(|package| package.workflows.get(WORKFLOW))
            .and_then(|versions| versions.future.clone());
        let expected_future = self
            .installed
            .iter()
            .filter(|candidate| current.as_ref().is_none_or(|current| *candidate > current))
            .filter(|candidate| Some(*candidate) != previous.as_ref())
            .filter(|candidate| self.marker_of(candidate).is_none())
            .max()
            .cloned();
        prop_assert_eq!(
            &future,
            &expected_future,
            "future is not the newest installed release newer than current"
        );
        if let Some(recorded) = &future {
            prop_assert_eq!(
                self.kind(recorded),
                ReleaseKind::Future,
                "the recorded future is a future release"
            );
        }
        self.check_markers(op, moved, before, current.as_ref(), previous.as_ref())?;
        self.check_kinds(current.as_ref(), previous.as_ref())?;
        self.check_releases_recorded()?;
        self.check_chains()?;
        self.check_links()
    }

    /// A marker is written only by an install, by an enabling that makes a future release current, or by a return that
    /// moves `current` up, for a release below the new `current`, and is never removed or rewritten.
    pub(super) fn check_markers(
        &mut self,
        op: &Op,
        moved: Moved,
        before: &Before,
        current: Option<&Version>,
        previous: Option<&Version>,
    ) -> Result<(), TestCaseError> {
        let now = self.marker_set();
        for version in now.difference(&before.markers) {
            prop_assert!(
                matches!(op, Op::Install(_)) || matches!(moved, Moved::ByFuture | Moved::BySwap),
                "{version} was marked by {op:?}, which neither installs, makes a future release current, nor returns"
            );
            prop_assert!(
                current.is_some_and(|current| version < current),
                "{version} was marked and is not below current"
            );
        }
        for minor in 0..RELEASES {
            let version = release(minor);
            let marker = self.marker_of(&version);
            if let Some(earlier) = self.markers.get(&version) {
                prop_assert_eq!(marker, Some(*earlier), "marker on {} was removed or rewritten", version);
            }
            if let Some(at) = marker {
                self.markers.insert(version.clone(), at);
            }
            if Some(&version) == current || Some(&version) == previous {
                prop_assert!(
                    marker.is_none(),
                    "{version} is {:?} and carries a marker",
                    self.kind(&version)
                );
            }
            // An installed release reads as retired without a marker only when a return passed `current` over it: a
            // release older than `current` is marked when it is installed or superseded, and a return that moves
            // `current` up marks what it passes, so what stays unmarked is only what a return moved `current` down past.
            let unmarked_retired =
                self.installed.contains(&version) && marker.is_none() && self.kind(&version) == ReleaseKind::Retired;
            if unmarked_retired && !self.unmarked_retired.contains(&version) {
                prop_assert_eq!(
                    moved,
                    Moved::BySwap,
                    "{} became retired with no marker by {:?}, which is not a return",
                    version,
                    op
                );
            }
            if unmarked_retired {
                self.unmarked_retired.insert(version.clone());
            } else {
                self.unmarked_retired.remove(&version);
            }
        }
        Ok(())
    }

    /// Every release is exactly one of current, previous, future, or retired.
    pub(super) fn check_kinds(
        &self,
        current: Option<&Version>,
        previous: Option<&Version>,
    ) -> Result<(), TestCaseError> {
        for minor in 0..RELEASES {
            let version = release(minor);
            let marker = self.marker_of(&version);
            let other = Some(&version) != current && Some(&version) != previous;
            let facts = [
                Some(&version) == current,
                Some(&version) == previous,
                other && marker.is_some(),
                other && marker.is_none() && current.is_none_or(|c| &version > c),
                other && marker.is_none() && current.is_some_and(|c| &version < c),
            ];
            prop_assert_eq!(
                facts.iter().filter(|fact| **fact).count(),
                1,
                "{} has {:?}",
                version,
                facts
            );
            let expected = [
                ReleaseKind::Current,
                ReleaseKind::Previous,
                ReleaseKind::Retired,
                ReleaseKind::Future,
                ReleaseKind::Retired,
            ][facts.iter().position(|f| *f).unwrap_or(0)];
            prop_assert_eq!(self.kind(&version), expected, "{}", version);
        }
        Ok(())
    }

    /// No job's recorded release ever changes, except by the version change in place of a job not yet enabled.
    pub(super) fn check_releases_recorded(&mut self) -> Result<(), TestCaseError> {
        let Some(workflow) = self.state.workflows.get(WORKFLOW) else {
            return Ok(());
        };
        for (key, job) in &workflow.jobs {
            let Some(identity) = job.identity() else {
                continue;
            };
            let Some(release) = job.release() else {
                prop_assert!(
                    !self.recorded.contains_key(identity),
                    "the release recorded for {key} was dropped"
                );
                continue;
            };
            let held = self.recorded.entry(identity.clone()).or_insert_with(|| release.clone());
            prop_assert_eq!(&*held, release, "the release recorded for {} changed", key);
        }
        Ok(())
    }

    /// At most one job of a chain is enabled or suspended, as far as `self.chains` follows the chain.
    pub(super) fn check_chains(&self) -> Result<(), TestCaseError> {
        match self.chains {
            Chains::DirectPairs => self.check_direct_pairs(),
            Chains::Whole => self.check_whole_chains(),
        }
    }

    fn check_direct_pairs(&self) -> Result<(), TestCaseError> {
        let Some(workflow) = self.state.workflows.get(WORKFLOW) else {
            return Ok(());
        };
        let live = |state: JobLifecycle| matches!(state, JobLifecycle::Enabled | JobLifecycle::Suspended);
        for (key, successor) in &workflow.jobs {
            let Some(link) = successor.link() else { continue };
            for (other, predecessor) in &workflow.jobs {
                if predecessor.identity() == Some(&link.identity) {
                    prop_assert!(
                        !(live(successor.lifecycle()) && live(predecessor.lifecycle())),
                        "{} and its predecessor {} are both live",
                        key,
                        other
                    );
                }
            }
        }
        Ok(())
    }

    /// Follows each link by identity through predecessors of predecessors, including ones deleted since.
    fn check_whole_chains(&self) -> Result<(), TestCaseError> {
        let Some(workflow) = self.state.workflows.get(WORKFLOW) else {
            return Ok(());
        };
        let mut parents: BTreeMap<String, String> = BTreeMap::new();
        let find = |parents: &BTreeMap<String, String>, node: &str| {
            let mut found = node.to_owned();
            while let Some(parent) = parents.get(&found) {
                found.clone_from(parent);
            }
            found
        };
        for job in workflow.jobs.values() {
            let (Some(identity), Some(link)) = (job.identity(), job.link()) else {
                continue;
            };
            let (one, other) = (
                find(&parents, identity.as_str()),
                find(&parents, link.identity.as_str()),
            );
            if one != other {
                parents.insert(one, other);
            }
        }
        let mut live: BTreeMap<String, Vec<&String>> = BTreeMap::new();
        for (key, job) in &workflow.jobs {
            if let (Some(identity), JobLifecycle::Enabled | JobLifecycle::Suspended) = (job.identity(), job.lifecycle())
            {
                live.entry(find(&parents, identity.as_str())).or_default().push(key);
            }
        }
        for members in live.values() {
            prop_assert!(
                members.len() <= 1,
                "jobs {:?} of one chain are enabled or suspended together",
                members
            );
        }
        Ok(())
    }

    /// A key used again by a job of another identity is told apart from the job a link recorded.
    pub(super) fn check_links(&self) -> Result<(), TestCaseError> {
        for (key, file) in &self.files {
            let (Some(name), Some(link)) = (
                file.predecessor
                    .as_deref()
                    .and_then(|name| PredecessorKey::try_from(name).ok()),
                self.state.job(WORKFLOW, key).and_then(|job| job.link()),
            ) else {
                continue;
            };
            let Some(held) = self.state.job(WORKFLOW, name.as_str()) else {
                continue;
            };
            let verdict = self.state.check_link(WORKFLOW, key, &name);
            let reused = held.identity() != Some(&link.identity);
            prop_assert_eq!(
                matches!(verdict, Err(LinkRefusal::IdentityChanged { .. })),
                reused,
                "{} names {} held by a job of {} identity: {:?}",
                key,
                name.as_str(),
                if reused { "another" } else { "the same" },
                verdict
            );
        }
        Ok(())
    }
}
