//! What the UI asks the core to do.
//!
//! # The whole outbound surface is this one enum
//!
//! Keeping it in a single type — and mirroring it as a single `Actions` global
//! on the Slint side — means the entire UI→core API is readable in one sitting.
//! A new variant carrying a [`Secret`] is then impossible to add unnoticed,
//! which is the property worth having.
//!
//! # The variants that carry secret material
//!
//! Five, and this list is the record — if it disagrees with the enum, the enum
//! is right and someone added a variant without arguing for it:
//!
//! * [`Command::CreateWallet`] — the new passphrase
//! * [`Command::Unlock`] — the passphrase
//! * [`Command::ChangePassphrase`] — both of them
//! * [`Command::ImportKey`] — a phrase or a WIF, plus the passphrase
//! * [`Command::RevealBackup`] — the passphrase, re-asked on purpose
//!
//! [`Command::ConfirmPhrase`] deliberately carries plain `String`s rather than
//! [`Secret`]s: three of twenty-four words, which the user is reading off the
//! screen at that moment, leave twenty-one unknown words from a 2048-word list.
//! That is not key material in any useful sense, and wrapping it would blur what
//! [`Secret`] means everywhere else.

use crate::models::{ScreenId, SendDraft};
use crate::secret::Secret;

/// How a key is being brought into the wallet.
///
/// # Why a phrase and free text are separate variants
///
/// Verus hashes a transparent seed phrase **verbatim**, so free text really is
/// a legitimate key — refusing it would strand funds that another Verus wallet
/// can reach. But accepting everything silently means a mistyped word produces
/// a valid, empty wallet with nothing to say why, and BIP-39 spends its last
/// bits on a checksum built to catch exactly that.
///
/// So the distinction is the user's, made before the import: [`Self::Phrase`]
/// is checked and refused when it fails, [`Self::Text`] is taken as typed. One
/// variant that quietly did both would give up the checksum for everyone in
/// order to serve the rare case.
#[derive(Debug)]
pub enum ImportMaterial {
    /// A BIP-39 recovery phrase. Core validates it and refuses a failure.
    Phrase(Secret),
    /// Free text, hashed exactly as typed. Nothing can check it, which is why
    /// choosing it is explicit.
    Text(Secret),
    /// A WIF private key.
    Wif(Secret),
}

/// Where a refresh should reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshScope {
    /// Every key. What the Refresh button does.
    All,
    /// One key, after it was added or selected.
    Key(String),
    /// Only the chain tip. What the poller does, and it is cheap.
    TipOnly,
}

/// What to do about a transaction whose broadcast we could not confirm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingAction {
    /// Ask the node whether it landed after all.
    CheckNow,
    /// Send the **same stored bytes** again. Never a rebuild — see
    /// `chainvue_core::send`.
    ResendSameBytes,
    /// Stop tracking it. The user has decided it is gone.
    Abandon,
}

