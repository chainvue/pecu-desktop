//! The view models the core hands the UI.
//!
//! # Money is a string here, and that is not laziness
//!
//! Slint's `int` is an `i32` and its `float` is an `f32`. A Verus balance is
//! measured in satoshis at 1e8 per coin, so 21.5 coins already overflows an
//! `i32`, and an `f32` cannot represent a satoshi count exactly past ~16.7
//! million satoshis. Either type silently produces a wrong number on a screen
//! whose entire job is to be right about money.
//!
//! So every amount crosses this boundary as a **decimal string of satoshis**
//! (`"1248442100000"`), with a separate pre-formatted display string alongside
//! it. Parsing back to `i64` happens in Rust, on the core side, where the type
//! is honest.

use serde::{Deserialize, Serialize};

use crate::error::UiError;

/// How a list changed, so the UI can patch a model instead of rebuilding it.
///
/// Rebuilding a `VecModel` tears down and recreates every element, which — apart
/// from the allocation — silently cancels any in-flight `states` transition on a
/// row. The pending→confirmed animation depends on the row element surviving,
/// which is why [`ListDelta::UpdateAt`] exists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ListDelta<T> {
    /// Everything changed, or this is the first load.
    Replace(Vec<T>),
    /// New rows at the end (older history, paged in).
    Append(Vec<T>),
    /// New rows at the front (a transaction just arrived).
    Prepend(Vec<T>),
    /// Exactly one row changed. The common case, and the cheap one.
    UpdateAt {
        index: usize,
        row: T,
    },
    RemoveAt(usize),
}

// ── Wallet ──────────────────────────────────────────────────────────────────

/// One key in the vault, as the UI may see it.
///
/// Note what is absent: anything secret. The address is public, and having it
/// while locked is what lets the key list, and a receive address, work without a
/// passphrase.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyVm {
    pub label: String,
    pub address: String,
    /// Where it came from — decides whether a recovery phrase can be shown.
    pub origin: KeyOrigin,
    /// Whether this address has ever appeared in a transaction. Drives the
    /// "unused" hint on the Receive screen.
    pub used: bool,
    /// Whether the user has been shown the recovery phrase and proved they
    /// wrote it down. Always true for a WIF import, which never had a phrase to
    /// write down.
    pub backed_up: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyOrigin {
    /// Generated here, from OS entropy. Has a recovery phrase.
    #[default]
    Generated,
    /// Imported as a recovery phrase. Has one.
    ImportedPhrase,
    /// Imported as a WIF. Has no phrase, and the backup screen must say so
    /// rather than offering a button that cannot work.
    ImportedWif,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletVm {
    pub name: String,
    pub exists: bool,
    pub locked: bool,
    pub keys: Vec<KeyVm>,
    pub active_key: Option<String>,
    /// Minutes of inactivity before locking; `None` is "never".
    pub auto_lock_minutes: Option<u32>,
    /// The label of a key that has a recovery phrase nobody has written down.
    ///
    /// This survives a restart, because the flag lives in the vault file. A
    /// wallet created and then closed before the backup was finished would
    /// otherwise have no route back to its own phrase — the words are still in
    /// there, sealed, with nothing on screen offering to show them.
    pub needs_backup: Option<String>,
}

/// One word of a recovery phrase, on its way to a screen that shows it once.
///
/// Deliberately **not** a joined string. A joined phrase is the single object
/// that makes an accidental clipboard write or one stray log line catastrophic;
/// per-word means no such object exists anywhere in the UI layer.
///
/// `Debug` is written by hand and prints nothing. [`Event`](crate::Event)
/// derives `Debug`, and both the core's and the bridge's catch-all arms log an
/// unhandled variant with `?event` — so a derived `Debug` here would mean that
/// adding one variant, somewhere else, could put a recovery phrase in a log
/// file. Also no `Serialize`: this must not be able to reach a config file or
/// an IPC boundary by being part of some larger struct.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SeedWordVm {
    /// 1-based, as it is shown.
    pub index: u32,
    pub word: String,
}

impl std::fmt::Debug for SeedWordVm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The position is not the secret and is useful when a test fails.
        write!(
            f,
            "SeedWordVm {{ index: {}, word: <redacted> }}",
            self.index
        )
    }
}

// ── Network ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reachability {
    #[default]
    Unknown,
    Probing,
    Online,
    /// Answers, but not usably — syncing, wrong chain, or refusing a method.
    Degraded,
    Offline,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeVm {
    pub id: u32,
    pub url: String,
    pub label: String,
    pub status: Reachability,
    /// Present only once the node has said which chain it is on. Derived from
    /// `chain_info().name`, never guessed from the URL.
    pub network: Option<String>,
    pub tip: Option<u32>,
    pub latency_ms: Option<u32>,
    /// Why it is degraded, in words, when it is.
    pub note: NoteVm,
    /// Built-in nodes cannot be deleted.
    pub builtin: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkVm {
    /// What the user asked for.
    pub requested: String,
    /// What the active node reports. Only this decides anything.
    pub effective: Option<String>,
    pub nodes: Vec<NodeVm>,
    pub active_node: Option<u32>,
    pub tip: Option<u32>,
    pub syncing: bool,
    pub allow_mainnet_spend: bool,
    /// True when the app is running against the mock chain, so the UI can say
    /// so loudly and permanently.
    pub mock_mode: bool,
}

// ── Portfolio ───────────────────────────────────────────────────────────────

/// A balance is several numbers, and a wallet that shows one cannot explain
/// "you have 500 but can spend 20".
///
/// These map directly onto what `verus_flows::spendable` reports: `Funding`
/// separates what is spendable now from what is merely owned, and the SDK does
/// that work because the distinction is real — an immature coinbase builds and
/// signs perfectly and is then rejected by the daemon.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceVm {
    /// Satoshis, as a decimal string. See the module docs.
    pub total_sats: String,
    pub spendable_sats: String,
    /// Held in outputs that are not spendable *yet* — usually coinbase under
    /// 100 confirmations.
    pub immature_sats: String,
    /// Already spent by something in the mempool. Not "waiting", gone.
    pub pending_out_sats: String,
    /// Arriving, seen in the mempool, not yet mined.
    pub pending_in_sats: String,
    /// Blocks until the nearest immature output matures, when known.
    pub matures_in_blocks: Option<u32>,
    /// Pre-formatted for display, so the UI never formats money itself.
    pub total_display: String,
    pub spendable_display: String,
    pub immature_display: String,
    /// Money that has left and not settled.
    pub pending_display: String,
    /// Money arriving: seen in the mempool, not yet mined.
    pub incoming_display: String,
}

