use std::collections::BTreeMap;
use std::sync::Mutex;

use contextos_core::ContentHash;

use super::*;

fn path(raw: &str) -> Result<WorkflowFilePath, WorkflowFilePathError> {
    WorkflowFilePath::try_from(raw)
}

#[test]
fn a_plain_relative_path_is_accepted_as_written() -> Result<(), WorkflowFilePathError> {
    for raw in [
        "AGENTS.md",
        "scaffold/memory/index.md",
        ".contextos/fences.toml",
        "a b/c-d_e.f",
    ] {
        assert_eq!(path(raw)?.as_str(), raw);
    }
    Ok(())
}

#[test]
fn a_path_that_could_leave_the_root_or_has_two_spellings_is_refused() {
    for raw in [
        "",
        "/etc/passwd",
        "../secret.md",
        "scaffold/../../secret.md",
        "scaffold/./AGENTS.md",
        "./AGENTS.md",
        "scaffold//AGENTS.md",
        "scaffold/",
        "scaffold\\..\\secret.md",
        "C:secret.md",
        "C:/secret.md",
        "notes.md:stream",
        "nul\0byte",
        "..",
        ".",
        "CON",
        "scaffold/nul.txt",
        "Aux.md",
        "com1",
        "LPT9.rhai",
        "scaffold/name.",
        "scaffold/name ",
    ] {
        assert!(path(raw).is_err(), "{raw:?} should be refused");
    }
}

#[test]
fn a_path_holds_itself_and_what_lies_beneath_it_but_not_a_sibling_sharing_its_prefix()
-> Result<(), WorkflowFilePathError> {
    let folder = path("scaffold")?;
    assert!(folder.holds(&path("scaffold")?));
    assert!(folder.holds(&path("scaffold/AGENTS.md")?));
    assert!(folder.holds(&path("scaffold/memory/index.md")?));
    assert!(!folder.holds(&path("scaffolding/AGENTS.md")?));
    assert!(!folder.holds(&path("other/scaffold/AGENTS.md")?));
    assert!(!path("scaffold/AGENTS.md")?.holds(&folder));
    Ok(())
}

#[test]
fn a_declaration_admits_its_files_and_everything_beneath_its_folders() -> Result<(), WorkflowFilePathError> {
    let declared = DeclaredFiles::try_from(vec!["scaffold".to_owned(), "rules/lint.toml".to_owned()])?;

    assert_eq!(declared.entries().len(), 2);
    assert!(declared.admits(&path("scaffold/AGENTS.md")?));
    assert!(declared.admits(&path("rules/lint.toml")?));
    assert!(!declared.admits(&path("rules/other.toml")?));
    assert!(!declared.admits(&path("entry.rhai")?));
    Ok(())
}

#[test]
fn a_declaration_with_a_malformed_entry_is_refused_naming_it() {
    let refused = DeclaredFiles::try_from(vec!["scaffold".to_owned(), "../outside".to_owned()]);

    assert_eq!(refused.map_err(|error| error.path), Err("../outside".to_owned()));
}

#[test]
fn an_empty_declaration_admits_nothing() -> Result<(), WorkflowFilePathError> {
    let declared = DeclaredFiles::try_from(Vec::new())?;

    assert!(declared.is_empty());
    assert!(!declared.admits(&path("AGENTS.md")?));
    Ok(())
}

/// A workflow root held in memory: file paths and their text, with every read and listing it was asked for.
#[derive(Default)]
struct MemorySource {
    files: BTreeMap<String, String>,
    asked: Mutex<Vec<String>>,
}

impl MemorySource {
    fn with(files: &[(&str, &str)]) -> Self {
        Self {
            files: files
                .iter()
                .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
                .collect(),
            asked: Mutex::default(),
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl WorkflowFileSource for MemorySource {
    fn read<'a>(
        &'a self,
        path: &'a WorkflowFilePath,
    ) -> Pin<Box<dyn Future<Output = Result<WorkflowFile, WorkflowFileFailure>> + Send + 'a>> {
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("read {path}"));
        let found = self.files.get(path.as_str()).cloned();
        Box::pin(async move {
            found
                .map(|content| WorkflowFile {
                    content,
                    content_hash: ContentHash::from([0; 32]),
                })
                .ok_or_else(|| WorkflowFileFailure::new(WorkflowFailureKind::FileNotFound, "missing"))
        })
    }

