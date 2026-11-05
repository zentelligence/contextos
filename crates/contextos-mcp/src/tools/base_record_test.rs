use serde_json::json;

use super::*;

fn definition_with_filters(filters: &str) -> Map<String, Value> {
    let mut definition = Map::new();
    definition.insert("filters".to_owned(), json!(filters));
    definition
}

#[test]
fn resolve_target_folder_reads_in_folder_combined_with_other_conditions() -> Result<(), Box<dyn std::error::Error>> {
    let definition = definition_with_filters("file.inFolder(\"memory/tasks\") && file.basename != \"index\"");

    assert_eq!(resolve_target_folder(&definition, None)?, "memory/tasks");
    Ok(())
}

#[test]
fn resolve_target_folder_accepts_single_quoted_literals() -> Result<(), Box<dyn std::error::Error>> {
    let definition = definition_with_filters("file.inFolder('memory/tasks')");

    assert_eq!(resolve_target_folder(&definition, None)?, "memory/tasks");
    Ok(())
}

#[test]
fn resolve_target_folder_reads_a_folder_equality() -> Result<(), Box<dyn std::error::Error>> {
    let definition = definition_with_filters("file.folder == \"memory/decisions\"");

    assert_eq!(resolve_target_folder(&definition, None)?, "memory/decisions");
    Ok(())
}

#[test]
fn resolve_target_folder_reads_a_folder_inside_structured_filters() -> Result<(), Box<dyn std::error::Error>> {
    let mut definition = Map::new();
    definition.insert(
        "filters".to_owned(),
        json!({"and": ["type == \"task\"", "file.inFolder(\"work/tasks\")"]}),
    );

    assert_eq!(resolve_target_folder(&definition, None)?, "work/tasks");
    Ok(())
}

#[test]
fn resolve_target_folder_rejects_filters_that_name_no_folder() {
    let definition = definition_with_filters("file.basename != \"index\"");

    assert!(matches!(
        resolve_target_folder(&definition, None),
        Err(ToolError::Invalid(_))
    ));
}

#[test]
fn resolve_target_folder_uses_the_bare_literal_when_folder_is_omitted() -> Result<(), Box<dyn std::error::Error>> {
    let definition = definition_with_filters("file.inFolder(\"memory/tasks\")");

    let resolved = resolve_target_folder(&definition, None)?;

    assert_eq!(resolved, "memory/tasks");
    Ok(())
}

#[test]
fn resolve_target_folder_appends_a_relative_folder_to_the_literal() -> Result<(), Box<dyn std::error::Error>> {
    let definition = definition_with_filters("file.inFolder(\"zenith/projects/orion/requirements\")");

    let resolved = resolve_target_folder(&definition, Some("functional"))?;

    assert_eq!(resolved, "zenith/projects/orion/requirements/functional");
    Ok(())
}

#[test]
fn resolve_target_folder_treats_a_leading_slash_as_vault_root_relative_ignoring_in_folder()
-> Result<(), Box<dyn std::error::Error>> {
    let definition = definition_with_filters("file.inFolder(\"memory/tasks\")");

    let resolved = resolve_target_folder(&definition, Some("/somewhere/else"))?;

    assert_eq!(resolved, "somewhere/else");
    Ok(())
}

#[test]
fn resolve_target_folder_rejects_a_relative_folder_with_no_in_folder_scope() {
    let definition = Map::new();

    let result = resolve_target_folder(&definition, Some("functional"));

    assert!(matches!(result, Err(ToolError::Invalid(_))));
}

#[test]
fn resolve_target_folder_rejects_an_omitted_folder_with_no_in_folder_scope() {
    let definition = Map::new();

    let result = resolve_target_folder(&definition, None);

    assert!(matches!(result, Err(ToolError::Invalid(_))));
}

