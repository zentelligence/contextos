//! `contextos-web config` is wired as a real subcommand: its help text is
//! reachable from the binary, closing the gap that `contextos-web` had no
//! config-creation path at all (unlike `contextos config`). The wizard's
//! own behaviour is covered by `src/config_interview_test.rs` against a
//! scripted interviewer; this only proves the CLI surface exists.

use std::process::Command;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[test]
fn the_config_subcommand_help_is_reachable_from_the_binary() -> Result<(), BoxError> {
    let output = Command::new(env!("CARGO_BIN_EXE_contextos-web"))
        .args(["help", "config"])
        .output()?;

    assert!(
        output.status.success(),
        "`contextos-web help config` exited with {}",
        output.status
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("web.toml"),
        "config help should describe the web.toml file it edits, got: {text}"
    );
    Ok(())
}

#[test]
fn the_top_level_help_lists_the_config_subcommand() -> Result<(), BoxError> {
    let output = Command::new(env!("CARGO_BIN_EXE_contextos-web"))
        .arg("--help")
        .output()?;

    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("config"),
        "top-level help should list `config`, got: {text}"
    );
    Ok(())
}
