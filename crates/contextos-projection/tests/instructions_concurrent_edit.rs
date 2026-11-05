//! The instructions file is the operator's own: an edit saved between the server reading it and replacing it survives.

mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{TestResult, world};
use contextos_projection::{HostKind, InstructionsObserver, InstructionsStep};
use tempfile::tempdir;

const GLOBAL: &str = "registry/instructions/global.md";
const BEGIN: &str = "<!-- contextos:orientation:begin -->";
const END: &str = "<!-- contextos:orientation:end -->";

/// Overwrites the Claude instructions file the first time the server is about to replace it, as an operator saving
/// from their editor at that moment would.
struct SavesFromAnEditor {
    file: PathBuf,
    replacement: &'static str,
    reached: AtomicUsize,
}

impl InstructionsObserver for SavesFromAnEditor {
    fn reached(&self, host: HostKind, step: InstructionsStep) {
        if host == HostKind::ClaudeCode
            && step == InstructionsStep::Planned
            && self.reached.fetch_add(1, Ordering::SeqCst) == 0
        {
            let _ = fs::write(&self.file, self.replacement);
        }
    }
}

#[test]
fn an_edit_saved_between_the_read_and_the_replace_survives_and_the_block_is_recomputed_from_it() -> TestResult {
    let world = world(1)?;
    let home = tempdir()?;
    fs::create_dir_all(home.path().join(".claude"))?;
    fs::create_dir_all(home.path().join(".codex"))?;
    let claude = home.path().join(".claude/CLAUDE.md");
    let codex = home.path().join(".codex/AGENTS.md");
    fs::write(&claude, "mine\n")?;
    world.write_vault(0, GLOBAL, "Orient.\n")?;
    let observer = Arc::new(SavesFromAnEditor {
        file: claude.clone(),
        replacement: "mine\nsaved just now\n",
        reached: AtomicUsize::new(0),
    });
    let service = world
        .service_with_instructions("config-one", &claude, &codex)?
        .with_instructions_observer(observer.clone());

    let warnings = service.refresh();

    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(
        observer.reached.load(Ordering::SeqCst) >= 1,
        "the observer was never reached"
    );
    let result = fs::read_to_string(&claude)?;
    assert!(
        result.starts_with("mine\nsaved just now\n"),
        "the operator's edit was lost: {result:?}"
    );
    assert_eq!(result.matches(BEGIN).count(), 1, "{result:?}");
    assert!(result.ends_with(&format!("{BEGIN}\nOrient.\n{END}\n")), "{result:?}");
    Ok(())
}

/// Overwrites the Claude instructions file every time the server is about to replace it, each time with different
/// bytes, as an operator who keeps saving would.
struct KeepsSaving {
    file: PathBuf,
    reached: AtomicUsize,
}

impl InstructionsObserver for KeepsSaving {
    fn reached(&self, host: HostKind, step: InstructionsStep) {
        if host == HostKind::ClaudeCode && step == InstructionsStep::Planned {
            let save = self.reached.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = fs::write(&self.file, format!("mine\nsave number {save}\n"));
        }
    }
}

#[test]
fn a_file_that_changes_on_every_attempt_keeps_the_operators_last_bytes_and_warns_that_the_write_failed() -> TestResult {
    let world = world(1)?;
    let home = tempdir()?;
    fs::create_dir_all(home.path().join(".claude"))?;
    fs::create_dir_all(home.path().join(".codex"))?;
    let claude = home.path().join(".claude/CLAUDE.md");
    let codex = home.path().join(".codex/AGENTS.md");
    fs::write(&claude, "mine\n")?;
    world.write_vault(0, GLOBAL, "Orient.\n")?;
    let observer = Arc::new(KeepsSaving {
        file: claude.clone(),
        reached: AtomicUsize::new(0),
    });
    let service = world
        .service_with_instructions("config-one", &claude, &codex)?
        .with_instructions_observer(observer.clone());

    let warnings = service.refresh();

    let claude_warnings = warnings
        .iter()
        .filter(|warning| warning.message.contains("claude"))
        .collect::<Vec<_>>();
    assert_eq!(claude_warnings.len(), 1, "{warnings:?}");
    assert_eq!(claude_warnings[0].code, "projection/write-failed", "{warnings:?}");
    let attempts = observer.reached.load(Ordering::SeqCst);
    assert!(attempts >= 2, "the file was never retried: {attempts}");
    assert_eq!(
        fs::read_to_string(&claude)?,
        format!("mine\nsave number {attempts}\n"),
        "the operator's last save must be left exactly as it is"
    );
    Ok(())
}
