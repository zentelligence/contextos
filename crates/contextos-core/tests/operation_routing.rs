use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use contextos_core::{
    EventsBatch, EventsOutcome, LogsOperations, MaintainsIndexes, NoEvents, NoProjection, OpKind, OperationEvent,
    OperationRoute, OperationRouter, OperationRouterConfig, OperationService, OperationWarning, Origin,
    ProjectsDefinitions, RecordsEvents, RoutesOperations, UpdatesSearch, VaultPath, VaultPathInput, VaultRoot,
    VaultRootId, VaultRootInput, VaultSet, VersionsVault,
};
use proptest::prelude::*;
use time::OffsetDateTime;

#[derive(Clone, Debug, Default)]
struct InvocationCount(Arc<AtomicUsize>);

impl InvocationCount {
    fn increment(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn observed(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Debug)]
struct IndexRecorder(InvocationCount);

impl MaintainsIndexes for IndexRecorder {
    fn reconcile(&self, _event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning> {
        self.0.increment();
        Ok(Vec::new())
    }
}

#[derive(Clone, Debug)]
struct OperationLogRecorder(InvocationCount);

impl LogsOperations for OperationLogRecorder {
    fn append(&self, _event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning> {
        self.0.increment();
        Ok(Vec::new())
    }
}

#[derive(Clone, Debug)]
struct GitRecorder(InvocationCount);

impl VersionsVault for GitRecorder {
    fn stage(&self, _event: &OperationEvent) -> Result<(), OperationWarning> {
        self.0.increment();
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct SearchRecorder(InvocationCount);

impl UpdatesSearch for SearchRecorder {
    fn update(&self, _event: &OperationEvent) -> Result<(), OperationWarning> {
        self.0.increment();
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct DerivingIndex(InvocationCount);

impl MaintainsIndexes for DerivingIndex {
    fn reconcile(&self, event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning> {
        self.0.increment();
        let mut internal = event.clone();
        internal.origin = Origin::Internal("index".to_owned());
        Ok(vec![internal])
    }
}

#[derive(Clone, Debug)]
struct DerivingOperationLog(InvocationCount);

impl LogsOperations for DerivingOperationLog {
    fn append(&self, event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning> {
        self.0.increment();
        let mut internal = event.clone();
        internal.origin = Origin::Internal("oplog".to_owned());
        Ok(vec![internal])
    }
}

proptest! {
    #[test]
    fn internal_events_never_route_to_recursive_services(
        service_name in "[a-z][a-z0-9_-]{0,63}"
    ) {
        let origin = Origin::Internal(service_name);
        let route = OperationRoute::from(&origin);
        let index_count = InvocationCount::default();
        let log_count = InvocationCount::default();
        let git_count = InvocationCount::default();
        let search_count = InvocationCount::default();
        let router = OperationRouter::from(OperationRouterConfig {
            indexes: IndexRecorder(index_count.clone()),
            operation_log: OperationLogRecorder(log_count.clone()),
            versions: GitRecorder(git_count.clone()),
            search: SearchRecorder(search_count.clone()),
            projection: NoProjection,
            events: NoEvents,
        });
        let event = OperationEvent {
            kind: OpKind::Modify,
            paths: Vec::new(),
            origin,
            summary: "internal write".to_owned(),
            at: OffsetDateTime::UNIX_EPOCH,
        };

        prop_assert!(!route.includes(OperationService::Index));
        prop_assert!(!route.includes(OperationService::OperationLog));
        prop_assert!(!route.includes(OperationService::Events));
        prop_assert!(route.includes(OperationService::Git));
        prop_assert!(route.includes(OperationService::Search));
        prop_assert!(route.includes(OperationService::Projection));
        prop_assert!(router.route(&event).is_empty());
        prop_assert_eq!(index_count.observed(), 0);
        prop_assert_eq!(log_count.observed(), 0);
        prop_assert_eq!(git_count.observed(), 1);
        prop_assert_eq!(search_count.observed(), 1);
    }
}

#[test]
fn tool_events_route_to_every_substrate_service() {
    let route = OperationRoute::from(&Origin::Tool("fs_write_file".to_owned()));

    assert!(route.includes(OperationService::Index));
    assert!(route.includes(OperationService::OperationLog));
    assert!(route.includes(OperationService::Git));
    assert!(route.includes(OperationService::Search));
    assert!(route.includes(OperationService::Projection));
}

#[test]
fn derived_internal_events_are_staged_without_recursive_redispatch() {
    let index_count = InvocationCount::default();
    let log_count = InvocationCount::default();
    let git_count = InvocationCount::default();
    let search_count = InvocationCount::default();
    let router = OperationRouter::from(OperationRouterConfig {
        indexes: DerivingIndex(index_count.clone()),
        operation_log: DerivingOperationLog(log_count.clone()),
        versions: GitRecorder(git_count.clone()),
        search: SearchRecorder(search_count.clone()),
        projection: NoProjection,
        events: NoEvents,
    });
    let event = OperationEvent {
        kind: OpKind::Modify,
        paths: Vec::new(),
        origin: Origin::Tool("fs_write_file".to_owned()),
        summary: "user write".to_owned(),
        at: OffsetDateTime::UNIX_EPOCH,
    };

    let warnings = router.route(&event);

    assert!(warnings.is_empty(), "`warnings` should be empty, got {warnings:?}");
    assert_eq!(index_count.observed(), 1);
    assert_eq!(log_count.observed(), 1);
    assert_eq!(git_count.observed(), 3);
    assert_eq!(search_count.observed(), 3);
}

#[derive(Clone, Debug)]
struct WarnsOnEveryEvent(InvocationCount);

impl RoutesOperations for WarnsOnEveryEvent {
    fn route(&self, event: &OperationEvent) -> Vec<OperationWarning> {
        self.0.increment();
        vec![OperationWarning {
            code: "git/stage".to_owned(),
            message: event.summary.clone(),
        }]
    }
}

fn internal_event(summary: &str) -> OperationEvent {
    OperationEvent {
        kind: OpKind::Modify,
        paths: Vec::new(),
        origin: Origin::Internal("index".to_owned()),
        summary: summary.to_owned(),
        at: OffsetDateTime::UNIX_EPOCH,
    }
}

#[test]
fn routing_a_batch_routes_every_event_in_order_and_keeps_every_warning() {
    let count = InvocationCount::default();
    let sink = WarnsOnEveryEvent(count.clone());

    let outcome = sink.route_all(vec![internal_event("first"), internal_event("second")]);

    assert_eq!(count.observed(), 2);
    assert_eq!(
        outcome
            .events
            .iter()
            .map(|event| event.summary.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert_eq!(
        outcome
            .warnings
            .iter()
            .map(|warning| warning.message.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
}

#[test]
fn routing_an_empty_batch_touches_nothing() {
    let count = InvocationCount::default();

    let outcome = WarnsOnEveryEvent(count.clone()).route_all(Vec::new());

    assert_eq!(count.observed(), 0);
    assert!(outcome.events.is_empty() && outcome.warnings.is_empty());
}

#[derive(Clone, Debug, Default)]
struct BatchingSearch {
    single_updates: InvocationCount,
    batches: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
}

impl UpdatesSearch for BatchingSearch {
    fn update(&self, _event: &OperationEvent) -> Result<(), OperationWarning> {
        self.single_updates.increment();
        Ok(())
    }

    fn update_all(&self, events: &[OperationEvent]) -> Result<(), OperationWarning> {
        if let Ok(mut batches) = self.batches.lock() {
            batches.push(events.iter().map(|event| event.summary.clone()).collect());
        }
        Ok(())
    }
}

#[test]
fn a_routed_batch_reaches_search_as_one_batch_in_arrival_order() {
    let search = BatchingSearch::default();
    let git_count = InvocationCount::default();
    let router = OperationRouter::from(OperationRouterConfig {
        indexes: IndexRecorder(InvocationCount::default()),
        operation_log: OperationLogRecorder(InvocationCount::default()),
        versions: GitRecorder(git_count.clone()),
        search: search.clone(),
        projection: NoProjection,
        events: NoEvents,
    });

    let outcome = router.route_all(vec![
        internal_event("first"),
        internal_event("second"),
        internal_event("third"),
    ]);

    assert!(
        outcome.warnings.is_empty(),
        "`outcome.warnings` should be empty, got {:?}",
        outcome.warnings
    );
    assert_eq!(outcome.events.len(), 3);
    assert_eq!(git_count.observed(), 3, "every write is still staged individually");
    assert_eq!(search.single_updates.observed(), 0);
    let batches = search.batches.lock().map(|batches| batches.clone()).unwrap_or_default();
    assert_eq!(
        batches,
        vec![vec!["first".to_owned(), "second".to_owned(), "third".to_owned()]]
    );
}

#[test]
fn events_derived_from_a_batched_event_join_the_same_search_batch() {
    let search = BatchingSearch::default();
    let router = OperationRouter::from(OperationRouterConfig {
        indexes: DerivingIndex(InvocationCount::default()),
        operation_log: OperationLogRecorder(InvocationCount::default()),
        versions: GitRecorder(InvocationCount::default()),
        search: search.clone(),
        projection: NoProjection,
        events: NoEvents,
    });
    let mut tool_event = internal_event("user write");
    tool_event.origin = Origin::Tool("fs_write_file".to_owned());

    let outcome = router.route_all(vec![tool_event]);

    assert!(
        outcome.warnings.is_empty(),
        "`outcome.warnings` should be empty, got {:?}",
        outcome.warnings
    );
    let batches = search.batches.lock().map(|batches| batches.clone()).unwrap_or_default();
    assert_eq!(
        batches,
        vec![vec!["user write".to_owned(), "user write".to_owned()]],
        "the tool's own event and the index event it caused, in one batch"
    );
}

#[test]
fn a_batch_with_nothing_for_search_does_not_call_it() {
    let search = BatchingSearch::default();
    let router = OperationRouter::from(OperationRouterConfig {
        indexes: IndexRecorder(InvocationCount::default()),
        operation_log: OperationLogRecorder(InvocationCount::default()),
        versions: GitRecorder(InvocationCount::default()),
        search: search.clone(),
        projection: NoProjection,
        events: NoEvents,
    });

    let outcome = router.route_all(Vec::new());

    assert!(
        outcome.events.is_empty(),
        "`outcome.events` should be empty, got {:?}",
        outcome.events
    );
    assert!(search.batches.lock().is_ok_and(|batches| batches.is_empty()));
}

/// Records the summary of every event it is handed and answers with one warning per call, so a test can see both
/// what reached projection and where its warnings land in the route result.
#[derive(Clone, Debug, Default)]
struct ProjectionRecorder {
    seen: Arc<std::sync::Mutex<Vec<String>>>,
}

impl ProjectionRecorder {
    fn summaries(&self) -> Vec<String> {
        self.seen.lock().map(|seen| seen.clone()).unwrap_or_default()
    }
}

impl ProjectsDefinitions for ProjectionRecorder {
    fn project(&self, event: &OperationEvent) -> Vec<OperationWarning> {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(event.summary.clone());
        }
        vec![OperationWarning {
            code: "projection/recorded".to_owned(),
            message: event.summary.clone(),
        }]
    }
}

#[derive(Clone, Debug)]
struct WarningGit;

impl VersionsVault for WarningGit {
    fn stage(&self, _event: &OperationEvent) -> Result<(), OperationWarning> {
        Err(OperationWarning {
            code: "git/stage".to_owned(),
            message: "git".to_owned(),
        })
    }
}

#[derive(Clone, Debug)]
struct WarningSearch;

impl UpdatesSearch for WarningSearch {
    fn update(&self, _event: &OperationEvent) -> Result<(), OperationWarning> {
        Err(OperationWarning {
            code: "search/update".to_owned(),
            message: "search".to_owned(),
        })
    }
}

fn vault_set(dir: &std::path::Path) -> Result<VaultSet, Box<dyn std::error::Error>> {
    Ok(VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: dir.to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?])?)
}

fn event_at(
    roots: &VaultSet,
    kind: OpKind,
    origin: Origin,
    raw: &[&str],
    summary: &str,
) -> Result<OperationEvent, Box<dyn std::error::Error>> {
    let paths = raw
        .iter()
        .map(|raw| VaultPath::try_from(VaultPathInput { roots, raw }))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(OperationEvent {
        kind,
        paths,
        origin,
        summary: summary.to_owned(),
        at: OffsetDateTime::UNIX_EPOCH,
    })
}

fn projecting_router(
    projection: ProjectionRecorder,
) -> OperationRouter<IndexRecorder, OperationLogRecorder, GitRecorder, SearchRecorder, ProjectionRecorder> {
    OperationRouter::from(OperationRouterConfig {
        indexes: IndexRecorder(InvocationCount::default()),
        operation_log: OperationLogRecorder(InvocationCount::default()),
        versions: GitRecorder(InvocationCount::default()),
        search: SearchRecorder(InvocationCount::default()),
        projection,
        events: NoEvents,
    })
}

#[test]
fn a_tool_write_to_an_agent_definition_reaches_projection_once_and_a_note_write_does_not()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let roots = vault_set(vault.path())?;
    let projection = ProjectionRecorder::default();
    let router = projecting_router(projection.clone());
    let tool = || Origin::Tool("fs_write_file".to_owned());

    let agent = event_at(&roots, OpKind::Modify, tool(), &["registry/agents/critic.md"], "agent")?;
    let note = event_at(&roots, OpKind::Modify, tool(), &["notes/a.md"], "note")?;

    let agent_warnings = router.route(&agent);
    let note_warnings = router.route(&note);

    assert_eq!(projection.summaries(), ["agent"]);
    assert_eq!(agent_warnings.len(), 1);
    assert!(
        note_warnings.is_empty(),
        "a note write should raise nothing, got {note_warnings:?}"
    );
    Ok(())
}

#[test]
fn an_internal_write_to_a_skill_file_reaches_projection() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let roots = vault_set(vault.path())?;
    let projection = ProjectionRecorder::default();
    let router = projecting_router(projection.clone());
    let event = event_at(
        &roots,
        OpKind::Modify,
        Origin::Internal("note_create".to_owned()),
        &["registry/skills/x/SKILL.md"],
        "skill",
    )?;

    let _ = router.route(&event);

    assert_eq!(projection.summaries(), ["skill"]);
    Ok(())
}

#[test]
fn a_managed_index_inside_a_projected_folder_is_never_projected() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let roots = vault_set(vault.path())?;
    let projection = ProjectionRecorder::default();
    let router = projecting_router(projection.clone());
    let tool = || Origin::Tool("fs_write_file".to_owned());

    for raw in ["registry/agents/index.md", "registry/skills/index.md"] {
        let event = event_at(&roots, OpKind::Modify, tool(), &[raw], "index")?;
        assert!(router.route(&event).is_empty(), "{raw} must not warn");
    }

    assert!(projection.summaries().is_empty(), "got {:?}", projection.summaries());
    Ok(())
}

#[test]
fn a_move_into_the_agent_registry_reaches_projection_exactly_once() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let roots = vault_set(vault.path())?;
    let projection = ProjectionRecorder::default();
    let router = projecting_router(projection.clone());
    let event = event_at(
        &roots,
        OpKind::Move,
        Origin::Tool("fs_move_file".to_owned()),
        &["notes/a.md", "registry/agents/a.md"],
        "move",
    )?;

    let warnings = router.route(&event);

    assert_eq!(projection.summaries(), ["move"]);
    assert_eq!(warnings.len(), 1);
    Ok(())
}

#[test]
fn a_move_out_of_the_agent_registry_reaches_projection_exactly_once() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let roots = vault_set(vault.path())?;
    let projection = ProjectionRecorder::default();
    let router = projecting_router(projection.clone());
    let event = event_at(
        &roots,
        OpKind::Move,
        Origin::Tool("fs_move_file".to_owned()),
        &["registry/agents/a.md", "registry/agents/b.md"],
        "rename",
    )?;

    let _ = router.route(&event);

    assert_eq!(
        projection.summaries(),
        ["rename"],
        "two definition paths still make one event"
    );
    Ok(())
}

#[test]
fn projection_warnings_follow_git_and_precede_search() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let roots = vault_set(vault.path())?;
    let router = OperationRouter::from(OperationRouterConfig {
        indexes: IndexRecorder(InvocationCount::default()),
        operation_log: OperationLogRecorder(InvocationCount::default()),
        versions: WarningGit,
        search: WarningSearch,
        projection: ProjectionRecorder::default(),
        events: NoEvents,
    });
    let event = event_at(
        &roots,
        OpKind::Modify,
        Origin::Tool("fs_write_file".to_owned()),
        &["registry/instructions/global.md"],
        "orientation",
    )?;

    let codes = router
        .route(&event)
        .into_iter()
        .map(|warning| warning.code)
        .collect::<Vec<_>>();

    assert_eq!(codes, ["git/stage", "projection/recorded", "search/update"]);
    Ok(())
}

/// What the events consumer was handed for one batch.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SeenBatch {
    summaries: Vec<String>,
    warning_codes: Vec<String>,
    attempted: Vec<OperationService>,
}

/// An events consumer that notes the order of its call against the shared `sequence`, keeps what it was handed, and
/// answers with `derived` internal events and `warning`.
#[derive(Clone, Debug, Default)]
struct EventsRecorder {
    sequence: Arc<std::sync::Mutex<Vec<&'static str>>>,
    seen: Arc<std::sync::Mutex<Vec<SeenBatch>>>,
    derived: Vec<OperationEvent>,
    warning: Option<OperationWarning>,
}

impl EventsRecorder {
    fn batches(&self) -> Vec<SeenBatch> {
        self.seen.lock().map(|seen| seen.clone()).unwrap_or_default()
    }