    fn list<'a>(
        &'a self,
        entry: &'a WorkflowFilePath,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkflowFilePath>, WorkflowFileFailure>> + Send + 'a>> {
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("list {entry}"));
        let found: Vec<WorkflowFilePath> = self
            .files
            .keys()
            .filter_map(|raw| WorkflowFilePath::try_from(raw.as_str()).ok())
            .filter(|file| entry.holds(file))
            .collect();
        Box::pin(async move {
            if found.len() > limit {
                return Err(WorkflowFileFailure::new(WorkflowFailureKind::FileTooLarge, "too many"));
            }
            Ok(found)
        })
    }
}

fn files(declared: &[&str], source: &Arc<MemorySource>) -> Result<WorkflowFiles, WorkflowFilePathError> {
    let declared = DeclaredFiles::try_from(declared.iter().map(|entry| (*entry).to_owned()).collect::<Vec<_>>())?;
    let source: Arc<dyn WorkflowFileSource> = Arc::clone(source) as Arc<dyn WorkflowFileSource>;
    Ok(WorkflowFiles::new(declared, source))
}

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn a_declared_file_is_read_through_the_source() -> TestResult {
    let source = Arc::new(MemorySource::with(&[("scaffold/AGENTS.md", "# Agents\n")]));

    let file = files(&["scaffold"], &source)?.read("scaffold/AGENTS.md").await?;

    assert_eq!(file.content, "# Agents\n");
    assert_eq!(file.content_hash, ContentHash::from([0; 32]));
    Ok(())
}

#[tokio::test]
async fn an_undeclared_file_is_refused_without_asking_the_source() -> TestResult {
    let source = Arc::new(MemorySource::with(&[
        ("entry.rhai", "secret"),
        ("scaffold/AGENTS.md", ""),
    ]));

    let refused = files(&["scaffold"], &source)?.read("entry.rhai").await;

    assert_eq!(
        refused.map_err(|failure| failure.kind),
        Err(WorkflowFailureKind::FileUndeclared)
    );
    assert_eq!(source.asked(), Vec::<String>::new(), "the source was asked");
    Ok(())
}

#[tokio::test]
async fn a_traversal_is_refused_as_an_invalid_path_without_asking_the_source() -> TestResult {
    let source = Arc::new(MemorySource::with(&[("scaffold/AGENTS.md", "")]));

    let refused = files(&["scaffold"], &source)?.read("scaffold/../../secret.md").await;

    assert_eq!(
        refused.map_err(|failure| failure.kind),
        Err(WorkflowFailureKind::FilePathInvalid)
    );
    assert_eq!(source.asked(), Vec::<String>::new(), "the source was asked");
    Ok(())
}

#[tokio::test]
async fn a_run_whose_workflow_declares_nothing_reads_nothing() {
    let refused = WorkflowFiles::none().read("AGENTS.md").await;
    let listed = WorkflowFiles::none().list("").await;

    assert_eq!(
        refused.map_err(|failure| failure.kind),
        Err(WorkflowFailureKind::FileUndeclared)
    );
    assert_eq!(listed, Ok(Vec::new()));
}

#[tokio::test]
async fn content_over_the_limit_from_a_source_is_refused_whatever_the_source_allowed() -> TestResult {
    let oversized = "a".repeat(usize::try_from(MAX_WORKFLOW_FILE_BYTES)? + 1);
    let source = Arc::new(MemorySource::with(&[("big.md", oversized.as_str())]));

    let refused = files(&["big.md"], &source)?.read("big.md").await;

    assert_eq!(
        refused.map_err(|failure| failure.kind),
        Err(WorkflowFailureKind::FileTooLarge)
    );
    Ok(())
}

