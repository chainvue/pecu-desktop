//! A currency somebody has decided to make, held across everything in between.
//!
//! # Why this file exists
//!
//! Creating a currency from a name nobody owns yet is two transactions with a
//! wait between them: register the identity, wait for it to be mined, then
//! define the currency under it. The wallet does the second one by itself,
//! which is the whole promise — and a promise that does not survive the process
//! ending is not one.
//!
//! What is at stake is not the same as `registration.rs`, and the difference
//! decides how this behaves. There, the thing being written down is a salt that
//! **cannot be recovered from anywhere**, so the write is fatal if it fails: a
//! commitment broadcast without its salt has burned a fee for nothing.
//!
//! Here, what is written down is a **decision**: which kind, which reserves,
//! what supply, what start. Losing it costs the work of filling in a form
//! again — real, but not money. So the write is best-effort and never blocks a
//! launch, and that asymmetry is deliberate rather than an oversight.
//!
//! What losing it *does* cost is worse than the form: it leaves an identity
//! that exists for no reason, registered and paid for, with nothing in the
//! wallet remembering what it was for.
//!
//! # And why it is a file rather than a row in SQLite
//!
//! It sits beside `registration.json`, in the same per-chain directory, for the
//! same reason: a launch that spans two transactions is one unfinished thing,
//! and one file whose whole content is that thing is easier to reason about
//! than a table that could hold several. It carries no key material — a
//! definition is public the moment it is mined — so unlike the salt this is not
//! secret. It gets the same owner-only permissions anyway, because a file
//! beside the vault that is readable by anything else invites the question of
//! which of them are.
//!
//! # There is deliberately only ever one
//!
//! An identity registration already permits only one at a time, and this hangs
//! off one. Two would mean two names in flight against a person who can watch
//! one.

use std::path::{Path, PathBuf};

/// How far along an unfinished launch is.
///
/// Recorded rather than inferred. "Waiting for a name" and "the name exists,
/// the currency does not" look identical from the outside and mean completely
/// different things: the first may still be abandoned for free, the second has
/// an identity already paid for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// The identity is being registered. `registration.json` is the authority
    /// on how that is going; this only records that a currency is waiting for
    /// it.
    AwaitingIdentity,
    /// The identity exists. The currency has not been defined, and the wallet
    /// owes one.
    ReadyToDefine,
}

/// One currency waiting to be made.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Record {
    /// The identity it will be defined under, by name.
    ///
    /// A name rather than an i-address, because on the path that starts from a
    /// new name the address does not exist yet — the identity has not been
    /// registered. It is resolved when the launch is built.
    pub identity: String,
    /// Which key funds it. The same key that registered the identity: the
    /// launch spends its coins, and a wallet with several would otherwise pick
    /// whichever was active when it resumed.
    pub key_label: String,
    pub step: Step,
    /// What was configured, exactly as it was typed.
    ///
    /// Stored as the draft rather than as a built `CurrencyDefinition` on
    /// purpose. A definition needs the parent currency and a start height, and
    /// both are facts about the chain at the moment of building — a height
    /// written down today is in the past by the time somebody resumes tomorrow,
    /// and a launch aimed at the past is refused. The draft carries a *delay*,
    /// which is still true whenever it is read.
    pub draft: pecu_protocol::CurrencyDraft,
}

/// The one unfinished launch, on disk.
pub struct Intent {
    path: PathBuf,
    record: Option<Record>,
}

