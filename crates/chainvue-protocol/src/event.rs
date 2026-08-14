//! What the core tells the UI.
//!
//! Every variant is a finished view model. Nothing here is a handle, a lock, a
//! key, or anything the UI would have to interpret — the interpreting already
//! happened, on the core side, where the SDK types are.

use crate::error::UiError;
use crate::models::DraftValidationVm;
use crate::models::{
    ChartVm, HistoryRowVm, IdentityDetailVm, IdentityVm, KnownAddressVm, ListDelta, LockReason,
    NetworkVm, PendingVm, PortfolioVm, RegistrationVm, SeedWordVm, SendOutcomeVm, SendReviewVm,
    TaskKind, TxDetailVm, WalletVm,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Wallet(WalletVm),
    /// What the window looked like last time. Emitted once at startup, before
    /// anything else — a theme that arrives after the first frame is a flash of
    /// the wrong one.
    Appearance {
        dark: bool,
        reduce_motion: bool,
    },
    /// Who this wallet has paid, and what they have been named.
    AddressBook(Vec<KnownAddressVm>),
    Network(NetworkVm),
    Portfolio(PortfolioVm),
    /// The balance over time, as readings rather than as a picture.
    ///
    /// The geometry is computed in `chainvue-ui`, which knows how big the
    /// element is — a chart plotted here would need the interface to report its
    /// size back through the actor on every resize, and would still be one
    /// frame behind.
    Chart(ChartVm),
    History {
        key: String,
        delta: ListDelta<HistoryRowVm>,
    },
    TxDetail(TxDetailVm),
    /// Whether the scan has reached the start of the chain.
    ///
    /// Separate from the list itself, because "there is nothing older" and
    /// "this is what we have so far" are different claims and only one of them
    /// justifies taking the button away.
    HistoryExhausted(bool),
    Pending(ListDelta<PendingVm>),

    /// The identities this wallet's keys control, and the ones somebody looked
    /// up.
    ///
    /// **Two lists, deliberately.** They answer different questions — one is a
    /// fact about your keys, the other is what you just asked about — and a
    /// single list forces the heading over it to lie about one of them. It did:
    /// a stranger's identity sat under "found by asking the chain which names
    /// your keys control" until the wallet was restarted.
    ///
    /// An identity that is looked up and turns out to be yours appears in
    /// `yours` only.
    Identities {
        yours: Vec<IdentityVm>,
        looked_up: Vec<IdentityVm>,
    },
    /// A lookup that found nothing, or could not be answered. Carries what was
    /// typed, so a reply arriving after the field moved on is discardable.
    IdentityMissing {
        typed: String,
        reason: String,
    },
    /// A change to an identity, built and signed but not sent.
    IdentityChangePrepared {
        ticket: u64,
        /// What it will do, in a sentence.
        description: String,
        fee_display: String,
        /// Whether sending it needs a word typed first. Only a revocation does.
        needs_confirmation: bool,
    },
    /// It was accepted by the network.
    IdentityChanged {
        txid: String,
    },
    /// Whether a name can be claimed, and what it would cost.
    NameChecked {
        name: String,
        /// Empty when the name is fine. Otherwise why it is not.
        problem: String,
        /// The registration fee, formatted. Empty when it is not known yet.
        fee_display: String,
    },
    /// How a registration in progress is doing. `None` when there is none.
    Registration(Option<Box<RegistrationVm>>),
    /// The detail sheet's contents. `None` closes it.
    IdentityDetail(Option<Box<IdentityDetailVm>>),
    /// A VDXF URI resolved to a key, and whether the open identity has it.
    ContentKeyDerived {
        uri: String,
        key: String,
        present: bool,
    },

    SendValidation(DraftValidationVm),
    SendPrepared(SendReviewVm),
    SendResult(SendOutcomeVm),

    /// A backup is under way: which words will be asked for, and how many
    /// there are in total.
    ///
    /// The positions are chosen in core rather than in the UI. A screen that
    /// picked its own would be free to pick the same three every time, or the
    /// three it happens to have on hand — and the confirmation step is only
    /// worth anything if the wallet chose.
    ///
    /// `word_count` arrives here, before any word does, so the grid can be laid
    /// out and masked without ever having been given the phrase.
    PhraseChallenge {
        /// 1-based, as the words are numbered on screen.
        positions: Vec<u32>,
        word_count: u32,
    },

    /// The recovery phrase, word by word, for the one screen that shows it.
    ///
    /// An empty vector means "blank it now" — the hide path overwrites each
    /// word before clearing the model, so a stale `SharedString` cannot linger
    /// in a row that is merely no longer visible.
    SeedWords(Vec<SeedWordVm>),
    /// The answer to `ConfirmPhrase`: right or wrong, and nothing more.
    PhraseConfirmed(bool),

    /// Long-running work started or finished, so the UI can show a spinner
    /// without inventing its own idea of what is in flight.
    Busy {
        task: TaskKind,
        on: bool,
    },
    Notice(UiError),
    Locked {
        reason: LockReason,
    },
}