#[test]
fn resolve_target_folder_accepts_a_leading_slash_folder_even_with_no_in_folder_scope()
-> Result<(), Box<dyn std::error::Error>> {
    let definition = Map::new();

    let resolved = resolve_target_folder(&definition, Some("/anywhere"))?;

    assert_eq!(resolved, "anywhere");
    Ok(())
}

fn number_of(prefix: &str, key: &str) -> Option<u64> {
    KeyNumber::try_from(SeriesKey { prefix, key })
        .ok()
        .map(|number| number.0)
}

#[test]
fn a_key_in_the_series_yields_its_number_whatever_its_padding() {
    assert_eq!(number_of("REQ", "REQ-001"), Some(1));
    assert_eq!(number_of("REQ", "REQ-42"), Some(42));
    assert_eq!(number_of("REQ", "REQ-1000"), Some(1000));
}

#[test]
fn a_key_of_another_series_yields_no_number() {
    assert_eq!(number_of("REQ", "DEC-005"), None);
    assert_eq!(number_of("REQ", "XREQ-060"), None);
    assert_eq!(number_of("REQ", "REQ2-005"), None);
}

#[test]
fn a_key_whose_suffix_is_not_plain_digits_yields_no_number() {
    for key in [
        "REQ-", "REQ-abc", "REQ-12a", "REQ--5", "REQ-+5", "REQ- 5", "REQ-5.0", "REQ",
    ] {
        assert_eq!(number_of("REQ", key), None, "{key}");
    }
}

#[test]
fn a_number_too_large_for_a_counter_yields_no_number() {
    assert_eq!(number_of("REQ", "REQ-99999999999999999999999"), None);
}

#[test]
fn a_prefix_is_compared_exactly_not_by_case() {
    assert_eq!(number_of("REQ", "req-001"), None);
}

// ---------------------------------------------------------------------------
// Reserving the key in the sidecar before the note is written
// ---------------------------------------------------------------------------

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use contextos_core::{
    DeleteMutation, DeleteOutcome, RestoreMutation, SystemClock, VaultRoot, VaultRootInput, WritesVault,
};
use contextos_fs::{
    FilesystemConfig, FilesystemService, FilesystemServiceConfig, FsError, FsLimits, default_hidden_patterns,
};

type BoxError = Box<dyn std::error::Error>;

const TIMESTAMP: &str = "2026-10-01T10:00:00+10:00";

fn sidecar_text(counter: u64, last_update: &str) -> String {
    format!(
        "---\nkey_prefix: REQ\nkey_format: \"{{key_prefix}}-{{key_id}}\"\nkey_prefixes:\n  REQ: {counter}\nlast_create: t0\nlast_update: {last_update}\n---\n"
    )
}

fn note_text(id: &str) -> String {
    format!("---\nid: {id}\ntitle: Written by another process\n---\nBody.\n")
}

/// A real vault on disk with a `REQ`-keyed base over `requirements/`, and a real filesystem service to write with.
struct Vault {
    dir: tempfile::TempDir,
    roots: VaultSet,
    filesystem: Filesystem,
    service: FilesystemService<SystemClock>,
}