impl Intent {
    /// Read whatever is there.
    ///
    /// An unparseable file is **left alone**, logged, and treated as absent.
    /// Overwriting it would throw away the only record of what somebody was
    /// making — and unlike a salt it is at least readable by hand afterwards,
    /// which is worth preserving.
    pub fn open(path: PathBuf) -> Self {
        let record = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Record>(&text) {
                Ok(record) => Some(record),
                Err(error) => {
                    tracing::error!(
                        %error,
                        path = %path.display(),
                        "an unfinished currency launch could not be read; leaving the file alone",
                    );
                    None
                }
            },
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::error!(%error, path = %path.display(), "could not read the launch file");
                }
                None
            }
        };
        Self { path, record }
    }

    pub fn current(&self) -> Option<&Record> {
        self.record.as_ref()
    }

    pub fn in_progress(&self) -> bool {
        self.record.is_some()
    }

    /// Write down what somebody has decided to make.
    ///
    /// **Best effort, and it returns nothing.** Failing to record the decision
    /// must not stop the identity being registered: the registration is the
    /// step that costs money and the one somebody is waiting on, and refusing
    /// to start it because a note could not be filed would trade a real thing
    /// for a convenience.
    ///
    /// This is the opposite of `registration::Reservation::reserve`, which
    /// refuses outright — because there what cannot be written is a salt, and
    /// losing it burns a fee.
    pub fn begin(&mut self, identity: &str, key_label: &str, draft: pecu_protocol::CurrencyDraft) {
        self.record = Some(Record {
            identity: identity.to_string(),
            key_label: key_label.to_string(),
            step: Step::AwaitingIdentity,
            draft,
        });
        if let Err(error) = self.persist() {
            tracing::warn!(%error, "the currency being made could not be written down");
        }
    }

    /// The identity landed. The wallet now owes a currency.
    pub fn identity_exists(&mut self) {
        if let Some(record) = &mut self.record {
            record.step = Step::ReadyToDefine;
        }
        if let Err(error) = self.persist() {
            tracing::warn!(%error, "the launch step could not be written down");
        }
    }

    /// Done, or given up on. Removes the file.
    pub fn finish(&mut self) {
        self.record = None;
        if let Err(error) = std::fs::remove_file(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%error, "could not remove the finished launch");
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

    fn draft() -> pecu_protocol::CurrencyDraft {
        pecu_protocol::CurrencyDraft {
            kind: "basket".to_string(),
            identity: String::new(),
            new_name: String::new(),
            mintable: true,
            start_delay: "20".to_string(),
            reserves: vec![pecu_protocol::ReserveDraft {
                currency: "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq".to_string(),
                name: "VRSCTEST".to_string(),
                weight: "100".to_string(),
            }],
            preallocations: Vec::new(),
        }
    }

    /// The whole point: a decision made before the identity existed is still
    /// there after the process that made it has gone.
    #[test]
    fn a_launch_outlives_the_process_that_started_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("launch.json");

        {
            let mut intent = Intent::open(path.clone());
            assert!(!intent.in_progress());
            intent.begin("market@", "main", draft());
            assert!(intent.in_progress());
        }

        let resumed = Intent::open(path);
        let record = resumed.current().expect("the launch came back");
        assert_eq!(record.identity, "market@");
        assert_eq!(record.key_label, "main");
        assert_eq!(record.step, Step::AwaitingIdentity);
        // And the configuration with it — the part that exists nowhere else.
        assert_eq!(record.draft.kind, "basket");
        assert_eq!(record.draft.reserves.len(), 1);
        assert!(record.draft.mintable);
    }

    /// A delay survives; a height would not.
    ///
    /// The draft is stored rather than a built definition because a definition
    /// carries an absolute start height, and a height written down today is in
    /// the past by the time somebody resumes tomorrow — which consensus
    /// refuses. This asserts the stored shape stays the one that is still true
    /// later.
    #[test]
    fn what_is_stored_is_a_delay_rather_than_a_height() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("launch.json");

        let mut intent = Intent::open(path.clone());
        intent.begin("market@", "main", draft());

        let text = std::fs::read_to_string(&path).expect("written");
        assert!(text.contains("start_delay"), "{text}");
        assert!(
            !text.contains("start_block"),
            "an absolute height was written down and will be in the past on resume: {text}",
        );
    }

    /// The two steps mean different things and both have to survive.
    ///
    /// "Waiting for a name" may still be abandoned for free. "The name exists"
    /// means an identity has been paid for and the wallet owes a currency.
    #[test]
    fn the_step_survives_and_says_which_of_the_two_it_is() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("launch.json");

        {
            let mut intent = Intent::open(path.clone());
            intent.begin("market@", "main", draft());
            intent.identity_exists();
        }

        let resumed = Intent::open(path);
        assert_eq!(
            resumed.current().expect("still there").step,
            Step::ReadyToDefine,
        );
    }

    /// An unreadable file is not thrown away.
    ///
    /// It is the only record of what somebody was making, and unlike a salt it
    /// can be read by hand afterwards.
    #[test]
    fn an_unreadable_launch_is_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("launch.json");
        std::fs::write(&path, "{ not json at all").expect("write");

        let intent = Intent::open(path.clone());
        assert!(!intent.in_progress());
        assert!(
            path.exists(),
            "an unreadable launch file was removed rather than left for somebody to look at",
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("still readable"),
            "{ not json at all",
        );
    }

    #[test]
    fn finishing_removes_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("launch.json");

        let mut intent = Intent::open(path.clone());
        intent.begin("market@", "main", draft());
        assert!(path.exists());

        intent.finish();
        assert!(!intent.in_progress());
        assert!(!path.exists());
    }

    /// Beside the vault, and owner-only.
    ///
    /// This carries no secret — a definition is public the moment it is mined —
    /// but a file beside the vault that anything can read invites the question
    /// of which of its neighbours are.
    #[cfg(unix)]
    #[test]
    fn the_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("launch.json");

        let mut intent = Intent::open(path.clone());
        intent.begin("market@", "main", draft());

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
    }
}
