//! Test seams in the snapshot read, keyed by store root so tests that run side by side never see each other's hooks.
//! They let a test change the store at the exact point a real install or removal would race a read.

use std::collections::BTreeMap;
use std::path::Path;

/// The thread each store was last read on, so a test can tell where a read ran.
pub(crate) static READING_THREADS: std::sync::Mutex<BTreeMap<std::path::PathBuf, std::thread::ThreadId>> =
    std::sync::Mutex::new(BTreeMap::new());

/// Records that the calling thread is reading `store`.
pub(crate) fn note_reading_thread(store: &Path) {
    READING_THREADS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(store.to_path_buf(), std::thread::current().id());
}

pub(crate) type Hook = Box<dyn Fn(&str) + Send + Sync>;

/// What to do, per store, after a module's size has been read and before the module is.
pub(crate) static AFTER_SIZE: std::sync::Mutex<BTreeMap<std::path::PathBuf, Hook>> =
    std::sync::Mutex::new(BTreeMap::new());

/// What to do, per store, once every module has been read.
pub(crate) static AFTER_MODULES: std::sync::Mutex<BTreeMap<std::path::PathBuf, Hook>> =
    std::sync::Mutex::new(BTreeMap::new());

/// What to do, per store, once a snapshot has been read whole and checked, before it is handed to its caller.
pub(crate) static AFTER_READ: std::sync::Mutex<BTreeMap<std::path::PathBuf, Hook>> =
    std::sync::Mutex::new(BTreeMap::new());

fn run_hook(hooks: &std::sync::Mutex<BTreeMap<std::path::PathBuf, Hook>>, store: &Path, module: &str) {
    if let Some(hook) = hooks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(store)
    {
        hook(module);
    }
}

pub(crate) fn set_hook(hooks: &std::sync::Mutex<BTreeMap<std::path::PathBuf, Hook>>, store: &Path, hook: Hook) {
    hooks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(store.to_path_buf(), hook);
}

/// Called between a module's size being read and the module being read.
pub(crate) fn after_size_checked(store: &Path, module: &str) {
    run_hook(&AFTER_SIZE, store, module);
}

/// Called once every module has been read, before the releases are checked again.
pub(crate) fn after_modules_read(store: &Path) {
    run_hook(&AFTER_MODULES, store, "");
}

/// Called once a snapshot has been read and its releases checked again, with the snapshot about to be returned.
pub(crate) fn after_snapshot_read(store: &Path) {
    run_hook(&AFTER_READ, store, "");
}
