//! Opening, reading and writing the vault.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use pecu_protocol::Secret;
use verus_sdk::verus_keys::{KeyError, PrivateKey};
use zeroize::Zeroizing;

use crate::entropy;
use crate::envelope::{Kdf, KeyEntry, Origin, Sealed, VaultDoc, VAULT_VERSION};

/// Argon2id cost for a new vault.
///
/// 64 MiB / 3 passes / 1 lane — roughly 100–150 ms on a desktop, and about 3.4×
/// an offline attacker's cost over OWASP's *minimum* interactive profile. The
/// sibling CLI uses that minimum because a CLI may run on a constrained host; a
/// desktop wallet is not constrained. Stored per vault, so this can be raised
/// in a later version without stranding anything already written.
const MEMORY_KIB: u32 = 64 * 1024;
const ITERATIONS: u32 = 3;
const PARALLELISM: u32 = 1;

const SALT_BYTES: usize = 16;
const NONCE_BYTES: usize = 24;
const KEY_BYTES: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("`{0}` is not a usable key name")]
    BadLabel(String),

    #[error("there is already a key called `{0}`")]
    DuplicateLabel(String),

    #[error("no key called `{0}`")]
    NoSuchKey(String),

    #[error("the wallet is locked")]
    Locked,

    #[error("that passphrase does not unlock this wallet")]
    WrongPassphrase,

    #[error("a passphrase is required")]
    EmptyPassphrase,

    #[error("this file is not a vault this version understands: {0}")]
    Corrupt(String),

    #[error("cannot {action} {}", path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("that is not a valid private key")]
    Key(#[from] KeyError),

    #[error("the operating system would not supply randomness")]
    NoEntropy,
}

/// A key as the outside world may see it: public facts only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyRef {
    pub label: String,
    pub address: String,
    pub origin: Origin,
    pub created: u64,
    /// Whether the recovery phrase has been shown and confirmed. See
    /// [`crate::envelope::KeyEntry::backed_up`].
    pub backed_up: bool,
}

/// How a key is being added.
pub enum NewKey {
    /// Freshly generated, with the phrase that reproduces it.
    Generated {
        key: PrivateKey,
        phrase: Zeroizing<String>,
    },
    /// Restored from a recovery phrase.
    FromPhrase {
        key: PrivateKey,
        phrase: Zeroizing<String>,
    },
    /// Imported as a WIF. No phrase exists for it, ever.
    FromWif { key: PrivateKey },
}

/// The wallet file.
///
/// # What is held while unlocked
///
/// Only the 32-byte data key. **Never a `PrivateKey`** — those are decrypted
/// inside [`Vault::with_key`] and dropped before it returns, so the window in
/// which a signing key exists is one operation rather than one session.
pub struct Vault {
    path: PathBuf,
    doc: RwLock<VaultDoc>,
    dek: RwLock<Option<Zeroizing<[u8; KEY_BYTES]>>>,
}

impl Vault {
    /// Create a new vault and write it.
    pub fn create(path: &Path, name: &str, passphrase: &Secret) -> Result<Self, VaultError> {
        if passphrase.expose().is_empty() {
            return Err(VaultError::EmptyPassphrase);
        }

        let mut wallet_id = [0u8; 16];
        getrandom::fill(&mut wallet_id).map_err(|_| VaultError::NoEntropy)?;
        let salt = entropy()?;

        let kdf = Kdf {
            algorithm: "argon2id".to_string(),
            salt: hex::encode(&salt[..SALT_BYTES]),
            memory_kib: MEMORY_KIB,
            iterations: ITERATIONS,
            parallelism: PARALLELISM,
        };

        let dek = entropy()?;
        let mut doc = VaultDoc {
            version: VAULT_VERSION,
            wallet_id: hex::encode(wallet_id),
            name: name.to_string(),
            created: now(),
            network_hint: "testnet".to_string(),
            kdf,
            // Placeholder; replaced immediately below, once the AAD can be
            // computed from the finished document.
            wrapped_dek: Sealed {
                algorithm: String::new(),
                nonce: String::new(),
                ciphertext: String::new(),
            },
            keys: Vec::new(),
        };

        let kek = derive(&doc.kdf, passphrase)?;
        doc.wrapped_dek = seal(&kek, dek.as_ref(), doc.dek_aad().as_bytes())?;

        let vault = Self {
            path: path.to_path_buf(),
            doc: RwLock::new(doc),
            dek: RwLock::new(Some(dek)),
        };
        vault.persist()?;
        Ok(vault)
    }

