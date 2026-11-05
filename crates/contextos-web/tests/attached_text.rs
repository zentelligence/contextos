//! `contextos-web` reads every vault file through `fs_attach_file`, which returns a file's complete content and the hash
//! of exactly those bytes. `fs_read_text_file` is never used: it truncates at the inline threshold, so a caller that
//! used it would silently work from part of a file. These tests exercise the shared reader against a real
//! `contextos` session and a real file larger than that threshold, and guard against the other tool coming back.

mod support;

use std::path::Path;

use contextos_web::McpClientSet;
use contextos_web::vault_content::{AttachedRead, AttachedText, file_hash, read_attached, read_attached_text};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

async fn read(vault_dir: &Path, relative: &str) -> Result<Option<AttachedText>, BoxError> {
    let config_dir = tempfile::tempdir()?;
    let config_path = support::write_vault_config(config_dir.path(), vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = McpClientSet::connect(&[entry]).await?;
    let client = clients.get("contextos").ok_or("the contextos client is connected")?;
    Ok(read_attached_text(&client, &format!("{VAULT_NAME}://{relative}")).await?)
}

async fn read_outcome(vault_dir: &Path, relative: &str) -> Result<AttachedRead, BoxError> {
    let config_dir = tempfile::tempdir()?;
    let config_path = support::write_vault_config(config_dir.path(), vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = McpClientSet::connect(&[entry]).await?;
    let client = clients.get("contextos").ok_or("the contextos client is connected")?;
    Ok(read_attached(&client, &format!("{VAULT_NAME}://{relative}")).await?)
}

#[tokio::test]
async fn a_file_larger_than_the_inline_threshold_is_returned_whole() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let text = format!("{}\nthe last line\n", "a line of ordinary text\n".repeat(2_000));
    assert!(text.len() > 40_000, "the fixture must exceed the truncation threshold");
    std::fs::write(vault.path().join("big.md"), &text)?;

    let read = read(vault.path(), "big.md").await?.ok_or("the file is readable")?;

    assert_eq!(read.text, text);
    assert!(read.text.ends_with("the last line\n"));
    Ok(())
}

#[tokio::test]
async fn the_hash_is_the_one_the_server_reports_for_that_file() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    std::fs::write(vault.path().join("note.md"), "---\ntitle: T\n---\n\nBody.\n")?;
    let config_dir = tempfile::tempdir()?;
    let config_path = support::write_vault_config(config_dir.path(), vault.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = McpClientSet::connect(&[entry]).await?;
    let client = clients.get("contextos").ok_or("the contextos client is connected")?;

    let read = read_attached_text(&client, &format!("{VAULT_NAME}://note.md"))
        .await?
        .ok_or("the file is readable")?;

    let reported = file_hash(&client, VAULT_NAME, "note.md").await;
    assert!(read.content_hash.is_some(), "a hash is returned with the content");
    assert_eq!(read.content_hash, reported, "usable directly as expected_hash");
    Ok(())
}

#[tokio::test]
async fn a_missing_file_is_none() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;

    assert_eq!(read(vault.path(), "nothing-here.md").await?, None);
    let AttachedRead::Refused { code } = read_outcome(vault.path(), "nothing-here.md").await? else {
        return Err("a missing file must be refused".into());
    };
    assert_eq!(code.as_deref(), Some("path/not-found"));
    Ok(())
}

#[tokio::test]
async fn a_file_that_is_not_text_is_none_rather_than_garbled_text() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    std::fs::write(
        vault.path().join("pic.png"),
        [0x89, b'P', b'N', b'G', 0, 1, 2, 3, 0xff, 0xfe],
    )?;

    assert_eq!(read(vault.path(), "pic.png").await?, None);
    assert_eq!(
        read_outcome(vault.path(), "pic.png").await?,
        AttachedRead::NotText,
        "a file that exists but is not text is told apart from one that could not be read"
    );
    Ok(())
}

/// Every non-test source file of this crate, recursively.
fn sources(directory: &Path, found: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            sources(&path, found)?;
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("_test.rs"))
        {
            found.push(path);
        }
    }
    Ok(())
}

#[test]
fn no_source_file_reads_through_the_truncating_tool() -> Result<(), BoxError> {
    let mut files = Vec::new();
    sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files)?;
    assert!(!files.is_empty(), "the guard found no sources to check");

    let offenders = files
        .iter()
        .filter(|path| std::fs::read_to_string(path).is_ok_and(|text| text.contains("\"fs_read_text_file\"")))
        .map(|path| {
            path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect::<Vec<_>>();

    assert!(
        offenders.is_empty(),
        "these files call fs_read_text_file; read through vault_content::read_attached_text instead: {offenders:#?}"
    );
    Ok(())
}