impl Vault {
    fn new() -> Result<Self, BoxError> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("requirements"))?;
        std::fs::write(
            dir.path().join("requirements.base"),
            "filters: file.inFolder(\"requirements\")\nviews:\n- type: table\n  name: All\n",
        )?;
        std::fs::write(dir.path().join("requirements.md"), sidecar_text(0, "t0"))?;
        let root = VaultRoot::try_from(VaultRootInput {
            path: dir.path().to_path_buf(),
            managed: false,
            name: Some("vault".to_owned()),
        })?;
        let roots = VaultSet::try_from(vec![root])?;
        let filesystem = Filesystem::try_from(FilesystemConfig {
            roots: roots.clone(),
            limits: vec![FsLimits {
                max_read_bytes: 1024 * 1024,
                max_batch_files: 50,
            }],
            hidden: vec![default_hidden_patterns()],
            atomic_write_guard: None,
        })?;
        let service = FilesystemService::from(FilesystemServiceConfig {
            filesystem: filesystem.clone(),
            clock: SystemClock,
        });
        Ok(Self {
            dir,
            roots,
            filesystem,
            service,
        })
    }

    fn path(&self, relative: &str) -> std::path::PathBuf {
        self.dir.path().join(relative)
    }

    fn read(&self, relative: &str) -> Result<String, BoxError> {
        Ok(std::fs::read_to_string(self.path(relative))?)
    }

    fn create(&self, writes: &impl WritesVault<Error = FsError>) -> Result<RecordCreateOutcome, ToolError> {
        self.create_with_content("Body.\n", writes)
    }

    fn create_with_content(
        &self,
        content: &str,
        writes: &impl WritesVault<Error = FsError>,
    ) -> Result<RecordCreateOutcome, ToolError> {
        self.create_named(content, None, writes)
    }

    fn create_named(
        &self,
        content: &str,
        filename: Option<&str>,
        writes: &impl WritesVault<Error = FsError>,
    ) -> Result<RecordCreateOutcome, ToolError> {
        let base_path = VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw: self.path("requirements.base").to_str().unwrap_or_default(),
        })?;
        create_record(
            &self.filesystem,
            writes,
            &RecordCreateRequest {
                base_path,
                roots: &self.roots,
                frontmatter: Map::new(),
                content: content.to_owned(),
                folder: None,
                filename,
                prefix: None,
                unique_fields: None,
                timestamp: TIMESTAMP,
            },
        )
    }
}

/// Runs an action just before the nth write is attempted (counting from zero), so a test changes the vault at the
/// exact point another process could, without depending on timing.
struct Interfering<'a> {
    inner: &'a FilesystemService<SystemClock>,
    before_write: Mutex<Box<dyn FnMut(usize) + Send + 'a>>,
    writes_attempted: AtomicUsize,
}

impl<'a> Interfering<'a> {
    fn new(inner: &'a FilesystemService<SystemClock>, before_write: impl FnMut(usize) + Send + 'a) -> Self {
        Self {
            inner,
            before_write: Mutex::new(Box::new(before_write)),
            writes_attempted: AtomicUsize::new(0),
        }
    }

    fn writes_attempted(&self) -> usize {
        self.writes_attempted.load(Ordering::SeqCst)
    }
}

impl WritesVault for Interfering<'_> {
    type Error = FsError;

    fn persist(&self, request: &WriteMutation) -> Result<PipelineResult<WriteOutcome>, FsError> {
        let attempt = self.writes_attempted.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut before) = self.before_write.lock() {
            (*before)(attempt);
        }
        self.inner.persist(request)
    }

    fn restore(&self, request: &RestoreMutation) -> Result<PipelineResult<WriteOutcome>, FsError> {
        self.inner.restore(request)
    }

    fn delete(&self, request: &DeleteMutation) -> Result<PipelineResult<DeleteOutcome>, FsError> {
        self.inner.delete(request)
    }
}

#[test]
fn a_sidecar_changed_after_it_was_read_is_read_again_and_the_record_is_still_created() -> Result<(), BoxError> {
    let vault = Vault::new()?;
    let sidecar = vault.path("requirements.md");
    let writes = Interfering::new(&vault.service, |attempt| {
        if attempt == 0 {
            // Another process rewrites the sidecar between this call's read and its write.
            let _ = std::fs::write(&sidecar, sidecar_text(0, "t-other"));
        }
    });

    let outcome = vault.create(&writes).map_err(|error| error.to_string())?;

    assert_eq!(outcome.key.as_deref(), Some("REQ-001"));
    assert!(vault.read("requirements/req-001.md")?.contains("id: REQ-001"));
    assert!(vault.read("requirements.md")?.contains("REQ: 1"));
    // The refused reservation, the reservation that succeeded, then the note.
    assert_eq!(writes.writes_attempted(), 3);
    Ok(())
}