/// One currency the wallet holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetVm {
    /// The currency's i-address.
    pub currency_id: String,
    /// Friendly name where the node could supply one, else the i-address.
    pub name: String,
    pub amount_sats: String,
    pub amount_display: String,
    /// True for the chain's own currency, which sorts first and is never hidden.
    pub native: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortfolioVm {
    pub balance: BalanceVm,
    pub assets: Vec<AssetVm>,
    /// Set when the figures are from cache and a refresh is in flight or failed.
    pub stale: bool,
    /// Token balances failed to load. Distinct from "no tokens": an error here
    /// means unknown, never zero.
    pub tokens_unknown: bool,
}

/// A registration in progress.
///
/// Two transactions with a deadline between them, and the deadline is what this
/// exists to make visible: the commitment expires about twenty blocks after it
/// was signed, and missing that window spends the fee for nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrationVm {
    pub name: String,
    /// "reserved" | "committed" | "waiting" | "ready" | "registering" | "done"
    /// | "expired" | "lost"
    pub step: String,
    /// What is happening, in a sentence somebody can act on.
    pub note: NoteVm,
    /// The block the commitment must be registered by, and how long that is.
    /// Empty when there is no deadline to state yet.
    pub deadline: NoteVm,
    /// The registration fee, formatted.
    pub fee_display: String,
    /// The identity's address, once it exists.
    pub address: String,
    /// Whether the wallet is waiting on a node right now.
    pub busy: bool,
    /// Set when this identity was registered as its own recovery authority, so
    /// the screen can offer the fix while somebody is still looking at it.
    pub cannot_be_revoked: bool,
    /// The three transactions a claim is made of, and how far it has got.
    ///
    /// Empty when the claim ended without finishing — see
    /// [`crate::FlowStepVm::state`]; the note says what happened, and a diagram
    /// of a journey nobody is on any more is worse than none.
    pub steps: Vec<FlowStepVm>,
}

/// One VerusID, as a row in the list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityVm {
    /// `name.parent@` where the parent is known, otherwise the bare name.
    pub name: String,
    /// The i-address. The identifier to prefer for anything destructive, and
    /// the one thing about an identity that never changes.
    pub address: String,
    /// "Active" · "Locked" · "Unlocking" · "Revoked".
    pub status: String,
    /// The pill colour, in the vocabulary the node list already uses.
    pub tone: String,
    /// What the status means, in a sentence. Empty for the ordinary case,
    /// because a row that explains "Active" is a row nobody reads.
    pub note: NoteVm,
    /// Whether this wallet holds enough keys to sign for it. Decides which
    /// actions are offered, and is a fact about this wallet rather than about
    /// the identity.
    pub mine: bool,
}

/// One currency, as a row in the list.
///
/// # A currency is an identity wearing a second hat
///
/// The currency's i-address **is** the identity's — defining a currency flips a
/// flag on the identity of the same name rather than creating a separate thing.
/// That is why this carries the identity's address and why the list is derived
/// from the identity list rather than fetched on its own.
///
/// It is also why the wallet can say "this identity has no currency yet" with
/// certainty: the flag is on the identity object it already reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencyVm {
    /// The name, as the chain spells it. **No trailing `@`** — that suffix is
    /// the identity convention and no currency on VRSCTEST carries one.
    pub name: String,
    /// The i-address, shared with the identity that defines it.
    pub address: String,
    /// "Token" · "Basket" · "NFT", read off the options bitfield rather than
    /// guessed from which fields happen to be set.
    pub kind: String,
    /// The pill colour, in the vocabulary the node list already uses.
    pub tone: String,
    /// What this currency is, in a sentence somebody who did not define it can
    /// read. Empty when the kind alone says it.
    pub note: NoteVm,
    /// Whether it has reached its start block.
    ///
    /// False for the window between the definition being mined and the currency
    /// beginning, which is where a freshly launched one sits for twenty-odd
    /// minutes doing nothing at all. Without this the list shows it exactly as
    /// it shows one that has been running for a month.
    pub started: bool,
    /// Whether the supply can still grow. A fixed-supply currency and a
    /// mintable one are different promises to whoever holds it, and the
    /// difference is permanent.
    pub mintable: bool,
    /// The block it starts at, formatted. A currency defined ahead of the tip
    /// exists before it does anything.
    pub start_block: String,
}

/// An identity that could define a currency but has not.
///
/// The picker's rows. Separate from [`IdentityVm`] because the question is not
/// "what is this identity" but "may it still be used for this" — and the answer
/// is a fact about a flag that cannot be unset.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EligibleIdentityVm {
    pub name: String,
    pub address: String,
    /// Empty when it may be used. Otherwise why it may not, in a sentence —
    /// an identity already carrying a currency, or one this wallet cannot sign
    /// for. Shown rather than hidden: a name missing from a list with no
    /// explanation reads as a bug.
    pub refusal: NoteVm,
}

/// One reserve of a basket, as typed.
///
/// The weight is a percentage string rather than a number because the whole
/// draft crosses as text and because the constraint on it is a *sum*: consensus
/// wants the weights to add to exactly one coin, so a value rounded on the way
/// through would move the total off by a satoshi and the launch would build a
/// market nobody asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReserveDraft {
    /// The reserve's **i-address**, which is what the definition carries.
    ///
    /// # Why not the name somebody typed
    ///
    /// Because `currency::definition` parses this field as an address and
    /// refuses anything else — a reserve written as `VRSCTEST` failed the build
    /// after the launch had already been agreed to, with a message about
    /// i-addresses that nothing on the form had ever mentioned. It is chosen
    /// from a list now, and a chosen reserve carries the value consensus wants.
    pub currency: String,
    /// The name that address belongs to, fully qualified, for reading.
    ///
    /// Carried beside the address rather than looked up again: the picker
    /// already had it, and an i-address is not something anybody can check by
    /// eye against the currency they meant. Empty only for a draft that came
    /// from somewhere other than the picker.
    pub name: String,
    /// The share of the basket, in per cent.
    pub weight: String,
}

