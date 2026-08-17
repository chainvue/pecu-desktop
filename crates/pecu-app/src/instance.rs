//! One wallet process per home directory.
//!
//! # Why this is correctness rather than politeness
//!
//! Two copies of Pecu on the same home do not merely draw the same figures
//! twice. They share four files, and only one of them tolerates it:
//!
//! * `wallet.sqlite` and `cache.sqlite` are opened in WAL mode, which is built
//!   for concurrent processes. These are fine.
//! * `pending.json`, `registration.json` and `launch.json` are **not**. Each is
//!   written whole, through a temp file and a rename — which makes a crash safe
//!   and does nothing at all about a second writer. The last process to write
//!   wins, and what it wins with is its own idea of the file, formed when it
//!   started.
//!
//! `registration.json` is the one that costs money. It holds the salt of a name
//! being claimed, and that salt exists nowhere else: it cannot be recovered from
//! the chain, from the vault, or from the first transaction. A second instance
//! that overwrites it has spent the claim fee for a name that can no longer be
//! registered. `launch.json` is the same shape one step further along, holding
//! the currency an already-paid-for identity was registered *for*.
//!
//! So this is not a "you already have it open" convenience. It is the guard on
//! two files that record money that has already left.
//!
//! # Why a file lock and not a PID file
//!
//! Because the lock belongs to the file descriptor and the kernel closes those.
//! A wallet killed, crashed or force-quit releases it on the way out with no
//! cleanup and no stale-lock case to reason about — which is exactly what a PID
//! file cannot do, and what makes PID files fail in the direction that locks
//! somebody out of their own wallet.
//!
//! `File::try_lock` has been stable since Rust 1.89 and this workspace requires
//! 1.95, so this costs no dependency.

use std::fs::File;
use std::path::Path;

/// Held for the life of the process. Dropping it releases the lock, and so does
/// the process ending for any reason.
///
/// `#[must_use]` is not enough on its own — a caller could bind it to `_`, which
/// drops immediately — so the one call site binds it to a named variable and
/// says why.
pub struct Guard(#[allow(dead_code)] File);

/// Take the lock for this home, or say who has it.
///
/// # What a failure to *create* the file means
///
/// It fails open: the guard is skipped and the wallet starts. A home directory
/// that cannot be written to is a wallet that will fail at its next real write
/// anyway, with a message about the thing it was actually doing — and refusing
/// to start over a lock file would turn a recoverable problem into an
/// unusable application. A second instance is rarer than a broken directory.
pub fn acquire(home: &Path) -> Result<Guard, Busy> {
    if let Err(error) = std::fs::create_dir_all(home) {
        tracing::warn!(%error, "the home directory could not be created");
        return Err(Busy::NoLockFile);
    }

    let path = home.join("pecu.lock");
    let file = match File::create(&path) {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "no lock file; starting unguarded");
            return Err(Busy::NoLockFile);
        }
    };

    match file.try_lock() {
        Ok(()) => Ok(Guard(file)),
        Err(std::fs::TryLockError::WouldBlock) => {
            tracing::warn!(path = %path.display(), "another Pecu holds this wallet");
            Err(Busy::AlreadyRunning)
        }
        // The lock could not be attempted — a filesystem that does not support
        // it, for instance. Same reasoning as a missing lock file: this is not
        // a reason to refuse somebody their wallet.
        Err(error) => {
            tracing::warn!(%error, "the lock could not be taken; starting unguarded");
            Err(Busy::NoLockFile)
        }
    }
}

/// Why there is no guard.
pub enum Busy {
    /// Another process has this home open. **Do not start.**
    AlreadyRunning,
    /// The lock could not be attempted at all. Start anyway — see [`acquire`].
    NoLockFile,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The second one is refused, and the first is unaffected.
    ///
    /// Two `acquire` calls in one process really do contend: on Unix this is
    /// `flock(2)`, which locks per **open file description**, and each call
    /// opens the file again. So this exercises the same path two processes
    /// take, without needing two processes.
    #[test]
    fn a_second_wallet_on_one_home_is_refused() {
        let home = tempfile::tempdir().expect("a temp home");

        let first = acquire(home.path());
        assert!(first.is_ok(), "the first wallet was refused");

        assert!(
            matches!(acquire(home.path()), Err(Busy::AlreadyRunning)),
            "a second wallet was allowed onto the same home",
        );

        // Released with the guard, so the next start is not locked out by a
        // wallet that has already exited.
        drop(first);
        assert!(
            acquire(home.path()).is_ok(),
            "the lock outlived the process that held it",
        );
    }

    /// Different homes are different wallets. Two chains, two windows, no
    /// shared files — refusing that would be refusing something safe.
    #[test]
    fn two_homes_do_not_contend() {
        let one = tempfile::tempdir().expect("a temp home");
        let two = tempfile::tempdir().expect("a temp home");

        let first = acquire(one.path());
        assert!(first.is_ok(), "the first wallet was refused");
        assert!(
            acquire(two.path()).is_ok(),
            "a wallet on one home locked out a wallet on another",
        );
    }
}