    fn sequence(&self) -> Vec<&'static str> {
        self.sequence
            .lock()
            .map(|sequence| sequence.clone())
            .unwrap_or_default()
    }
}

impl RecordsEvents for EventsRecorder {
    fn record(&self, batch: &EventsBatch<'_>) -> EventsOutcome {
        if let Ok(mut sequence) = self.sequence.lock() {
            sequence.push("events");
        }
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(SeenBatch {
                summaries: batch.events.iter().map(|event| event.summary.clone()).collect(),
                warning_codes: batch
                    .warnings
                    .iter()
                    .map(|scoped| scoped.warning.code.clone())
                    .collect(),
                attempted: batch.attempted.iter().map(|scoped| scoped.service).collect(),
            });
        }
        EventsOutcome {
            events: self.derived.clone(),
            warnings: self.warning.clone().into_iter().collect(),
        }
    }
}

/// A search service that notes the order of its call against the same sequence as [`EventsRecorder`].
#[derive(Clone, Debug)]
struct SequencedSearch(Arc<std::sync::Mutex<Vec<&'static str>>>);

impl UpdatesSearch for SequencedSearch {
    fn update(&self, _event: &OperationEvent) -> Result<(), OperationWarning> {
        if let Ok(mut sequence) = self.0.lock() {
            sequence.push("search");
        }
        Ok(())
    }
}

fn recording_router<V, S>(
    versions: V,
    search: S,
    events: EventsRecorder,
) -> OperationRouter<IndexRecorder, OperationLogRecorder, V, S, NoProjection, EventsRecorder>
where
    V: VersionsVault,
    S: UpdatesSearch,
{
    OperationRouter::from(OperationRouterConfig {
        indexes: IndexRecorder(InvocationCount::default()),
        operation_log: OperationLogRecorder(InvocationCount::default()),
        versions,
        search,
        projection: NoProjection,
        events,
    })
}

#[test]
fn a_completed_tool_write_reaches_the_events_consumer_once_after_the_other_services_and_an_internal_write_does_not() {
    let events = EventsRecorder::default();
    let router = recording_router(
        GitRecorder(InvocationCount::default()),
        SequencedSearch(events.sequence.clone()),
        events.clone(),
    );
    let mut tool_event = internal_event("tool write");
    tool_event.origin = Origin::Tool("fs_write_file".to_owned());

    let _ = router.route(&tool_event);

    assert_eq!(events.sequence(), ["search", "events"], "events follow search, once");
    let _ = router.route(&internal_event("internal write"));
    assert_eq!(
        events.batches().len(),
        1,
        "an internal write must not reach the consumer"
    );
    assert_eq!(events.batches()[0].summaries, ["tool write"]);
}

#[test]
fn the_events_consumer_receives_the_warnings_other_services_raised_and_the_services_attempted() {
    let events = EventsRecorder::default();
    let router = recording_router(WarningGit, WarningSearch, events.clone());
    let mut tool_event = internal_event("tool write");
    tool_event.origin = Origin::Tool("fs_write_file".to_owned());

    let _ = router.route(&tool_event);

    let seen = events.batches();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].warning_codes, ["git/stage", "search/update"]);
    assert_eq!(
        seen[0].attempted,
        [
            OperationService::Index,
            OperationService::OperationLog,
            OperationService::Git,
            OperationService::Search,
        ]
    );
}

