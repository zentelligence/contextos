//! The host-wide advisory lock that serialises mutations of one vault across every process on the host.
//!
//! One operator commonly runs several `contextos` processes against the same vault at once: one per interactive
//! agent session, one per scheduled session, and the one `contextos-web` holds. A conflict-checked write compares the
//! file's hash and then renames a temporary file over it, and two processes that both pass the comparison would both
//! rename, the second silently replacing the first. So a mutation holds an operating-system advisory lock on a file
//! in the vault's state directory, which every process on the host resolves to the same place. The advisory lock is
//! released by the operating system when its holder exits, so a crashed process never leaves a vault locked.
//!
//! The lock lives in the state directory rather than the vault because advisory locks are only dependable on a local
//! disk, and a vault may sit in a synchronised or network folder. It is shared by processes that resolve the same
//! state directory, which is every process run by the same user on the same host with default configuration.
//!
//! Processes waiting for the host lock are served in the order they asked, not by whichever happens to poll at the
//! instant it comes free; otherwise a process that releases the lock and immediately asks again wins almost every
//! time, and its sustained writes can starve another process past the wait. Beside the lock file sits a queue
//! directory (`vault-write.lock.queue/`) of numbered ticket files and a queue lock file (`vault-write.lock.queue.lock`)
//! that guards it. A waiting process takes the next ticket number, creates that ticket, and holds an exclusive
//! advisory lock on it for as long as it waits: the ticket's lock is the proof that its owner is still alive, and
//! the operating system releases it when the owner exits. A process tries the host lock only once no live ticket is
//! numbered below its own, and gives up its ticket once it holds the lock or stops waiting, so asking again means
//! queueing behind everyone already waiting. A ticket whose lock nobody holds belongs to a process that died while
//! waiting, and whoever finds it removes it, so a crash never wedges the queue. Creating a ticket and removing an
//! abandoned one both happen only while holding the queue lock, so a ticket that has been created but not yet locked
//! is never mistaken for an abandoned one. Only advisory locks and ordinary files are involved, so the queue behaves
//! the same on Linux, macOS, and Windows.
//!
//! An older build that tries the lock file without taking a ticket still contends for it by polling, exactly as
//! before: mutual exclusion holds across builds, but the arrival-order guarantee holds only among processes that
//! queue.

use std::ffi::OsStr;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::{FsError, try_lock_exclusive};

/// The name of the host write lock file in each vault's state directory.
pub const HOST_WRITE_LOCK: &str = "vault-write.lock";

/// How long a mutating call waits for another process to finish writing the same vault before it is refused as busy,
/// counted from when it first asks, queueing included. Writes hold the lock for the length of one tool call and
/// waiting processes are served in turn, so this is far beyond a normal wait.
pub const DEFAULT_HOST_WAIT: Duration = Duration::from_secs(30);

/// How often a waiting call looks again at the processes queued ahead of it and, once none are, tries the host lock.
const HOST_RETRY_INTERVAL: Duration = Duration::from_millis(10);

/// How often a call tries the queue lock again. The queue lock is held only for the few file operations that take
/// a ticket or remove abandoned ones, so it is retried far more often than the host lock.
const QUEUE_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(1);

/// Appended to the host lock file's name to name the queue directory beside it.
const QUEUE_DIRECTORY_SUFFIX: &str = ".queue";

/// Appended to the host lock file's name to name the queue lock file beside it.
const QUEUE_LOCK_SUFFIX: &str = ".queue.lock";

/// The extension of every ticket file in a queue directory.
const TICKET_EXTENSION: &str = "ticket";

/// The width of a ticket file's zero-padded number, wide enough for any `u64`, so the files' names sort in the same
/// order as their numbers.
const TICKET_DIGITS: usize = 20;

/// A host lock held, released when dropped.
///
/// The lock belongs to the open file description, not to this handle, and a process that is started while the lock is
/// held (between its `fork` and its `exec`) briefly holds a duplicate of the descriptor, so closing this handle alone
/// would leave the lock held until that process executes. Dropping unlocks explicitly, which releases the lock for
/// every duplicate at once.
#[derive(Debug)]
pub struct HostLockGuard(File);

impl std::ops::Deref for HostLockGuard {
    type Target = File;

    fn deref(&self) -> &File {
        &self.0
    }
}

impl Drop for HostLockGuard {
    fn drop(&mut self) {
        // A failure to unlock leaves the lock to be released when the last duplicate closes, which is what dropping
        // without unlocking would have done anyway.
        let _ = self.0.unlock();
    }
}

