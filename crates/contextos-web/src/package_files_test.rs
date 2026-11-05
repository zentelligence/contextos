use std::path::Path;

use contextos_workflows::{MAX_WORKFLOW_FILE_BYTES, WorkflowFailureKind};

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn write(root: &Path, relative: &str, bytes: &[u8]) -> std::io::Result<()> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

fn path(raw: &str) -> Result<WorkflowFilePath, contextos_workflows::WorkflowFilePathError> {
    WorkflowFilePath::try_from(raw)
}

fn release() -> std::io::Result<tempfile::TempDir> {
    let root = tempfile::tempdir()?;
    write(root.path(), "manifest.toml", b"name = \"acme\"\n")?;
    write(root.path(), "scaffold/AGENTS.md", "# Agents, café\n".as_bytes())?;
    write(root.path(), "scaffold/memory/index.md", b"# Memory\n")?;
    write(root.path(), "scaffold/empty.md", b"")?;
    Ok(root)
}

fn kind<T>(result: Result<T, WorkflowFileFailure>) -> Option<WorkflowFailureKind> {
    result.err().map(|failure| failure.kind)
}

#[tokio::test]
async fn a_file_is_read_whole_with_the_sha256_of_its_bytes() -> TestResult {
    let root = release()?;
    let files = InstalledPackageFiles::from(root.path().to_path_buf());

    let file = files.read(&path("scaffold/AGENTS.md")?).await?;

    assert_eq!(file.content, "# Agents, café\n");
    assert_eq!(
        <&str>::from(&file.content_hash),
        sha256_hex("# Agents, café\n".as_bytes())
    );
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().fold(String::new(), |mut hex, byte| {
        let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"));
        hex
    })
}

#[tokio::test]
async fn an_empty_file_is_read_as_empty_text() -> TestResult {
    let root = release()?;

    let file = InstalledPackageFiles::from(root.path().to_path_buf())
        .read(&path("scaffold/empty.md")?)
        .await?;

    assert_eq!(file.content, "");
    assert_eq!(<&str>::from(&file.content_hash), sha256_hex(b""));
    Ok(())
}

#[tokio::test]
async fn a_missing_file_and_a_folder_are_not_found() -> TestResult {
    let root = release()?;
    let files = InstalledPackageFiles::from(root.path().to_path_buf());

    assert_eq!(
        kind(files.read(&path("scaffold/absent.md")?).await),
        Some(WorkflowFailureKind::FileNotFound)
    );
    assert_eq!(
        kind(files.read(&path("scaffold/memory")?).await),
        Some(WorkflowFailureKind::FileNotFound)
    );
    Ok(())
}

#[tokio::test]
async fn binary_content_is_refused_as_not_text() -> TestResult {
    let root = release()?;
    write(root.path(), "scaffold/logo.png", &[0x89, b'P', b'N', b'G', 0xff, 0xfe])?;

    let refused = InstalledPackageFiles::from(root.path().to_path_buf())
        .read(&path("scaffold/logo.png")?)
        .await;

    assert_eq!(kind(refused), Some(WorkflowFailureKind::FileNotText));
    Ok(())
}

#[tokio::test]
async fn a_file_exactly_at_the_limit_is_read_and_one_byte_over_is_refused() -> TestResult {
    let root = release()?;
    let limit = usize::try_from(MAX_WORKFLOW_FILE_BYTES)?;
    write(root.path(), "big/at.md", "a".repeat(limit).as_bytes())?;
    write(root.path(), "big/over.md", "a".repeat(limit + 1).as_bytes())?;
    let files = InstalledPackageFiles::from(root.path().to_path_buf());

    assert_eq!(files.read(&path("big/at.md")?).await?.content.len(), limit);
    assert_eq!(
        kind(files.read(&path("big/over.md")?).await),
        Some(WorkflowFailureKind::FileTooLarge)
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_link_out_of_the_release_is_refused_rather_than_followed_at_any_level() -> TestResult {
    let root = release()?;
    let outside = tempfile::tempdir()?;
    write(outside.path(), "secret.md", b"secret\n")?;
    std::os::unix::fs::symlink(outside.path().join("secret.md"), root.path().join("scaffold/secret.md"))?;
    std::os::unix::fs::symlink(outside.path(), root.path().join("linked"))?;
    let files = InstalledPackageFiles::from(root.path().to_path_buf());

    assert_eq!(
        kind(files.read(&path("scaffold/secret.md")?).await),
        Some(WorkflowFailureKind::FileUnreadable)
    );
    assert_eq!(
        kind(files.read(&path("linked/secret.md")?).await),
        Some(WorkflowFailureKind::FileUnreadable)
    );
    assert_eq!(
        kind(files.list(&path("linked")?, 10).await),
        Some(WorkflowFailureKind::FileUnreadable)
    );
    assert_eq!(
        kind(files.list(&path("scaffold")?, 10).await),
        Some(WorkflowFailureKind::FileUnreadable)
    );
    Ok(())
}

#[tokio::test]
async fn a_folder_lists_every_file_beneath_it_and_a_file_lists_itself() -> TestResult {
    let root = release()?;
    let files = InstalledPackageFiles::from(root.path().to_path_buf());

    let mut listed: Vec<String> = files
        .list(&path("scaffold")?, 10)
        .await?
        .into_iter()
        .map(|file| file.as_str().to_owned())
        .collect();
    listed.sort();
    let single = files.list(&path("scaffold/AGENTS.md")?, 10).await?;

    assert_eq!(
        listed,
        ["scaffold/AGENTS.md", "scaffold/empty.md", "scaffold/memory/index.md"]
    );
    assert_eq!(single, vec![path("scaffold/AGENTS.md")?]);
    Ok(())
}

#[tokio::test]
async fn listing_a_missing_entry_is_not_found_and_past_the_limit_is_too_large() -> TestResult {
    let root = release()?;
    let files = InstalledPackageFiles::from(root.path().to_path_buf());

    assert_eq!(
        kind(files.list(&path("absent")?, 10).await),
        Some(WorkflowFailureKind::FileNotFound)
    );
    assert_eq!(
        kind(files.list(&path("scaffold")?, 2).await),
        Some(WorkflowFailureKind::FileTooLarge)
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_file_whose_name_no_script_could_address_fails_the_listing() -> TestResult {
    let root = release()?;
    write(root.path(), "scaffold/odd:name.md", b"")?;

    let refused = InstalledPackageFiles::from(root.path().to_path_buf())
        .list(&path("scaffold")?, 10)
        .await;

    assert_eq!(kind(refused), Some(WorkflowFailureKind::FileUnreadable));
    Ok(())
}
