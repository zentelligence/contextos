//! Shared fixtures for the engine's unit tests: an interface type, identifiers, and, further down, an in-memory
//! [`crate::StoreIo`] with fault injection and a small manifest model.

// The in-memory store does its work under a `std` mutex without ever waiting, but the trait it implements is
// asynchronous, so its methods are `async` without an `.await`.
#![allow(clippy::unused_async, clippy::unused_async_trait_impl)]

use serde::{Deserialize, Serialize};

use crate::name::{ReleaseDigest, ReleaseId, ReleaseKey, ReleaseName, ReleaseVersion};

/// A stand-in for a manifest model's interface.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TestInterface {
    pub inputs: Vec<String>,
    pub note: Option<String>,
}

impl crate::step::InterfaceChanges for TestInterface {
    fn changes_from(&self, previous: &Self) -> Vec<crate::step::InterfaceChange> {
        use crate::step::{InterfaceChange, VersionStep};
        let removed = previous
            .inputs
            .iter()
            .filter(|input| !self.inputs.contains(input))
            .map(|input| InterfaceChange {
                step: VersionStep::Major,
                what: format!("input {input} removed"),
            });
        let added = self
            .inputs
            .iter()
            .filter(|input| !previous.inputs.contains(input))
            .map(|input| InterfaceChange {
                step: VersionStep::Minor,
                what: format!("input {input} added"),
            });
        removed.chain(added).collect()
    }
}

pub fn key(name: &str, version: &str) -> ReleaseKey {
    ReleaseKey {
        name: ReleaseName::try_from(name).must("name"),
        version: ReleaseVersion::try_from(version).must("version"),
    }
}

pub fn digest(fill: char) -> ReleaseDigest {
    ReleaseDigest::try_from(format!("sha256:{}", fill.to_string().repeat(64)).as_str()).must("digest")
}