    /// Read a vault from disk. Does not unlock it.
    pub fn open(path: &Path) -> Result<Self, VaultError> {
        let text = std::fs::read_to_string(path).map_err(|source| VaultError::Io {
            action: "read",
            path: path.to_path_buf(),
            source,
        })?;
        let doc: VaultDoc =
            serde_json::from_str(&text).map_err(|e| VaultError::Corrupt(e.to_string()))?;

        if doc.version != VAULT_VERSION {
            return Err(VaultError::Corrupt(format!(
                "vault version {} — this build reads version {VAULT_VERSION}",
                doc.version
            )));
        }
        if doc.kdf.algorithm != "argon2id" {
            return Err(VaultError::Corrupt(format!(
                "unknown kdf `{}`",
                doc.kdf.algorithm
            )));
        }

        Ok(Self {
            path: path.to_path_buf(),
            doc: RwLock::new(doc),
            dek: RwLock::new(None),
        })
    }

    pub fn exists(path: &Path) -> bool {
        path.is_file()
    }

    /// Derive the key-encryption key and unwrap the data key. One Argon2 run.
    pub fn unlock(&self, passphrase: &Secret) -> Result<(), VaultError> {
        let doc = self.doc.read().map_err(|_| VaultError::Locked)?;
        let kek = derive(&doc.kdf, passphrase)?;
        let opened = open_sealed(&kek, &doc.wrapped_dek, doc.dek_aad().as_bytes())
            .map_err(|_| VaultError::WrongPassphrase)?;

        if opened.len() != KEY_BYTES {
            return Err(VaultError::Corrupt("data key is the wrong size".into()));
        }
        let mut dek = Zeroizing::new([0u8; KEY_BYTES]);
        dek.copy_from_slice(&opened);

        *self.dek.write().map_err(|_| VaultError::Locked)? = Some(dek);
        Ok(())
    }

    /// Drop the data key. Everything sealed under it becomes unreadable again.
    pub fn lock(&self) {
        if let Ok(mut dek) = self.dek.write() {
            *dek = None;
        }
    }

    pub fn is_unlocked(&self) -> bool {
        self.dek.read().is_ok_and(|d| d.is_some())
    }

    pub fn name(&self) -> String {
        self.doc.read().map(|d| d.name.clone()).unwrap_or_default()
    }

