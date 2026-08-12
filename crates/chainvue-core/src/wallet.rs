//! The vault's lifecycle, as the actor sees it.
//!
//! # Where the entropy decision lives
//!
//! Creating a wallet is the one moment a key comes into existence, and the SDK
//! deliberately refuses to do it: `verus-keys` has no `PrivateKey::generate`,
//! because where the bytes come from is the most security-critical choice a
//! wallet makes and a library that picks quietly moves it somewhere nobody
//! reviews. So it happens here, in one readable function, from the OS CSPRNG.

use std::path::PathBuf;
use std::sync::Arc;

use chainvue_keystore::{entropy, NewKey, Origin, Vault, VaultError};
use chainvue_protocol::{KeyOrigin, KeyVm, Secret, SeedWordVm, WalletVm};
use verus_sdk::verus_keys::{
    bip39, bip39::MnemonicError, private_key_from_seed_phrase, KeyError, PrivateKey,
};
use zeroize::Zeroizing;

/// How long without activity before the wallet locks itself.
pub const DEFAULT_AUTO_LOCK: std::time::Duration = std::time::Duration::from_mins(5);

/// How many words the confirmation step asks the user to type back.
const CHALLENGE_WORDS: usize = 3;

/// The wallet file and what is currently known about it.
pub struct Wallet {
    path: PathBuf,
    /// `Arc` so that signing can happen **off the actor**.
    ///
    /// Building and signing a payment is blocking work — it reads the funding
    /// set first — and an actor sitting inside it is an actor that will not
    /// answer Lock. `Vault` is `Send + Sync` (its interior is two `RwLock`s), so
    /// a clone of this handle can cross to a worker thread.
    ///
    /// What does **not** change is the key's reach: it is still only ever a
    /// `&PrivateKey` inside a `with_key` closure, decrypted for one operation
    /// and dropped before the closure returns. The thread it happens on is not
    /// what that guarantee was ever about.
    vault: Option<Arc<Vault>>,
    pub auto_lock: Option<std::time::Duration>,
    pub last_activity: std::time::Instant,
    pub active_key: Option<String>,
    /// A phrase being shown to its owner right now, and nothing else.
    ///
    /// Held between the moment a backup starts and the moment it is finished or
    /// abandoned. [`Wallet::lock`] drops it, so a wallet that locks itself while
    /// the backup screen is open does not leave a plaintext phrase behind —
    /// the flag on the key entry survives instead, and the screen can be
    /// reached again with the passphrase.
    backup: Option<Backup>,
}

/// The state of a backup in progress.
struct Backup {
    /// Which key's phrase this is.
    label: String,
    phrase: Zeroizing<String>,
    /// 1-based positions the confirmation step will ask for.
    challenge: Vec<u32>,
}

impl Backup {
    fn words(&self) -> Vec<&str> {
        self.phrase.split_whitespace().collect()
    }
}

impl Wallet {
    /// Look for a vault at `path`, without unlocking it.
    pub fn open_or_absent(path: PathBuf) -> Self {
        let vault = if Vault::exists(&path) {
            match Vault::open(&path) {
                Ok(vault) => Some(Arc::new(vault)),
                Err(error) => {
                    tracing::error!(%error, path = %path.display(), "the wallet file could not be read");
                    None
                }
            }
        } else {
            None
        };

        let active_key = vault
            .as_ref()
            .and_then(|v| v.keys().first().map(|k| k.label.clone()));

        Self {
            path,
            vault,
            auto_lock: Some(DEFAULT_AUTO_LOCK),
            last_activity: std::time::Instant::now(),
            active_key,
            backup: None,
        }
    }

    pub fn exists(&self) -> bool {
        self.vault.is_some()
    }

    pub fn is_unlocked(&self) -> bool {
        self.vault.as_ref().is_some_and(|vault| vault.is_unlocked())
    }

    /// A handle that can cross to a worker thread. See the field's docs.
    pub fn vault(&self) -> Option<Arc<Vault>> {
        self.vault.clone()
    }

