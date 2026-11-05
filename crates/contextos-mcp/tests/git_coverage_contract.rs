//! Every change an MCP tool makes to a Git-initialised vault is staged as it is made and committed within the
//! debounce window, so with all mutations arriving through MCP nothing is ever left uncommitted. Each scenario
//! flushes the pending batch, which is what the debounce timer does, and asserts one new commit and a clean tree.

mod support;
use serde_json::{Value, json};
use support::doctor_output::resolution_subjects;
use support::git_harness::{BoxError, assert_one_commit_and_clean_tree, harness};

#[tokio::test]
async fn a_whole_vault_index_rebuild_is_one_commit_and_leaves_a_clean_tree() -> Result<(), BoxError> {
    let harness = harness().await?;
    let before = harness.commit_count().await?;

    harness.structured("vault_index_rebuild", json!({"path": "."})).await?;

    assert_one_commit_and_clean_tree(&harness, before, "vault_index_rebuild").await
}

#[tokio::test]
async fn a_manual_log_entry_is_committed_rather_than_left_unstaged() -> Result<(), BoxError> {
    let harness = harness().await?;
    let before = harness.commit_count().await?;

    harness
        .structured(
            "vault_log_append",
            json!({"entry": "Reviewed the vault", "files": ["alpha/note.md"]}),
        )
        .await?;

    assert_one_commit_and_clean_tree(&harness, before, "vault_log_append").await
}

#[tokio::test]
async fn resolving_a_stale_managed_index_through_the_doctor_is_committed() -> Result<(), BoxError> {
    let harness = harness().await?;
    let before = harness.commit_count().await?;

    let resolved = harness.structured("doctor_resolve", json!({})).await?;

    let outcomes = resolved
        .get("outcomes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        outcomes.iter().any(|outcome| {
            outcome.get("subject") == Some(&json!("Managed indexes")) && outcome.get("resolved") == Some(&json!(true))
        }),
        "the doctor should have rebuilt the managed indexes: {outcomes:?}"
    );
    assert_one_commit_and_clean_tree(&harness, before, "doctor_resolve").await
}

#[tokio::test]
async fn a_failed_commit_is_visible_in_git_status_until_a_commit_succeeds() -> Result<(), BoxError> {
    let harness = harness().await?;
    harness
        .structured(
            "fs_write_file",
            json!({"path": "alpha/created.md", "content": "# Created\n"}),
        )
        .await?;
    let lock = harness.vault.path().join(".git/refs/heads/main.lock");
    std::fs::write(&lock, "")?;

    assert!(
        harness.server.flush_git().is_err(),
        "the commit should fail while the branch is locked"
    );
    let failing = harness.structured("git_status", json!({})).await?;
    std::fs::remove_file(&lock)?;
    harness.server.flush_git()?;
    let recovered = harness.structured("git_status", json!({})).await?;

    let failure = failing
        .get("commit_failure")
        .ok_or("git_status should report the failed commit")?;
    assert_eq!(failure.get("code"), Some(&json!("git/commit")));
    assert_eq!(failure.get("attempts"), Some(&json!(1)));
    assert!(
        failing
            .get("pending_paths")
            .and_then(Value::as_array)
            .is_some_and(|paths| !paths.is_empty())
    );
    assert_eq!(recovered.get("commit_failure"), None);
    Ok(())
}

/// Backdates every pending marker, as if the changes had been waiting for `seconds`.
fn age_pending_markers(vault: &std::path::Path, seconds: u64) -> Result<(), BoxError> {
    let stamped = std::time::SystemTime::now() - std::time::Duration::from_secs(seconds);
    for entry in std::fs::read_dir(vault.join(".git/contextos-mcp/pending"))? {
        let path = entry?.path();
        if path.extension().and_then(std::ffi::OsStr::to_str) == Some("pending") {
            std::fs::File::options().write(true).open(path)?.set_modified(stamped)?;
        }
    }
    Ok(())
}