#[derive(Debug)]
pub enum Command {
    // ── Wallet ──────────────────────────────────────────────────────────
    /// Generate a wallet. Entropy comes from the OS inside core; the UI never
    /// supplies or sees it.
    CreateWallet {
        name: String,
        passphrase: Secret,
    },
    /// Show the phrase core is currently holding for the backup screen.
    ///
    /// Carries no passphrase, and works only while a backup is in progress —
    /// that is, between [`Command::CreateWallet`] (or a successful
    /// [`Command::RevealBackup`]) and whichever of [`Command::ConfirmPhrase`]
    /// or [`Command::CancelBackup`] ends it. Reaching an existing key's phrase
    /// from a standing start is `RevealBackup`, which re-runs the key
    /// derivation.
    ///
    /// Sent on every press of hold-to-reveal, so it must stay cheap: core
    /// already has the words in memory and only formats them.
    ShowNewPhrase,
    /// Check the words the user re-typed.
    ///
    /// Core answers with a single bool and deliberately never says *which* word
    /// was wrong — otherwise the confirmation screen becomes a brute-force
    /// oracle. A correct answer **is** the finish: core records the backup on
    /// the key and drops the phrase there and then, rather than waiting for a
    /// second command the UI could fail to send.
    ConfirmPhrase {
        checks: Vec<(u32, String)>,
    },
    /// Bring in a key that already exists somewhere else.
    ///
    /// **Creates the wallet when there is not one yet**, using `passphrase` —
    /// which is what restoring on a fresh install is. When a wallet is already
    /// open, `passphrase` is ignored and the open session is used, because
    /// adding a second key to a wallet you just unlocked should not re-prompt.
    ///
    /// Nothing is written until the material has been checked and the key
    /// derived, so a rejected phrase leaves no empty wallet on disk.
    ImportKey {
        label: String,
        material: ImportMaterial,
        passphrase: Secret,
    },
    Unlock {
        passphrase: Secret,
    },
    Lock,
    ChangePassphrase {
        old: Secret,
        new: Secret,
    },
    /// Show the recovery phrase or WIF. Re-prompts for the passphrase on
    /// purpose: this is the highest-consequence read in the application and
    /// must not ride on a session unlocked twenty minutes ago.
    RevealBackup {
        label: String,
        passphrase: Secret,
    },
    /// Conceal the words without ending the backup.
    ///
    /// Sent on every release of hold-to-reveal, so the phrase stays in core and
    /// can be shown again. What leaves is the copy the UI was given.
    HideBackup,
    /// End the backup and drop the phrase from memory.
    ///
    /// The key keeps its "not backed up" flag, so the offer comes back — this
    /// abandons the attempt rather than declining it permanently.
    CancelBackup,
    /// Generate another key in a wallet that already exists.
    ///
    /// Carries no passphrase: the wallet is open, and adding a key needs the
    /// data key rather than the one derived from a passphrase. Asking for it
    /// again where nothing is revealed would train people to type it at any box
    /// that asks for it.
    ///
    /// The new key's phrase has never been seen by anyone, so this starts the
    /// same backup conversation that creating a wallet does.
    AddKey {
        label: String,
    },
    /// Rename a key.
    ///
    /// Not a cosmetic change in the vault: the label is inside the associated
    /// data of the sealed key and the sealed phrase, so this re-seals both. It
    /// is here rather than in the UI for the usual reason — the label rules are
    /// the vault's, and a name the vault would refuse must not reach a screen
    /// that has already told someone it worked.
    RenameKey {
        from: String,
        to: String,
    },
    /// Name an address, or rename it. An empty label clears the name without
    /// forgetting that the address was paid.
    LabelAddress {
        address: String,
        label: String,
    },
    /// Forget an address — which also forgets that it was paid, so the review
    /// will warn about it again. That is the honest consequence of the
    /// request: somebody removing an address is saying they no longer
    /// recognise it.
    ForgetAddress(String),
    SetActiveKey(String),
    SetAutoLockMinutes(Option<u32>),
    /// How the window should look, and whether it should move.
    ///
    /// Neither is wallet state and neither reaches a node — but both are
    /// choices a person made, and a preference that resets every launch is not
    /// a preference. So they go to the core to be written down, and come back
    /// as [`crate::Event::Appearance`] at the next start.
    SetAppearance {
        dark: bool,
        /// Motion off: no movement, no scale, no stagger. Cross-fades under
        /// 100 ms stay — opacity is not a vestibular trigger, and a hard swap
        /// is jarring for motion-sensitive people too.
        reduce_motion: bool,
    },

    // ── Network ─────────────────────────────────────────────────────────
    SelectNode(u32),
    AddNode {
        url: String,
        label: String,
    },
    RemoveNode(u32),
    ProbeNodes,
    SetRequestedNetwork(String),
    /// Turning on mainnet spending. `typed_confirmation` must be the literal
    /// word `mainnet`, and core checks it — putting that check in the UI would
    /// make it a decoration.
    SetAllowMainnetSpend {
        on: bool,
        typed_confirmation: String,
    },