    /// Create a wallet with one freshly generated key.
    ///
    /// The 32 bytes come from `getrandom`; the phrase comes from the SDK's
    /// BIP-39 encoder; the transparent key comes from the SDK's own
    /// `private_key_from_seed_phrase`, which is the Agama/Verus-Mobile
    /// derivation. Nothing about the derivation is reimplemented here — only
    /// the entropy is ours.
    ///
    /// Returns the backup challenge, because the phrase this just generated has
    /// never been seen by anyone and the next screen is the only chance to see
    /// it before it is sealed.
    pub fn create(&mut self, name: &str, passphrase: &Secret) -> Result<Challenge, VaultError> {
        let vault = Vault::create(&self.path, name, passphrase)?;

        let seed = entropy()?;
        let phrase = bip39::mnemonic_from_entropy(&seed);
        let key = private_key_from_seed_phrase(&phrase)?;

        vault.add_key(
            "main",
            NewKey::Generated {
                key,
                phrase: phrase.clone(),
            },
        )?;

        self.active_key = Some("main".to_string());
        self.vault = Some(Arc::new(vault));
        self.touch();
        // The phrase stays in memory from here until the backup screen is
        // finished with it. Nothing else in the process has a copy.
        self.begin_backup("main", phrase)
    }

    // ── Backup ──────────────────────────────────────────────────────────────

    /// Start showing an existing key's phrase.
    ///
    /// Takes the passphrase and runs the key derivation again even though the
    /// wallet is open — see [`Vault::reveal_phrase`]. This is the route back for
    /// a wallet that was created and then closed before the words were written
    /// down.
    pub fn begin_reveal(
        &mut self,
        label: &str,
        passphrase: &Secret,
    ) -> Result<Challenge, VaultError> {
        let phrase = self
            .vault
            .as_ref()
            .ok_or(VaultError::Locked)?
            .reveal_phrase(label, passphrase)?;
        self.touch();
        self.begin_backup(label, phrase)
    }

    fn begin_backup(
        &mut self,
        label: &str,
        phrase: Zeroizing<String>,
    ) -> Result<Challenge, VaultError> {
        let word_count = phrase.split_whitespace().count();
        let challenge = pick_challenge(word_count, &*entropy()?);

        self.backup = Some(Backup {
            label: label.to_string(),
            phrase,
            challenge: challenge.clone(),
        });

        Ok(Challenge {
            positions: challenge,
            word_count: u32::try_from(word_count).unwrap_or(0),
        })
    }