fn check<'a>(report: &'a Value, subject: &str) -> Option<&'a Value> {
    report
        .get("checks")
        .and_then(Value::as_array)?
        .iter()
        .find(|check| check.get("subject").and_then(Value::as_str) == Some(subject))
}

#[tokio::test]
async fn the_doctor_fails_on_overdue_tool_changes_and_resolves_them_by_committing_only_those() -> Result<(), BoxError> {
    let harness = harness().await?;
    // Indexes are brought up to date and committed first, so the commit check is the only finding.
    harness.structured("vault_index_rebuild", json!({"path": "."})).await?;
    harness.settle()?;
    let before = harness.commit_count().await?;
    harness
        .structured(
            "fs_write_file",
            json!({"path": "alpha/created.md", "content": "# Created\n"}),
        )
        .await?;
    std::fs::write(harness.vault.path().join("alpha/note.md"), "# Edited in Obsidian\n")?;
    let clean = harness.structured("doctor", json!({})).await?;
    age_pending_markers(harness.vault.path(), 3600)?;

    let overdue = harness.structured("doctor", json!({})).await?;
    let resolved = harness.structured("doctor_resolve", json!({})).await?;

    assert_eq!(
        check(&clean, "Uncommitted MCP changes").and_then(|check| check.get("status")),
        Some(&json!("pass")),
        "changes inside the debounce window are not a fault: {clean}"
    );
    let failing = check(&overdue, "Uncommitted MCP changes").ok_or("the doctor should have a commit check")?;
    assert_eq!(failing.get("status"), Some(&json!("fail")));
    assert!(
        failing
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("outside MCP"))
    );
    let outcomes = resolved
        .get("outcomes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        outcomes.iter().any(|outcome| {
            outcome.get("subject") == Some(&json!("Uncommitted MCP changes"))
                && outcome.get("resolved") == Some(&json!(true))
        }),
        "{outcomes:?}"
    );
    assert_eq!(harness.commit_count().await?, before + 1);
    assert_eq!(
        harness.dirty_paths().await?,
        vec!["unstaged: alpha/note.md".to_owned()],
        "the operator's own edit must be left alone"
    );
    Ok(())
}