#[test]
fn internal_events_the_consumer_returns_are_staged_and_its_warnings_are_reported_last() {
    let git_count = InvocationCount::default();
    let events = EventsRecorder {
        derived: vec![internal_event("event note")],
        warning: Some(OperationWarning {
            code: "events/write-failed".to_owned(),
            message: "events".to_owned(),
        }),
        ..EventsRecorder::default()
    };
    let router = recording_router(GitRecorder(git_count.clone()), WarningSearch, events);
    let mut tool_event = internal_event("tool write");
    tool_event.origin = Origin::Tool("fs_write_file".to_owned());

    let codes = router
        .route(&tool_event)
        .into_iter()
        .map(|warning| warning.code)
        .collect::<Vec<_>>();

    assert_eq!(git_count.observed(), 2, "the tool write and the event note it caused");
    assert_eq!(codes, ["search/update", "events/write-failed"]);
}

#[test]
fn only_tool_writes_route_to_the_events_service() {
    assert!(OperationRoute::from(&Origin::Tool("fs_write_file".to_owned())).includes(OperationService::Events));
    assert!(!OperationRoute::from(&Origin::Internal("index".to_owned())).includes(OperationService::Events));
}

/// A Git service whose repository for one vault cannot stage: it fails only for an event that names a path there.
#[derive(Clone, Debug)]
struct GitBrokenFor(VaultRootId);

