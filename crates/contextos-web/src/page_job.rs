//! A job as a page sees it: its state, which the operator operation changes, and its runs, which a page starts and
//! stops.
//!
//! The two are kept by different adapters, so this pairs them for the page responders, which ask both of one job.

use std::future::Future;

use contextos_workflows::{Changed, JobFingerprint, JobLifecycle, JobState, StoredValidation, Transition};
use time::OffsetDateTime;

use crate::job_chain::RunHistory;
use crate::job_operation::{JobStore, OperationError, StoreError};
use crate::page_runs::PageRunKind;
use crate::run_control::{RunActivity, RunControl, RunRefusal, StartedRun};

/// One job's state store and its run control.
pub(crate) struct PageJob<'a, S, R> {
    pub(crate) store: &'a S,
    pub(crate) control: &'a R,
}

impl<S: JobStore + Sync, R: Sync> JobStore for PageJob<'_, S, R> {
    fn stored(&self) -> impl Future<Output = Result<Option<JobState>, StoreError>> + Send {
        self.store.stored()
    }

    fn exists(&self) -> impl Future<Output = Result<bool, StoreError>> + Send {
        self.store.exists()
    }

    fn lifecycle(&self) -> impl Future<Output = Result<JobLifecycle, StoreError>> + Send {
        self.store.lifecycle()
    }

    fn version(&self) -> impl Future<Output = Result<Option<JobFingerprint>, StoreError>> + Send {
        self.store.version()
    }

    fn validate(&self, now: OffsetDateTime) -> impl Future<Output = Result<StoredValidation, StoreError>> + Send {
        self.store.validate(now)
    }

    fn removes_files_on_delete(&self) -> bool {
        self.store.removes_files_on_delete()
    }

    fn apply(
        &self,
        transition: Transition,
        now: OffsetDateTime,
    ) -> impl Future<Output = Result<Changed, OperationError>> + Send {
        self.store.apply(transition, now)
    }
}

impl<S: Sync, R: RunControl + Sync> RunControl for PageJob<'_, S, R> {
    fn activity(&self) -> impl Future<Output = Result<RunActivity, RunRefusal>> + Send {
        self.control.activity()
    }

    fn history(&self) -> impl Future<Output = Result<RunHistory, RunRefusal>> + Send {
        self.control.history()
    }

    fn start(&self, kind: PageRunKind) -> impl Future<Output = Result<StartedRun, RunRefusal>> + Send {
        self.control.start(kind)
    }

    fn cancel(&self, run_id: u64) -> impl Future<Output = Result<(), RunRefusal>> + Send {
        self.control.cancel(run_id)
    }

    fn schedule_zone(&self) -> String {
        self.control.schedule_zone()
    }
}