    /// The keys, by their public facts. **Works while locked.**
    pub fn keys(&self) -> Vec<KeyRef> {
        self.doc
            .read()
            .map(|doc| {
                doc.keys
                    .iter()
                    .map(|entry| KeyRef {
                        label: entry.label.clone(),
                        address: entry.address.clone(),
                        origin: entry.origin,
                        created: entry.created,
                        backed_up: entry.backed_up,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Run `f` with the private key for `label`.
    ///
    /// The only way to reach a `PrivateKey` anywhere in this workspace. It is
    /// decrypted here, borrowed for the call, and dropped — so even
    /// `pecu-core`, which *can* name the type, cannot obtain one that
    /// outlives the operation.
    pub fn with_key<R>(
        &self,
        label: &str,
        f: impl FnOnce(&PrivateKey) -> R,
    ) -> Result<R, VaultError> {
        let dek_guard = self.dek.read().map_err(|_| VaultError::Locked)?;
        let dek = dek_guard.as_ref().ok_or(VaultError::Locked)?;

        let doc = self.doc.read().map_err(|_| VaultError::Locked)?;
        let entry = doc
            .keys
            .iter()
            .find(|k| k.label == label)
            .ok_or_else(|| VaultError::NoSuchKey(label.to_string()))?;

        let opened = open_sealed(
            dek,
            &entry.secret,
            entry.aad(&doc.wallet_id, "secret").as_bytes(),
        )
        .map_err(|_| VaultError::Corrupt("this key entry does not decrypt".into()))?;

        if opened.len() != KEY_BYTES {
            return Err(VaultError::Corrupt("private key is the wrong size".into()));
        }
        let mut scalar = Zeroizing::new([0u8; KEY_BYTES]);
        scalar.copy_from_slice(&opened);

        let key = PrivateKey::from_bytes(&scalar, entry.compressed)?;

        // Cheap consistency check. This catches a writer bug rather than an
        // attacker — an attacker editing the address is already caught by the
        // AAD above, which is the stronger guarantee.
        if key.address().to_string() != entry.address {
            return Err(VaultError::Corrupt(
                "the decrypted key does not control the address this entry claims".into(),
            ));
        }

        Ok(f(&key))
    }

    /// Add a key. Needs the vault unlocked, but **not** the passphrase — adding
    /// a second key to an open wallet should not re-prompt.
    pub fn add_key(&self, label: &str, new: NewKey) -> Result<KeyRef, VaultError> {
        check_label(label)?;

        let dek_guard = self.dek.read().map_err(|_| VaultError::Locked)?;
        let dek = dek_guard.as_ref().ok_or(VaultError::Locked)?;
        let mut doc = self.doc.write().map_err(|_| VaultError::Locked)?;

        if doc.keys.iter().any(|k| k.label == label) {
            return Err(VaultError::DuplicateLabel(label.to_string()));
        }

        let (key, phrase, origin) = match new {
            NewKey::Generated { key, phrase } => (key, Some(phrase), Origin::Generated),
            NewKey::FromPhrase { key, phrase } => (key, Some(phrase), Origin::ImportedPhrase),
            NewKey::FromWif { key } => (key, None, Origin::ImportedWif),
        };

        // The entry is built first so its AAD — which covers the label, the
        // address and the origin — is fixed before anything is sealed under it.
        let mut entry = KeyEntry {
            label: label.to_string(),
            address: key.address().to_string(),
            compressed: key.is_compressed(),
            created: now(),
            origin,
            secret: empty_sealed(),
            phrase: None,
            // Only a key this wallet generated has words the user has never
            // seen. An imported phrase they typed in, and a WIF has none —
            // asking either to be "backed up" here would be theatre.
            backed_up: origin != Origin::Generated,
        };

        let scalar = key.to_bytes();
        entry.secret = seal(
            dek,
            scalar.as_slice(),
            entry.aad(&doc.wallet_id, "secret").as_bytes(),
        )?;
        if let Some(phrase) = phrase {
            entry.phrase = Some(seal(
                dek,
                phrase.as_bytes(),
                entry.aad(&doc.wallet_id, "phrase").as_bytes(),
            )?);
        }

        let reference = KeyRef {
            label: entry.label.clone(),
            address: entry.address.clone(),
            origin: entry.origin,
            created: entry.created,
            backed_up: entry.backed_up,
        };
        doc.keys.push(entry);
        drop(doc);
        drop(dek_guard);

        self.persist()?;
        Ok(reference)
    }

    /// Rename a key.
    ///
    /// # Why this is not a field assignment
    ///
    /// The label is inside the associated data of both sealed blobs — see
    /// [`crate::envelope::KeyEntry::aad`] — precisely so that editing the file
    /// to move an entry between names fails to decrypt rather than quietly
    /// producing a key that lies about which one it is. A legitimate rename has
    /// to go the same way: open under the old name, re-seal under the new one.
    ///
    /// So it needs the vault unlocked, but not the passphrase. Renaming reveals
    /// nothing — the plaintext never leaves this function, and re-prompting for
    /// something with no consequence trains people to type their passphrase at
    /// any box that asks.
    ///
    /// # Nothing is half-renamed
    ///
    /// Everything is built on a **copy** of the entry, so a failure at any step
    /// — including the write — leaves both the file and the in-memory document
    /// exactly as they were. A key whose secret is sealed under one name and
    /// whose phrase is sealed under another would be unreadable, which is to
    /// say the funds would be gone.
    pub fn rename_key(&self, from: &str, to: &str) -> Result<(), VaultError> {
        check_label(to)?;
        if from == to {
            return Ok(());
        }

        let previous = {
            let dek_guard = self.dek.read().map_err(|_| VaultError::Locked)?;
            let dek = dek_guard.as_ref().ok_or(VaultError::Locked)?;
            let mut doc = self.doc.write().map_err(|_| VaultError::Locked)?;

            if doc.keys.iter().any(|k| k.label == to) {
                return Err(VaultError::DuplicateLabel(to.to_string()));
            }
            let index = doc
                .keys
                .iter()
                .position(|k| k.label == from)
                .ok_or_else(|| VaultError::NoSuchKey(from.to_string()))?;

            let wallet_id = doc.wallet_id.clone();
            let entry = &doc.keys[index];

            // Opened under the old name.
            let secret = open_sealed(
                dek,
                &entry.secret,
                entry.aad(&wallet_id, "secret").as_bytes(),
            )
            .map_err(|_| VaultError::Corrupt("the key does not decrypt".into()))?;

            let phrase = match &entry.phrase {
                Some(sealed) => Some(
                    open_sealed(dek, sealed, entry.aad(&wallet_id, "phrase").as_bytes())
                        .map_err(|_| VaultError::Corrupt("the phrase does not decrypt".into()))?,
                ),
                None => None,
            };

            // Re-sealed under the new one, on a copy. The AAD has to be
            // computed from the entry as it will be *after* the rename, which
            // is why the label is set before either seal.
            let mut renamed = entry.clone();
            renamed.label = to.to_string();
            renamed.secret = seal(
                dek,
                secret.as_ref(),
                renamed.aad(&wallet_id, "secret").as_bytes(),
            )?;
            renamed.phrase = match phrase {
                Some(opened) => Some(seal(
                    dek,
                    opened.as_ref(),
                    renamed.aad(&wallet_id, "phrase").as_bytes(),
                )?),
                None => None,
            };

            let previous = std::mem::replace(&mut doc.keys[index], renamed);
            (index, previous)
        };

        if let Err(error) = self.persist() {
            // Put it back. The file on disk still says the old name, and the
            // document must not disagree with it.
            if let Ok(mut doc) = self.doc.write() {
                doc.keys[previous.0] = previous.1;
            }
            return Err(error);
        }

        Ok(())
    }

    /// Reveal a recovery phrase.
    ///
    /// Takes the passphrase and runs Argon2 again, even though the vault is
    /// already unlocked. Showing recovery words is the highest-consequence read
    /// in the application and must not ride on a session someone opened twenty
    /// minutes ago and walked away from.
    pub fn reveal_phrase(
        &self,
        label: &str,
        passphrase: &Secret,
    ) -> Result<Zeroizing<String>, VaultError> {
        let doc = self.doc.read().map_err(|_| VaultError::Locked)?;
        let kek = derive(&doc.kdf, passphrase)?;
        let dek_bytes = open_sealed(&kek, &doc.wrapped_dek, doc.dek_aad().as_bytes())
            .map_err(|_| VaultError::WrongPassphrase)?;

        let mut dek = Zeroizing::new([0u8; KEY_BYTES]);
        if dek_bytes.len() != KEY_BYTES {
            return Err(VaultError::Corrupt("data key is the wrong size".into()));
        }
        dek.copy_from_slice(&dek_bytes);

        let entry = doc
            .keys
            .iter()
            .find(|k| k.label == label)
            .ok_or_else(|| VaultError::NoSuchKey(label.to_string()))?;

        let sealed = entry
            .phrase
            .as_ref()
            .ok_or_else(|| VaultError::NoSuchKey(format!("{label} has no recovery phrase")))?;

        let opened = open_sealed(&dek, sealed, entry.aad(&doc.wallet_id, "phrase").as_bytes())
            .map_err(|_| VaultError::Corrupt("the phrase does not decrypt".into()))?;

        String::from_utf8(opened.to_vec())
            .map(Zeroizing::new)
            .map_err(|_| VaultError::Corrupt("the phrase is not text".into()))
    }

    /// Change the passphrase.
    ///
    /// # Why this does not touch a single key
    ///
    /// The two-level wrapping exists for this moment. The data key is unchanged;
    /// only its wrapper is replaced, so a wallet with fifty keys costs the same
    /// as a wallet with one — one Argon2 run to check the old passphrase, one to
    /// derive the new key-encryption key, and 32 bytes re-sealed.
    ///
    /// A **fresh salt** rather than the old one. Reusing it would mean the two
    /// wrappers were derived from the same salt, so anyone holding an old copy
    /// of the file could test a candidate passphrase against both at once. It
    /// also means the KDF cost can be raised at the same time, later, without a
    /// separate migration.
    ///
    /// # If the write fails, nothing changed
    ///
    /// The in-memory document is rolled back to what is still on disk. A vault
    /// whose memory says one passphrase and whose file says another is a vault
    /// that stops opening after the next restart.
    pub fn change_passphrase(&self, old: &Secret, new: &Secret) -> Result<(), VaultError> {
        if new.expose().is_empty() {
            return Err(VaultError::EmptyPassphrase);
        }

        let (previous_kdf, previous_wrapped) = {
            let doc = self.doc.read().map_err(|_| VaultError::Locked)?;
            (doc.kdf.clone(), doc.wrapped_dek.clone())
        };

        // Unwrap with the old passphrase. This is also the check that the old
        // one is right — there is no separate verification step to get wrong.
        let old_kek = derive(&previous_kdf, old)?;
        let dek_bytes = {
            let doc = self.doc.read().map_err(|_| VaultError::Locked)?;
            open_sealed(&old_kek, &doc.wrapped_dek, doc.dek_aad().as_bytes())
                .map_err(|_| VaultError::WrongPassphrase)?
        };
        if dek_bytes.len() != KEY_BYTES {
            return Err(VaultError::Corrupt("data key is the wrong size".into()));
        }
        let mut dek = Zeroizing::new([0u8; KEY_BYTES]);
        dek.copy_from_slice(&dek_bytes);

        let salt = entropy()?;
        let kdf = Kdf {
            algorithm: "argon2id".to_string(),
            salt: hex::encode(&salt[..SALT_BYTES]),
            memory_kib: MEMORY_KIB,
            iterations: ITERATIONS,
            parallelism: PARALLELISM,
        };
        let new_kek = derive(&kdf, new)?;

        {
            let mut doc = self.doc.write().map_err(|_| VaultError::Locked)?;
            // The KDF parameters are inside the AAD, so the new wrapper has to
            // be sealed against the document as it will be *after* the change.
            doc.kdf = kdf;
            let aad = doc.dek_aad();
            doc.wrapped_dek = seal(&new_kek, dek.as_ref(), aad.as_bytes())?;
        }

        if let Err(error) = self.persist() {
            if let Ok(mut doc) = self.doc.write() {
                doc.kdf = previous_kdf;
                doc.wrapped_dek = previous_wrapped;
            }
            return Err(error);
        }

        Ok(())
    }

    /// Record that the phrase for `label` has been written down.
    ///
    /// Needs neither the passphrase nor an unlocked vault: the flag is not
    /// sealed under anything, because it protects nobody. Requiring an unlock
    /// here would only mean the fact could be lost by an auto-lock landing
    /// between the last word and the Done button.
    pub fn mark_backed_up(&self, label: &str) -> Result<(), VaultError> {
        {
            let mut doc = self.doc.write().map_err(|_| VaultError::Locked)?;
            let entry = doc
                .keys
                .iter_mut()
                .find(|k| k.label == label)
                .ok_or_else(|| VaultError::NoSuchKey(label.to_string()))?;

            if entry.backed_up {
                return Ok(());
            }
            entry.backed_up = true;
        }
        self.persist()
    }

    /// Write the vault out atomically.
    ///
    /// tmp → fsync → rename → fsync(dir), keeping the previous good copy as
    /// `.bak`. A half-written vault is unrecoverable and would destroy funds,
    /// so this is not an optimisation.
    fn persist(&self) -> Result<(), VaultError> {
        let doc = self.doc.read().map_err(|_| VaultError::Locked)?;
        let text =
            serde_json::to_string_pretty(&*doc).map_err(|e| VaultError::Corrupt(e.to_string()))?;

        let io = |action: &'static str, source: std::io::Error| VaultError::Io {
            action,
            path: self.path.clone(),
            source,
        };

        if self.path.exists() {
            let backup = self.path.with_extension("json.bak");
            std::fs::copy(&self.path, &backup).map_err(|e| io("back up", e))?;
        }

        let tmp = self.path.with_extension("json.tmp");
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp).map_err(|e| io("create", e))?;
            file.write_all(text.as_bytes())
                .map_err(|e| io("write", e))?;
            file.sync_all().map_err(|e| io("flush", e))?;
        }
        restrict(&tmp);
        std::fs::rename(&tmp, &self.path).map_err(|e| io("replace", e))?;

        if let Some(dir) = self.path.parent() {
            // Fsyncing the directory is what makes the rename itself durable.
            if let Ok(handle) = std::fs::File::open(dir) {
                let _ = handle.sync_all();
            }
        }
        Ok(())
    }
}

/// Owner-only permissions. A no-op off Unix, where the containing profile is
/// the protection instead.
fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Argon2id over the passphrase.
fn derive(kdf: &Kdf, passphrase: &Secret) -> Result<Zeroizing<[u8; KEY_BYTES]>, VaultError> {
    if passphrase.expose().is_empty() {
        return Err(VaultError::EmptyPassphrase);
    }
    let salt = hex::decode(&kdf.salt).map_err(|_| VaultError::Corrupt("salt is not hex".into()))?;

    let params = Params::new(
        kdf.memory_kib,
        kdf.iterations,
        kdf.parallelism,
        Some(KEY_BYTES),
    )
    .map_err(|e| VaultError::Corrupt(format!("unusable kdf parameters: {e}")))?;

    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; KEY_BYTES]);
    argon
        .hash_password_into(passphrase.expose().as_bytes(), &salt, out.as_mut())
        .map_err(|e| VaultError::Corrupt(format!("key derivation failed: {e}")))?;
    Ok(out)
}