/// One preallocation, as typed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreallocationDraft {
    /// The identity that receives it — `name@` or an i-address. It must already
    /// exist: consensus pays a preallocation to an identity, not to an address.
    pub recipient: String,
    /// In coins, as typed.
    pub amount: String,
}

/// A currency being configured. Sent to core on every edit; core owns the
/// verdict, because every rule here is a consensus rule.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencyDraft {
    /// "token" | "basket" | "nft".
    pub kind: String,
    /// The i-address of the identity that will define it. Empty until one is
    /// chosen — and one of this and [`Self::new_name`] must be, because a
    /// currency cannot exist without an identity.
    pub identity: String,
    /// The name being claimed, when this launch starts by registering one.
    ///
    /// # Why this is not just `identity` holding a name
    ///
    /// Because they are answers to different questions and one of them has no
    /// address yet. An identity that exists is named by its i-address, which is
    /// the thing that never changes and the thing every later step uses. A name
    /// being claimed has no address until the registration is mined — it cannot
    /// be computed early without duplicating consensus arithmetic, and a field
    /// that means "an address, unless it is a name" is a field every reader has
    /// to check the mode of first.
    ///
    /// Exactly one of the two is set. Both empty is a draft with nothing to
    /// define under; both filled is two answers to one question, and
    /// `currency::problems` refuses each by name.
    pub new_name: String,
    /// Whether the supply can grow later. Permanent either way.
    pub mintable: bool,
    /// Blocks after the current tip at which the currency begins.
    pub start_delay: String,
    pub reserves: Vec<ReserveDraft>,
    pub preallocations: Vec<PreallocationDraft>,
}

/// Something wrong with a draft, or something about to happen that is
/// permanent.
///
/// Two severities, not one. A refusal is the chain saying no; a warning is the
/// chain saying yes to something that cannot be undone — and a form that
/// rendered those the same either blocks a legal currency or waves through the
/// one mistake that has no second attempt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencyProblemVm {
    /// True when this stops the launch, false when it only has to be read.
    pub blocking: bool,
    /// What is wrong, in a sentence somebody can act on.
    pub text: NoteVm,
}

/// What core makes of a draft: the problems, and the numbers the picture is
/// drawn from.
// `Eq` is absent: a slice carries an `f32` proportion, and a proportion is
// not a value anybody compares for exact equality.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CurrencyDraftVm {
    pub problems: Vec<CurrencyProblemVm>,
    /// Nothing blocking. The Review button is gated on this and on nothing else.
    pub ready: bool,
    /// The reserve weights as parts of one whole, already normalised into
    /// percentages for the bar. Empty for anything that is not a basket.
    ///
    /// **Pre-computed here rather than in the interface** for the reason every
    /// figure in this protocol is: the bar's slices have to agree with the
    /// numbers printed beside them, and two places dividing the same totals
    /// will one day disagree.
    pub slices: Vec<CurrencySliceVm>,
    /// Whether the weights add to exactly one whole. The bar draws short when
    /// they do not, and this is what says so in words.
    pub weights_total: String,
    /// The supply this launch would create, formatted, and how it splits.
    pub supply_total: String,
    pub supply_slices: Vec<CurrencySliceVm>,
    /// The block it would start at, formatted.
    pub start_block: String,
    /// What it would cost, assembled — see `currency::cost`.
    pub fee_display: String,
    /// Label/value pairs of what would go on chain, in the order a definition
    /// is read. The preview panel renders exactly this.
    pub preview: Vec<CurrencyFieldVm>,
    /// What this launch will involve, as a diagram. Two steps under an identity
    /// that already exists; four when a name is claimed first.
    ///
    /// A **plan**, not progress — nothing here has happened yet. The same
    /// diagram appears on [`LaunchPendingVm`] once one has, and that one is
    /// progress.
    pub steps: Vec<FlowStepVm>,
}

/// One currency somebody could pick as a reserve.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencyPickVm {
    /// Fully qualified, the way the chain spells it — `Bridge.vETH`, not
    /// `Bridge`. Two currencies can share a last component.
    pub name: String,
    /// Its i-address, which is what a definition carries.
    pub address: String,
    /// "Token" | "Basket" | "NFT", read off the options bitfield.
    pub kind: String,
    /// Something worth knowing before it is used as a reserve, or empty.
    /// Currently only "has not started yet".
    pub note: NoteVm,
}

/// The answer to one search of the currency list.
///
/// # Why the whole list is not simply handed over
///
/// `listcurrencies` is one reply with no pagination — 464KB and 290 currencies
/// on VRSCTEST, measured, and it grows with the chain. It is fetched once and
/// kept in the core; what crosses to the interface is the answer to a question.
/// Slint cannot filter a model in a binding, so a screen holding all of them
/// could not narrow them anyway.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencyChoicesVm {
    /// The matches, best first, capped — see [`Self::more`].
    pub rows: Vec<CurrencyPickVm>,
    /// How many matched beyond the ones carried. Zero when the list is
    /// complete; a screen that silently truncated would read as "that is all
    /// there is" while hiding the currency somebody was looking for.
    pub more: usize,
    /// Whether the chain is still being asked. True only for the first search
    /// of a session, which is the one that fetches the list.
    pub loading: bool,
    /// Why there is nothing to choose from, or empty. A node that could not be
    /// asked is not the same as a chain with no currencies on it.
    pub problem: NoteVm,
}

