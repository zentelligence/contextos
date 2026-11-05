use std::process::Command;

use tempfile::tempdir;

#[path = "support/doctor_output.rs"]
mod doctor_output;
use doctor_output::resolution_subjects;

/// A run against a throwaway vault must keep its derived state inside the test's fixture: the default state root is
/// under the home directory, so a leak would otherwise accumulate in the operator's real `~/.contextos/vaults`.
fn assert_left_no_state_in(home: &std::path::Path) {
    assert!(
        !home.join(".contextos").exists(),
        "doctor wrote derived state under the home directory"
    );
}

#[test]
fn doctor_validates_configuration_and_reports_a_healthy_vault() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let home = tempdir()?;
    let vault = fixture.path().join("vault");
    let state = fixture.path().join("state");
    std::fs::create_dir(&vault)?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r"
            [[vault]]
            path = {vault:?}
            state_directory = {state:?}
            [vault.index_md]
            enabled = false
            [vault.git]
            enabled = false
            ",
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args(["--config", config.to_str().ok_or("non-UTF-8 config")?, "doctor"])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    assert_left_no_state_in(home.path());

    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("Configuration | PASS"));
    // The doctor reports the resolved vault path, not the raw one
    // constructed here: they can differ, for example under Windows 8.3
    // short-name generation.
    let resolved_vault = dunce::canonicalize(&vault)?;
    assert!(stdout.contains(resolved_vault.to_str().ok_or("non-UTF-8 vault")?));
    assert!(stdout.contains("Managed indexes | PASS | disabled"));
    assert!(stdout.contains("Git recovery | PASS | disabled"));
    Ok(())
}

#[test]
fn doctor_reports_semantic_search_disabled_by_default() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let home = tempdir()?;
    let vault = fixture.path().join("vault");
    let state = fixture.path().join("state");
    std::fs::create_dir(&vault)?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r"
            [[vault]]
            path = {vault:?}
            state_directory = {state:?}
            [vault.index_md]
            enabled = false
            [vault.git]
            enabled = false
            ",
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args(["--config", config.to_str().ok_or("non-UTF-8 config")?, "doctor"])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    assert_left_no_state_in(home.path());

    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("Semantic search | PASS | disabled"));
    Ok(())
}

#[test]
fn doctor_fails_with_an_action_when_semantic_is_enabled_without_a_model_directory()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let home = tempdir()?;
    let vault = fixture.path().join("vault");
    let state = fixture.path().join("state");
    std::fs::create_dir(&vault)?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r"
            [[vault]]
            path = {vault:?}
            state_directory = {state:?}
            [vault.index_md]
            enabled = false
            [vault.git]
            enabled = false
            [vault.search]
            semantic = true
            ",
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args(["--config", config.to_str().ok_or("non-UTF-8 config")?, "doctor"])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    assert_left_no_state_in(home.path());

    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("Semantic search | FAIL"));
    assert!(stdout.contains("model_directory"));
    assert!(stdout.contains("Action: Correct [vault.search.embedding]"));
    Ok(())
}

#[test]
fn doctor_returns_failure_with_an_action_for_missing_indexes() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let home = tempdir()?;
    let vault = fixture.path().join("vault");
    let state = fixture.path().join("state");
    std::fs::create_dir(&vault)?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r"
            [[vault]]
            path = {vault:?}
            state_directory = {state:?}
            [vault.git]
            enabled = false
            ",
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args(["doctor", "--config", config.to_str().ok_or("non-UTF-8 config")?])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    assert_left_no_state_in(home.path());

    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("Managed indexes | FAIL"));
    assert!(stdout.contains("Action: Call vault_index_rebuild"));
    Ok(())
}

#[test]
fn doctor_resolve_flag_rebuilds_a_stale_index_and_reports_it_healthy() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let home = tempdir()?;
    let vault = fixture.path().join("vault");
    let state = fixture.path().join("state");
    std::fs::create_dir(&vault)?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r"
            [[vault]]
            path = {vault:?}
            state_directory = {state:?}
            [vault.git]
            enabled = false
            ",
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args([
            "--config",
            config.to_str().ok_or("non-UTF-8 config")?,
            "doctor",
            "--resolve",
        ])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    assert_left_no_state_in(home.path());

    assert!(output.status.success(), "{stdout}");
    assert!(vault.join("index.md").exists());
    assert!(stdout.contains("Resolved: Managed indexes"));
    assert!(stdout.contains("Managed indexes | PASS"));
    Ok(())
}

#[test]
fn doctor_resolve_dry_run_previews_without_writing() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let home = tempdir()?;
    let vault = fixture.path().join("vault");
    let state = fixture.path().join("state");
    std::fs::create_dir(&vault)?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r"
            [[vault]]
            path = {vault:?}
            state_directory = {state:?}
            [vault.git]
            enabled = false
            ",
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args([
            "--config",
            config.to_str().ok_or("non-UTF-8 config")?,
            "doctor",
            "--resolve",
            "--dry-run",
        ])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    assert_left_no_state_in(home.path());

    assert!(!output.status.success(), "{stdout}");
    assert!(!vault.join("index.md").exists());
    assert!(!state.exists(), "a dry run created the vault's derived state directory");
    assert!(stdout.contains("Would resolve: Managed indexes"));
    assert!(stdout.contains("Managed indexes | FAIL"));
    Ok(())
}