#[test]
fn a_key_another_process_took_in_the_meantime_is_skipped_and_the_next_one_assigned() -> Result<(), BoxError> {
    let vault = Vault::new()?;
    let (sidecar, other_note) = (vault.path("requirements.md"), vault.path("requirements/req-001.md"));
    let writes = Interfering::new(&vault.service, |attempt| {
        if attempt == 0 {
            // A whole creation by another process lands between this call's read and its write.
            let _ = std::fs::write(&other_note, note_text("REQ-001"));
            let _ = std::fs::write(&sidecar, sidecar_text(1, "t-other"));
        }
    });

    let outcome = vault.create(&writes).map_err(|error| error.to_string())?;

    assert_eq!(outcome.key.as_deref(), Some("REQ-002"));
    assert!(
        vault
            .read("requirements/req-001.md")?
            .contains("Written by another process")
    );
    assert!(vault.read("requirements/req-002.md")?.contains("id: REQ-002"));
    assert!(vault.read("requirements.md")?.contains("REQ: 2"));
    Ok(())
}

#[test]
fn a_sidecar_that_keeps_changing_fails_with_a_conflict_and_writes_no_note() -> Result<(), BoxError> {
    let vault = Vault::new()?;
    let sidecar = vault.path("requirements.md");
    let writes = Interfering::new(&vault.service, |attempt| {
        let _ = std::fs::write(&sidecar, sidecar_text(0, &format!("t-other-{attempt}")));
    });

    let result = vault.create(&writes);

    assert!(
        matches!(result, Err(ToolError::Filesystem(FsError::Conflict { .. }))),
        "expected a conflict"
    );
    assert_eq!(writes.writes_attempted(), RESERVATION_ATTEMPTS);
    assert_eq!(std::fs::read_dir(vault.path("requirements"))?.count(), 0);
    assert!(vault.read("requirements.md")?.contains("REQ: 0"));
    Ok(())
}

#[test]
fn the_key_is_reserved_in_the_sidecar_before_the_note_is_written() -> Result<(), BoxError> {
    let vault = Vault::new()?;
    let observed = Mutex::new(None);
    let (sidecar, note) = (vault.path("requirements.md"), vault.path("requirements/req-001.md"));
    let writes = Interfering::new(&vault.service, |attempt| {
        if attempt == 1
            && let Ok(mut slot) = observed.lock()
        {
            *slot = Some((std::fs::read_to_string(&sidecar).unwrap_or_default(), note.exists()));
        }
    });

    vault.create(&writes).map_err(|error| error.to_string())?;

    let observed = observed.lock().map_err(|error| error.to_string())?.clone();
    let (sidecar_at_note_write, note_existed) = observed.ok_or("the note write was never attempted")?;
    assert!(sidecar_at_note_write.contains("REQ: 1"), "{sidecar_at_note_write}");
    assert!(!note_existed, "the note must not exist before its key is reserved");
    Ok(())
}

#[test]
fn a_note_that_cannot_be_written_after_its_key_was_reserved_names_the_reserved_key() -> Result<(), BoxError> {
    let vault = Vault::new()?;
    let target = vault.path("requirements/req-001.md");
    let writes = Interfering::new(&vault.service, |attempt| {
        if attempt == 1 {
            // Something takes the note's path after the key was reserved and before the note is written.
            let _ = std::fs::write(&target, "taken\n");
        }
    });

    let result = vault.create(&writes);

    assert!(
        matches!(&result, Err(ToolError::KeyReservedNoteNotWritten { key, .. }) if key == "REQ-001"),
        "expected the reserved key to be named"
    );
    // The counter already counts the key, so the series has a gap at REQ-001 and the next creation skips it.
    assert!(vault.read("requirements.md")?.contains("REQ: 1"));
    std::fs::remove_file(&target)?;
    let next = vault.create(&vault.service).map_err(|error| error.to_string())?;
    assert_eq!(next.key.as_deref(), Some("REQ-002"));
    Ok(())
}