#[tokio::test]
async fn a_resolution_that_cannot_commit_says_it_did_not_resolve_anything() -> Result<(), BoxError> {
    let harness = harness().await?;
    harness
        .structured(
            "fs_write_file",
            json!({"path": "alpha/created.md", "content": "# Created\n"}),
        )
        .await?;
    age_pending_markers(harness.vault.path(), 3600)?;
    std::fs::write(harness.vault.path().join(".git/refs/heads/main.lock"), "")?;

    let resolved = harness.structured("doctor_resolve", json!({})).await?;

    let outcomes = resolved
        .get("outcomes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let outcome = outcomes
        .iter()
        .find(|outcome| outcome.get("subject") == Some(&json!("Uncommitted MCP changes")))
        .ok_or("the doctor should have attempted the commit")?;
    assert_eq!(outcome.get("resolved"), Some(&json!(false)), "{outcome}");
    assert!(
        !harness.pending_paths().await?.is_empty(),
        "the paths must stay pending for a retry"
    );
    Ok(())
}

/// `contextos doctor --resolve` is a one-shot process: it exits as soon as it has resolved, long before any debounce
/// timer could fire, so it must commit what it changed itself.
#[tokio::test]
async fn a_one_shot_doctor_resolution_commits_what_it_changed_before_it_returns() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("alpha"))?;
    std::fs::write(vault.path().join("alpha/note.md"), "# Note\n")?;
    let repository = git2::Repository::init(vault.path())?;
    std::fs::write(vault.path().join(".gitignore"), ".contextos/\n")?;
    let mut index = repository.index()?;
    index.add_all(["*"], git2::IndexAddOption::DEFAULT, None)?;
    index.write()?;
    let tree = repository.find_tree(index.write_tree()?)?;
    let signature = git2::Signature::now("Fixture", "fixture@example.invalid")?;
    repository.commit(Some("HEAD"), &signature, &signature, "baseline", &tree, &[])?;
    let commits_before = repository
        .revwalk()
        .and_then(|mut walk| walk.push_head().map(|()| walk.count()))?;

    let outcomes = contextos_mcp::resolve_for_cli(support::scoped_config(vec![vault.path().to_path_buf()])?)
        .await?
        .outcomes;

    assert!(
        outcomes
            .iter()
            .any(|outcome| outcome.subject == "Managed indexes" && outcome.resolved),
        "{outcomes:?}"
    );
    let mut options = git2::StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = repository.statuses(Some(&mut options))?;
    let mut dirty = Vec::new();
    for entry in statuses.iter() {
        if let Ok(path) = entry.path() {
            dirty.push(path.to_owned());
        }
    }
    assert!(dirty.is_empty(), "the resolution left the tree dirty: {dirty:?}");
    let commits_after = repository
        .revwalk()
        .and_then(|mut walk| walk.push_head().map(|()| walk.count()))?;
    let mut walk = repository.revwalk()?;
    walk.push_head()?;
    let mut summaries = Vec::new();
    for id in walk {
        let commit = repository.find_commit(id?)?;
        let files = repository
            .diff_tree_to_tree(
                commit.parent(0).ok().and_then(|parent| parent.tree().ok()).as_ref(),
                Some(&commit.tree()?),
                None,
            )?
            .deltas()
            .filter_map(|delta| delta.new_file().path().map(|path| path.display().to_string()))
            .collect::<Vec<_>>();
        summaries.push(format!(
            "{} => {files:?}",
            commit.message().unwrap_or("").lines().next().unwrap_or("")
        ));
    }
    assert_eq!(
        commits_after,
        commits_before + 1,
        "one commit for the whole resolution: {summaries:#?}"
    );
    Ok(())
}

/// A dry run is the preview an operator approves, so `contextos doctor --resolve` must act on exactly what its dry
/// run listed, and report every write it makes: changes an MCP tool left uncommitted are committed by the
/// resolution, as a reported outcome, not silently by the process starting up before the doctor looks.
#[test]
fn the_cli_resolution_commits_left_over_mcp_changes_as_the_outcome_its_dry_run_previewed() -> Result<(), BoxError> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let support::git_harness::Harness { vault, server } = runtime.block_on(async {
        let harness = harness().await?;
        harness
            .structured(
                "fs_write_file",
                json!({"path": "alpha/created.md", "content": "# Created\n"}),
            )
            .await?;
        Ok::<_, BoxError>(harness)
    })?;
    // The server that made the change stops before committing it, as one that crashed or was killed would.
    drop(server);
    drop(runtime);
    age_pending_markers(vault.path(), 3600)?;
    let fixture = tempfile::tempdir()?;
    let home = tempfile::tempdir()?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[[vault]]\npath = {:?}\nstate_directory = \".contextos\"\n",
            vault.path()
        ),
    )?;
    let run = |extra: &[&str]| -> Result<String, BoxError> {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_contextos"))
            .args([
                "--config",
                config.to_str().ok_or("non-UTF-8 config")?,
                "doctor",
                "--resolve",
            ])
            .args(extra)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .output()?;
        Ok(String::from_utf8(output.stdout)?)
    };

    let previewed = run(&["--dry-run"])?;
    let resolved = run(&[])?;

    assert!(
        previewed.contains("Would resolve: Uncommitted MCP changes"),
        "{previewed}"
    );
    assert!(resolved.contains("Resolved: Uncommitted MCP changes"), "{resolved}");
    assert_eq!(resolution_subjects(&previewed), resolution_subjects(&resolved));
    let repository = git2::Repository::open(vault.path())?;
    let mut options = git2::StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    assert!(
        repository.statuses(Some(&mut options))?.is_empty(),
        "the resolution left the tree dirty"
    );
    Ok(())
}
