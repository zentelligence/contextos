//! One derived store as the search service holds it: open, or closed with the failure that closed it, and able to be
//! reopened, released when found damaged, and recovered.
//!
//! A store that cannot be opened no longer stops the vault's search service from starting: its slot starts closed,
//! naming why, and every later use tries to open it again, so a store another instance has since repaired is picked
//! up. Every handle on an open store carries this process's registration as one of the store's holders, dropped only
//! when the last use of that handle ends, so a store is never moved aside while any call in this process still has
//! it open. A store found damaged is released: the slot stops handing it out, and once the calls still using it finish,
//! the handle closes and the registration goes, so whichever instance is last to release it can move it aside and
//! recreate it ([`Slot::recover`]). A store closed as damaged stays closed, never quietly reopened, until it is
//! recovered here or another instance has moved it aside and recreated it.

use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use contextos_core::Clock;

use crate::SearchError;
use crate::store::{
    DerivedStore, HolderRegistry, StoreFailure, StoreFailureKind, StoreHolder, StoreRecovery, quarantine,
    quarantined_count,
};

/// Opens a store's handle from its configuration.
type Opener<T> = Box<dyn Fn() -> Result<T, SearchError> + Send + Sync>;

/// Where a store lives and how to open it: everything a [`Slot`] needs to open, reopen, and recover it.
pub(super) struct SlotConfig<T> {
    pub(super) store: DerivedStore,
    /// The vault's derived-state directory, where the store's holders are registered and a damaged store is moved.
    pub(super) state_directory: PathBuf,
    /// The files and directories the store keeps, moved aside together when it is damaged, in the order they are
    /// moved; the last is the store itself.
    pub(super) members: Vec<PathBuf>,
    /// How long registering as a holder, or checking for other holders, waits for the holder registry.
    pub(super) wait: Duration,
    pub(super) open: Opener<T>,
}

/// An open store handle together with this process's registration as its holder. The handle is declared first, so it
/// is closed before the registration is dropped: the store counts as held for as long as any handle is open.
pub(super) struct Held<T> {
    value: T,
    _holder: StoreHolder,
}

impl<T> Deref for Held<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

/// One derived store's handle, or why there is none.
pub(super) struct Slot<T> {
    store: DerivedStore,
    state_directory: PathBuf,
    members: Vec<PathBuf>,
    wait: Duration,
    registry: HolderRegistry,
    open: Opener<T>,
    state: RwLock<SlotState<T>>,
    last_recovery: Mutex<Option<StoreRecovery>>,
}

/// What recovering a slot's store did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Recovered {
    /// This instance moved the damaged store aside and opened a fresh one, which the caller repopulates.
    MovedAside(StoreRecovery),
    /// Another instance had already moved the damaged store aside and recreated it; this one opened the replacement.
    AlreadyReplaced,
}

enum SlotState<T> {
    Open(Arc<Held<T>>),
    /// Closed by `failure`, when the store had been moved aside `replaced` times in all: a damaged store is reopened
    /// only once that count has grown, meaning another instance has replaced it.
    Closed {
        failure: StoreFailure,
        replaced: usize,
    },
}

impl<T> Slot<T> {
    /// A slot for the store `config` describes, opened at once when it can be.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::GraphLocked`] as it is: a store another process holds open exclusively is not this
    /// slot's to report, and the caller disables the capability instead. Every other failure to open leaves the slot
    /// closed, naming it.
    pub(super) fn try_open(config: SlotConfig<T>) -> Result<Self, SearchError> {
        let slot = Self {
            store: config.store,
            registry: HolderRegistry::new(&config.state_directory, config.store),
            state_directory: config.state_directory,
            members: config.members,
            wait: config.wait,
            open: config.open,
            state: RwLock::new(SlotState::Closed {
                failure: StoreFailure::io(config.store, "", "not yet opened"),
                replaced: 0,
            }),
            last_recovery: Mutex::new(None),
        };
        let state = match slot.open_registered() {
            Ok(held) => SlotState::Open(held),
            Err(error @ SearchError::GraphLocked { .. }) => return Err(error),
            Err(error) => slot.closed(&error),
        };
        *slot.write_state() = state;
        Ok(slot)
    }

    /// Which store this is.
    pub(super) const fn store(&self) -> DerivedStore {
        self.store
    }

    /// The open store, opening it again first when it is closed for any reason but damage, or when another instance
    /// has replaced a damaged store since.
    ///
    /// # Errors
    ///
    /// Returns the failure that keeps the store closed: its damage, until it is recovered, or the failure that still
    /// keeps it from opening.
    pub(super) fn get(&self) -> Result<Arc<Held<T>>, SearchError> {
        if let SlotState::Open(held) = &*self.read_state() {
            return Ok(Arc::clone(held));
        }
        let mut state = self.write_state();
        match &*state {
            SlotState::Open(held) => return Ok(Arc::clone(held)),
            SlotState::Closed { failure, replaced } if self.still_damaged(failure, *replaced) => {
                return Err(failure.clone().into());
            }
            SlotState::Closed { .. } => {}
        }
        match self.open_registered() {
            Ok(held) => {
                *state = SlotState::Open(Arc::clone(&held));
                Ok(held)
            }
            Err(error) => {
                *state = self.closed(&error);
                Err(self.failure_of(&error).into())
            }
        }
    }

    /// Why the store is closed, or `None` while it is open.
    #[cfg(test)]
    pub(super) fn closed_by(&self) -> Option<StoreFailure> {
        match &*self.read_state() {
            SlotState::Open(_) => None,
            SlotState::Closed { failure, .. } => Some(failure.clone()),
        }
    }