/// One slice of a proportional bar.
// `Eq` is absent: a slice carries an `f32` proportion, and a proportion is
// not a value anybody compares for exact equality.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CurrencySliceVm {
    pub label: String,
    /// Its share of the bar, 0–100, as a number the layout can multiply by.
    /// The one place in this protocol where a figure crosses as a number rather
    /// than a string, because it is a *proportion* and not an amount — nobody
    /// reads it, the layout divides by it.
    pub percent: f32,
    /// Where the slice starts along the bar, 0–100, for the same reason and
    /// with the same caveat: a coordinate, not a figure anybody reads.
    ///
    /// Carried rather than accumulated by the interface, because the running
    /// total and the share it belongs to are one piece of arithmetic. Two of
    /// them, in two languages, is two chances to disagree about where a slice
    /// begins — and the bar exists to be checked by eye against the numbers
    /// beside it.
    pub offset_percent: f32,
    /// The same share written out, which is what somebody actually reads.
    pub percent_display: String,
    /// The tone to draw it in, cycled so adjacent slices differ.
    pub tone: String,
}

/// One step of the launch, as the diagram draws it.
///
/// # Why the steps are built in core and not laid out in the interface
///
/// Because how many there are is a fact about which path a launch is on, and
/// which of them cost money is a fact about the chain. A launch under an
/// identity that already exists is one transaction; one that starts from a name
/// is three, two of which are paid for. An interface that decided that for
/// itself would be a second opinion about what is about to be spent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowStepVm {
    pub label: NoteVm,
    /// "done" | "now" | "later" | "failed".
    ///
    /// `failed` exists because one of these flows can end without finishing: a
    /// name claim expires about twenty blocks after it is signed, and the fee
    /// is spent either way. A diagram that could only say done, now or later
    /// would have to draw that as one of the three, and all three of them are
    /// untrue.
    pub state: String,
    /// Whether reaching this step spends money. Marked, because not all of them
    /// do and a diagram that treats them alike has lied about the one somebody
    /// wanted to stop before.
    pub costs: bool,
}

impl FlowStepVm {
    /// One step. Here rather than in each caller because two modules in the
    /// core build these lists and a third would otherwise copy the shape.
    pub fn new(label: NoteVm, state: &str, costs: bool) -> Self {
        Self {
            label,
            state: state.to_string(),
            costs,
        }
    }
}

/// One line of the definition preview.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencyFieldVm {
    /// What this line is, named. The words are in `note.slint`.
    pub label: NoteVm,
    /// What it says, when that is a value — a name, an address, an amount.
    pub value: String,
    /// …and when it is a **sentence** instead. Two fields because most of these
    /// lines carry data the core must not translate and two of them carry prose
    /// it must not write. Exactly one is ever set.
    pub value_note: NoteVm,
    /// True for the fields that cannot be changed after the launch. The preview
    /// marks them, because "permanent" is the only property of this screen
    /// worth interrupting somebody for.
    pub permanent: bool,
}

/// A launch, built and signed, waiting for a yes.
///
/// Read back off what was built rather than echoed from the form — the same
/// rule the send review follows, and for the same reason: between the form and
/// the signature sit coin selection, a fee read from chain policy and the
/// identity's own output, and a review that replays the form cannot show a
/// mistake in any of them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchReviewVm {
    pub ticket: u64,
    /// The currency's name, off the definition that was signed.
    pub name: String,
    /// What it will be, in a sentence.
    pub description: NoteVm,
    /// What chain policy charges, and the two halves it splits into. Both are
    /// funded; only one of them comes back as an output.
    pub fee_display: String,
    pub deposit_display: String,
    pub burned_display: String,
    /// The block it begins at.
    pub start_block: String,
}

/// A currency that has been decided on but not made.
///
/// Two waits, and they are not the same. Waiting for a name may still be
/// abandoned for nothing; waiting to define under a name that already exists
/// means an identity has been registered and paid for with nothing made under
/// it — and that identity can never be used for a different currency.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchPendingVm {
    /// The identity it is for, as a name.
    pub identity: String,
    /// "awaiting-identity" | "ready".
    pub step: String,
    /// What is happening, in a sentence somebody can act on.
    pub note: NoteVm,
    /// Whether the wallet is waiting on a press rather than on the chain.
    pub can_continue: bool,
    /// How far along it is, as the same diagram the form drew as a plan.
    pub steps: Vec<FlowStepVm>,
}

/// A launch that reached the network.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchDoneVm {
    pub txid: String,
    /// The new currency's i-address, which is its identity's.
    pub address: String,
    pub name: String,
    pub start_block: String,
}

/// Everything the detail sheet shows about one identity.
///
/// Read through `current_identity`, which decodes the identity from the output
/// script rather than from the node's JSON rendering — the same read every
/// write operation starts from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityDetailVm {
    pub name: String,
    pub address: String,
    pub status: String,
    pub tone: String,
    /// The addresses that may sign, and how many of them are needed.
    pub primary_addresses: Vec<String>,
    pub signatures_required: String,
    /// "This wallet holds 1 of the 1 key needed" — or that it holds none.
    pub control_note: NoteVm,
    /// Whether this wallet holds enough keys to sign for it.
    ///
    /// Its own field rather than something inferred from `control_note`. A
    /// capability read out of a sentence breaks the moment the sentence is
    /// reworded, and what breaks is a button that builds a transaction the
    /// chain then refuses — which costs a fee to discover.
    pub can_sign: bool,
    pub revocation_authority: String,
    pub recovery_authority: String,
    /// Set when the recovery authority is the identity itself, which makes it
    /// **unrevokable**: consensus refuses a revocation whose subject is its own
    /// recovery authority. A freshly registered identity has exactly this shape
    /// by default, and nothing says so at the time.
    pub cannot_be_revoked: bool,
    /// The lock, in words. Never "locked: true" — the two locked states behave
    /// nothing alike and only one of them ends on its own.
    pub timelock_note: NoteVm,
    /// What it holds, already formatted.
    pub balance_display: String,
    /// Published content as it stands now.
    pub content: Vec<ContentEntryVm>,
    /// Every value ever published, which is a different question — see
    /// `ChainReader::identity_content`.
    pub content_history: Vec<ContentEntryVm>,
}

/// One key in an identity's content map, with its values.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentEntryVm {
    /// The VDXF key as the i-address the map is keyed by.
    pub key: String,
    /// The URI this key hashes from, when this wallet knows one that does.
    /// Empty otherwise — and that is permanent, not pending: a VDXF key is a
    /// one-way hash and there is no inverse.
    pub name: String,
    pub values: Vec<ContentValueVm>,
}

