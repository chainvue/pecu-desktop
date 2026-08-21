//! A name reservation that has to survive the wallet being closed.
//!
//! # Why this file exists
//!
//! Registering a VerusID is two transactions with a secret between them. Step
//! one publishes a commitment to a name; step two reveals it and creates the
//! identity. The thing that ties them together is a 32-byte salt, and the SDK
//! is blunt about it:
//!
//! > The salt is **not recoverable from the chain**: a registration that loses
//! > it between the two steps has burned the commitment fee and cannot
//! > complete, so a wallet has to be able to write this down before it
//! > broadcasts anything.
//!
//! So this is written **before** the commitment is broadcast, exactly as
//! `pending::Ledger` commits a row before a payment is handed to a node. The
//! ordering is the whole point: a crash between the write and the broadcast
//! costs nothing, and a crash between the broadcast and the write costs the
//! fee with nothing to show.
//!
//! # And why it is a file rather than a row in SQLite
//!
//! Same answer `pecu-store` gives for the pending ledger. A salt is key
//! material in every way that matters — losing it loses money, and revealing it
//! early lets somebody else claim the name — and the store's own documentation
//! says no key material lives in there. Owner-only permissions, an atomic
//! write, and one file whose whole content is one unfinished registration.
//!
//! # There is deliberately only ever one
//!
//! A commitment expires roughly twenty blocks after it is signed, and the
//! expiry is inside the bytes the signature covers. Several at once would mean
//! several clocks running against a person who can only watch one, so the
//! wallet finishes or abandons the one it has before starting another.

use std::path::{Path, PathBuf};

use verus_sdk::network::{AwaitingCommitment, Pending};

/// How far along an unfinished registration is.
///
/// Recorded rather than inferred, because the difference between "signed, not
/// sent" and "sent" is the difference between an abandoned attempt costing
/// nothing and one costing a fee.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Built and signed. Nothing has been broadcast, so abandoning is free.
    Reserved,
    /// The commitment is on its way or on the chain. The fee is spent, and the
    /// only way to get value from it is to finish.
    Committed,
}

/// One unfinished registration.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Record {
    /// The name being claimed, as it was typed.
    pub name: String,
    /// The wallet key that signed the commitment. Step two **must** use the
    /// same one — the commitment output is locked to its address, and the SDK
    /// refuses a mismatch rather than producing an unspendable transaction.
    pub key_label: String,
    pub step: Step,
    /// The SDK's own value, salt included. Serialized whole so nothing here has
    /// to understand its shape — and so a field added upstream travels with it.
    pub pending: Pending<AwaitingCommitment>,
}

/// The one unfinished registration, on disk.
pub struct Reservation {
    path: PathBuf,
    record: Option<Record>,
}

