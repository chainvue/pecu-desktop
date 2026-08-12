//! Signed bytes whose fate is unknown, kept where a crash cannot lose them.
//!
//! # Why this is written before the broadcast, not after
//!
//! A transport failure on `sendrawtransaction` is ambiguous: the node may have
//! accepted and relayed the transaction before the connection dropped. The only
//! safe resolution is to ask whether it confirmed, and — if it did not — to
//! re-send **the same bytes**.
//!
//! That is only possible if the bytes still exist. If they were recorded after
//! the attempt, a process that died mid-broadcast would leave a payment that may
//! or may not be propagating, with nothing to check it against and nothing to
//! re-send. So the record is committed first, and the broadcast happens second.
//!
//! # Why a file and not a database
//!
//! `chainvue-store` does not exist yet. This is a handful of rows that must
//! survive a crash, and a JSON file with an atomic replace does that. When the
//! SQLite layer lands this moves into it — the shape is already a row.
//!
//! # What is NOT in here
//!
//! No key material. A signed transaction is public the moment it is broadcast,
//! and these are transactions we are trying to broadcast. The file is written
//! owner-only anyway, because it says who you paid.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One transaction handed to a node with an unknown outcome.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Record {
    pub id: u64,
    /// The id computed locally from the bytes, not one a node reported.
    pub txid: String,
    /// The signed transaction. **This is what gets re-sent** — never a rebuild.
    pub hex: String,
    pub to_address: String,
    pub amount_display: String,
    pub created: u64,
    /// How many times we have asked whether it confirmed.
    pub checks: u32,
    pub state: State,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Handed over, outcome unknown.
    Uncertain,
    /// The node has it. Nothing more to do.
    Confirmed,
    /// Checked repeatedly and never seen. A resend is safe.
    Absent,
    /// The same bytes were sent again.
    Resent,
    /// The user has decided it is gone.
    Abandoned,
}

/// The file, and the rows in it.
pub struct Ledger {
    path: PathBuf,
    records: Vec<Record>,
    next: u64,
}

impl Ledger {
    /// Read the ledger beside the wallet, or start an empty one.
    ///
    /// A file that cannot be parsed is **kept**, not replaced: it may hold the
    /// only copy of a transaction someone is owed, and overwriting it to get a
    /// clean start would destroy exactly the thing this exists to protect.
    pub fn open(path: PathBuf) -> Self {
        let records: Vec<Record> = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
                tracing::error!(
                    %error,
                    path = %path.display(),
                    "the pending-broadcast file could not be read; it is being left alone",
                );
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };

        let next = records.iter().map(|r| r.id + 1).max().unwrap_or(1);
        Self {
            path,
            records,
            next,
        }
    }

    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// Record a transaction **before** it is handed to a node.
    ///
    /// Returns its id, and an error if it could not be written — in which case
    /// the caller must not broadcast. Sending bytes we have not managed to
    /// record is precisely the situation this module exists to prevent.
    pub fn commit(
        &mut self,
        txid: &str,
        hex: &str,
        to_address: &str,
        amount_display: &str,
    ) -> Result<u64, std::io::Error> {
        let id = self.next;
        self.next += 1;

        self.records.push(Record {
            id,
            txid: txid.to_string(),
            hex: hex.to_string(),
            to_address: to_address.to_string(),
            amount_display: amount_display.to_string(),
            created: now(),
            checks: 0,
            state: State::Uncertain,
        });

        self.persist()?;
        Ok(id)
    }

    /// Move a record on. Unknown ids are ignored rather than erroring: the
    /// caller is usually reacting to an event, not asserting a fact.
    pub fn set_state(&mut self, id: u64, state: State) {
        if let Some(record) = self.records.iter_mut().find(|r| r.id == id) {
            record.state = state;
        }
        if let Err(error) = self.persist() {
            tracing::error!(%error, "the pending-broadcast file could not be updated");
        }
    }

    /// Forget a record that resolved cleanly.
    ///
    /// Only for [`State::Confirmed`]: everything else is either still in doubt
    /// or is a decision worth keeping a trace of.
    pub fn forget_confirmed(&mut self) {
        self.records.retain(|r| r.state != State::Confirmed);
        if let Err(error) = self.persist() {
            tracing::error!(%error, "the pending-broadcast file could not be pruned");
        }
    }

    pub fn get(&self, id: u64) -> Option<&Record> {
        self.records.iter().find(|r| r.id == id)
    }

    /// Note that we asked the node about this one.
    pub fn note_check(&mut self, id: u64) {
        if let Some(record) = self.records.iter_mut().find(|r| r.id == id) {
            record.checks = record.checks.saturating_add(1);
        }
        if let Err(error) = self.persist() {
            tracing::error!(%error, "the pending-broadcast file could not be updated");
        }
    }

    /// Records still worth asking about.
    pub fn unresolved(&self) -> impl Iterator<Item = &Record> {
        self.records
            .iter()
            .filter(|r| matches!(r.state, State::Uncertain | State::Absent | State::Resent))
    }
}

