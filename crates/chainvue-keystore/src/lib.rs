//! Private keys on disk, encrypted.
//!
//! # What is protected, and from what
//!
//! Argon2id turns the passphrase into a key-encryption key; XChaCha20-Poly1305
//! seals a 32-byte data key under it; each private key is sealed under *that*.
//! This defends a stolen file against an offline guess. It does not defend a
//! running process — once unlocked, the data key is in memory.
//!
//! # Why two levels and not one
//!
//! A wallet here is N named keys, and the user's model is one passphrase for
//! the wallet. With a key derived per file, unlocking five keys would be five
//! Argon2 runs and changing the passphrase would re-encrypt every one. Wrapping
//! a single data key makes unlock O(1) and a passphrase change a matter of
//! re-sealing 32 bytes.
//!
//! # This is not custom cryptography
//!
//! Composing `argon2` and `chacha20poly1305` — both RustCrypto, both reviewed —
//! with published parameters is *using* cryptography. What would count as
//! rolling our own, and what is not done here: inventing a KDF, deriving a
//! nonce from anything but the OS CSPRNG, implementing an AEAD, or touching
//! ECDSA. Every key operation stays in `verus-keys`.
//!
//! The SDK deliberately offers no `PrivateKey::generate` and no at-rest
//! encryption, on the grounds that where the bytes come from is the most
//! security-critical decision a wallet makes and a library that picks quietly
//! moves it somewhere nobody reviews. This module is that decision, in the
//! open.
//!
//! # The key never escapes
//!
//! [`Vault::with_key`] hands a `&PrivateKey` to a closure and wipes it
//! afterwards. There is no accessor that returns one, so **a signing key exists
//! for the duration of one signature**, not for the length of the unlocked
//! session.

mod envelope;
mod vault;

pub use envelope::{Kdf, KeyEntry, Origin, Sealed, VaultDoc, VAULT_VERSION};
pub use vault::{KeyRef, NewKey, Vault, VaultError};

use zeroize::Zeroizing;

/// 32 bytes from the OS CSPRNG.
///
/// The one place entropy enters the wallet. `verus-keys` refuses to do this on
/// purpose; doing it here, visibly, is the point.
///
/// # Errors
///
/// If the OS will not supply randomness. Do not work around that — a key from a
/// weak source protects nothing, and it is not recoverable from later.
pub fn entropy() -> Result<Zeroizing<[u8; 32]>, VaultError> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    getrandom::fill(bytes.as_mut()).map_err(|_| VaultError::NoEntropy)?;
    Ok(bytes)
}