    // ── Portfolio ───────────────────────────────────────────────────────
    Refresh(RefreshScope),
    LoadHistory {
        key: String,
        before_height: Option<u32>,
    },
    LoadTxDetail(String),

    // ── Send ────────────────────────────────────────────────────────────
    ValidateDraft(SendDraft),
    /// Build and sign, without broadcasting. Core keeps the signed bytes; the
    /// UI receives only a decoded summary.
    PrepareSend(SendDraft),
    ConfirmSend {
        ticket: u64,
    },
    CancelSend {
        ticket: u64,
    },
    ResolvePending {
        id: u64,
        action: PendingAction,
    },

    // ── VerusIDs ────────────────────────────────────────────────────────
    /// Find the identities this wallet's keys control. One request per key, so
    /// it is asked for rather than run on a timer.
    RefreshIdentities,
    /// Look one up by `name@` or i-address — anyone's, not only your own.
    LookUpIdentity(String),
    /// Point an identity's authorities somewhere else.
    ///
    /// The one that fixes the unrevokable default. Empty leaves that authority
    /// alone; both empty is nothing to do.
    SetIdentityAuthorities {
        address: String,
        revocation: String,
        recovery: String,
    },
    /// Hold an identity's funds, with this many blocks of wait once somebody
    /// asks to unlock.
    LockIdentity {
        address: String,
        delay_blocks: u32,
    },
    /// Start the countdown. Does **not** unlock — the funds stay held until the
    /// chain reaches the height this publishes.
    UnlockIdentity {
        address: String,
        extra_blocks: u32,
    },
    /// Revoke an identity. Signed by its revocation authority.
    RevokeIdentity {
        address: String,
    },
    /// Bring a revoked one back. Signed by its recovery authority.
    RecoverIdentity {
        address: String,
    },
    /// Send a prepared identity change.
    ///
    /// `typed` carries the confirmation word for the changes that need one —
    /// checked in the core, never in the interface, so a different interface
    /// cannot skip it.
    ConfirmIdentityChange {
        ticket: u64,
        typed: String,
    },
    /// Throw one away unsent.
    CancelIdentityChange {
        ticket: u64,
    },
    /// Check whether a name can be registered, and what it would cost.
    ///
    /// Offline where it can be — the SDK's own name rule is a local check — and
    /// then against the chain for availability and the fee.
    CheckName(String),
    /// Build and sign step one, write the salt down, and broadcast it.
    ///
    /// One command rather than three, because the three are one decision: the
    /// salt is written between the build and the broadcast, and a caller who
    /// could interleave anything there could lose it.
    StartRegistration {
        name: String,
        /// Who may revoke it. Empty leaves the identity as its own, which makes
        /// it unrevokable until somebody changes it.
        revocation_authority: String,
        /// Who may recover it. Empty means the same, and this is the one that
        /// decides whether revocation works at all.
        recovery_authority: String,
    },
    /// Run step two, now that the commitment has confirmed.
    FinishRegistration,
    /// Give up on the reservation in progress.
    ///
    /// Before the commitment is broadcast this costs nothing. After it, the
    /// commitment fee is already spent and abandoning only stops the wallet
    /// asking about it.
    AbandonRegistration,
    /// Forget the looked-up identities. Yours are untouched.
    ClearLookups,
    /// Stop watching one of them, by i-address.
    UnwatchIdentity(String),
    /// Open the detail sheet on an identity, by i-address.
    OpenIdentity(String),
    /// Derive a VDXF key from a URI, so somebody can test whether an identity
    /// published under a name they know. A search, not a lookup: the hash has
    /// no inverse, and this is the only direction that exists.
    DeriveContentKey(String),

    // ── Shell ───────────────────────────────────────────────────────────
    /// Entering a screen. Core uses this to start screen-scoped polling.
    ScreenEntered(ScreenId),
    /// Leaving one cancels every request that screen started.
    ScreenLeft(ScreenId),
    /// Any user input at all, for the auto-lock idle timer.
    UserActivity,
    Shutdown,
}
