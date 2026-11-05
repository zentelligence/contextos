//! A wrapper over a real store adapter that fails every changing call from the n-th, as a process that crashed would
//! leave a store: the calls before it have happened, the call itself and every one after it have not.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use contextos_packages::{StoreEntry, StoreFailure, StoreFailureKind, StoreHome, StoreIo, StorePath, StoreTreeEntry};

/// Counts the changing calls made and says from which one on they fail.
#[derive(Clone, Debug)]
pub(super) struct FaultPlan {
    calls: Arc<AtomicUsize>,
    fail_from: Arc<AtomicUsize>,
}

impl FaultPlan {
    /// A plan whose changing calls from number `n` (counting from zero) fail.
    pub(super) fn failing_from(n: usize) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            fail_from: Arc::new(AtomicUsize::new(n)),
        }
    }

    /// A plan that never fails, to count the changing calls an operation makes.
    pub(super) fn never_failing() -> Self {
        Self::failing_from(usize::MAX)
    }

    /// How many changing calls were attempted, the failed ones included.
    pub(super) fn attempted(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn admit(&self, what: &str) -> Result<(), StoreFailure> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call >= self.fail_from.load(Ordering::SeqCst) {
            return Err(StoreFailure {
                kind: StoreFailureKind::Failed,
                detail: format!("injected failure at changing call {call} ({what})"),
            });
        }
        Ok(())
    }
}

/// `inner`, with its changing calls (`write_new`, `replace`, `create_dir_all`, `rename`, and `remove_tree`) gated by
/// `plan`. Reads and the lock pass straight through.
pub(super) struct FaultyIo<S> {
    pub(super) inner: S,
    pub(super) plan: FaultPlan,
}

impl<S: StoreIo> StoreIo for FaultyIo<S> {
    type Lock = S::Lock;

    fn home(&self) -> StoreHome {
        self.inner.home()
    }

    async fn lock(&self) -> Result<Self::Lock, StoreFailure> {
        self.inner.lock().await
    }

    async fn list(&self, path: &StorePath) -> Result<Vec<StoreEntry>, StoreFailure> {
        self.inner.list(path).await
    }

    async fn list_tree(&self, path: &StorePath, max_entries: usize) -> Result<Vec<StoreTreeEntry>, StoreFailure> {
        self.inner.list_tree(path, max_entries).await
    }

    async fn read(&self, path: &StorePath, max_bytes: u64) -> Result<Vec<u8>, StoreFailure> {
        self.inner.read(path, max_bytes).await
    }

    async fn write_new(&self, path: &StorePath, contents: &[u8]) -> Result<(), StoreFailure> {
        self.plan.admit("write_new")?;
        self.inner.write_new(path, contents).await
    }

    async fn replace(&self, path: &StorePath, contents: &[u8]) -> Result<(), StoreFailure> {
        self.plan.admit("replace")?;
        self.inner.replace(path, contents).await
    }

    async fn create_dir_all(&self, path: &StorePath) -> Result<(), StoreFailure> {
        self.plan.admit("create_dir_all")?;
        self.inner.create_dir_all(path).await
    }

    async fn rename(&self, from: &StorePath, to: &StorePath) -> Result<(), StoreFailure> {
        self.plan.admit("rename")?;
        self.inner.rename(from, to).await
    }

    async fn remove_tree(&self, path: &StorePath) -> Result<bool, StoreFailure> {
        self.plan.admit("remove_tree")?;
        self.inner.remove_tree(path).await
    }
}

#[cfg(test)]
#[path = "faulty_io_test.rs"]
mod tests;