/// One published value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentValueVm {
    /// The bytes as text, when reading them that way is defensible. Empty
    /// otherwise.
    pub text: String,
    /// Always available. The honest rendering, and the one to fall back to.
    pub hex: String,
    /// "12 bytes", for values that are not text.
    pub size: String,
    /// The daemon's own rendering, when it recognised the key. Cannot be turned
    /// back into bytes, so it stands beside them rather than replacing them.
    pub structured: String,
}

/// An address this wallet has paid, or been given a name for.
///
/// On screen because a wallet that quietly keeps a list of who you paid, and
/// never shows it to you, is keeping a secret from its owner. Everything in
/// here is already on the chain — what is private is the *name*, which is this
/// wallet's note and nobody else's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownAddressVm {
    pub address: String,
    /// Empty until somebody names it.
    pub label: String,
    /// What the chain calls it — `dude.VRSCTEST@` — for an i-address that is a
    /// VerusID. Empty for a plain address.
    ///
    /// A payment to a VerusID records the i-address it resolved to, because
    /// that is what the transaction pays. Without this the list of people you
    /// have paid is a list of `i4YzoP8Z…`, which is nobody.
    pub name: String,
    /// "3 payments · 2 days ago", or "never paid" for one that was only named.
    pub summary: String,
}

// ── The chart ───────────────────────────────────────────────────────────────

/// One reading of the balance.
///
/// The one place an amount crosses this boundary as a **number** rather than a
/// string, and it is deliberate: these never reach a Slint property. They are
/// consumed by `pecu-chart` to compute geometry, and the only figures that
/// reach a screen are the ones the interface formats through
/// [`crate::format::coins`] — the same function the core uses.
///
/// It has to be that way round because the amount under a chart cursor is
/// chosen by a pointer moving at sixty hertz. Pre-formatting every point would
/// mean carrying a string per sample through a downsampler that drops most of
/// them; asking the core per frame would be a round trip per frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChartPointVm {
    /// Unix seconds.
    pub t: i64,
    /// The native balance at that moment, in satoshis.
    pub sats: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChartVm {
    /// Oldest first.
    pub points: Vec<ChartPointVm>,
    /// The scan has reached the start of the chain, so the earliest point
    /// really is the beginning. When false, the chart covers only what has been
    /// looked at so far — and the range buttons that would claim more than that
    /// are the ones the interface has to refuse.
    pub complete: bool,
    /// What to write next to an amount.
    pub ticker: String,
}

// ── Transactions ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TxDirection {
    #[default]
    Incoming,
    Outgoing,
    /// Value moved between our own addresses; only the fee actually left.
    Self_,
}

/// One row in the activity list.
///
/// `confirmations` is deliberately **not** here. It is derived in the UI from
/// this row's `height` and a single `tip` property on the store global, so a new
/// block updates one integer and every visible row recomputes itself — no model
/// mutation, no notification storm, once per block.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRowVm {
    pub txid: String,
    /// The same id, head and tail, for a line that has room for neither.
    ///
    /// Head **and** tail rather than just the tail: sixty-four hex characters
    /// elided to fit are indistinguishable from each other, and a bare `…a5f2`
    /// could be the end of anything. Truncated here rather than in the
    /// interface so that one rule decides what an abbreviated identifier looks
    /// like everywhere.
    pub txid_short: String,
    /// 0 while unconfirmed.
    pub height: u32,
    pub block_time: i64,
    pub direction: TxDirection,
    /// Signed satoshis, decimal string. Negative for outgoing.
    pub net_sats: String,
    pub net_display: String,
    /// Non-native currencies this transaction moved, pre-formatted.
    pub currency_lines: Vec<String>,
    /// Pre-formatted "2 hours ago" / "yesterday".
    pub when_display: NoteVm,
    /// Non-empty when this row begins a new day: the heading to draw above it
    /// ("Today", "Yesterday", "12 March 2026").
    ///
    /// Computed here rather than in the UI because it depends on comparing this
    /// row with the one before it, which a `for` loop over a model cannot do —
    /// and because a calendar day is a fact about a timezone, not about a list.
    pub group: NoteVm,
    pub pending: bool,
    /// "payment" · "convert" · "login" · "identity".
    ///
    /// What sort of thing happened, which is what the filters act on. Carried
    /// rather than inferred from the amount: a declined login and an identity
    /// update both move nothing, and telling them apart by their emptiness is
    /// not telling them apart.
    ///
    /// Only `payment` is produced today. The other three are the shapes the
    /// design's history has, and they arrive when the features behind them do
    /// — a login needs VDXF consent, a convert needs the conversion flow, and
    /// an identity action needs `getidentityhistory` folded into this list.
    ///
    /// `serde(default)` because this list is **cached on disk**, and a snapshot
    /// written before this field existed has no key for it.
    /// `Store::load_snapshot` deserialises with `.ok()?`, so a missing field
    /// does not fail loudly — it silently discards the whole cached history and
    /// every existing wallet comes up empty until it has re-read the chain.
    #[serde(default)]
    pub kind: String,
    /// The line under the title: what this was, in the words of the thing it
    /// was. "via Bridge.vETH", "as robert.VRSCTEST@", "Recovery address
    /// changed". Empty when the time alone says enough.
    ///
    /// **Not the counterparty for a payment.** History is read from address
    /// deltas and a delta names nobody — that is a property of how the chain is
    /// queried, not an omission here.
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxDetailVm {
    pub txid: String,
    pub height: u32,
    pub confirmations: Option<u32>,
    pub block_time: i64,
    pub when_display: NoteVm,
    pub net_display: String,
    /// Whether `net_display` is the chain's own currency.
    ///
    /// False for a transaction that moved a token and no native value: the
    /// amount is then already a token line naming its own currency, and
    /// labelling it with the chain's ticker as well would read as
    /// "12 345 mambo VRSCTEST".
    pub amount_is_native: bool,
    /// Only when the node reported one. A fee is inputs minus outputs, and the
    /// input values live in *other* transactions — computing it for an
    /// arbitrary transaction would cost one lookup per input. `None` means
    /// unknown, and the screen says so rather than showing a zero.
    pub fee_display: Option<String>,
    pub direction: TxDirection,
    /// Non-native currencies this transaction moved, pre-formatted.
    pub currency_lines: Vec<String>,
    /// Where to look this up. Not opened for you — see the Advanced section.
    pub explorer_url: Option<String>,
    /// Raw decoded JSON, shown only under Advanced. `None` when the node
    /// declined or has not been asked.
    pub raw_json: Option<String>,
}

