//! What the core tells the UI.
//!
//! Every variant is a finished view model. Nothing here is a handle, a lock, a
//! key, or anything the UI would have to interpret — the interpreting already
//! happened, on the core side, where the SDK types are.

use crate::error::UiError;
use crate::models::DraftValidationVm;
use crate::models::{
    HistoryRowVm, ListDelta, LockReason, NetworkVm, PendingVm, PortfolioVm, SeedWordVm,
    SendOutcomeVm, SendReviewVm, TaskKind, TxDetailVm, WalletVm,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Wallet(WalletVm),
    Network(NetworkVm),
    Portfolio(PortfolioVm),
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