/// How long to wait before asking again, given how many times we already have.
///
/// `10s, 30s, 60s, 5m, 15m…` — quick at first, because a transaction that did
/// land usually lands within a block or two, then slow, because a transaction
/// that has not appeared in a quarter of an hour is not about to. Asking a
/// public node every ten seconds for an hour would be rude and would learn
/// nothing.
pub fn recheck_after(checks: u32) -> std::time::Duration {
    use std::time::Duration;

    match checks {
        0 => Duration::from_secs(10),
        1 => Duration::from_secs(30),
        2 => Duration::from_mins(1),
        3 => Duration::from_mins(5),
        _ => Duration::from_mins(15),
    }
}

/// After this many fruitless checks, the transaction is treated as absent and a
/// resend of the SAME bytes is offered.
///
/// Offered, never automatic. A resend is only safe because the bytes are
/// identical, and even then it is the user's call — the alternative is a wallet
/// that decides, on its own, to hand a payment to the network a second time.
pub const CHECKS_BEFORE_ABSENT: u32 = 8;

impl Ledger {
    /// tmp → rename, so a crash mid-write leaves the previous file intact
    /// rather than a truncated one.
    fn persist(&self) -> Result<(), std::io::Error> {
        if self.records.is_empty() && !self.path.exists() {
            return Ok(());
        }

        let text = serde_json::to_string_pretty(&self.records)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let tmp = self.path.with_extension("json.tmp");
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        restrict(&tmp);
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_committed_record_survives_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pending.json");

        let id = {
            let mut ledger = Ledger::open(path.clone());
            ledger
                .commit("abc123", "0400008085202f89", "RQr2c…", "1.0000 0000")
                .expect("commit")
        };

        // Exactly the property that matters: the process is gone and the bytes
        // are still there, so the transaction can be asked about and, if it
        // never landed, sent again unchanged.
        let reopened = Ledger::open(path);
        let record = reopened.get(id).expect("the record survived");
        assert_eq!(record.hex, "0400008085202f89");
        assert_eq!(record.txid, "abc123");
        assert_eq!(record.state, State::Uncertain);
    }

    #[test]
    fn ids_do_not_repeat_across_restarts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pending.json");

        let first = Ledger::open(path.clone())
            .commit("a", "00", "R", "1")
            .expect("commit");
        let second = Ledger::open(path.clone())
            .commit("b", "11", "R", "1")
            .expect("commit");

        assert_ne!(first, second, "a restart reused an id");
    }

    #[test]
    fn only_confirmed_records_are_forgotten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut ledger = Ledger::open(dir.path().join("pending.json"));

        let kept = ledger.commit("a", "00", "R", "1").expect("commit");
        let gone = ledger.commit("b", "11", "R", "1").expect("commit");

        ledger.set_state(gone, State::Confirmed);
        ledger.set_state(kept, State::Absent);
        ledger.forget_confirmed();

        assert!(ledger.get(gone).is_none());
        assert!(
            ledger.get(kept).is_some(),
            "an unresolved record was dropped"
        );
    }

    /// Quick at first, then slow — and never faster on a later attempt than on
    /// an earlier one.
    #[test]
    fn the_backoff_only_lengthens() {
        let mut previous = std::time::Duration::ZERO;
        for checks in 0..20u32 {
            let wait = recheck_after(checks);
            assert!(
                wait >= previous,
                "check {checks} waits {wait:?} after {previous:?}",
            );
            previous = wait;
        }

        assert_eq!(recheck_after(0), std::time::Duration::from_secs(10));
        // It settles rather than growing forever: a quarter of an hour is slow
        // enough to be polite and often enough to still be useful.
        assert_eq!(recheck_after(50), std::time::Duration::from_mins(15));
    }

    #[test]
    fn checks_are_counted_and_survive_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pending.json");

        let id = {
            let mut ledger = Ledger::open(path.clone());
            let id = ledger.commit("abc", "00", "R", "1").expect("commit");
            ledger.note_check(id);
            ledger.note_check(id);
            id
        };

        assert_eq!(Ledger::open(path).get(id).expect("record").checks, 2);
    }

    /// Only rows still worth asking about are offered to the screen.
    #[test]
    fn resolved_records_leave_the_unresolved_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut ledger = Ledger::open(dir.path().join("pending.json"));

        let waiting = ledger.commit("a", "00", "R", "1").expect("commit");
        let missing = ledger.commit("b", "11", "R", "1").expect("commit");
        let given_up = ledger.commit("c", "22", "R", "1").expect("commit");

        ledger.set_state(missing, State::Absent);
        ledger.set_state(given_up, State::Abandoned);

        let open: Vec<u64> = ledger.unresolved().map(|r| r.id).collect();
        // Absent still counts: the node has not seen it, which is exactly when
        // a resend is offered.
        assert_eq!(open, vec![waiting, missing]);
    }

    /// A file we cannot parse may hold the only copy of a transaction someone
    /// is owed. Starting fresh must not mean overwriting it.
    #[test]
    fn an_unreadable_file_is_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pending.json");
        std::fs::write(&path, "{ this is not json").expect("write");

        let ledger = Ledger::open(path.clone());
        assert!(ledger.records().is_empty());

        // Nothing was written back over it.
        let still_there = std::fs::read_to_string(&path).expect("read");
        assert_eq!(still_there, "{ this is not json");
    }
}