impl VersionsVault for GitBrokenFor {
    fn stage(&self, event: &OperationEvent) -> Result<(), OperationWarning> {
        if event.paths.iter().any(|path| path.root_id() == self.0) {
            Err(OperationWarning {
                code: "git/stage".to_owned(),
                message: "git".to_owned(),
            })
        } else {
            Ok(())
        }
    }
}

/// What the events consumer was told about each of two vaults.
#[derive(Clone, Debug, Default)]
struct PerVaultRecorder(Arc<std::sync::Mutex<Vec<ForOneVault>>>);

/// The warning codes and attempted services the consumer was given for one vault.
type ForOneVault = (Vec<String>, Vec<OperationService>);

impl RecordsEvents for PerVaultRecorder {
    fn record(&self, batch: &EventsBatch<'_>) -> EventsOutcome {
        if let (Ok(mut seen), Ok(first), Ok(second)) = (
            self.0.lock(),
            VaultRootId::try_from(0_usize),
            VaultRootId::try_from(1_usize),
        ) {
            for root in [first, second] {
                seen.push((
                    batch
                        .warnings_for(root)
                        .into_iter()
                        .map(|warning| warning.code)
                        .collect(),
                    batch.attempted_for(root),
                ));
            }
        }
        EventsOutcome::default()
    }
}