/// A transaction we handed to a node and could not confirm reached it.
///
/// This exists because `FlowError::BroadcastUncertain` exists: a transport
/// failure on broadcast is genuinely ambiguous — the node may well have accepted
/// and relayed it. The bytes are kept so the *same* transaction can be re-sent
/// rather than rebuilt, because rebuilding would select different inputs and
/// could spend twice.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingVm {
    pub id: u64,
    pub txid: String,
    pub to_address: String,
    pub amount_display: String,
    pub created_display: String,
    pub checks: u32,
    pub state: String,
}

// ── Send ────────────────────────────────────────────────────────────────────

/// What the user has typed so far. Sent to core for validation; core owns the
/// verdict, because address parsing and fee rules live in the SDK.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendDraft {
    pub from_label: String,
    pub to: String,
    /// As typed, in coins. Core parses it with `Amount::from_coins_str`.
    pub amount: String,
}

/// A sentence the **interface** writes, named by the core.
///
/// # Why the core stopped writing sentences
///
/// It used to return finished English prose — "More than the 12.5 you can spend
/// now." — and roughly a hundred and forty of those are scattered through
/// `pecu-core`. That is a translation problem and it is not fixable with a
/// catalogue: `@tr` reaches `.slint` and nothing else, so every one of those
/// sentences would have been permanently English no matter how many `.po` files
/// the project grew.
///
/// It is also a layering problem that predates the translation one. Deciding
/// *what is wrong* is the core's job — it owns the address rules and the fee
/// rules. Deciding *how to say it* is the interface's, and handing over a
/// finished sentence takes that decision away from the only side that knows how
/// much room the line has, what is beside it, and which language it is in.
///
/// So the core names the reason and supplies the values; the interface has the
/// words. An unknown code renders as itself rather than as nothing — a message
/// nobody wrote is a bug, and a blank line hides it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteVm {
    /// A stable identifier, never shown to anybody. Empty means "say nothing",
    /// which is a real answer and not a missing one.
    pub code: String,
    /// The values the sentence needs, already formatted by the core — money is
    /// spelled in exactly one place in this workspace and the interface is not
    /// a second one.
    ///
    /// A list, because the longest of these takes three: a name, the address it
    /// points at now, and the one it pointed at before. Slint carries this as
    /// `[string]` inside the struct and a binding can index it — checked by
    /// compiling it, after the first version of this comment asserted the
    /// opposite and was wrong.
    pub args: Vec<String>,
}

impl NoteVm {
    /// A note with nothing in it. Say nothing.
    pub fn none() -> Self {
        Self::default()
    }

    /// A note with no values in it.
    pub fn plain(code: &str) -> Self {
        Self {
            code: code.to_string(),
            args: Vec::new(),
        }
    }

    /// A note that names some values.
    pub fn with(code: &str, args: impl IntoIterator<Item = String>) -> Self {
        Self {
            code: code.to_string(),
            args: args.into_iter().collect(),
        }
    }
}

/// What core makes of the two boxes on the send form.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftValidationVm {
    pub to_valid: bool,
    /// What sort of address it is, or why it is not one.
    pub to_note: NoteVm,
    pub amount_valid: bool,
    pub amount_note: NoteVm,
    /// A name this wallet has given the recipient's address, or empty.
    ///
    /// Beside the note rather than inside it: the core used to compose
    /// `"{note} · {label}"`, which decided a separator and a line break on
    /// behalf of a screen it cannot see.
    pub to_label: String,
    /// Everything checks out and Review may be pressed.
    pub ready: bool,
}

/// The review step, built by decoding the transaction that was actually
/// **built and signed** — not an echo of what the user typed.
///
/// The distinction is the whole point of the step. Between the form and the
/// signature sit coin selection, the fee rule and change placement; a review
/// that replays the form cannot show a mistake in any of them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendReviewVm {
    pub ticket: u64,
    /// Every output as decoded from the signed bytes, change included.
    pub outputs: Vec<ReviewOutputVm>,
    pub amount_display: String,
    pub fee_display: String,
    pub total_display: String,
    pub change_display: String,
    pub balance_after_display: String,
    pub from_address: String,
    /// True when we have never sent to this address before. Worth saying.
    pub first_time_recipient: bool,
    /// The VerusID name this was addressed to, when it was addressed by name.
    /// Empty for an address typed out.
    ///
    /// The outputs below carry the i-address, decoded from the signed bytes.
    /// This is the *question* that produced it, and the review shows both:
    /// somebody who typed `meineid@` cannot check an i-address they have never
    /// seen, and somebody shown only the name is being asked to trust a lookup
    /// they were not told happened.
    pub recipient_name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewOutputVm {
    /// The address the output pays, where the script decodes to one. A
    /// CryptoCondition this build cannot read is reported as such rather than
    /// guessed at.
    pub address: Option<String>,
    /// What kind of output this is, as a named reason rather than a phrase.
    pub kind: NoteVm,
    pub amount_display: String,
    pub is_change: bool,
}

