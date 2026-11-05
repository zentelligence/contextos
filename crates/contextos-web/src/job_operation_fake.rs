//! An in-memory job for tests of the operator operation and of the pages that call it: its files hold a script, its state
//! is an ordinary [`JobsState`], and what validating it finds is set by the test.

use std::future::Future;
use std::sync::Mutex;

use contextos_workflows::{
    Actor, Changed, JobFingerprint, JobLifecycle, JobSchedule, JobState, JobsState, ReleaseFingerprintInput, StoreKind,
    StoredValidation, SuspensionReason, Transition, TransitionError, ValidationCode, ValidationFailure,
    ValidationRecord, ValidationReport,
};
use time::OffsetDateTime;
use time::macros::datetime;

use crate::job_chain::RunHistory;
use crate::job_operation::{JobStore, OperationError, StoreError};
use crate::job_runs::RunSummary;
use crate::package_retention::RetentionError;
use crate::page_runs::{PageRunKind, PageRunsOfJob};
use crate::run_claim::{ActiveRun, ClaimState};
use crate::run_control::{RunActivity, RunControl, RunRefusal, StartedRun};
use crate::store_retention::RetentionWarning;

pub(crate) type TestResult = Result<(), Box<dyn std::error::Error>>;

pub(crate) const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);

pub(crate) fn version(script: &str) -> JobFingerprint {
    JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Vault,
        package: "ops",
        release: "1.0.0",
        digest: script,
        job_toml: "",
        release_manifest: "",
        libraries: &[],
    })
}

pub(crate) fn schedule(expression: &str) -> Result<JobSchedule, Box<dyn std::error::Error>> {
    Ok(JobSchedule::try_from(expression)?)
}

pub(crate) fn reason(text: &str) -> Result<SuspensionReason, Box<dyn std::error::Error>> {
    Ok(SuspensionReason::try_from(text)?)
}

/// What validating the fake's job finds.
#[derive(Clone)]
pub(crate) enum Finding {
    Passes,
    Fails,
    CannotCheck,
    FilesUnreadable,
}

/// A job whose files hold the script `script` and whose state is an ordinary [`JobsState`].
pub(crate) struct FakeJob {
    state: Mutex<JobsState>,
    script: Mutex<String>,
    finding: Mutex<Finding>,
    validations_run: Mutex<usize>,
    exists: Mutex<bool>,
    /// What the job's claim shows, set by a test.
    claim: Mutex<ClaimState>,
    /// What the table of runs started from pages holds, set by a test.
    page_runs: Mutex<PageRunsOfJob>,
    /// What starting a run answers, until a test says otherwise.
    start_answer: Mutex<StartAnswer>,
    started: Mutex<Vec<PageRunKind>>,
    cancelled: Mutex<Vec<u64>>,
    /// The job's run records, newest first, or `None` when they cannot be read.
    history: Mutex<Option<Vec<RunSummary>>>,
    /// How many times retention was run, and whether it is to fail.
    retentions: Mutex<(usize, bool)>,
}

/// How the fake answers a request to start a run.
#[derive(Clone)]
pub(crate) enum StartAnswer {
    Starts,
    NotRunnable(String),
    CouldNotStart(String),
    HostUnavailable,
}

impl FakeJob {
    pub(crate) fn new(script: &str) -> Self {
        Self {
            state: Mutex::new(JobsState::default()),
            script: Mutex::new(script.to_owned()),
            finding: Mutex::new(Finding::Passes),
            validations_run: Mutex::new(0),
            exists: Mutex::new(true),
            claim: Mutex::new(ClaimState::Idle),
            page_runs: Mutex::new(PageRunsOfJob::default()),
            start_answer: Mutex::new(StartAnswer::Starts),
            started: Mutex::new(Vec::new()),
            cancelled: Mutex::new(Vec::new()),
            history: Mutex::new(Some(Vec::new())),
            retentions: Mutex::new((0, false)),
        }
    }