    /// The most recent time this slot moved its store aside, if it has.
    pub(super) fn last_recovery(&self) -> Option<StoreRecovery> {
        self.last_recovery
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Releases the store when `error` says it is damaged: the slot stops handing it out, and the handle closes and
    /// this process's registration goes once every call still using it has finished, so the store can be moved aside
    /// once no instance holds it. Any other error leaves it open.
    pub(super) fn note(&self, error: &SearchError) {
        let Some(failure) = classify(self.store, &self.location(), error) else {
            return;
        };
        if failure.kind() != StoreFailureKind::Corrupt {
            return;
        }
        tracing::warn!(
            code = failure.code(),
            store = self.store.name(),
            "a damaged derived store was released; it is moved aside and rebuilt once no instance holds it"
        );
        *self.write_state() = SlotState::Closed {
            failure,
            replaced: self.replaced(),
        };
    }

    /// Whether the store is closed because it is damaged and has not been replaced since, so recovering it is the fix.
    pub(super) fn needs_recovery(&self) -> bool {
        match &*self.read_state() {
            SlotState::Open(_) => false,
            SlotState::Closed { failure, replaced } => self.still_damaged(failure, *replaced),
        }
    }

    /// Moves the damaged store aside and opens a fresh one in its place, provided no instance holds it, this one's own
    /// calls included; or, when another instance has already done so, opens its replacement. The fresh store is empty:
    /// repopulating it from the vault is the caller's part, for [`Recovered::MovedAside`] only.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::StoreInUse`] when any other holder remains, in this process or another, leaving the
    /// store closed and in place; the failure to move it aside; and the failure to open the fresh store, which leaves
    /// the move recorded in [`Slot::last_recovery`].
    pub(super) fn recover(&self, clock: &impl Clock) -> Result<Recovered, SearchError> {
        let mut state = self.write_state();
        let (damage, replaced) = match &*state {
            SlotState::Closed { failure, replaced } => (failure.clone(), *replaced),
            SlotState::Open(_) => (
                StoreFailure::corrupt(self.store, self.location(), "found damaged"),
                self.replaced(),
            ),
        };
        // Released first, so this slot's own handle is not among the holders it waits for; a call still using it is.
        *state = SlotState::Closed {
            failure: damage.clone(),
            replaced,
        };
        if !self.still_damaged(&damage, replaced) {
            *state = match self.open_registered() {
                Ok(held) => SlotState::Open(held),
                Err(error) => self.closed(&error),
            };
            return match &*state {
                SlotState::Open(_) => Ok(Recovered::AlreadyReplaced),
                SlotState::Closed { failure, .. } => Err(failure.clone().into()),
            };
        }
        let moved = self.registry.while_sole_holder_then_register(self.wait, || {
            quarantine(&self.state_directory, self.store, &self.members, clock.now())
        })?;
        let (holder, recovery) = match moved {
            Ok((holder, recovery)) => (holder, recovery?),
            Err(census) => {
                return Err(SearchError::StoreInUse {
                    store: self.store,
                    path: self.location(),
                    other_processes: census.other_processes,
                    this_process: census.this_process,
                });
            }
        };
        // Recorded as soon as the files are moved, so where they went is reported even if the fresh store fails to
        // open.
        *self.last_recovery.lock().unwrap_or_else(PoisonError::into_inner) = Some(recovery.clone());
        match (self.open)() {
            Ok(value) => {
                *state = SlotState::Open(Arc::new(Held { value, _holder: holder }));
                Ok(Recovered::MovedAside(recovery))
            }
            Err(error) => {
                *state = self.closed(&error);
                Err(self.failure_of(&error).into())
            }
        }
    }

    /// Whether a store closed by `failure`, when it had been moved aside `replaced` times, is still the damaged one.
    fn still_damaged(&self, failure: &StoreFailure, replaced: usize) -> bool {
        failure.kind() == StoreFailureKind::Corrupt && self.replaced() <= replaced
    }

    /// How many times this store has been moved aside, by any instance.
    fn replaced(&self) -> usize {
        quarantined_count(&self.state_directory, self.store)
    }

    /// The state for a store closed by `error`.
    fn closed(&self, error: &SearchError) -> SlotState<T> {
        SlotState::Closed {
            failure: self.failure_of(error),
            replaced: self.replaced(),
        }
    }

    /// Registers this process as a holder, then opens the store.
    fn open_registered(&self) -> Result<Arc<Held<T>>, SearchError> {
        let holder = self.registry.register(self.wait)?;
        let value = (self.open)()?;
        Ok(Arc::new(Held { value, _holder: holder }))
    }

    /// The store failure `error` amounts to: its own classification when it has one, otherwise a failure to read or
    /// write the store.
    pub(super) fn failure_of(&self, error: &SearchError) -> StoreFailure {
        classify(self.store, &self.location(), error)
            .unwrap_or_else(|| StoreFailure::io(self.store, self.location(), error.to_string()))
    }

    /// Where the store lives, for messages: its last member, the store itself.
    fn location(&self) -> String {
        self.members
            .last()
            .map(|member| member.display().to_string())
            .unwrap_or_default()
    }

    fn read_state(&self) -> std::sync::RwLockReadGuard<'_, SlotState<T>> {
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_state(&self) -> std::sync::RwLockWriteGuard<'_, SlotState<T>> {
        self.state.write().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The failure of `store`, kept at `location`, that `error` reports, if it is one: a typed store failure of that store,
/// or a text index failure tantivy reports as damage, a format it cannot read, or a schema that does not match.
pub(super) fn classify(store: DerivedStore, location: &str, error: &SearchError) -> Option<StoreFailure> {
    match error {
        SearchError::Store(failure) if failure.store() == store => Some(failure.clone()),
        SearchError::IndexStorage { source } if store == DerivedStore::Text => {
            crate::text::tantivy_damage(location, source)
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "slot_test.rs"]
mod tests;