    /// The words being backed up, if a backup is in progress.
    ///
    /// Numbered and separate. A joined string is the single object that turns a
    /// stray log line or an accidental clipboard write into a total loss, so one
    /// is never built above this line.
    pub fn backup_words(&self) -> Vec<SeedWordVm> {
        self.backup
            .as_ref()
            .map(|backup| {
                backup
                    .words()
                    .iter()
                    .enumerate()
                    .map(|(index, word)| SeedWordVm {
                        index: u32::try_from(index + 1).unwrap_or(0),
                        word: (*word).to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the typed words match, and nothing else.
    ///
    /// One bool on purpose. Saying *which* word was wrong would turn this
    /// screen into a checker someone could feed guesses to one position at a
    /// time, which is the opposite of what it is for.
    pub fn confirm_backup(&self, checks: &[(u32, String)]) -> bool {
        let Some(backup) = &self.backup else {
            return false;
        };

        // Answering a different question than the one asked does not count.
        let asked: Vec<u32> = {
            let mut positions: Vec<u32> = checks.iter().map(|(p, _)| *p).collect();
            positions.sort_unstable();
            positions
        };
        if asked != backup.challenge {
            return false;
        }

        let words = backup.words();
        checks.iter().all(|(position, typed)| {
            usize::try_from(*position)
                .ok()
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| words.get(index))
                .is_some_and(|word| typed.trim().eq_ignore_ascii_case(word))
        })
    }

    /// Record the backup as done and drop the phrase.
    pub fn finish_backup(&mut self) -> Result<(), VaultError> {
        let Some(backup) = self.backup.take() else {
            return Ok(());
        };
        self.touch();

        let vault = self.vault.as_ref().ok_or(VaultError::Locked)?;
        vault.mark_backed_up(&backup.label)
    }

    /// Drop the phrase without recording anything. The key stays flagged as
    /// needing a backup, so the offer comes back.
    pub fn abandon_backup(&mut self) {
        self.backup = None;
    }

    pub fn backup_in_progress(&self) -> bool {
        self.backup.is_some()
    }

    /// Bring in a key that exists somewhere else.
    ///
    /// # Nothing is written until the key exists
    ///
    /// The material is checked and the key derived *first*, and only then is a
    /// vault created or touched. Get that order wrong and a rejected phrase
    /// leaves an empty wallet file on disk — after which the app stops offering
    /// to create one and starts asking for a passphrase to a wallet that holds
    /// nothing.
    ///
    /// `passphrase` is used only when there is no wallet yet, which is what
    /// restoring on a fresh install is. With one already open it is ignored:
    /// adding a key to a wallet you just unlocked should not re-prompt.
    pub fn import(
        &mut self,
        label: &str,
        material: Imported,
        passphrase: &Secret,
    ) -> Result<(), ImportError> {
        let new = prepare(material)?;

        if self.vault.is_none() {
            // Named for the wallet, not the key: this is a restore, and the
            // person doing it is naming their wallet, not a slot in it.
            self.vault = Some(Arc::new(Vault::create(&self.path, "ChainVue", passphrase)?));
        }

        let vault = self.vault.as_ref().ok_or(VaultError::Locked)?;
        vault.add_key(label, new)?;

        if self.active_key.is_none() {
            self.active_key = Some(label.to_string());
        }
        self.touch();
        Ok(())
    }

    /// Change the passphrase that protects this wallet.
    ///
    /// Costs two Argon2 runs and re-seals 32 bytes — no key is re-encrypted.
    /// See [`Vault::change_passphrase`].
    pub fn change_passphrase(&mut self, old: &Secret, new: &Secret) -> Result<(), VaultError> {
        self.vault
            .as_ref()
            .ok_or(VaultError::Locked)?
            .change_passphrase(old, new)?;
        self.touch();
        Ok(())
    }

    pub fn unlock(&mut self, passphrase: &Secret) -> Result<(), VaultError> {
        let vault = self.vault.as_ref().ok_or(VaultError::Locked)?;
        vault.unlock(passphrase)?;
        self.touch();
        Ok(())
    }

    /// Drop the data key — and any phrase currently on screen.
    ///
    /// The second half is the reason this takes `&mut self`. Locking while the
    /// backup screen is open has to leave nothing behind; the words go back to
    /// being sealed bytes, and the key's `backed_up` flag brings the offer back
    /// after the next unlock.
    pub fn lock(&mut self) {
        self.backup = None;
        if let Some(vault) = &self.vault {
            vault.lock();
        }
    }

    /// Note that the user did something, for the idle timer.
    pub fn touch(&mut self) {
        self.last_activity = std::time::Instant::now();
    }

    /// Whether the idle timeout has elapsed.
    pub fn should_auto_lock(&self) -> bool {
        self.is_unlocked()
            && self
                .auto_lock
                .is_some_and(|limit| self.last_activity.elapsed() >= limit)
    }

    /// The address of the active key, if there is one. Readable while locked.
    pub fn active_address(&self) -> Option<String> {
        let vault = self.vault.as_ref()?;
        let label = self.active_key.as_ref()?;
        vault
            .keys()
            .into_iter()
            .find(|k| &k.label == label)
            .map(|k| k.address)
    }

    pub fn view(&self) -> WalletVm {
        let keys: Vec<KeyVm> = self
            .vault
            .as_ref()
            .map(|v| {
                v.keys()
                    .into_iter()
                    .map(|k| KeyVm {
                        label: k.label,
                        address: k.address,
                        origin: match k.origin {
                            Origin::Generated => KeyOrigin::Generated,
                            Origin::ImportedPhrase => KeyOrigin::ImportedPhrase,
                            Origin::ImportedWif => KeyOrigin::ImportedWif,
                        },
                        // Whether an address has ever been paid needs the chain;
                        // filled in once the portfolio service lands.
                        used: false,
                        backed_up: k.backed_up,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let needs_backup = keys
            .iter()
            .find(|k| !k.backed_up && k.origin == KeyOrigin::Generated)
            .map(|k| k.label.clone());

        WalletVm {
            name: self
                .vault
                .as_ref()
                .map(|vault| vault.name())
                .unwrap_or_default(),
            exists: self.exists(),
            locked: !self.is_unlocked(),
            keys,
            active_key: self.active_key.clone(),
            auto_lock_minutes: self
                .auto_lock
                .map(|d| u32::try_from(d.as_secs() / 60).unwrap_or(u32::MAX)),
            needs_backup,
        }
    }
}

/// Material handed in for an import.
pub enum Imported {
    /// A BIP-39 mnemonic. Checked, and refused when the check fails.
    Phrase(Secret),
    /// Free text, hashed exactly as typed.
    Text(Secret),
    Wif(Secret),
}

/// Why an import did not happen.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error(transparent)]
    Vault(#[from] VaultError),
    /// The phrase is not a BIP-39 mnemonic. Kept as its own variant so the
    /// three ways that can be true stay three different messages on screen —
    /// "a word is misspelled" and "that is not a phrase at all" call for
    /// different next steps.
    #[error(transparent)]
    Mnemonic(#[from] MnemonicError),
    /// The key material itself would not parse — a WIF from another chain, or
    /// one with a character dropped.
    #[error(transparent)]
    Key(#[from] KeyError),
}

/// Turn material into a key, or fail. Touches nothing.
///
/// The BIP-39 checksum is the only thing standing between a mistyped word and a
/// valid-looking wallet with no funds in it, so a phrase that fails it is
/// refused here rather than imported with a warning nobody reads. Free text is
/// a separate choice precisely so that this check can be strict.
fn prepare(material: Imported) -> Result<NewKey, ImportError> {
    Ok(match material {
        Imported::Phrase(phrase) => {
            bip39::validate_mnemonic(phrase.expose())?;
            NewKey::FromPhrase {
                key: private_key_from_seed_phrase(phrase.expose())?,
                phrase: Zeroizing::new(phrase.expose().to_string()),
            }
        }
        Imported::Text(text) => NewKey::FromPhrase {
            key: private_key_from_seed_phrase(text.expose())?,
            phrase: Zeroizing::new(text.expose().to_string()),
        },
        Imported::Wif(wif) => NewKey::FromWif {
            key: PrivateKey::from_wif(wif.expose())?,
        },
    })
}

/// What the confirmation step will ask for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Challenge {
    /// 1-based word positions, ascending.
    pub positions: Vec<u32>,
    pub word_count: u32,
}

/// Pick [`CHALLENGE_WORDS`] distinct positions in `1..=word_count`.
///
/// Rejection sampling rather than a plain modulo: with 24 words, `byte % 24`
/// would make positions 1–16 more likely than 17–24, because 256 is not a
/// multiple of 24. It changes nothing an attacker could use, but a biased draw
/// in the file that picks which words get checked is the kind of detail that
/// gets copied into somewhere it does matter.
fn pick_challenge(word_count: usize, bytes: &[u8; 32]) -> Vec<u32> {
    let mut picks: Vec<u32> = Vec::with_capacity(CHALLENGE_WORDS);
    if word_count == 0 {
        return picks;
    }

    let wanted = CHALLENGE_WORDS.min(word_count);
    let limit = 256 - (256 % word_count.min(256));

    for byte in bytes {
        if picks.len() == wanted {
            break;
        }
        let value = usize::from(*byte);
        if value >= limit {
            continue;
        }
        let position = u32::try_from(value % word_count).unwrap_or(0) + 1;
        if !picks.contains(&position) {
            picks.push(position);
        }
    }

    // Running out of bytes needs 32 draws to collide or be rejected, which for
    // 24 words is vanishingly unlikely — but "unlikely" is not "never", and a
    // short challenge would silently weaken the check.
    let mut fallback = 1u32;
    while picks.len() < wanted {
        if !picks.contains(&fallback) {
            picks.push(fallback);
        }
        fallback += 1;
    }

    picks.sort_unstable();
    picks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_created_wallet_is_unlocked_and_has_one_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut wallet = Wallet::open_or_absent(dir.path().join("vault.json"));
        assert!(!wallet.exists());

        wallet
            .create("test", &Secret::from("a passphrase"))
            .expect("create");

        assert!(wallet.exists());
        assert!(wallet.is_unlocked());

        let view = wallet.view();
        assert_eq!(view.keys.len(), 1);
        assert_eq!(view.keys[0].origin, KeyOrigin::Generated);
        assert!(view.keys[0].address.starts_with('R'), "{:?}", view.keys[0]);
        assert!(!view.locked);
    }

    /// Two wallets created with the same passphrase must not share a key —
    /// otherwise the entropy is not doing its job.
    #[test]
    fn two_wallets_do_not_share_an_address() {
        let dir = tempfile::tempdir().expect("tempdir");

        let mut first = Wallet::open_or_absent(dir.path().join("one.json"));
        first.create("one", &Secret::from("same")).expect("create");

        let mut second = Wallet::open_or_absent(dir.path().join("two.json"));
        second.create("two", &Secret::from("same")).expect("create");

        assert_ne!(
            first.active_address(),
            second.active_address(),
            "two wallets produced the same key",
        );
    }

    #[test]
    fn a_reopened_wallet_is_locked_but_lists_its_addresses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("vault.json");

        let mut wallet = Wallet::open_or_absent(path.clone());
        wallet
            .create("test", &Secret::from("pass"))
            .expect("create");
        let address = wallet.active_address().expect("an address");

        let reopened = Wallet::open_or_absent(path);
        assert!(reopened.exists());
        assert!(!reopened.is_unlocked());
        // The address survives a lock, which is what makes a receive screen
        // work without a passphrase.
        assert_eq!(reopened.active_address(), Some(address));
    }

    /// The failure that would hurt most: a refused phrase must not leave a
    /// wallet file behind. If it did, the app would stop offering to create one
    /// and start asking for the passphrase to a wallet holding nothing.
    #[test]
    fn a_refused_phrase_leaves_no_wallet_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("vault.json");
        let mut wallet = Wallet::open_or_absent(path.clone());

        // Twelve real words with the checksum broken by swapping the last one.
        let mistyped = "abandon abandon abandon abandon abandon abandon \
                        abandon abandon abandon abandon abandon abandon";
        let error = wallet
            .import(
                "main",
                Imported::Phrase(Secret::from(mistyped)),
                &Secret::from("pass"),
            )
            .expect_err("a broken checksum must be refused");

        assert!(matches!(
            error,
            ImportError::Mnemonic(MnemonicError::Checksum)
        ));
        assert!(!wallet.exists());
        assert!(!path.exists(), "a rejected import created a wallet file");
    }

    #[test]
    fn a_restore_creates_the_wallet_it_lands_in() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("vault.json");
        let mut wallet = Wallet::open_or_absent(path.clone());
        assert!(!wallet.exists());

        let phrase = "abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon about";
        wallet
            .import(
                "main",
                Imported::Phrase(Secret::from(phrase)),
                &Secret::from("pass"),
            )
            .expect("restore");

        assert!(wallet.exists());
        assert!(wallet.is_unlocked());

        let view = wallet.view();
        assert_eq!(view.keys.len(), 1);
        assert_eq!(view.keys[0].origin, KeyOrigin::ImportedPhrase);
        // Nothing to write down: they already have it.
        assert_eq!(view.needs_backup, None);

        // And it is the same key after a restart, which is the entire promise
        // of a restore.
        let address = wallet.active_address().expect("an address");
        assert_eq!(Wallet::open_or_absent(path).active_address(), Some(address));
    }

    /// The same words twice must give the same key, or "restore" means nothing.
    #[test]
    fn a_phrase_restores_the_same_address_every_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let phrase = "abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon about";

        let mut addresses = Vec::new();
        for name in ["one.json", "two.json"] {
            let mut wallet = Wallet::open_or_absent(dir.path().join(name));
            wallet
                .import(
                    "main",
                    Imported::Phrase(Secret::from(phrase)),
                    &Secret::from("pass"),
                )
                .expect("restore");
            addresses.push(wallet.active_address());
        }
        assert_eq!(addresses[0], addresses[1]);
        assert!(addresses[0].is_some());
    }

    /// Free text is a legitimate Verus transparent phrase, but it is a separate
    /// choice — asking for a mnemonic and getting free text is a typo, not a
    /// preference.
    #[test]
    fn free_text_is_accepted_only_when_it_is_asked_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        let text = "not a bip39 mnemonic at all";

        let mut refusing = Wallet::open_or_absent(dir.path().join("refuse.json"));
        assert!(refusing
            .import(
                "main",
                Imported::Phrase(Secret::from(text)),
                &Secret::from("pass"),
            )
            .is_err());

        let mut accepting = Wallet::open_or_absent(dir.path().join("accept.json"));
        accepting
            .import(
                "main",
                Imported::Text(Secret::from(text)),
                &Secret::from("pass"),
            )
            .expect("free text must import when chosen");
        assert!(accepting.active_address().is_some());
    }

    /// A Bitcoin WIF is a valid key for a chain this wallet does not speak.
    #[test]
    fn a_key_from_another_chain_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut wallet = Wallet::open_or_absent(dir.path().join("vault.json"));

        let bitcoin_wif = "5HueCGU8rMjxEXxiPuD5BDku4MkFqeZyd4dZ1jvhTVqvbTLvyTJ";
        let error = wallet
            .import(
                "main",
                Imported::Wif(Secret::from(bitcoin_wif)),
                &Secret::from("pass"),
            )
            .expect_err("a foreign WIF must be refused");

        assert!(matches!(error, ImportError::Key(_)), "{error:?}");
        assert!(!wallet.exists());
    }

    #[test]
    fn a_wif_import_lands_with_the_right_origin() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut wallet = Wallet::open_or_absent(dir.path().join("vault.json"));
        wallet
            .create("test", &Secret::from("pass"))
            .expect("create");

        wallet
            .import(
                "imported",
                Imported::Wif(Secret::from(
                    "UusoQWsobQKUkezgBJa22D9G4t9Avo6k8wD5UUxmmfAEoTN8bawc",
                )),
                &Secret::from("pass"),
            )
            .expect("import");

        let view = wallet.view();
        assert_eq!(view.keys.len(), 2);
        let imported = view
            .keys
            .iter()
            .find(|k| k.label == "imported")
            .expect("the imported key");
        assert_eq!(imported.origin, KeyOrigin::ImportedWif);
        assert_eq!(imported.address, "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX");
    }

    #[test]
    fn a_new_wallet_asks_for_three_of_its_own_words() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut wallet = Wallet::open_or_absent(dir.path().join("vault.json"));
        let challenge = wallet
            .create("test", &Secret::from("pass"))
            .expect("create");

        assert_eq!(challenge.word_count, 24);
        assert_eq!(challenge.positions.len(), 3);
        assert!(challenge.positions.iter().all(|p| (1..=24).contains(p)));
        assert!(
            challenge.positions.windows(2).all(|w| w[0] < w[1]),
            "positions must be distinct and ascending: {:?}",
            challenge.positions,
        );

        let words = wallet.backup_words();
        assert_eq!(words.len(), 24);
        assert_eq!(words[0].index, 1);
        assert!(words.iter().all(|w| !w.word.is_empty()));

        // The right answers pass; one wrong word fails the whole check.
        let answers: Vec<(u32, String)> = challenge
            .positions
            .iter()
            .map(|p| {
                let word = words
                    .iter()
                    .find(|w| w.index == *p)
                    .map(|w| w.word.clone())
                    .unwrap_or_default();
                (*p, word)
            })
            .collect();
        assert!(wallet.confirm_backup(&answers));

        let mut wrong = answers.clone();
        wrong[0].1 = "definitely-not-the-word".to_string();
        assert!(!wallet.confirm_backup(&wrong));

        // Case and stray whitespace are the user's typing, not a mistake.
        let sloppy: Vec<(u32, String)> = answers
            .iter()
            .map(|(p, w)| (*p, format!("  {} ", w.to_uppercase())))
            .collect();
        assert!(wallet.confirm_backup(&sloppy));
    }

