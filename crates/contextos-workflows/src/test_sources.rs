//! A workflow's files held in memory, and its script built from them, for tests of modules and validation.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use contextos_core::ContentHash;

use crate::caller::WorkflowFailureKind;
use crate::library_name::LibraryName;
use crate::script_modules::{DeclaredModules, WorkflowScript};
use crate::vault_libraries::{LibraryReadFailure, LibrarySource, SourceFuture};
use crate::workflow_files::{WorkflowFile, WorkflowFileFailure, WorkflowFilePath, WorkflowFileSource};

/// A workflow root held in memory, keyed by relative path.
pub(crate) struct MemorySource(pub(crate) BTreeMap<String, String>);

impl MemorySource {
    pub(crate) fn of(files: &[(&str, &str)]) -> Self {
        Self(
            files
                .iter()
                .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
                .collect(),
        )
    }
}

impl WorkflowFileSource for MemorySource {
    fn read<'a>(
        &'a self,
        path: &'a WorkflowFilePath,
    ) -> Pin<Box<dyn Future<Output = Result<WorkflowFile, WorkflowFileFailure>> + Send + 'a>> {
        let found = self.0.get(path.as_str()).cloned();
        Box::pin(async move {
            found
                .map(|content| WorkflowFile {
                    content,
                    content_hash: contextos_core::ContentHash::from([0; 32]),
                })
                .ok_or_else(|| {
                    WorkflowFileFailure::new(WorkflowFailureKind::FileNotFound, format!("{path} is missing"))
                })
        })
    }

    fn list<'a>(
        &'a self,
        entry: &'a WorkflowFilePath,
        _limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkflowFilePath>, WorkflowFileFailure>> + Send + 'a>> {
        let found = self
            .0
            .keys()
            .filter_map(|raw| WorkflowFilePath::try_from(raw.as_str()).ok())
            .filter(|file| entry.holds(file))
            .collect();
        Box::pin(async move { Ok(found) })
    }
}

/// `entry` with every one of `modules` declared and loaded.
pub(crate) async fn script_with(
    entry: &str,
    modules: &[(&str, &str)],
) -> Result<WorkflowScript, Box<dyn std::error::Error>> {
    let declared = DeclaredModules::try_from(modules.iter().map(|(path, _)| (*path).to_owned()).collect::<Vec<_>>())?;
    Ok(WorkflowScript::load(entry.to_owned(), &declared, &MemorySource::of(modules)).await?)
}

/// A vault's `registry/libraries/` held in memory, which records every file it is asked to read.
pub(crate) struct MemoryLibraries {
    folders: Vec<String>,
    files: BTreeMap<(String, String), String>,
    reads: Mutex<Vec<String>>,
    understated: BTreeMap<(String, String), u64>,
    down: bool,
}

impl MemoryLibraries {
    pub(crate) fn new() -> Self {
        Self {
            folders: Vec::new(),
            files: BTreeMap::new(),
            reads: Mutex::new(Vec::new()),
            understated: BTreeMap::new(),
            down: false,
        }
    }

    /// Adds a library folder with `library.toml` and its module files.
    pub(crate) fn library(mut self, folder: &str, manifest: &str, modules: &[(&str, &str)]) -> Self {
        self.folders.push(folder.to_owned());
        self.files
            .insert((folder.to_owned(), "library.toml".to_owned()), manifest.to_owned());
        for (path, text) in modules {
            self.files
                .insert((folder.to_owned(), (*path).to_owned()), (*text).to_owned());
        }
        self
    }

    /// A library that exports `client.rhai` and names `others`.
    pub(crate) fn simple(self, folder: &str, others: &[&str]) -> Self {
        let manifest = manifest_naming(others);
        self.library(folder, &manifest, &[("client.rhai", "fn id() { 1 }")])
    }

    /// Reports `size` for the file `path` of `folder` when its details are asked for, whatever its length, as a file
    /// does that changes between the details and the read.
    pub(crate) fn reporting_size(mut self, folder: &str, path: &str, size: u64) -> Self {
        self.understated.insert((folder.to_owned(), path.to_owned()), size);
        self
    }

    /// The same libraries, with a source that cannot be asked.
    pub(crate) fn unreachable(mut self) -> Self {
        self.down = true;
        self
    }

    /// Every file asked for in full, as `library/path`, in order.
    pub(crate) fn read_log(&self) -> Vec<String> {
        self.reads.lock().map(|reads| reads.clone()).unwrap_or_default()
    }
}

/// A `library.toml` that exports `client.rhai` and names `others`, each without a requirement, as a library in a vault's
/// source may, so each is read from its own source.
pub(crate) fn manifest_naming(others: &[&str]) -> String {
    let named: String = others.iter().fold(String::new(), |mut text, other| {
        let _ = writeln!(text, "[[libraries]]\nname = \"{other}\"");
        text
    });
    format!("modules = [\"client.rhai\"]\nexports = [\"client.rhai\"]\n{named}")
}

/// A `library.toml` that exports `client.rhai` and requires `others`, each at any version, so each comes from an
/// installed release and none is read from source.
pub(crate) fn manifest_requiring(others: &[&str]) -> String {
    let required: String = others.iter().fold(String::new(), |mut text, other| {
        let _ = writeln!(text, "[[libraries]]\nname = \"{other}\"\nrequirement = \"*\"");
        text
    });
    format!("modules = [\"client.rhai\"]\nexports = [\"client.rhai\"]\n{required}")
}

fn not_found(path: &WorkflowFilePath) -> LibraryReadFailure {
    LibraryReadFailure::File(WorkflowFileFailure::new(
        WorkflowFailureKind::FileNotFound,
        format!("{path} is missing"),
    ))
}

impl LibrarySource for MemoryLibraries {
    fn folders(&self) -> SourceFuture<'_, Vec<String>> {
        Box::pin(async move {
            if self.down {
                return Err(LibraryReadFailure::Unreachable("the server is down".to_owned()));
            }
            Ok(self.folders.clone())
        })
    }

    fn size<'a>(&'a self, library: &'a LibraryName, path: &'a WorkflowFilePath) -> SourceFuture<'a, u64> {
        Box::pin(async move {
            let key = (library.to_string(), path.to_string());
            if let Some(size) = self.understated.get(&key) {
                return Ok(*size);
            }
            self.files
                .get(&key)
                .map(|text| u64::try_from(text.len()).unwrap_or(u64::MAX))
                .ok_or_else(|| not_found(path))
        })
    }

    fn read<'a>(&'a self, library: &'a LibraryName, path: &'a WorkflowFilePath) -> SourceFuture<'a, WorkflowFile> {
        Box::pin(async move {
            if let Ok(mut reads) = self.reads.lock() {
                reads.push(format!("{library}/{path}"));
            }
            self.files
                .get(&(library.to_string(), path.to_string()))
                .map(|content| WorkflowFile {
                    content: content.clone(),
                    content_hash: ContentHash::from([7; 32]),
                })
                .ok_or_else(|| not_found(path))
        })
    }
}