#[tokio::test]
async fn listing_returns_every_declared_file_sorted_and_once() -> TestResult {
    let source = Arc::new(MemorySource::with(&[
        ("scaffold/b.md", ""),
        ("scaffold/a/c.md", ""),
        ("rules/lint.toml", ""),
        ("entry.rhai", ""),
    ]));

    let listed = files(&["scaffold", "rules/lint.toml", "scaffold/b.md"], &source)?
        .list("")
        .await?;

    assert_eq!(listed, vec!["rules/lint.toml", "scaffold/a/c.md", "scaffold/b.md"]);
    Ok(())
}

#[tokio::test]
async fn listing_under_a_prefix_returns_only_what_lies_beneath_it() -> TestResult {
    let source = Arc::new(MemorySource::with(&[
        ("scaffold/memory/index.md", ""),
        ("scaffold/memoryx.md", ""),
        ("scaffold/AGENTS.md", ""),
    ]));

    let listed = files(&["scaffold"], &source)?.list("scaffold/memory").await?;

    assert_eq!(listed, vec!["scaffold/memory/index.md"]);
    Ok(())
}

#[tokio::test]
async fn listing_a_prefix_that_overlaps_no_declaration_is_refused_as_undeclared() -> TestResult {
    let source = Arc::new(MemorySource::with(&[
        ("scaffold/AGENTS.md", ""),
        ("secrets/key.txt", ""),
    ]));

    let refused = files(&["scaffold"], &source)?.list("secrets").await;

    assert_eq!(
        refused.map_err(|failure| failure.kind),
        Err(WorkflowFailureKind::FileUndeclared)
    );
    assert_eq!(source.asked(), Vec::<String>::new(), "the source was asked");
    Ok(())
}

#[tokio::test]
async fn listing_with_a_malformed_prefix_is_refused() -> TestResult {
    let source = Arc::new(MemorySource::with(&[("scaffold/AGENTS.md", "")]));

    let refused = files(&["scaffold"], &source)?.list("../").await;

    assert_eq!(
        refused.map_err(|failure| failure.kind),
        Err(WorkflowFailureKind::FilePathInvalid)
    );
    Ok(())
}

#[tokio::test]
async fn a_listing_over_the_limit_across_declarations_is_refused_rather_than_cut_short() -> TestResult {
    let names: Vec<(String, String)> = (0..=MAX_LISTED_WORKFLOW_FILES)
        .map(|index| {
            (
                format!("{}/{index:04}.md", if index % 2 == 0 { "a" } else { "b" }),
                String::new(),
            )
        })
        .collect();
    let borrowed: Vec<(&str, &str)> = names
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect();
    let source = Arc::new(MemorySource::with(&borrowed));

    let refused = files(&["a", "b"], &source)?.list("").await;

    assert_eq!(
        refused.map_err(|failure| failure.kind),
        Err(WorkflowFailureKind::FileTooLarge)
    );
    Ok(())
}

#[tokio::test]
async fn listing_a_prefix_narrower_than_a_large_declared_folder_asks_only_for_the_prefix() -> TestResult {
    let mut names: Vec<(String, String)> = (0..=MAX_LISTED_WORKFLOW_FILES)
        .map(|index| (format!("scaffold/bulk/{index:04}.md"), String::new()))
        .collect();
    names.push(("scaffold/memory/a.md".to_owned(), String::new()));
    names.push(("scaffold/memory/b.md".to_owned(), String::new()));
    let borrowed: Vec<(&str, &str)> = names
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect();
    let source = Arc::new(MemorySource::with(&borrowed));

    let listed = files(&["scaffold"], &source)?.list("scaffold/memory").await?;

    assert_eq!(listed, vec!["scaffold/memory/a.md", "scaffold/memory/b.md"]);
    assert_eq!(source.asked(), vec!["list scaffold/memory".to_owned()]);
    Ok(())
}

#[test]
fn a_malformed_path_converts_to_a_path_invalid_failure_naming_it() {
    let failure = WorkflowFileFailure::from(WorkflowFilePathError {
        path: "../x".to_owned(),
        reason: "it has a `.` or `..` segment",
    });

    assert_eq!(failure.kind, WorkflowFailureKind::FilePathInvalid);
    assert!(failure.message.contains("../x"), "{}", failure.message);
}
