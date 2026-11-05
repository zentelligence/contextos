#![forbid(unsafe_code)]

//! Vault events: what the server fires and clients record, in one note shape.
//!
//! This crate holds the pure logic behind events and none of the wiring: the kinds the server fires ([`ServerKind`]),
//! the shape and rendering of an event note ([`EventRecord`]), the definitions a vault declares for the kinds clients
//! may record ([`KindDefinition`], [`KindRegistry`]) and the validation of a recorded event against one, the
//! last-written task table and its comparison ([`TaskTable`]), and the classification of secondary-service warnings
//! into failures and notices with the transitions they cause ([`CurrentFailures`]). Time is always passed in.

mod attention;
mod definition;
mod error;
mod field;
mod health;
mod kinds;
mod lifecycle;
mod member;
mod persist;
mod record;
mod retention;
mod tasks;
mod validate;
mod warning_codes;

pub use attention::{
    JOB_RECOVERED, NoSubject, NotJudgeable, Occurrence, SubjectFields, SubjectIdentity, TASK_MOVED, VAULT_RECOVERED,
    link_target,
};
pub use definition::{KindDefinition, KindDefinitionSource, KindRegistry, SubjectRule};
pub use error::{DefinitionRejection, EventsError, FieldRejection, SeriesRejection, SubjectRejection};
pub use field::{FieldName, FieldValue, Scalar};
pub use health::{CurrentFailures, HealthChange};
pub use kinds::{KindName, SCAFFOLD_KINDS, ServerKind, Severity, is_shipped_kind};
pub use lifecycle::TaskLifecycles;
pub use member::MemberStateChange;
pub use record::{EventRecord, EventSubject, resolve_entity};
pub use retention::{RetentionPolicy, SweepDay, SweepNote, Verdict};
pub use tasks::{NotATask, TaskChange, TaskSite, TaskTable, TrackedTask};
pub use validate::{RecordContext, RecordRequest, SubjectNote, SubjectRecord};
pub use warning_codes::{WRITE_FAILED_CODE, WarningClass, service_name};