    /// A live run `run_id` holds the job's claim.
    pub(crate) fn runs_live(&self, run_id: u64) {
        *lock(&self.claim) = ClaimState::Held(Some(ActiveRun { run_id, started: NOW }));
    }

    /// A run holds the claim and has not said which run it is yet.
    pub(crate) fn is_starting(&self) {
        *lock(&self.claim) = ClaimState::Held(None);
    }

    pub(crate) fn page_has(&self, runs: PageRunsOfJob) {
        *lock(&self.page_runs) = runs;
    }

    pub(crate) fn answers_start_with(&self, answer: StartAnswer) {
        *lock(&self.start_answer) = answer;
    }

    pub(crate) fn has_history(&self, history: Option<Vec<RunSummary>>) {
        *lock(&self.history) = history;
    }

    pub(crate) fn started(&self) -> Vec<PageRunKind> {
        lock(&self.started).clone()
    }

    pub(crate) fn cancelled(&self) -> Vec<u64> {
        lock(&self.cancelled).clone()
    }

    /// Makes the job's files and state vanish from the store's point of view.
    pub(crate) fn vanish(&self) {
        *self.exists.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = false;
        *self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = JobsState::default();
    }

    pub(crate) fn edit_files(&self, script: &str) {
        *self.script.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = script.to_owned();
    }

    pub(crate) fn finds(&self, finding: Finding) {
        *self.finding.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = finding;
    }