#[test]
fn a_warning_reaches_the_events_consumer_with_the_vault_of_the_write_that_raised_it()
-> Result<(), Box<dyn std::error::Error>> {
    let (first, second) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let roots = VaultSet::try_from(vec![
        VaultRoot::try_from(VaultRootInput {
            path: first.path().to_path_buf(),
            managed: true,
            name: Some("first".to_owned()),
        })?,
        VaultRoot::try_from(VaultRootInput {
            path: second.path().to_path_buf(),
            managed: true,
            name: Some("second".to_owned()),
        })?,
    ])?;
    let events = PerVaultRecorder::default();
    let router = OperationRouter::from(OperationRouterConfig {
        indexes: IndexRecorder(InvocationCount::default()),
        operation_log: OperationLogRecorder(InvocationCount::default()),
        versions: GitBrokenFor(VaultRootId::try_from(1_usize)?),
        search: WarningSearch,
        projection: NoProjection,
        events: events.clone(),
    });
    let moved = event_at(
        &roots,
        OpKind::Move,
        Origin::Tool("fs_move_file".to_owned()),
        &["first://a.md", "second://b.md"],
        "move between vaults",
    )?;

    let _ = router.route_batch(&[moved]);

    let seen = events.0.lock().map_err(|_| "poisoned")?.clone();
    assert_eq!(seen.len(), 2);
    let git_and_search = vec![
        OperationService::Index,
        OperationService::OperationLog,
        OperationService::Git,
        OperationService::Search,
    ];
    assert_eq!(
        seen[0],
        (vec!["search/update".to_owned()], git_and_search.clone()),
        "the first vault's Git staged cleanly; only the batch-wide search warning is its own too"
    );
    assert_eq!(
        seen[1],
        (vec!["git/stage".to_owned(), "search/update".to_owned()], git_and_search),
        "the second vault's Git failed, and the search warning belongs to every vault in the batch"
    );
    Ok(())
}