/// A conversion, signed and not sent, as the review shows it.
///
/// Built the same way [`SendReviewVm`] is and for the same reason: by decoding
/// the transaction that will actually be broadcast. On this screen that matters
/// more rather than less, because a conversion's whole meaning is in a
/// CryptoCondition payload nobody can read by eye — which currency, how much,
/// which basket it routes through, and where the result lands are all *inside*
/// the output, and a review that echoed the form could not show one of them
/// wrong.
///
/// Two figures here are **not** from the bytes, because they cannot be:
/// [`Self::estimate_display`] and [`Self::minimum_display`]. Neither exists in
/// the protocol. See [`Self::minimum_display`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvertReviewVm {
    pub ticket: u64,
    /// Every output as decoded from the signed bytes, change included.
    pub outputs: Vec<ReviewOutputVm>,
    /// The currencies, named as the chain spells them.
    pub from: String,
    pub to: String,
    /// The basket this routes through, or empty when it is direct. Read out of
    /// the transfer's own `destCurrencyID`, not copied from the quote.
    pub via: String,
    /// What goes in, read out of the reserve transfer.
    pub pay_display: String,
    /// What the node expected to come out when this was planned, moments before
    /// it was signed. **Advisory**, like every conversion figure: the chain
    /// performs the conversion when it imports the output, at whatever the
    /// ratios are then.
    pub estimate_display: String,
    /// The floor that was checked against that estimate before signing.
    ///
    /// **Checked once, there, and never again.** Nothing in the protocol
    /// enforces it — if the price moves after this is broadcast, the conversion
    /// still executes at whatever the ratios are. It is a record of intent, and
    /// the review has to keep saying so rather than presenting it as a bound.
    pub minimum_display: String,
    /// The transfer fee written into the conversion itself, in the chain's own
    /// currency. Read out of the payload.
    pub conversion_fee_display: String,
    /// What the miners are paid to carry the transaction. A different fee to
    /// different people, and never folded into the one above.
    pub network_fee_display: String,
    /// What leaves the wallet in the chain's own currency: the amount plus both
    /// fees when the source **is** the chain's currency, the fees alone when it
    /// is a token — in which case the token side leaves through the payload and
    /// is not native at all.
    pub total_display: String,
    pub balance_after_display: String,
    pub from_address: String,
    /// Where the converted value is delivered, decoded from the transfer.
    ///
    /// Always this wallet's own address today, and shown anyway: it is written
    /// as a bare key hash by the builder, so an identity address here would pay
    /// the R-form of the same twenty bytes, which nobody holds a key for. A
    /// destination somebody can read is the only defence against that class of
    /// mistake.
    pub recipient: String,
}

/// `Serialize` only — it carries a [`UiError`], whose `code` is a
/// `&'static str`. See [`crate::error::UiError`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum SendOutcomeVm {
    Sent {
        txid: String,
        fee_display: String,
        /// Where to look this up, or empty on a chain with no explorer this
        /// build knows about — see `pecu_chain::Network::explorer`.
        ///
        /// Built in the core because *which* explorer is a fact about the
        /// chain, and the interface has no business knowing one. Empty rather
        /// than `Option` because it crosses into `.slint`, where the empty
        /// string is how absence is already spelled everywhere else.
        explorer_url: String,
    },
    /// Broadcast failed in a way that is genuinely ambiguous.
    Uncertain {
        txid: String,
        pending_id: u64,
        /// Carried here too, and it matters more: this is the state where
        /// somebody most wants to go and look for themselves.
        explorer_url: String,
    },
    Failed(UiError),
}

// ── UI shell ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScreenId {
    Unlock,
    #[default]
    Dashboard,
    Send,
    Receive,
    Activity,
    /// What everything is worth. Like `Nodes` and `Identities`, its being open
    /// is what justifies the two requests it makes — nothing else on any other
    /// screen reads a price.
    Markets,
    /// The node list. Separate from `Settings` because it is the one screen
    /// whose being open justifies asking every configured node a question —
    /// polling endpoints nobody is looking at is asking public infrastructure
    /// for something nothing will read.
    Nodes,
    /// The VerusIDs this wallet's keys control. Like `Nodes`, its being open is
    /// what justifies the requests it makes: finding them costs one call per
    /// key, and nothing else on any other screen reads the answer.
    Identities,
    /// The currencies those identities define. Same justification again, and
    /// one more request per identity on top: a currency is read by asking about
    /// the identity that defines it.
    Currencies,
    Settings,
    CreateWallet,
    ImportWallet,
}

/// Why the wallet locked, so the unlock screen can say something true.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LockReason {
    Manual,
    Timeout,
    Shutdown,
}

/// Long-running work the UI should show as busy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskKind {
    Unlocking,
    RefreshingBalance,
    LoadingHistory,
    ProbingNodes,
    PreparingSend,
    Broadcasting,
    /// Building, signing or sending a conversion.
    ///
    /// One kind covering both halves rather than two, because the convert
    /// screen has one busy flag and the difference between "signing" and
    /// "sending" is not one it draws. `PreparingSend` and `Broadcasting` are
    /// two because the send screen shows a different step for each.
    Converting,
    CreatingWallet,
}

/// A conversion somebody is composing.
///
/// Currencies by **i-address**, not by name. The pickers hand back what the
/// chain's own list carries, and two currencies on VRSCTEST can share a name
/// component — `Bridge.vETH` and `Bridge.CHIPS` are both `Bridge` — so a draft
/// keyed by name is a draft that can name the wrong thing.
///
/// The amount stays as it was typed. Parsing it is a rule, rules live in the
/// core, and a draft that arrived already parsed could not carry "0.0.1".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvertDraft {
    pub from: String,
    pub to: String,
    pub pay: String,
}

/// What a conversion would cost, priced but not signed.
///
/// # Why this is net of fees where the markets screen is not
///
/// Two sources, two purposes, and mixing them is how a wallet quotes a price it
/// cannot honour. `getcurrencystate` reports a mid price and is what a *display*
/// shows; `estimateconversion` answers what a node expects this particular
/// conversion to yield, after `conversionfees` and `fees`. This carries the
/// second. A number on the markets screen is what the currency is worth; a
/// number here is what somebody would actually receive.
///
/// Every figure is a formatted string for the same reason the market rows are:
/// the core owns how money is spelled, and `—` has to be sayable.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConvertQuoteVm {
    pub from: String,
    pub to: String,
    /// What goes in, as typed and then formatted back.
    pub pay: String,
    /// What the wallet holds of `from`, spelled here rather than in the
    /// interface. The convert form needs it under the paying box and the
    /// portfolio does not carry it in a shape a single leg can read.
    pub from_balance: String,
    /// What the node expects to come out. Advisory — the SDK says so, and so
    /// does this: the chain does not enforce it.
    pub get: String,
    /// The basket the conversion goes through, or empty when it is direct.
    pub via: String,
    /// "1 VRSCTEST = 0.5369 DAI.vETH", spelled by the core.
    pub rate: String,
    pub conversion_fee: String,
    pub network_fee: String,
    /// The least the caller is willing to accept.
    ///
    /// **Checked before signing and never again.** If the price moves after the
    /// transaction is broadcast the conversion still happens — the chain has no
    /// opinion about a floor. Recording it makes the intent explicit and catches
    /// a price that has already moved, which is all it can do.
    pub minimum: String,
    /// How far the estimate is from the mid price, as the person will read it.
    pub slippage: String,
    /// "positive" · "warning" · "negative". Carried rather than derived from a
    /// threshold here, because the threshold is a rule and rules live in core.
    pub slippage_tone: String,
    /// Why this cannot be done, or what to be careful of. `NoteVm::none()` when
    /// neither — the core names the reason, `note.slint` has the words.
    pub note: NoteVm,
    /// Whether there is enough here to act on. The core's verdict rather than a
    /// length check in the interface: "enough" means a route exists, the amount
    /// parses, and the wallet holds it, and none of those is a property of the
    /// text in a field.
    pub ready: bool,
}