#[test]
fn an_existing_note_at_the_target_is_refused_before_any_key_is_reserved() -> Result<(), BoxError> {
    let vault = Vault::new()?;
    std::fs::write(vault.path("requirements/req-001.md"), "already here\n")?;
    let writes = Interfering::new(&vault.service, |_| {});

    let result = vault.create(&writes);

    assert!(matches!(result, Err(ToolError::Invalid(_))), "expected a refusal");
    assert_eq!(
        writes.writes_attempted(),
        0,
        "nothing may be written, the sidecar included"
    );
    assert!(vault.read("requirements.md")?.contains("REQ: 0"));
    assert_eq!(vault.read("requirements/req-001.md")?, "already here\n");
    Ok(())
}

#[test]
fn content_the_note_cannot_take_is_refused_before_any_key_is_reserved() -> Result<(), BoxError> {
    let vault = Vault::new()?;
    let writes = Interfering::new(&vault.service, |_| {});

    // A callout with no closing bracket is rejected when the note is validated.
    let result = vault.create_with_content("> [!note\nBody.\n", &writes);

    assert!(result.is_err(), "the malformed callout must be refused");
    assert!(
        !matches!(result, Err(ToolError::KeyReservedNoteNotWritten { .. })),
        "a refusal of the caller's input is not a reserved key"
    );
    assert_eq!(
        writes.writes_attempted(),
        0,
        "nothing may be written, the sidecar included"
    );
    assert!(vault.read("requirements.md")?.contains("REQ: 0"));
    Ok(())
}

#[test]
fn a_catch_up_warning_survives_a_retry() -> Result<(), BoxError> {
    let vault = Vault::new()?;
    std::fs::write(vault.path("requirements/held.md"), note_text("REQ-001"))?;
    let sidecar = vault.path("requirements.md");
    let writes = Interfering::new(&vault.service, |attempt| {
        if attempt == 0 {
            let _ = std::fs::write(&sidecar, sidecar_text(0, "t-other"));
        }
    });

    let outcome = vault.create(&writes).map_err(|error| error.to_string())?;

    assert_eq!(outcome.key.as_deref(), Some("REQ-002"));
    assert_eq!(outcome.sidecar_warnings.len(), 1, "{:?}", outcome.sidecar_warnings);
    assert!(outcome.sidecar_warnings[0].contains("REQ-001 already existed"));
    Ok(())
}

#[test]
fn a_directory_at_the_target_is_refused_before_any_key_is_reserved() -> Result<(), BoxError> {
    let vault = Vault::new()?;
    std::fs::create_dir_all(vault.path("requirements/req-001.md"))?;
    let writes = Interfering::new(&vault.service, |_| {});

    let result = vault.create(&writes);

    assert!(matches!(result, Err(ToolError::Invalid(_))), "expected a refusal");
    assert_eq!(writes.writes_attempted(), 0);
    assert!(vault.read("requirements.md")?.contains("REQ: 0"));
    Ok(())
}

#[test]
fn a_sidecar_created_by_another_process_after_none_was_found_is_read_and_the_record_still_created()
-> Result<(), BoxError> {
    let vault = Vault::new()?;
    std::fs::remove_file(vault.path("requirements.md"))?;
    let sidecar = vault.path("requirements.md");
    let writes = Interfering::new(&vault.service, |attempt| {
        if attempt == 0 {
            // No sidecar existed when this call read, so its first write expects none; another process creates one.
            let _ = std::fs::write(
                &sidecar,
                "---\nkey_format: FILEPATH\nlast_id: 0\nlast_create: t0\nlast_update: t-other\n---\n",
            );
        }
    });

    let outcome = vault
        .create_named("Body.\n", Some("note.md"), &writes)
        .map_err(|error| error.to_string())?;

    assert_eq!(outcome.key, None);
    assert!(vault.read("requirements/note.md")?.contains("Body."));
    assert_eq!(writes.writes_attempted(), 3);
    Ok(())
}