/// Takes the write lock of the vault whose derived state lives in `state_directory`: the [`HOST_WRITE_LOCK`] file
/// there, queued for as [`lock_host`] does. Every process that resolves the same state directory contends for the
/// same lock, so `contextos-mcp` and `contextos-web` serialise their mutations of a vault through it.
///
/// # Errors
///
/// As [`lock_host`].
pub fn lock_vault_writes(state_directory: &Path, wait: Duration) -> Result<HostLockGuard, FsError> {
    lock_host(&state_directory.join(HOST_WRITE_LOCK), wait)
}

/// Opens `path`, creating it, its directory, and its queue when absent, joins the queue, and takes the lock file's
/// exclusive advisory lock once every process that asked earlier has been served, giving up once `wait` has passed.
/// The lock is released when the returned file is dropped.
///
/// # Errors
///
/// Returns [`FsError::HostLockBusy`] when another holder keeps the lock beyond `wait`, and [`FsError::HostLock`] when
/// the lock file or its queue cannot be created, opened, or locked.
pub fn lock_host(path: &Path, wait: Duration) -> Result<HostLockGuard, FsError> {
    let queue = HostQueue::beside(path, wait)?;
    std::fs::create_dir_all(&queue.directory).map_err(failure(&queue.directory))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(failure(path))?;
    let ticket = queue.enqueue()?;
    loop {
        if !queue.anyone_ahead_of(&ticket)? && try_lock_exclusive(&file).map_err(failure(path))? {
            // Holding the lock, this process is no longer waiting, so it leaves the queue.
            drop(ticket);
            return Ok(HostLockGuard(file));
        }
        if queue.expired() {
            // Dropping the ticket on return leaves the queue, so nobody waits behind a caller that has given up.
            return Err(queue.busy());
        }
        std::thread::sleep(HOST_RETRY_INTERVAL);
    }
}

/// Builds the error for a failed file operation on `path` while taking a host write lock or queueing for it.
fn failure(path: &Path) -> impl FnOnce(std::io::Error) -> FsError + '_ {
    move |source| FsError::HostLock {
        lock: path.to_path_buf(),
        source,
    }
}

/// The queue beside one host lock file, as seen by one call waiting for that lock.
#[derive(Debug)]
struct HostQueue<'a> {
    host: &'a Path,
    directory: PathBuf,
    lock: PathBuf,
    started: Instant,
    wait: Duration,
}

impl<'a> HostQueue<'a> {
    /// The queue beside the host lock file at `host`, for a call that started waiting now and waits up to `wait`.
    fn beside(host: &'a Path, wait: Duration) -> Result<Self, FsError> {
        let name = host.file_name().ok_or_else(|| {
            failure(host)(std::io::Error::new(
                ErrorKind::InvalidInput,
                "the host write lock path does not name a file",
            ))
        })?;
        let sibling = |suffix: &str| {
            let mut sibling = name.to_os_string();
            sibling.push(suffix);
            host.with_file_name(sibling)
        };
        Ok(Self {
            host,
            directory: sibling(QUEUE_DIRECTORY_SUFFIX),
            lock: sibling(QUEUE_LOCK_SUFFIX),
            started: Instant::now(),
            wait,
        })
    }

    fn expired(&self) -> bool {
        self.started.elapsed() >= self.wait
    }

    fn busy(&self) -> FsError {
        FsError::HostLockBusy {
            lock: self.host.to_path_buf(),
            seconds: self.wait.as_secs(),
        }
    }