/// One currency in the markets table.
///
/// Every figure is a **formatted string**, not a number, and that is the same
/// decision the rest of this protocol makes: the core owns how money is spelled,
/// so a total cannot be rounded one way on the dashboard and another way here.
///
/// It also lets a figure be honestly absent. `"—"` means the wallet does not
/// know — a currency with no converter has no price, and there is no number
/// that says that. Writing `0` there would be a claim.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MarketRowVm {
    pub name: String,
    /// The i-address, so a row can be opened without matching on its name.
    pub address: String,
    /// In the fiat the wallet prices against, or `"—"`.
    pub price: String,
    /// Signed and suffixed, or `"—"`.
    pub change: String,
    /// "positive" · "negative" · "unknown". Chooses the colour, and is not
    /// derived from `change` starting with a minus — a dash is neither.
    pub tone: String,
    /// What could be taken out before the price moves 2%. The one number on
    /// this screen that says whether the others can be acted on.
    pub depth: String,
    /// What the started baskets hold of it, in the quote currency, or `"—"`.
    ///
    /// The size of the market, and the column the table is **ordered** by. The
    /// only figure here that is comparable between rows: `depth` is in each
    /// row's own currency, and a table sorted by that would be sorting on a
    /// mixture of units.
    pub pooled: String,
}

/// One pool a currency trades in.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VenueVm {
    pub name: String,
    /// "active" · "unstarted" · "empty", in the vocabulary the node list uses.
    pub state: String,
    pub price: String,
    pub change: String,
    pub tone: String,
    pub depth: String,
}

/// One labelled figure on the currency detail.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StatVm {
    pub label: NoteVm,
    pub value: String,
}

/// The right-hand half of the markets screen: one currency, in detail.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MarketDetailVm {
    pub name: String,
    /// "Verus · PoW/PoS · 7 venues" — what it is, in one line.
    pub subtitle: String,
    pub price: String,
    pub change: String,
    pub tone: String,
    /// What the price did, over `market::WINDOW_DAYS`. Oldest first, in
    /// satoshis of the quote currency per one unit — an integer, because a
    /// series of floats renders differently on two machines.
    ///
    /// Empty for a pool with no published history, which is the honest picture
    /// of an idle market rather than a chart that failed.
    pub series: Vec<ChartPointVm>,
    pub stats: Vec<StatVm>,
    pub venues: Vec<VenueVm>,
    /// How the price was arrived at, as a chain of hops. On a wallet whose
    /// prices are derived from conversion routes rather than taken from a feed,
    /// this is not decoration: it is the difference between a number somebody
    /// can check and one they have to trust.
    pub route: String,
    /// Which hop is the constraint, and why — as a named reason. `NoteVm::none()`
    /// when there is nothing to say.
    pub route_note: NoteVm,
}

/// One thing the search found.
///
/// # Why the search runs in the core
///
/// Not by preference — by necessity, and it is worth writing down because it
/// looks like a UI concern. Slint's string type offers `is-empty`,
/// `character-count`, `to-lowercase`, `to-uppercase` and the two float
/// conversions. There is **no substring test**, so "does this name contain what
/// was typed" cannot be asked in `.slint` at all.
///
/// That turns out to be the right shape anyway: the interface holds no wallet
/// data of its own, and a search that filtered a copy of the rows it happens to
/// have rendered would answer for the screen rather than for the wallet.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchHitVm {
    /// "address" · "currency". Chooses the icon and tells the interface which
    /// screen to open, without it having to parse `target`.
    ///
    /// Both destinations are screens that are in the rail, which is the whole
    /// of the rule: a hit whose only home is a hidden screen strands whoever
    /// picks it. `Core::search` says what that cost.
    pub kind: String,
    /// What to show: a saved address's label, or a currency's name as the chain
    /// spells it. An address nobody has named repeats its address here rather
    /// than leaving the line empty.
    pub label: String,
    /// The address, under the name. A person searching for a name recognises
    /// it; a person searching for an address needs to see it echoed back or
    /// they cannot tell which of two similar rows they matched.
    pub sub: String,
    /// The address again, as the thing to act on. Separate from `sub` because
    /// what is *shown* and what is *opened* are allowed to diverge later, and
    /// discovering that they had been the same field is how a display change
    /// breaks navigation.
    pub target: String,
}

#[cfg(test)]
mod tests {
    use super::SeedWordVm;

    /// `Event` derives `Debug`, and unhandled variants get logged with `?event`
    /// in two places. If this ever prints the word, that is one added enum
    /// variant away from a recovery phrase in a log file.
    #[test]
    fn a_seed_word_never_prints_itself() {
        let word = SeedWordVm {
            index: 7,
            word: "abandon".to_string(),
        };

        let printed = format!("{word:?}");
        assert!(!printed.contains("abandon"), "{printed}");
        assert!(printed.contains('7'), "the position is not the secret");

        // And inside anything that wraps it, which is the case that matters.
        let wrapped = format!("{:?}", vec![word]);
        assert!(!wrapped.contains("abandon"), "{wrapped}");
    }
}