impl Reservation {
    /// Read whatever the last run left behind.
    ///
    /// A file that cannot be parsed is **left alone**, not replaced. It may be
    /// the only copy of a salt somebody's fee depends on, and overwriting it to
    /// get a clean start would destroy exactly the thing this file exists to
    /// protect. The wallet carries on without it and says so in the log.
    pub fn open(path: PathBuf) -> Self {
        let record = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Record>(&text) {
                Ok(record) => Some(record),
                Err(error) => {
                    tracing::error!(
                        %error,
                        path = %path.display(),
                        "the name reservation could not be read; it is being left alone",
                    );
                    None
                }
            },
            Err(_) => None,
        };
        Self { path, record }
    }

    pub fn current(&self) -> Option<&Record> {
        self.record.as_ref()
    }

    /// Whether a registration is under way. While one is, another cannot start.
    pub fn in_progress(&self) -> bool {
        self.record.is_some()
    }

    /// Write a freshly built reservation down.
    ///
    /// **Call this before broadcasting the commitment**, and do not broadcast
    /// if it fails. An error here means the salt is only in memory, and bytes
    /// on the network with a salt that is only in memory is precisely the loss
    /// this module prevents.
    ///
    /// # Errors
    ///
    /// If the file could not be written.
    pub fn reserve(
        &mut self,
        name: &str,
        key_label: &str,
        pending: Pending<AwaitingCommitment>,
    ) -> Result<(), std::io::Error> {
        self.record = Some(Record {
            name: name.to_string(),
            key_label: key_label.to_string(),
            step: Step::Reserved,
            pending,
        });
        self.persist()
    }

    /// Record that the commitment has been handed to a node.
    ///
    /// Best-effort on purpose, and the asymmetry is deliberate: [`Self::reserve`]
    /// refuses to proceed when it cannot write, because the salt would be at
    /// risk. By here the salt is already safely on disk — this only updates how
    /// far along it is, and refusing to continue over it would strand a
    /// registration whose fee is already spent.
    pub fn mark_committed(&mut self) {
        if let Some(record) = &mut self.record {
            record.step = Step::Committed;
        }
        if let Err(error) = self.persist() {
            tracing::warn!(%error, "could not record that the commitment was sent");
        }
    }

    /// Keep the SDK's value up to date — `poll` and `anchor` both mutate it.
    pub fn update(&mut self, pending: Pending<AwaitingCommitment>) {
        if let Some(record) = &mut self.record {
            record.pending = pending;
        }
        if let Err(error) = self.persist() {
            tracing::warn!(%error, "could not update the name reservation");
        }
    }

    /// Finished, or given up on. Removes the file.
    ///
    /// The only path that deletes a salt, and it is reached in exactly two
    /// places: the identity exists, or somebody said to stop. Neither happens
    /// by timeout — an expired commitment still gets said out loud first,
    /// because the money is gone either way and silently tidying up would hide
    /// that it ever happened.
    pub fn finish(&mut self) {
        self.record = None;
        if let Err(error) = std::fs::remove_file(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%error, "could not remove the finished reservation");
            }
        }
    }

    fn persist(&self) -> Result<(), std::io::Error> {
        let Some(record) = &self.record else {
            return Ok(());
        };

        let text = serde_json::to_string_pretty(record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        // tmp → fsync → rename, so a crash leaves either the old file or the
        // new one and never half of either.
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use verus_sdk::network::RegistrationOptions;
    use verus_sdk::verus_keys::PrivateKey;

    /// A reservation built against a scripted chain, so the salt is a real one.
    fn reservation(dir: &Path) -> (Reservation, Pending<AwaitingCommitment>) {
        use verus_flows::testing::ScriptedReader;

        let key = PrivateKey::from_bytes(&[7u8; 32], true).expect("a fixed scalar is a key");
        // The chain's own policy, because the fee comes from it. VRSCTEST's
        // real figure at the time of writing, so the reservation is built
        // against a plausible one rather than zero.
        let reader = ScriptedReader::new(1_000_000)
            .with_utxo(&key.address().to_string(), 999_000, 200 * 100_000_000)
            .with_policy(verus_sdk::network::CurrencyPolicy {
                currency_id: "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq".to_string(),
                name: "VRSCTEST".to_string(),
                id_registration_fee: verus_sdk::money::Amount::from_sat(100 * 100_000_000),
                id_referral_levels: 3,
                id_import_fee: verus_sdk::money::Amount::ZERO,
                currency_registration_fee: verus_sdk::money::Amount::ZERO,
                proof_protocol: 1,
            });
        let pending = verus_flows::prepare_registration(
            &reader,
            &key,
            "pecu-test",
            &RegistrationOptions::default(),
        )
        .expect("the reservation builds");

        (Reservation::open(dir.join("registration.json")), pending)
    }

    /// The salt is on disk before anything is broadcast, and survives a restart.
    ///
    /// This is the ordering the whole module exists for. A crash between the
    /// write and the broadcast costs nothing; a crash the other way round burns
    /// the commitment fee with no way to redeem it.
    #[test]
    fn a_reservation_outlives_the_process_that_made_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut store, pending) = reservation(dir.path());
        let salt = pending.reservation.salt;

        assert!(!store.in_progress(), "nothing has been reserved yet");
        store
            .reserve("pecu-test", "main", pending)
            .expect("the reservation is written");
        assert!(store.in_progress());

        // The process ends here, before any broadcast.
        drop(store);

        let reopened = Reservation::open(dir.path().join("registration.json"));
        let found = reopened.current().expect("the reservation came back");
        assert_eq!(found.name, "pecu-test");
        assert_eq!(found.key_label, "main", "step two needs the same key");
        assert_eq!(
            found.step,
            Step::Reserved,
            "nothing was broadcast, so abandoning is still free",
        );
        assert_eq!(
            found.pending.reservation.salt, salt,
            "the salt did not survive — the fee would be unredeemable",
        );
    }

    /// A file that cannot be parsed is left where it is.
    ///
    /// It may be the only copy of a salt somebody's fee depends on. Replacing
    /// it to get a clean start would destroy exactly what this protects, so the
    /// wallet carries on without it and the bytes stay on disk for a human.
    #[test]
    fn an_unreadable_reservation_is_not_thrown_away() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("registration.json");
        std::fs::write(&path, "{ this is not json").expect("write");

        let store = Reservation::open(path.clone());
        assert!(store.current().is_none(), "it could not be read");
        drop(store);

        assert_eq!(
            std::fs::read_to_string(&path).expect("still there"),
            "{ this is not json",
            "the wallet overwrote a file that might hold a salt",
        );
    }

    /// Finishing removes it; the file is the only place the salt lived.
    #[test]
    fn finishing_removes_the_salt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("registration.json");
        let (mut store, pending) = reservation(dir.path());

        store.reserve("pecu-test", "main", pending).expect("write");
        store.mark_committed();
        assert_eq!(store.current().expect("still there").step, Step::Committed);

        store.finish();
        assert!(!store.in_progress());
        assert!(!path.exists(), "the salt is still on disk after finishing");
    }

    /// Owner-only, like every other file this wallet writes.
    #[cfg(unix)]
    #[test]
    fn the_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("registration.json");
        let (mut store, pending) = reservation(dir.path());
        store.reserve("pecu-test", "main", pending).expect("write");

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
    }
}