    /// Takes the queue lock, released when the returned file is dropped, trying again until the wait has passed.
    fn lock_queue(&self) -> Result<HostLockGuard, FsError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.lock)
            .map_err(failure(&self.lock))?;
        loop {
            if try_lock_exclusive(&file).map_err(failure(&self.lock))? {
                return Ok(HostLockGuard(file));
            }
            if self.expired() {
                return Err(self.busy());
            }
            std::thread::sleep(QUEUE_LOCK_RETRY_INTERVAL);
        }
    }

    /// Every ticket in the queue, lowest number first. Files that are not tickets are ignored.
    fn tickets(&self) -> Result<Vec<(TicketNumber, PathBuf)>, FsError> {
        let mut tickets = Vec::new();
        for entry in std::fs::read_dir(&self.directory).map_err(failure(&self.directory))? {
            let entry = entry.map_err(failure(&self.directory))?;
            if let Ok(number) = TicketNumber::try_from(entry.file_name().as_os_str()) {
                tickets.push((number, entry.path()));
            }
        }
        tickets.sort_unstable_by_key(|(number, _)| *number);
        Ok(tickets)
    }

    /// Joins the back of the queue: creates and locks a ticket numbered one above the highest present.
    fn enqueue(&self) -> Result<Ticket, FsError> {
        let _queue = self.lock_queue()?;
        let number = match self.tickets()?.last() {
            None => TicketNumber(0),
            Some((highest, _)) => highest.following().ok_or_else(|| {
                failure(&self.directory)(std::io::Error::other(
                    "the host write queue has run out of ticket numbers",
                ))
            })?,
        };
        let path = self.directory.join(number.to_string());
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(failure(&path))?;
        let ticket = Ticket { number, path, file };
        // Under the queue lock nobody else tests a ticket's liveness, so a fresh ticket's lock is always free.
        if !try_lock_exclusive(&ticket.file).map_err(failure(&ticket.path))? {
            return Err(failure(&ticket.path)(std::io::Error::new(
                ErrorKind::WouldBlock,
                "a new host write queue ticket was already locked",
            )));
        }
        Ok(ticket)
    }

    /// Whether a live ticket is numbered below `ticket`, removing every abandoned one found on the way.
    fn anyone_ahead_of(&self, ticket: &Ticket) -> Result<bool, FsError> {
        let ahead: Vec<PathBuf> = self
            .tickets()?
            .into_iter()
            .take_while(|(number, _)| *number < ticket.number)
            .map(|(_, path)| path)
            .collect();
        if ahead.is_empty() {
            return Ok(false);
        }
        // Liveness is tested only under the queue lock, where no ticket can be between its creation and its lock.
        let _queue = self.lock_queue()?;
        let mut live = false;
        for path in &ahead {
            live |= ticket_is_live(path);
        }
        Ok(live)
    }
}

/// Tests whether the ticket at `path` still belongs to a waiting process, removing it when its owner has died. Only
/// called under the queue lock. A ticket that cannot be tested counts as live, so an unexpected error can only delay
/// a waiter, never let it overtake one.
fn ticket_is_live(path: &Path) -> bool {
    let file = match OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return false,
        Err(error) => {
            tracing::debug!(%error, "could not open a host write queue ticket to test it; treating it as live");
            return true;
        }
    };
    match try_lock_exclusive(&file) {
        Ok(true) => {
            // Its owner no longer holds its lock, so it died while waiting. The test handle keeps the lock until the
            // ticket is gone.
            remove_ticket(path);
            false
        }
        Ok(false) => true,
        Err(error) => {
            tracing::debug!(%error, "could not test a host write queue ticket's lock; treating it as live");
            true
        }
    }
}

/// Removes a ticket file, tolerating one already removed. Any other failure leaves an unlocked ticket behind, which
/// whoever next finds it removes, so it is logged and otherwise ignored.
fn remove_ticket(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            tracing::debug!(%error, "could not remove a host write queue ticket; whoever finds it unlocked removes it");
        }
    }
}

/// A place in a host lock's queue: lower numbers asked first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TicketNumber(u64);

impl TicketNumber {
    /// The number after this one, or none once the numbers are exhausted.
    fn following(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// A file name that is not a ticket's.
#[derive(Debug)]
struct NotATicket;

impl TryFrom<&OsStr> for TicketNumber {
    type Error = NotATicket;

    /// Reads a ticket file name: exactly [`TICKET_DIGITS`] decimal digits, a dot, and [`TICKET_EXTENSION`].
    fn try_from(name: &OsStr) -> Result<Self, Self::Error> {
        let digits = name
            .to_str()
            .and_then(|name| name.strip_suffix(TICKET_EXTENSION))
            .and_then(|name| name.strip_suffix('.'))
            .ok_or(NotATicket)?;
        if digits.len() != TICKET_DIGITS || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(NotATicket);
        }
        digits.parse().map(Self).map_err(|_| NotATicket)
    }
}

/// The ticket file's name.
impl fmt::Display for TicketNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:0width$}.{TICKET_EXTENSION}",
            self.0,
            width = TICKET_DIGITS
        )
    }
}

/// This call's place in the queue, held for as long as it waits. The ticket file's exclusive lock shows the owner is
/// alive; dropping the ticket removes the file and then closes it, which releases the lock, so the file never sits
/// unlocked under a name a later ticket could take.
#[derive(Debug)]
struct Ticket {
    number: TicketNumber,
    path: PathBuf,
    file: File,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        remove_ticket(&self.path);
        // Released for every duplicate of the descriptor, as for the host lock itself.
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
#[path = "host_lock_test.rs"]
mod tests;