    /// Answering three words the wallet did not ask about is not a pass — and
    /// neither is answering one of them three times.
    #[test]
    fn the_confirmation_only_accepts_the_positions_it_asked_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut wallet = Wallet::open_or_absent(dir.path().join("vault.json"));
        let challenge = wallet
            .create("test", &Secret::from("pass"))
            .expect("create");
        let words = wallet.backup_words();

        let word_at = |position: u32| {
            words
                .iter()
                .find(|w| w.index == position)
                .map(|w| w.word.clone())
                .unwrap_or_default()
        };

        // Correct words, wrong questions.
        let unasked: Vec<u32> = (1..=24)
            .filter(|p| !challenge.positions.contains(p))
            .take(3)
            .collect();
        let answers: Vec<(u32, String)> = unasked.iter().map(|p| (*p, word_at(*p))).collect();
        assert!(!wallet.confirm_backup(&answers));

        // One correct answer, repeated.
        let first = challenge.positions[0];
        let repeated = vec![
            (first, word_at(first)),
            (first, word_at(first)),
            (first, word_at(first)),
        ];
        assert!(!wallet.confirm_backup(&repeated));
    }

    #[test]
    fn finishing_the_backup_records_it_and_drops_the_phrase() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("vault.json");

        let mut wallet = Wallet::open_or_absent(path.clone());
        wallet
            .create("test", &Secret::from("pass"))
            .expect("create");

        assert!(wallet.backup_in_progress());
        assert_eq!(wallet.view().needs_backup.as_deref(), Some("main"));

        wallet.finish_backup().expect("finish");

        assert!(!wallet.backup_in_progress());
        assert!(wallet.backup_words().is_empty());
        assert_eq!(wallet.view().needs_backup, None);

        // And it survives a restart, which is the whole point of persisting it.
        let reopened = Wallet::open_or_absent(path);
        assert_eq!(reopened.view().needs_backup, None);
    }

    /// A wallet closed before the backup was finished must still be able to
    /// reach its own phrase, or the words are gone for good.
    #[test]
    fn an_unfinished_backup_can_be_picked_up_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("vault.json");

        let mut wallet = Wallet::open_or_absent(path.clone());
        let first = wallet
            .create("test", &Secret::from("pass"))
            .expect("create");
        let original = wallet.backup_words();

        // Locking mid-backup drops the phrase from memory...
        wallet.lock();
        assert!(!wallet.backup_in_progress());
        assert!(wallet.backup_words().is_empty());

        // ...and the offer comes back after a restart.
        let mut reopened = Wallet::open_or_absent(path);
        assert_eq!(reopened.view().needs_backup.as_deref(), Some("main"));

        reopened.unlock(&Secret::from("pass")).expect("unlock");
        let second = reopened
            .begin_reveal("main", &Secret::from("pass"))
            .expect("reveal");

        assert_eq!(reopened.backup_words(), original, "a different phrase");
        assert_eq!(second.word_count, first.word_count);
    }

    #[test]
    fn revealing_a_phrase_needs_the_right_passphrase() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut wallet = Wallet::open_or_absent(dir.path().join("vault.json"));
        wallet
            .create("test", &Secret::from("pass"))
            .expect("create");
        wallet.finish_backup().expect("finish");

        assert!(wallet.begin_reveal("main", &Secret::from("wrong")).is_err());
        assert!(!wallet.backup_in_progress());
        assert!(wallet.begin_reveal("main", &Secret::from("pass")).is_ok());
    }

    /// An imported key has no words the user has not already seen, so nothing
    /// should nag them to write anything down.
    #[test]
    fn an_imported_key_is_not_waiting_on_a_backup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut wallet = Wallet::open_or_absent(dir.path().join("vault.json"));
        wallet
            .create("test", &Secret::from("pass"))
            .expect("create");
        wallet.finish_backup().expect("finish");

        wallet
            .import(
                "imported",
                Imported::Wif(Secret::from(
                    "UusoQWsobQKUkezgBJa22D9G4t9Avo6k8wD5UUxmmfAEoTN8bawc",
                )),
                &Secret::from("pass"),
            )
            .expect("import");

        assert_eq!(wallet.view().needs_backup, None);
    }

    #[test]
    fn the_challenge_covers_every_position_over_many_draws() {
        // Every position must be reachable — a picker that could never ask for
        // word 24 would be a weaker check than it looks.
        let mut seen = std::collections::BTreeSet::new();
        for round in 0..64u8 {
            let mut bytes = [0u8; 32];
            for (index, slot) in bytes.iter_mut().enumerate() {
                *slot = round
                    .wrapping_mul(37)
                    .wrapping_add(u8::try_from(index).unwrap_or(0).wrapping_mul(11));
            }
            let picks = pick_challenge(24, &bytes);
            assert_eq!(picks.len(), 3);
            assert!(picks.windows(2).all(|w| w[0] < w[1]));
            seen.extend(picks);
        }
        assert_eq!(seen.len(), 24, "unreachable positions: {seen:?}");
    }

    #[test]
    fn auto_lock_fires_only_after_the_idle_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut wallet = Wallet::open_or_absent(dir.path().join("vault.json"));
        wallet
            .create("test", &Secret::from("pass"))
            .expect("create");

        wallet.auto_lock = Some(std::time::Duration::from_hours(1));
        assert!(!wallet.should_auto_lock());

        wallet.auto_lock = Some(std::time::Duration::ZERO);
        assert!(wallet.should_auto_lock());

        // A locked wallet has nothing left to lock.
        wallet.lock();
        assert!(!wallet.should_auto_lock());

        // "Never" means never.
        wallet.unlock(&Secret::from("pass")).expect("unlock");
        wallet.auto_lock = None;
        assert!(!wallet.should_auto_lock());
    }
}