pub fn release(name: &str, version: &str, fill: char) -> ReleaseId {
    ReleaseId {
        key: key(name, version),
        digest: digest(fill),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// An in-memory store with fault injection.
// ---------------------------------------------------------------------------------------------------------------

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use contextos_core::{Clock, ContentHash};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::check::ReadLimits;
use crate::engine::{PackageEngine, ReleaseRules, StoreRules};
use crate::io::{
    IssuesOperationIds, StoreEntry, StoreEntryKind, StoreFailure, StoreFailureKind, StoreHome, StoreIo, StoreTreeEntry,
};
use crate::ledger::ReleaseOrigin;
use crate::manifest::{ListedFile, ManifestRejection, ReleaseFiles, ReleaseManifest};
use crate::name::OperationId;
use crate::path::{ReleaseFilePath, StorePath};
use crate::store::InstallRequest;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Node {
    File(Vec<u8>),
    Dir,
    Link,
    Other,
}

type Segments = Vec<String>;

/// Runs once, just before the read operation it is registered for, against the store's state as it stands.
type Hook = Box<dyn FnOnce(&mut FakeState) + Send>;

#[derive(Default)]
pub struct FakeState {
    pub nodes: BTreeMap<Segments, Node>,
    /// Mutating calls made so far, counting the one that failed.
    pub mutations: usize,
    /// When set, the mutating call numbered this (from zero) and every one after it fails, as after a crash.
    pub crash_at: Option<usize>,
    /// When set, only the mutating call numbered this fails, with this kind.
    pub fail_once: Option<(usize, StoreFailureKind)>,
    pub unavailable: bool,
    pub lock_busy: bool,
    pub home: StoreHome,
    pub list_tree_calls: usize,
    pub read_calls: usize,
    /// Run before the n-th (from zero) `list_tree` call.
    pub before_list_tree: Vec<(usize, Hook)>,
    /// Run before the n-th (from zero) `read` call.
    pub before_read: Vec<(usize, Hook)>,
}

#[derive(Clone, Default)]
pub struct FakeIo(Arc<Mutex<FakeState>>);

pub struct FakeLock;

fn failure(kind: StoreFailureKind, detail: &str) -> StoreFailure {
    StoreFailure {
        kind,
        detail: detail.to_owned(),
    }
}

pub fn segments(path: &StorePath) -> Segments {
    path.segments().to_vec()
}

pub fn split(path: &str) -> Segments {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect()
}

impl FakeIo {
    /// A store whose root folder exists and is empty.
    pub fn with_root() -> Self {
        let io = Self::default();
        io.state().nodes.insert(Vec::new(), Node::Dir);
        io
    }

    pub fn state(&self) -> MutexGuard<'_, FakeState> {
        self.0.lock().must("fake state")
    }

    /// Puts a file, and the folders above it, directly into the store without counting as a mutation.
    pub fn put_file(&self, path: &str, bytes: &[u8]) {
        let mut state = self.state();
        let parts = split(path);
        for depth in 0..parts.len() {
            state.nodes.entry(parts[..depth].to_vec()).or_insert(Node::Dir);
        }
        state.nodes.insert(parts, Node::File(bytes.to_vec()));
    }

    pub fn put_dir(&self, path: &str) {
        let mut state = self.state();
        let parts = split(path);
        for depth in 0..=parts.len() {
            state.nodes.entry(parts[..depth].to_vec()).or_insert(Node::Dir);
        }
    }

    pub fn put_node(&self, path: &str, node: Node) {
        let mut state = self.state();
        let parts = split(path);
        for depth in 0..parts.len() {
            state.nodes.entry(parts[..depth].to_vec()).or_insert(Node::Dir);
        }
        state.nodes.insert(parts, node);
    }

    pub fn delete(&self, path: &str) {
        let parts = split(path);
        self.state().nodes.retain(|existing, _| !existing.starts_with(&parts));
    }

    pub fn file(&self, path: &str) -> Option<Vec<u8>> {
        match self.state().nodes.get(&split(path)) {
            Some(Node::File(bytes)) => Some(bytes.clone()),
            _ => None,
        }
    }

    pub fn exists(&self, path: &str) -> bool {
        self.state().nodes.contains_key(&split(path))
    }

    /// Every path below the root, sorted, as text.
    pub fn paths(&self) -> Vec<String> {
        self.state()
            .nodes
            .keys()
            .filter(|path| !path.is_empty())
            .map(|path| path.join("/"))
            .collect()
    }

    pub fn snapshot(&self) -> BTreeMap<Segments, Node> {
        self.state().nodes.clone()
    }

    pub fn crash_at(&self, mutation: usize) {
        self.state().crash_at = Some(mutation);
    }

    /// The store as it stands after a crash: the files stay, the failing calls stop.
    pub fn revive(&self) {
        let mut state = self.state();
        state.crash_at = None;
        state.fail_once = None;
    }

    pub fn fail_once(&self, mutation: usize, kind: StoreFailureKind) {
        self.state().fail_once = Some((mutation, kind));
    }

    pub fn mutations(&self) -> usize {
        self.state().mutations
    }

    pub fn reset_mutation_count(&self) {
        self.state().mutations = 0;
    }

    pub fn set_unavailable(&self, unavailable: bool) {
        self.state().unavailable = unavailable;
    }

    pub fn set_lock_busy(&self, busy: bool) {
        self.state().lock_busy = busy;
    }

    pub fn before_list_tree(&self, call: usize, hook: impl FnOnce(&mut FakeState) + Send + 'static) {
        self.state().before_list_tree.push((call, Box::new(hook)));
    }

    pub fn before_read(&self, call: usize, hook: impl FnOnce(&mut FakeState) + Send + 'static) {
        self.state().before_read.push((call, Box::new(hook)));
    }

    fn mutation_gate(state: &mut FakeState) -> Result<(), StoreFailure> {
        let call = state.mutations;
        state.mutations += 1;
        if state.crash_at.is_some_and(|at| call >= at) {
            return Err(failure(StoreFailureKind::Failed, "crashed"));
        }
        if let Some((at, kind)) = state.fail_once
            && at == call
        {
            state.fail_once = None;
            return Err(failure(kind, "injected"));
        }
        Ok(())
    }

    fn read_gate(state: &FakeState) -> Result<(), StoreFailure> {
        if state.unavailable {
            Err(failure(StoreFailureKind::Unavailable, "store unavailable"))
        } else {
            Ok(())
        }
    }

    fn parent_is_folder(state: &FakeState, path: &[String]) -> Result<(), StoreFailure> {
        let Some((_, parent)) = path.split_last() else {
            return Ok(());
        };
        match state.nodes.get(parent) {
            Some(Node::Dir) => Ok(()),
            Some(_) => Err(failure(StoreFailureKind::Failed, "parent is not a folder")),
            None => Err(failure(StoreFailureKind::NotFound, "parent is missing")),
        }
    }
}

impl StoreIo for FakeIo {
    type Lock = FakeLock;

    fn home(&self) -> StoreHome {
        self.state().home
    }

    async fn lock(&self) -> Result<FakeLock, StoreFailure> {
        if self.state().lock_busy {
            Err(failure(StoreFailureKind::LockBusy, "busy"))
        } else {
            Ok(FakeLock)
        }
    }

    async fn list(&self, path: &StorePath) -> Result<Vec<StoreEntry>, StoreFailure> {
        let state = self.state();
        Self::read_gate(&state)?;
        let path = segments(path);
        match state.nodes.get(&path) {
            None => return Err(failure(StoreFailureKind::NotFound, "no such folder")),
            Some(Node::Dir) => {}
            Some(_) => return Err(failure(StoreFailureKind::Failed, "not a folder")),
        }
        Ok(state
            .nodes
            .iter()
            .filter(|(candidate, _)| candidate.len() == path.len() + 1 && candidate.starts_with(&path))
            .map(|(candidate, node)| StoreEntry {
                name: candidate[path.len()].clone(),
                kind: kind_of(node),
            })
            .collect())
    }

    async fn list_tree(&self, path: &StorePath, max_entries: usize) -> Result<Vec<StoreTreeEntry>, StoreFailure> {
        let mut state = self.state();
        let call = state.list_tree_calls;
        state.list_tree_calls += 1;
        let mut hooks = std::mem::take(&mut state.before_list_tree);
        let (due, later): (Vec<_>, Vec<_>) = hooks.drain(..).partition(|(at, _)| *at == call);
        state.before_list_tree = later;
        for (_, hook) in due {
            hook(&mut state);
        }
        Self::read_gate(&state)?;
        let path = segments(path);
        match state.nodes.get(&path) {
            None => return Err(failure(StoreFailureKind::NotFound, "no such folder")),
            Some(Node::Dir) => {}
            Some(_) => return Err(failure(StoreFailureKind::Failed, "not a folder")),
        }
        let entries: Vec<StoreTreeEntry> = state
            .nodes
            .iter()
            .filter(|(candidate, _)| candidate.len() > path.len() && candidate.starts_with(&path))
            .map(|(candidate, node)| StoreTreeEntry {
                path: candidate[path.len()..].join("/"),
                kind: kind_of(node),
            })
            .collect();
        if entries.len() > max_entries {
            return Err(failure(StoreFailureKind::TooLarge, "too many entries"));
        }
        Ok(entries)
    }

    async fn read(&self, path: &StorePath, max_bytes: u64) -> Result<Vec<u8>, StoreFailure> {
        let mut state = self.state();
        let call = state.read_calls;
        state.read_calls += 1;
        let mut hooks = std::mem::take(&mut state.before_read);
        let (due, later): (Vec<_>, Vec<_>) = hooks.drain(..).partition(|(at, _)| *at == call);
        state.before_read = later;
        for (_, hook) in due {
            hook(&mut state);
        }
        Self::read_gate(&state)?;
        match state.nodes.get(&segments(path)) {
            None => Err(failure(StoreFailureKind::NotFound, "no such file")),
            Some(Node::File(bytes)) if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes => {
                Err(failure(StoreFailureKind::TooLarge, "too large"))
            }
            Some(Node::File(bytes)) => Ok(bytes.clone()),
            Some(_) => Err(failure(StoreFailureKind::Refused, "not a regular file")),
        }
    }

    async fn write_new(&self, path: &StorePath, contents: &[u8]) -> Result<(), StoreFailure> {
        let mut state = self.state();
        Self::mutation_gate(&mut state)?;
        let path = segments(path);
        Self::parent_is_folder(&state, &path)?;
        if state.nodes.contains_key(&path) {
            return Err(failure(StoreFailureKind::AlreadyExists, "exists"));
        }
        state.nodes.insert(path, Node::File(contents.to_vec()));
        Ok(())
    }

    async fn replace(&self, path: &StorePath, contents: &[u8]) -> Result<(), StoreFailure> {
        let mut state = self.state();
        Self::mutation_gate(&mut state)?;
        let path = segments(path);
        for depth in 0..path.len() {
            state.nodes.entry(path[..depth].to_vec()).or_insert(Node::Dir);
        }
        state.nodes.insert(path, Node::File(contents.to_vec()));
        Ok(())
    }

    async fn create_dir_all(&self, path: &StorePath) -> Result<(), StoreFailure> {
        let mut state = self.state();
        Self::mutation_gate(&mut state)?;
        let path = segments(path);
        for depth in 0..=path.len() {
            match state.nodes.get(&path[..depth]) {
                Some(Node::Dir) => {}
                Some(_) => return Err(failure(StoreFailureKind::AlreadyExists, "not a folder")),
                None => {
                    state.nodes.insert(path[..depth].to_vec(), Node::Dir);
                }
            }
        }
        Ok(())
    }

    async fn rename(&self, from: &StorePath, to: &StorePath) -> Result<(), StoreFailure> {
        let mut state = self.state();
        Self::mutation_gate(&mut state)?;
        let (from, to) = (segments(from), segments(to));
        if !state.nodes.contains_key(&from) {
            return Err(failure(StoreFailureKind::NotFound, "nothing to rename"));
        }
        if state.nodes.contains_key(&to) {
            return Err(failure(StoreFailureKind::AlreadyExists, "destination exists"));
        }
        Self::parent_is_folder(&state, &to)?;
        let moved: Vec<Segments> = state
            .nodes
            .keys()
            .filter(|path| path.starts_with(&from))
            .cloned()
            .collect();
        for old in moved {
            if let Some(node) = state.nodes.remove(&old) {
                let mut new = to.clone();
                new.extend(old[from.len()..].iter().cloned());
                state.nodes.insert(new, node);
            }
        }
        Ok(())
    }

    async fn remove_tree(&self, path: &StorePath) -> Result<bool, StoreFailure> {
        let mut state = self.state();
        Self::mutation_gate(&mut state)?;
        let path = segments(path);
        let before = state.nodes.len();
        state.nodes.retain(|existing, _| !existing.starts_with(&path));
        Ok(state.nodes.len() != before)
    }
}

fn kind_of(node: &Node) -> StoreEntryKind {
    match node {
        Node::File(bytes) => StoreEntryKind::File {
            size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        },
        Node::Dir => StoreEntryKind::Directory,
        Node::Link => StoreEntryKind::Link,
        Node::Other => StoreEntryKind::Other,
    }
}

// ---------------------------------------------------------------------------------------------------------------
// A small manifest model, a clock, and operation identifiers.
// ---------------------------------------------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
struct ManifestDto {
    name: String,
    version: String,
    digest: String,
    inputs: Vec<String>,
    #[serde(default)]
    files: Vec<FileDto>,
}

#[derive(Deserialize, Serialize)]
struct FileDto {
    path: String,
    hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestManifest {
    release: ReleaseId,
    files: Vec<ListedFile>,
    interface: TestInterface,
}

fn refuse(code: &'static str, message: String) -> ManifestRejection {
    ManifestRejection { code, message }
}

impl TryFrom<&str> for TestManifest {
    type Error = ManifestRejection;

    fn try_from(text: &str) -> Result<Self, Self::Error> {
        let dto: ManifestDto =
            toml::from_str(text).map_err(|error| refuse("packages/invalid-manifest", error.message().to_owned()))?;
        let name = ReleaseName::try_from(dto.name.as_str()).map_err(|error| refuse(error.code(), error.to_string()))?;
        let version =
            ReleaseVersion::try_from(dto.version.as_str()).map_err(|error| refuse(error.code(), error.to_string()))?;
        let digest =
            ReleaseDigest::try_from(dto.digest.as_str()).map_err(|error| refuse(error.code(), error.to_string()))?;
        let mut files = Vec::new();
        for file in dto.files {
            let path = ReleaseFilePath::try_from(file.path.as_str())
                .map_err(|error| refuse("packages/invalid-manifest", error.to_string()))?;
            let hash = ContentHash::try_from(file.hash.as_str())
                .map_err(|error| refuse("packages/invalid-manifest", error.to_string()))?;
            files.push(ListedFile { path, hash });
        }
        Ok(Self {
            release: ReleaseId {
                key: ReleaseKey { name, version },
                digest,
            },
            files,
            interface: TestInterface {
                inputs: dto.inputs,
                note: None,
            },
        })
    }
}

impl ReleaseManifest for TestManifest {
    type Interface = TestInterface;

    fn release(&self) -> &ReleaseId {
        &self.release
    }

    fn listed_files(&self) -> &[ListedFile] {
        &self.files
    }

    fn check_files(&self, files: &ReleaseFiles, home: StoreHome) -> Result<(), ManifestRejection> {
        let present = |name: &str| ReleaseFilePath::try_from(name).is_ok_and(|path| files.0.contains_key(&path));
        if present("forbidden.txt") {
            return Err(refuse(
                "packages/invalid-manifest",
                "forbidden.txt may not be shipped".to_owned(),
            ));
        }
        if home == StoreHome::Vault && present("vault-refused.txt") {
            return Err(refuse(
                "packages/test-vault-refusal",
                "vault-refused.txt may not go into a vault".to_owned(),
            ));
        }
        Ok(())
    }

    fn interface(&self, files: &ReleaseFiles) -> TestInterface {
        let note = ReleaseFilePath::try_from("note.txt")
            .ok()
            .and_then(|path| files.0.get(&path))
            .and_then(|bytes| String::from_utf8(bytes.clone()).ok());
        TestInterface {
            inputs: self.interface.inputs.clone(),
            note,
        }
    }
}

/// A release under construction: files, a manifest that describes them, and ways to make the two disagree.
#[derive(Clone)]
pub struct Built {
    pub name: String,
    pub version: String,
    pub inputs: Vec<String>,
    pub files: BTreeMap<String, Vec<u8>>,
    pub declared_digest: Option<String>,
    pub listed_hash_overrides: BTreeMap<String, String>,
    pub unlisted: BTreeSet<String>,
}

impl Built {
    pub fn new(name: &str, version: &str, files: &[(&str, &[u8])]) -> Self {
        Self {
            name: name.to_owned(),
            version: version.to_owned(),
            inputs: vec!["day".to_owned()],
            files: files
                .iter()
                .map(|(path, bytes)| ((*path).to_owned(), bytes.to_vec()))
                .collect(),
            declared_digest: None,
            listed_hash_overrides: BTreeMap::new(),
            unlisted: BTreeSet::new(),
        }
    }

    pub fn release_files(&self) -> ReleaseFiles {
        ReleaseFiles(
            self.files
                .iter()
                .map(|(path, bytes)| (ReleaseFilePath::try_from(path.as_str()).must("path"), bytes.clone()))
                .collect(),
        )
    }

    pub fn digest(&self) -> ReleaseDigest {
        StoreRules.digest(&self.release_files())
    }

    pub fn manifest_text(&self) -> String {
        let digest = self
            .declared_digest
            .clone()
            .unwrap_or_else(|| self.digest().as_str().to_owned());
        let files: Vec<FileDto> = self
            .files
            .iter()
            .filter(|(path, _)| !self.unlisted.contains(*path))
            .map(|(path, bytes)| FileDto {
                path: path.clone(),
                hash: self.listed_hash_overrides.get(path).cloned().unwrap_or_else(|| {
                    <&str>::from(&ContentHash::from(<[u8; 32]>::from(Sha256::digest(bytes)))).to_owned()
                }),
            })
            .collect();
        toml::to_string(&ManifestDto {
            name: self.name.clone(),
            version: self.version.clone(),
            digest,
            inputs: self.inputs.clone(),
            files,
        })
        .must("manifest renders")
    }

    pub fn request(&self, origin: ReleaseOrigin) -> InstallRequest {
        InstallRequest {
            manifest_text: self.manifest_text(),
            files: self.release_files(),
            origin,
        }
    }

    pub fn id(&self) -> ReleaseId {
        ReleaseId {
            key: key(&self.name, &self.version),
            digest: self.digest(),
        }
    }

    /// Writes the release under `{name}/{version}/` exactly as an install would leave it.
    pub fn put_into(&self, io: &FakeIo) {
        self.put_under(io, &format!("{}/{}", self.name, self.version));
    }

    pub fn put_under(&self, io: &FakeIo, folder: &str) {
        io.put_file(&format!("{folder}/manifest.toml"), self.manifest_text().as_bytes());
        for (path, bytes) in &self.files {
            io.put_file(&format!("{folder}/{path}"), bytes);
        }
    }
}

#[derive(Clone)]
pub struct TestClock(Arc<Mutex<OffsetDateTime>>);

impl TestClock {
    pub fn new(start: OffsetDateTime) -> Self {
        Self(Arc::new(Mutex::new(start)))
    }

    pub fn advance(&self, seconds: i64) {
        let mut now = self.0.lock().must("clock");
        *now += time::Duration::seconds(seconds);
    }
}

impl Clock for TestClock {
    fn now(&self) -> OffsetDateTime {
        *self.0.lock().must("clock")
    }
}

#[derive(Clone, Default)]
pub struct SequentialIds(Arc<AtomicUsize>);

impl IssuesOperationIds for SequentialIds {
    fn issue(&self) -> OperationId {
        let next = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        OperationId::try_from(format!("op{next}").as_str()).must("id")
    }
}

pub type TestEngine = PackageEngine<FakeIo, TestManifest, TestClock, SequentialIds>;

pub const START: OffsetDateTime = time::macros::datetime!(2026-11-03 09:00:00 UTC);

pub fn engine_over(io: FakeIo) -> TestEngine {
    PackageEngine {
        io,
        clock: TestClock::new(START),
        ids: SequentialIds::default(),
        limits: ReadLimits {
            max_entries: 1000,
            max_bytes: 1_000_000,
        },
        manifest: std::marker::PhantomData,
    }
}

pub fn engine() -> TestEngine {
    engine_over(FakeIo::with_root())
}

// ---------------------------------------------------------------------------------------------------------------
// Assertions shared by the engine's tests.
// ---------------------------------------------------------------------------------------------------------------

use crate::check::ReadCheck;
use crate::ledger::{Ledger, LedgerState};
use crate::store::PackageStore;

pub async fn ledger_of(engine: &TestEngine) -> Ledger<TestInterface> {
    match engine.ledger().await.must("ledger reads").state {
        LedgerState::Present(ledger) => ledger,
        other => fail(format!("expected a ledger, found {other:?}")),
    }
}

/// Folders left by operations that did not finish.
pub fn leftovers(io: &FakeIo) -> Vec<String> {
    io.paths()
        .into_iter()
        .filter(|path| !path.contains('/') && (path.starts_with("_staging-") || path.starts_with("_removing-")))
        .collect()
}

/// The crash-consistency invariant: whatever stands under a release's name is either a whole release or, when
/// `allow_damaged`, the damaged release an unfinished repair started from; never a release that is half written.
pub async fn assert_no_partial_release(engine: &TestEngine, key: &ReleaseKey, allow_damaged: bool) {
    match engine.read_release(key).await {
        ReadCheck::Verified(_) | ReadCheck::Removed => {}
        ReadCheck::Damaged(_) if allow_damaged => {}
        other => fail(format!("a partial release stands under its name: {other:?}")),
    }
}

pub fn tamper(io: &FakeIo, release: &Built, file: &str) {
    io.put_file(&format!("{}/{}/{file}", release.name, release.version), b"tampered");
}

// ---------------------------------------------------------------------------------------------------------------
// Failing a test at the point it goes wrong. The workspace denies `expect`, `unwrap`, and `panic!`; tests fail
// through these instead, which keep the expectation's message and the error's debug form.
// ---------------------------------------------------------------------------------------------------------------

/// Takes the value out of a `Result` or `Option` a test expects to hold one, failing the test otherwise.
pub trait Must<T> {
    /// The value, or a test failure naming `what`.
    fn must(self, what: &str) -> T;
}

impl<T, E: std::fmt::Debug> Must<T> for Result<T, E> {
    fn must(self, what: &str) -> T {
        match self {
            Ok(value) => value,
            Err(error) => fail(format!("{what}: {error:?}")),
        }
    }
}

impl<T> Must<T> for Option<T> {
    fn must(self, what: &str) -> T {
        match self {
            Some(value) => value,
            None => fail(format!("{what}: nothing there")),
        }
    }
}

/// Takes the error out of a `Result` a test expects to fail.
pub trait MustFail<E> {
    /// The error, or a test failure naming `what`.
    fn must_fail(self, what: &str) -> E;
}

impl<T: std::fmt::Debug, E> MustFail<E> for Result<T, E> {
    fn must_fail(self, what: &str) -> E {
        match self {
            Err(error) => error,
            Ok(value) => fail(format!("{what}: expected an error, got {value:?}")),
        }
    }
}

/// Fails the test with `message`.
// The one place a test failure is raised: it is test-only code, and a failing test is what it is for.
#[allow(clippy::panic)]
pub fn fail(message: impl std::fmt::Display) -> ! {
    std::panic::panic_any(message.to_string())
}
