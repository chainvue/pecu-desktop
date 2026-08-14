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
    pub note: Option<String>,
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
    pub note: String,
    /// The block the commitment must be registered by, and how long that is.
    /// Empty when there is no deadline to state yet.
    pub deadline: String,
    /// The registration fee, formatted.
    pub fee_display: String,
    /// The identity's address, once it exists.
    pub address: String,
    /// Whether the wallet is waiting on a node right now.
    pub busy: bool,
    /// Set when this identity was registered as its own recovery authority, so
    /// the screen can offer the fix while somebody is still looking at it.
    pub cannot_be_revoked: bool,
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
    pub note: String,
    /// Whether this wallet holds enough keys to sign for it. Decides which
    /// actions are offered, and is a fact about this wallet rather than about
    /// the identity.
    pub mine: bool,
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
    pub control_note: String,
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
    pub timelock_note: String,
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
    /// "3 payments · 2 days ago", or "never paid" for one that was only named.
    pub summary: String,
}

// ── The chart ───────────────────────────────────────────────────────────────

/// One reading of the balance.
///
/// The one place an amount crosses this boundary as a **number** rather than a
/// string, and it is deliberate: these never reach a Slint property. They are
/// consumed by `chainvue-chart` to compute geometry, and the only figures that
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
    pub when_display: String,
    /// Non-empty when this row begins a new day: the heading to draw above it
    /// ("Today", "Yesterday", "12 March 2026").
    ///
    /// Computed here rather than in the UI because it depends on comparing this
    /// row with the one before it, which a `for` loop over a model cannot do —
    /// and because a calendar day is a fact about a timezone, not about a list.
    pub group: String,
    pub pending: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxDetailVm {
    pub txid: String,
    pub height: u32,
    pub confirmations: Option<u32>,
    pub block_time: i64,
    pub when_display: String,
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftValidationVm {
    pub to_valid: bool,
    /// "Transparent address" / "VerusID" / the reason it is not one.
    pub to_note: String,
    pub amount_valid: bool,
    pub amount_note: String,
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
    pub kind: String,
    pub amount_display: String,
    pub is_change: bool,
}

/// `Serialize` only — it carries a [`UiError`], whose `code` is a
/// `&'static str`. See [`crate::error::UiError`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum SendOutcomeVm {
    Sent {
        txid: String,
        fee_display: String,
    },
    /// Broadcast failed in a way that is genuinely ambiguous.
    Uncertain {
        txid: String,
        pending_id: u64,
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
    /// The node list. Separate from `Settings` because it is the one screen
    /// whose being open justifies asking every configured node a question —
    /// polling endpoints nobody is looking at is asking public infrastructure
    /// for something nothing will read.
    Nodes,
    /// The VerusIDs this wallet's keys control. Like `Nodes`, its being open is
    /// what justifies the requests it makes: finding them costs one call per
    /// key, and nothing else on any other screen reads the answer.
    Identities,
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
    CreatingWallet,
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