fn cipher(key: &[u8; KEY_BYTES]) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(key.into())
}

fn seal(key: &[u8; KEY_BYTES], plaintext: &[u8], aad: &[u8]) -> Result<Sealed, VaultError> {
    let mut nonce = [0u8; NONCE_BYTES];
    getrandom::fill(&mut nonce).map_err(|_| VaultError::NoEntropy)?;

    let ciphertext = cipher(key)
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| VaultError::Corrupt("encryption failed".into()))?;

    Ok(Sealed {
        algorithm: "xchacha20poly1305".to_string(),
        nonce: hex::encode(nonce),
        ciphertext: hex::encode(ciphertext),
    })
}

fn open_sealed(
    key: &[u8; KEY_BYTES],
    sealed: &Sealed,
    aad: &[u8],
) -> Result<Zeroizing<Vec<u8>>, VaultError> {
    if sealed.algorithm != "xchacha20poly1305" {
        return Err(VaultError::Corrupt(format!(
            "unknown cipher `{}`",
            sealed.algorithm
        )));
    }
    let nonce =
        hex::decode(&sealed.nonce).map_err(|_| VaultError::Corrupt("nonce is not hex".into()))?;
    let ciphertext = hex::decode(&sealed.ciphertext)
        .map_err(|_| VaultError::Corrupt("ciphertext is not hex".into()))?;
    let nonce: [u8; NONCE_BYTES] = nonce
        .try_into()
        .map_err(|_| VaultError::Corrupt("nonce is the wrong size".into()))?;

    cipher(key)
        .decrypt(
            &XNonce::from(nonce),
            Payload {
                msg: &ciphertext,
                aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| VaultError::WrongPassphrase)
}

fn empty_sealed() -> Sealed {
    Sealed {
        algorithm: String::new(),
        nonce: String::new(),
        ciphertext: String::new(),
    }
}

/// Labels become file-adjacent identifiers and appear in AAD, so they are kept
/// boring: lowercase, no traversal, no surprises.
fn check_label(label: &str) -> Result<(), VaultError> {
    let ok = !label.is_empty()
        && label.len() <= 64
        && label
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && label
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());

    if ok {
        Ok(())
    } else {
        Err(VaultError::BadLabel(label.to_string()))
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}