    pub(crate) fn current(&self) -> JobFingerprint {
        version(&self.script.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    pub(crate) fn state(&self) -> JobsState {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// How many times retention was run for the job's store.
    pub(crate) fn retentions(&self) -> usize {
        lock(&self.retentions).0
    }

    /// Retention fails from now on.
    pub(crate) fn fails_retention(&self) {
        lock(&self.retentions).1 = true;
    }

    pub(crate) fn lifecycle_now(&self) -> JobLifecycle {
        self.state().lifecycle_of("pkg", "wf", "job")
    }

    pub(crate) fn validations(&self) -> usize {
        *self
            .validations_run
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn step(&self, actor: Actor, transition: &Transition) -> Result<(), TransitionError> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .transition("pkg", "wf", "job", actor, transition, NOW)
            .map(|_| ())
    }

    /// Takes the job to `registered` for the version its files have now, through the real transitions.
    pub(crate) fn register(&self) -> TestResult {
        self.step(Actor::Validator, &Transition::Validated(self.current()))?;
        self.step(Actor::DryRun, &Transition::DryRunCompleted(self.current()))?;
        Ok(())
    }

    /// Takes the job to `suspended` by the operator, its trial already passed.
    pub(crate) fn suspend_after_enabling(&self) -> TestResult {
        self.register()?;
        self.step(
            Actor::Operator,
            &Transition::Enable(schedule("0 3 * * *")?, self.current()),
        )?;
        self.step(Actor::System, &Transition::ProbationPassed(self.current()))?;
        self.step(
            Actor::Operator,
            &Transition::Suspend(contextos_workflows::Suspension::operator(reason("held")?)),
        )?;
        Ok(())
    }
}

impl FakeJob {
    /// Validates the fake's job, as an adapter would: the result is recorded, and a draft that passed moves.
    fn validation_now(&self, now: OffsetDateTime) -> StoredValidation {
        *self
            .validations_run
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        let finding = self
            .finding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut report = ValidationReport::default();
        let mut fingerprint = Some(self.current());
        match finding {
            Finding::Passes => {}
            Finding::Fails => report.failures.push(ValidationFailure {
                code: ValidationCode::ScriptSyntax,
                artefact: "sync.rhai".to_owned(),
                message: "unexpected token on line 3".to_owned(),
            }),
            Finding::CannotCheck => report.unverifiable.push(contextos_workflows::Unverifiable {
                check: "tools".to_owned(),
                reason: "the server could not be reached".to_owned(),
            }),
            Finding::FilesUnreadable => fingerprint = None,
        }
        let stored = StoredValidation::from(ValidationRecord {
            report: &report,
            fingerprint: fingerprint.clone(),
            at: now,
        });
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.keep_validation("pkg", "wf", "job", stored.clone(), now);
        stored
    }
}

// Every method answers at once, so none awaits: each returns a ready future.
impl JobStore for FakeJob {
    fn stored(&self) -> impl Future<Output = Result<Option<JobState>, StoreError>> + Send {
        std::future::ready(Ok(self.state().job("pkg", "wf", "job").cloned()))
    }

    fn exists(&self) -> impl Future<Output = Result<bool, StoreError>> + Send {
        std::future::ready(Ok(*self
            .exists
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)))
    }

    fn lifecycle(&self) -> impl Future<Output = Result<JobLifecycle, StoreError>> + Send {
        std::future::ready(Ok(self.lifecycle_now()))
    }

    fn version(&self) -> impl Future<Output = Result<Option<JobFingerprint>, StoreError>> + Send {
        let unreadable = matches!(
            *self.finding.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
            Finding::FilesUnreadable
        );
        std::future::ready(Ok((!unreadable).then(|| self.current())))
    }

    fn validate(&self, now: OffsetDateTime) -> impl Future<Output = Result<StoredValidation, StoreError>> + Send {
        std::future::ready(Ok(self.validation_now(now)))
    }

    fn retain(&self) -> impl Future<Output = Result<(), RetentionWarning>> + Send {
        let mut retentions = lock(&self.retentions);
        retentions.0 += 1;
        std::future::ready(if retentions.1 {
            Err(RetentionWarning::raised(
                StoreKind::Host,
                &RetentionError::from(std::io::Error::other("the jobs folder could not be listed")),
            ))
        } else {
            Ok(())
        })
    }

    fn apply(
        &self,
        transition: Transition,
        now: OffsetDateTime,
    ) -> impl Future<Output = Result<Changed, OperationError>> + Send {
        std::future::ready(
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .transition("pkg", "wf", "job", Actor::Operator, &transition, now)
                .map(Changed::from)
                .map_err(OperationError::from),
        )
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl RunControl for FakeJob {
    fn activity(&self) -> impl Future<Output = Result<RunActivity, RunRefusal>> + Send {
        std::future::ready(Ok(RunActivity {
            claim: lock(&self.claim).clone(),
            page: lock(&self.page_runs).clone(),
        }))
    }

    fn history(&self) -> impl Future<Output = Result<RunHistory, RunRefusal>> + Send {
        std::future::ready(
            lock(&self.history)
                .clone()
                .map(RunHistory::from)
                .ok_or(RunRefusal::Unavailable("read the run history")),
        )
    }

    fn start(&self, kind: PageRunKind) -> impl Future<Output = Result<StartedRun, RunRefusal>> + Send {
        let answer = lock(&self.start_answer).clone();
        std::future::ready(match answer {
            StartAnswer::NotRunnable(text) => Err(RunRefusal::NotRunnable(text)),
            StartAnswer::CouldNotStart(text) => Err(RunRefusal::CouldNotStart(text)),
            StartAnswer::HostUnavailable => Err(RunRefusal::Unavailable("start the run")),
            StartAnswer::Starts => {
                lock(&self.started).push(kind);
                Ok(StartedRun { run_id: Some(5), kind })
            }
        })
    }

    fn cancel(&self, run_id: u64) -> impl Future<Output = Result<(), RunRefusal>> + Send {
        let active = matches!(&*lock(&self.claim), ClaimState::Held(Some(run)) if run.run_id == run_id);
        std::future::ready(if active {
            lock(&self.cancelled).push(run_id);
            Ok(())
        } else {
            Err(RunRefusal::NotActive { run_id })
        })
    }

    fn schedule_zone(&self) -> String {
        "UTC".to_owned()
    }
}
