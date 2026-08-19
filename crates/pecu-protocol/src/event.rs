//! What the core tells the UI.
//!
//! Every variant is a finished view model. Nothing here is a handle, a lock, a
//! key, or anything the UI would have to interpret — the interpreting already
//! happened, on the core side, where the SDK types are.

use crate::error::UiError;
use crate::models::DraftValidationVm;
use crate::models::{
    ChartVm, ConvertQuoteVm, CurrencyChoicesVm, CurrencyDraftVm, CurrencyVm, EligibleIdentityVm,
    HistoryRowVm,
    IdentityDetailVm, IdentityVm, KnownAddressVm, LaunchDoneVm, LaunchPendingVm, LaunchReviewVm,
    ListDelta, LockReason, MarketDetailVm, MarketRowVm, NetworkVm, NoteVm, PendingVm, PortfolioVm,
    RegistrationVm, SeedWordVm,
    SearchHitVm, SendOutcomeVm, SendReviewVm, TaskKind, TxDetailVm, WalletVm,
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
        /// The size the window was last left at, in logical pixels, or `None`
        /// for a wallet that has never been resized. Applied before the window
        /// is shown, so there is no frame at the default size first.
        window: Option<(u32, u32)>,
    },
    /// Who this wallet has paid, and what they have been named.
    AddressBook(Vec<KnownAddressVm>),
    Network(NetworkVm),
    Portfolio(PortfolioVm),
    /// The balance over time, as readings rather than as a picture.
    ///
    /// The geometry is computed in `pecu-ui`, which knows how big the
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
        /// Why, as a named reason. The one string it can carry is a node's own
        /// error text, which is not the wallet's to translate.
        reason: NoteVm,
    },
    /// A change to an identity, built and signed but not sent.
    IdentityChangePrepared {
        ticket: u64,
        /// What it will do, as a named reason. `note.slint` has the sentence —
        /// this one is read before signing something that cannot be undone, so
        /// it is the last place prose should be stuck in English.
        description: NoteVm,
        fee_display: String,
        /// The word that has to be typed before this can be sent, or empty when
        /// none is needed. Only a revocation asks for one.
        ///
        /// The word itself rather than a flag, because the interface has to
        /// print it, gate a button on it and compare against it — and with a
        /// flag all three of those were a literal `"revoke"` written out in
        /// the interface, three copies of a rule that lives in the core.
        confirmation: String,
    },
    /// It was accepted by the network.
    IdentityChanged {
        txid: String,
    },
    /// Whether a name can be claimed, and what it would cost.
    NameChecked {
        name: String,
        /// `NoteVm::none()` when the name is fine. Otherwise why it is not.
        problem: NoteVm,
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

    /// The currencies this wallet's identities define, and the identities that
    /// could still define one.
    ///
    /// One event carrying both, because they are two readings of the same walk:
    /// every identity either has a currency or is eligible for one. Sending
    /// them separately would let the two halves describe wallets a second
    /// apart, and the picker would offer a name the list above it had just
    /// shown as taken.
    Currencies {
        yours: Vec<CurrencyVm>,
        eligible: Vec<EligibleIdentityVm>,
    },
    /// What a conversion would yield, and what it would cost.
    ///
    /// Every field is answered on every edit, including the ones that are
    /// empty — a quote that left the last conversion's fee on screen beside a
    /// new pair of currencies would be describing something nobody asked for.
    ConvertQuote(Box<ConvertQuoteVm>),
    /// Every currency any pool can price, and what it is worth.
    ///
    /// Replaces the table wholesale. A delta would be smaller and would also
    /// have to describe a currency whose price became unknown, which is a state
    /// no partial update expresses without inventing one.
    Markets {
        rows: Vec<MarketRowVm>,
        /// What every price in `rows` is denominated in, by name.
        ///
        /// Carried rather than assumed. The interface has no way to know, the
        /// currency is resolved from the chain's own list at read time, and a
        /// column of bare numbers with no unit on it is a price nobody can
        /// read — `0.5372` of what?
        quote: String,
    },
    /// One currency in full, or `None` when nothing is selected.
    MarketDetail(Option<Box<MarketDetailVm>>),
    /// What the palette should show. Carries the query it answers, so a reply
    /// that arrives after the person has typed further can be discarded rather
    /// than flickering an older list back onto the screen.
    SearchHits {
        query: String,
        hits: Vec<SearchHitVm>,
    },
    /// What core makes of the draft being configured, including the numbers the
    /// bars and the preview are drawn from.
    CurrencyDraftChecked(Box<CurrencyDraftVm>),
    /// What the chain's currency list holds that matches the picker's search.
    CurrencyChoices(Box<CurrencyChoicesVm>),
    /// A currency waiting for its identity, or waiting to be defined under one
    /// that already landed. `None` when there is none.
    ///
    /// Carries the identity's name and which of the two it is waiting on, which
    /// is the difference between "this may still be abandoned for free" and
    /// "an identity has been paid for and nothing has been made with it".
    LaunchPending(Option<Box<LaunchPendingVm>>),
    /// A launch built and signed but not sent. `None` closes the review.
    LaunchPrepared(Option<Box<LaunchReviewVm>>),
    /// It reached the network.
    LaunchDone(Box<LaunchDoneVm>),

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