#[test]
fn doctor_resolve_dry_run_leaves_a_versioned_vault_without_commits_untouched() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = tempdir()?;
    let home = tempdir()?;
    let vault = fixture.path().join("vault");
    let state = fixture.path().join("state");
    std::fs::create_dir(&vault)?;
    std::fs::write(vault.join("note.md"), "# Note\n")?;
    // A repository with no commits yet is what starting a server initialises (an ignore file and a first commit), so
    // it is the case where building the full server to preview a resolution would write.
    let repository = git2::Repository::init(&vault)?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r"
            [[vault]]
            path = {vault:?}
            state_directory = {state:?}
            ",
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args([
            "--config",
            config.to_str().ok_or("non-UTF-8 config")?,
            "doctor",
            "--resolve",
            "--dry-run",
        ])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    assert_left_no_state_in(home.path());

    assert!(stdout.contains("Would resolve: Managed indexes"), "{stdout}");
    assert!(stdout.contains("Would resolve: Git recovery"), "{stdout}");
    assert!(
        stdout.contains("Git recovery | FAIL | enabled, but the vault's Git repository has no commits yet"),
        "{stdout}"
    );
    assert!(stdout.contains("Uncommitted MCP changes | PASS"), "{stdout}");
    assert!(
        repository.head().is_err(),
        "a dry run committed to the vault's repository"
    );
    assert!(
        !vault.join(".gitignore").exists(),
        "a dry run wrote an ignore file into the vault"
    );
    assert!(!vault.join("index.md").exists());
    assert!(!state.exists(), "a dry run created the vault's derived state directory");
    Ok(())
}

#[test]
fn doctor_resolve_gives_a_repository_without_commits_its_first_commit_as_its_dry_run_previewed()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let home = tempdir()?;
    let vault = fixture.path().join("vault");
    let state = fixture.path().join("state");
    std::fs::create_dir(&vault)?;
    std::fs::write(vault.join("note.md"), "# Note\n")?;
    let repository = git2::Repository::init(&vault)?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r"
            [[vault]]
            path = {vault:?}
            state_directory = {state:?}
            ",
        ),
    )?;
    let run = |extra: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
        let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
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
    assert_left_no_state_in(home.path());

    assert!(previewed.contains("Would resolve: Git recovery"), "{previewed}");
    assert!(resolved.contains("Resolved: Git recovery"), "{resolved}");
    assert_eq!(resolution_subjects(&previewed), resolution_subjects(&resolved));
    assert!(resolved.contains("Git recovery | PASS"), "{resolved}");
    assert!(repository.head()?.peel_to_commit().is_ok());
    let mut options = git2::StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    let dirty = repository
        .statuses(Some(&mut options))?
        .iter()
        .filter_map(|entry| entry.path().ok().map(str::to_owned))
        .collect::<Vec<_>>();
    assert!(dirty.is_empty(), "the resolution left the tree dirty: {dirty:?}");
    Ok(())
}

/// A vault whose search state cannot be opened is still resolved: search is downstream of the resolution's writes, so
/// its failure is a warning beside the outcomes rather than an abort, and the run does exactly what its dry run
/// previewed.
#[test]
fn doctor_resolve_resolves_a_vault_whose_search_cannot_be_opened_and_warns_about_it()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let home = tempdir()?;
    let vault = fixture.path().join("vault");
    let state = fixture.path().join("state");
    std::fs::create_dir(&vault)?;
    let config = fixture.path().join("config.toml");
    // Semantic search on the local provider with no model directory cannot be opened.
    std::fs::write(
        &config,
        format!(
            r"
            [[vault]]
            path = {vault:?}
            state_directory = {state:?}
            [vault.git]
            enabled = false
            [vault.search]
            semantic = true
            ",
        ),
    )?;
    let run = |extra: &[&str]| -> Result<(String, String), Box<dyn std::error::Error>> {
        let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
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
        Ok((String::from_utf8(output.stdout)?, String::from_utf8(output.stderr)?))
    };

    let (previewed, _) = run(&["--dry-run"])?;
    let (resolved, diagnostics) = run(&[])?;
    assert_left_no_state_in(home.path());

    assert!(previewed.contains("Would resolve: Managed indexes"), "{previewed}");
    assert!(
        resolved.contains("Resolved: Managed indexes"),
        "{resolved}\n{diagnostics}"
    );
    assert_eq!(resolution_subjects(&previewed), resolution_subjects(&resolved));
    assert!(vault.join("index.md").exists());
    assert!(
        resolved.contains("Warning: search could not be opened") && resolved.contains("contextos index"),
        "{resolved}"
    );
    Ok(())
}

#[test]
fn doctor_resolve_labels_a_remediation_that_failed_as_not_resolved() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let home = tempdir()?;
    let vault = fixture.path().join("vault");
    let state = fixture.path().join("state");
    std::fs::create_dir(&vault)?;
    std::fs::write(vault.join("note.md"), "# Note\n")?;
    git2::Repository::init(&vault)?;
    // Another Git process holding the index lock makes the first commit impossible.
    std::fs::write(vault.join(".git/index.lock"), "")?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r"
            [[vault]]
            path = {vault:?}
            state_directory = {state:?}
            [vault.index_md]
            enabled = false
            ",
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args([
            "--config",
            config.to_str().ok_or("non-UTF-8 config")?,
            "doctor",
            "--resolve",
        ])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    assert_left_no_state_in(home.path());

    assert!(stdout.contains("Not resolved: Git recovery"), "{stdout}");
    assert!(!stdout.contains("Would resolve:"), "{stdout}");
    Ok(())
}
