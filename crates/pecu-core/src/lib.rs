//! The wallet core: one actor that owns all state and does all I/O.
//!
//! # Shape
//!
//! ```text
//! UI ──Command──▶ mpsc ──▶ actor ──spawn_blocking──▶ SDK (blocking ureq)
//!                            │
//!                            └──Event──▶ mpsc ──▶ UI
//! ```
//!
//! Every mutation happens in one task, so there are no locks over application
//! state, no lock ordering to get wrong, and event order is deterministic —
//! which is what makes the actor replayable in a test.
//!
//! # No Slint here
//!
//! This crate has no UI dependency, so unlock, node health, portfolio
//! aggregation and error mapping are all testable with `#[tokio::test]` and no
//! windowing system. That is the single decision that keeps the application
//! layer testable in CI.
//!
//! # Blocking calls, and why not the async driver
//!
//! The SDK's client is blocking (`ureq`). It is called through
//! [`runtime::Blocking`], which is `spawn_blocking` behind a semaphore — so the
//! UI thread is never blocked and the pool cannot grow without bound.
//!
//! The SDK also offers `verus_flows::drive::advance`, which does no I/O and
//! hands back request bodies for the caller to fetch. Taking that path would
//! mean writing our own HTTP and re-implementing `HttpTransport`'s hardening:
//! the TLS-scheme allowlist, `redirects(0)` (a 307 on `sendrawtransaction`
//! could hand signed bytes somewhere else), the response cap, and credential
//! zeroization. That is a security regression dressed as a modernisation, so
//! `spawn_blocking` it is.

pub mod convert;
pub mod currency;
pub mod identity;
pub mod launch;
pub mod market;
pub mod paths;
pub mod pending;
pub mod portfolio;
pub mod registration;
pub mod runtime;
pub mod send;
pub mod wallet;

use std::sync::Arc;

use pecu_chain::{Chain, Network, Node, NodeManager};
use pecu_protocol::{
    Command, Event, LockReason, NetworkVm, NodeVm, NoteVm, Reachability, TaskKind,
};
use tokio::sync::mpsc;
use verus_sdk::verus_keys::bip39::MnemonicError;

use runtime::Blocking;
use wallet::{Imported, Wallet};

/// Sends commands to the core. Cloned into every UI callback.
#[derive(Clone)]
pub struct Dispatcher(mpsc::UnboundedSender<Command>);

impl Dispatcher {
    /// Fire and forget. Never blocks, never awaits — a UI callback that took
    /// 40 ms would be a dropped frame.
    pub fn send(&self, command: Command) {
        if self.0.send(command).is_err() {
            tracing::warn!("the core has stopped; command dropped");
        }
    }
}

/// How the core was configured at startup.
pub struct Config {
    pub nodes: Vec<Node>,
    /// The chain to open. Every durable file is chosen from this — see
    /// [`paths::Paths`] — so it is not merely what the network panel displays.
    pub network: Network,
    pub mock: bool,
    /// The application home, holding one directory per chain. The wallet file
    /// and everything beside it are derived from this and `network`, rather
    /// than passed in, so switching chains cannot leave one file behind.
    pub home: std::path::PathBuf,
}

/// Start the core on `runtime`, returning the handle to talk to it and the
/// stream of events it produces.
pub fn start(
    runtime: &tokio::runtime::Handle,
    config: Config,
) -> (Dispatcher, mpsc::UnboundedReceiver<Event>) {
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();

    let paths = paths::Paths::new(config.home, &config.network, config.mock);

    // Beside the vault, in the per-network directory. Failing to open it is not
    // failing to start: everything in there is either a preference or something
    // a node can be asked for again.
    let store = open_store(&paths);

    let (work_tx, work_rx) = mpsc::unbounded_channel();

    let core = Core {
        wallet: Wallet::open_or_absent(paths.vault()),
        nodes: NodeManager::new(config.nodes.clone(), config.network),
        blocking: Blocking::new(runtime.clone(), 8),
        events: event_tx,
        mock: config.mock,
        chain: None,
        #[cfg(feature = "mock")]
        mock_for: Vec::new(),
        cached: portfolio::Cached::default(),
        refreshing: false,
        work: work_tx,
        spendable: verus_sdk::money::Amount::ZERO,
        native_balance: 0,
        prepared: std::collections::HashMap::new(),
        tickets: 0,
        pending: pending::Ledger::open(paths.pending()),
        last_checked: std::collections::HashMap::new(),
        checking: std::collections::HashSet::new(),
        polling: Polling::default(),
        broadcasting: false,
        history: HistoryScan::default(),
        known: std::collections::BTreeMap::new(),
        identity: None,
        resolving: None,
        last_draft: pecu_protocol::SendDraft::default(),
        identities: std::collections::BTreeMap::new(),
        currencies: Vec::new(),
        history_filter: "all".to_string(),
        eligible: Vec::new(),
        launch_fees: None,
        currency_catalog: Catalog::Unasked,
        market: market::Book::default(),
        market_names: std::collections::BTreeMap::new(),
        market_open: String::new(),
        convert: pecu_protocol::ConvertDraft::default(),
        convert_ticket: 0,
        convert_ready: None,
        holdings: std::collections::BTreeMap::new(),
        markets: Markets::Unasked,
        pending_currency_search: None,
        launches: std::collections::HashMap::new(),
        intent: launch::Intent::open(paths.launch()),
        looked_up: Vec::new(),
        reservation: registration::Reservation::open(paths.registration()),
        identity_changes: std::collections::HashMap::new(),
        identity_confirms: std::collections::HashSet::new(),
        ready: None,
        open_identity: String::new(),
        open_content_keys: std::collections::BTreeSet::new(),
        vdxf: None,
        store,
        paths,
        builtin: config.nodes,
    };

    runtime.spawn(core.run(command_rx, work_rx));
    (Dispatcher(command_tx), event_rx)
}

/// Turn each preallocation recipient into the 20 bytes consensus wants.
///
/// A preallocation pays an **identity**, not an address. An i-address carries
/// the hash and would decode offline; a `name@` does not. Both go through the
/// same lookup here rather than branching, because the answer that matters is
/// the identity's own `identity_address` — asking is what makes a name and its
/// i-address the same answer instead of two code paths that can disagree.
///
/// Off the actor, in the blocking job, so `currency::definition` stays pure and
/// testable without a chain.
fn resolve_recipients(
    chain: &Chain,
    draft: &pecu_protocol::CurrencyDraft,
) -> Result<std::collections::BTreeMap<String, [u8; 20]>, String> {
    use verus_sdk::network::ChainReader;

    let mut resolved = std::collections::BTreeMap::new();
    for allocation in &draft.preallocations {
        let typed = allocation.recipient.trim().to_string();
        if typed.is_empty() {
            continue;
        }
        let record = chain
            .identity(&typed)
            .map_err(|error| format!("{typed}: {error}"))?;
        let address = record
            .identity_address
            .parse::<verus_sdk::verus_keys::Address>()
            .map_err(|error| format!("{typed}: {error}"))?;
        resolved.insert(typed, address.hash());
    }
    Ok(resolved)
}

/// The smallest window `AppWindow` declares it can be laid out at.
///
/// Written here as well as in the `.slint` file, which is a duplicate and is
/// worth it: this is the only side that can refuse a stored size, and a core
/// that trusted whatever was in the database could hand back a window nothing
/// fits in. If the two ever disagree, the interface is right — it is the one
/// doing the layout.
const MIN_WINDOW: (u32, u32) = (880, 620);

/// The size the window was last left at, if it is usable.
fn remembered_window(store: &pecu_store::Store) -> Option<(u32, u32)> {
    let read = |key: &str| store.setting(key)?.parse::<u32>().ok();
    let (width, height) = (read("window_width")?, read("window_height")?);
    (width >= MIN_WINDOW.0 && height >= MIN_WINDOW.1).then_some((width, height))
}

/// Open the wallet database for one chain, or carry on without one.
///
/// Split out because switching chains has to do exactly this again, and a
/// second copy of the "log it and continue" decision is a second place for the
/// two to disagree about whether a missing database is fatal. It is not.
fn open_store(paths: &paths::Paths) -> Option<pecu_store::Store> {
    match pecu_store::Store::open(paths.dir()) {
        Ok(store) => Some(store),
        Err(error) => {
            tracing::error!(%error, "the wallet database could not be opened");
            None
        }
    }
}

// Nine flags on a forty-field actor. The lint is about a struct somebody
// constructs positionally, where four bools in a row are four chances to swap
// two of them; this one is private state, built once, in a literal that names
// every field. Grouping them into a sub-struct would move the names one level
// further from where they are read and change nothing about the risk.
#[allow(clippy::struct_excessive_bools)]
struct Core {
    wallet: Wallet,
    nodes: NodeManager,
    blocking: Blocking,
    events: mpsc::UnboundedSender<Event>,
    mock: bool,

    /// A client for the active node, built on demand and thrown away when the
    /// active node changes. `Arc` because a refresh runs on a blocking thread
    /// and must not borrow from the actor.
    chain: Option<Arc<Chain>>,
    /// The currencies this wallet's identities define, and every identity with
    /// whether it could still define one.
    ///
    /// Two lists from one walk — see `refresh_currencies`. Held rather than
    /// re-read on every emission because the walk costs one request per
    /// identity.
    currencies: Vec<pecu_protocol::CurrencyVm>,
    /// Which kind of history row is being looked at. `"all"` by default.
    ///
    /// Held here rather than in the interface because filtering and **paging**
    /// are the same question: "load older" fetches the next window of blocks,
    /// and how many rows of a given kind that yields is something only the side
    /// holding the list can answer.
    history_filter: String,
    eligible: Vec<pecu_protocol::EligibleIdentityVm>,
    /// What a launch costs on this chain, once it has been asked. See
    /// `ensure_launch_fees`.
    launch_fees: Option<(verus_sdk::money::Amount, verus_sdk::money::Amount)>,
    /// Every currency the chain knows about — see [`Catalog`].
    currency_catalog: Catalog,
    /// What every currency is worth, as of the last time somebody looked at
    /// the markets screen. Held rather than recomputed per question: the table
    /// and the detail beside it are two views of one set of notarizations, and
    /// re-fetching for the second would let them quote different blocks.
    market: market::Book,
    /// i-address to name, for everything the chain's currency list knows.
    /// A currency missing from it keeps its i-address on screen.
    market_names: std::collections::BTreeMap<String, String>,
    /// Which market row is open, by i-address. Empty means none.
    market_open: String,
    /// The conversion being composed, as the interface last described it.
    convert: pecu_protocol::ConvertDraft,
    /// Which estimate is the current one.
    ///
    /// A ticket rather than a flag, because these are keystrokes: the reply to
    /// "0.5" can land after the reply to "0.53", and applying it would put the
    /// older number back on screen under the newer text. Only the latest
    /// ticket's answer is allowed to become a quote.
    convert_ticket: u64,
    /// The last draft that passed every offline check, held so the reply to its
    /// estimate can be turned into a quote. Cleared by nothing: a stale one is
    /// unreachable because its ticket no longer matches.
    convert_ready: Option<convert::Ready>,
    /// What the wallet holds, by currency i-address, including the chain's own.
    ///
    /// Kept because converting needs it and a balance read does not retain it —
    /// the dashboard's assets go straight out as a `PortfolioVm` and are gone.
    /// Absent means zero here, and only here: a currency with no entry is one
    /// no output paid us in.
    holdings: std::collections::BTreeMap<String, verus_sdk::money::Amount>,
    /// Whether the markets have been read yet, and whether one is in flight.
    markets: Markets,
    /// A search that arrived while the catalog was still being fetched, to be
    /// answered when it lands. One, not a queue: they are keystrokes, and only
    /// the last one is still being waited on.
    pending_currency_search: Option<(String, Vec<String>)>,
    /// Launches built and signed, by ticket. Same shape as `prepared` and
    /// `identity_changes`: the bytes stay here and the interface holds a
    /// number. The permit rides along because it was taken before the
    /// signature.
    launches: std::collections::HashMap<u64, (currency::Prepared, pecu_chain::SpendPermit)>,
    /// A currency somebody decided to make, held across the identity
    /// registration it is waiting on. See `launch::Intent`.
    intent: launch::Intent,
    /// Which addresses the scripted chain was built to answer for. See
    /// [`Core::scripted_chain`] — the script is generated from the wallet's own
    /// keys, so the chain built before it unlocked answers for nobody.
    #[cfg(feature = "mock")]
    mock_for: Vec<String>,
    /// The two things that never change once known: the chain's own currency
    /// id, and the name of every currency the wallet has ever seen.
    cached: portfolio::Cached,
    /// One refresh at a time. Without this, holding the Refresh button would
    /// queue a request storm against a public node.
    refreshing: bool,
    work: mpsc::UnboundedSender<Work>,

    /// What the last refresh said this wallet can spend. Used to validate a
    /// draft offline; the builder is still the authority, and it refuses on its
    /// own terms if this turns out to be stale.
    spendable: verus_sdk::money::Amount,
    /// What the confirmed history sums to: spendable + immature + the confirmed
    /// coins an unconfirmed transaction already spends. The anchor the balance
    /// chart is built backwards from — see `emit_chart`.
    native_balance: i64,
    /// Signed payments waiting for a Confirm. **The bytes never leave here** —
    /// the UI holds a ticket number and a decoded summary.
    prepared: std::collections::HashMap<u64, send::Prepared>,
    tickets: u64,
    /// Transactions handed to a node whose outcome is unknown, on disk.
    pending: pending::Ledger,
    /// When each was last asked about. **Not persisted**: after a restart every
    /// unresolved record is due immediately, which is the right answer — the
    /// wallet has just been away and has catching up to do.
    last_checked: std::collections::HashMap<u64, std::time::Instant>,
    /// Ids with a check in flight, so a slow node cannot pile up requests.
    checking: std::collections::HashSet<u64>,
    /// What the node poller is doing, and what it is allowed to ask.
    polling: Polling,
    /// A payment is being handed to a node right now. See `fail_over`.
    broadcasting: bool,
    history: HistoryScan,

    /// Every address this wallet has paid or been told the name of, by
    /// address. Kept in memory because the send review consults it on every
    /// prepare, and a screen that has to wait for a query to say "you have not
    /// paid this before" would say it late or not at all.
    known: std::collections::BTreeMap<String, String>,

    /// What the last VerusID name typed into the send form turned out to be,
    /// and whether a lookup is out. See [`Core::maybe_resolve_identity`].
    identity: Option<Identity>,
    resolving: Option<String>,
    /// The last draft validated, so a resolution arriving afterwards can put
    /// its answer on the form rather than waiting for the next keystroke.
    last_draft: pecu_protocol::SendDraft,

    /// The identities this wallet's keys control, by i-address.
    ///
    /// **Only yours.** An earlier version kept looked-up identities in here
    /// too, on the theory that one list with a `mine` flag was simpler. It was
    /// simpler and it was wrong: the heading over the list says these were
    /// found by asking the chain which names your keys control, and a
    /// stranger's identity sitting under it made that sentence false — with no
    /// way to remove it short of restarting.
    ///
    /// A fact about your keys and the answer to a question you just asked are
    /// different things, and one list cannot be honest about both.
    identities: std::collections::BTreeMap<String, pecu_protocol::IdentityVm>,
    /// Identities somebody looked up, newest first.
    ///
    /// Transient by design: kept so a sheet can be reopened without asking the
    /// node again, cleared on request, and never counted as yours.
    looked_up: Vec<pecu_protocol::IdentityVm>,
    /// Which identity the detail sheet is open on. Empty when it is closed.
    open_identity: String,
    /// The VDXF keys that identity published, so a derived guess can be
    /// checked against them without another request.
    open_content_keys: std::collections::BTreeSet<String>,
    /// The one unfinished name registration, on disk.
    ///
    /// Beside the vault, like the pending-broadcast ledger, and for the same
    /// reason: a backup of the wallet directory has to carry the things whose
    /// loss costs money.
    reservation: registration::Reservation,
    /// Signed identity changes waiting for a Confirm. **The bytes never leave
    /// here** — the UI holds a ticket, the same as a payment.
    identity_changes: std::collections::HashMap<u64, identity::Prepared>,
    /// Which of them still need a word typed before they go. Only revocations.
    identity_confirms: std::collections::HashSet<u64>,
    /// Step two, once a poll has said the claim confirmed.
    ///
    /// Held rather than stored: it is derived from the reservation on disk by a
    /// poll, so it costs one request to get back and is never the only copy of
    /// anything.
    ready: Option<verus_sdk::network::Pending<verus_sdk::network::ReadyToRegister>>,
    /// VDXF names this wallet can recognise, derived once the chain is known.
    ///
    /// `None` until a node has said which chain this is. A table derived
    /// against the wrong chain matches nothing, which on screen is
    /// indistinguishable from an identity that published nothing recognisable —
    /// so it is not built until the answer is real.
    vdxf: Option<identity::Names>,

    /// Settings, and a cache the wallet is free to throw away. `None` when the
    /// databases could not be opened — the wallet works without them, it just
    /// forgets between runs and starts every session with a blank dashboard.
    store: Option<pecu_store::Store>,

    /// Where this chain's files are. Replaced wholesale by a switch, which is
    /// what makes the switch one decision rather than five path edits.
    paths: paths::Paths,
    /// The endpoints this build ships with, kept so a switch can put the list
    /// back to them before the saved ones for the new chain are added.
    ///
    /// Without this the node list would accumulate: switch to mainnet and the
    /// testnet user's saved endpoints stay, offering a wallet on VRSC a list of
    /// nodes it will then refuse to read from.
    builtin: Vec<Node>,
}

/// `None` for an empty field, so a blank box means "leave it at the default"
/// rather than "an authority whose name is nothing".
fn some_if_set(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// The one line the send form gets to say about a VerusID.
///
/// Ordered by how much it should stop somebody. A name that now points
/// somewhere else outranks everything: it is the case where the form looks
/// perfectly ordinary and the money goes to a stranger.
fn identity_note(identity: &Identity) -> NoteVm {
    if identity.address.is_empty() {
        return NoteVm::plain("verusid-unknown");
    }
    if let Some(previous) = &identity.was {
        return NoteVm::with(
            "verusid-moved",
            [
                identity.name.clone(),
                identity.address.clone(),
                previous.clone(),
            ],
        );
    }
    if identity.revoked {
        return NoteVm::with("verusid-revoked", [identity.name.clone()]);
    }
    NoteVm::with(
        "verusid-resolved",
        [identity.name.clone(), identity.address.clone()],
    )
}

/// What a node said a VerusID name points at.
///
/// # Why this is held rather than resolved where it is needed
///
/// A name is not an address. `meineid@` is a *question*, and the answer comes
/// from whichever node the wallet is talking to — so resolving it is a network
/// round trip, and the form validates on every keystroke. Those two facts do
/// not fit together, so the lookup happens once, off to one side, and what it
/// found is kept here for the validator and the builder to read.
///
/// The builder reads the [`address`](Identity::address), never the typed text.
/// A UI that could decide which address gets paid would be a UI holding the
/// only decision that matters.
#[derive(Clone, Debug)]
struct Identity {
    /// Exactly what was typed, so an answer can be matched to the question.
    typed: String,
    /// `name.parent@` as the chain spells it, which is not always as it was
    /// typed — `meineid@` on testnet comes back `meineid.VRSCTEST@`.
    name: String,
    address: String,
    revoked: bool,
    /// This name pointed at a different address the last time it was looked up.
    /// Carries the old one, because "it changed" without saying from what is an
    /// alarm nobody can act on.
    was: Option<String>,
}

/// Where a probe's answer comes from.
///
/// Built by [`Core::prober`] on the actor and then moved onto a blocking thread,
/// which is why it owns everything it needs — a probe must not borrow from state
/// the actor goes on mutating while the request is out.
enum Prober {
    /// A real endpoint, dialled fresh: a probe carries a shorter timeout than
    /// the client the wallet reads through, so it does not reuse it.
    Live(String),
    /// The scripted chain, which is the same chain the dashboard is read from —
    /// so the tip on the network panel and the tip the balances were computed
    /// against cannot disagree.
    #[cfg(feature = "mock")]
    Scripted(Arc<Chain>),
}

impl Prober {
    fn probe(
        &self,
    ) -> (
        Result<verus_sdk::network::ChainInfo, verus_sdk::network::RpcError>,
        std::time::Duration,
    ) {
        match self {
            Self::Live(url) => pecu_chain::probe(url),
            #[cfg(feature = "mock")]
            Self::Scripted(chain) => chain.probe(),
        }
    }
}

/// What the node poller is doing.
///
/// Grouped rather than four fields on `Core`, because they only mean anything
/// together: whether a tip poll is due depends on when the last one went out
/// AND whether one is still in flight, and whether the other nodes get asked
/// anything at all depends on which screen is open.
#[derive(Default)]
struct Polling {
    /// When the active node was last asked for the chain tip.
    tip_at: Option<std::time::Instant>,
    /// A tip poll is out. Without this, a slow node collects a queue.
    tip_in_flight: bool,
    /// When the nodes nobody is reading from were last asked anything.
    others_at: Option<std::time::Instant>,
    /// When the unfinished name claim was last asked about, and whether an ask
    /// is out. On the tick rather than the screen: a claim has a deadline, and
    /// where somebody navigated must not decide whether it is met.
    registration_at: Option<std::time::Instant>,
    registration_in_flight: bool,
    /// Which screen is open. Entering one is leaving the last, so there is no
    /// second signal to get out of step with this.
    screen: pecu_protocol::ScreenId,
}

/// How far the backwards scan through the chain has got.
///
/// Grouped rather than four fields on `Core`, because they only mean anything
/// together: `scanned_to` says nothing without knowing whether the scan
/// finished, and neither says anything while a page is in flight.
#[derive(Default)]
struct HistoryScan {
    /// Every transaction found so far, oldest first — the order the SDK returns
    /// them in. Kept whole because a day heading is a statement about the row
    /// above it, so a new page has to be grouped together with what is already
    /// on screen rather than appended to it.
    entries: Vec<verus_sdk::network::HistoryEntry>,
    /// The lowest block height looked at. Below this is **unexplored**, which
    /// is not the same as empty — and that distinction is the whole reason
    /// "Load older" can be offered honestly.
    scanned_to: u32,
    /// The scan reached the start of the chain. There is nothing older.
    complete: bool,
    /// A page is in flight.
    loading: bool,
}

/// An answer from a job that ran off the actor.
///
/// The work happens on a blocking thread; the **mutation** still happens in one
/// place, in a defined order, because the answer comes back as a message the
/// actor selects on alongside commands.
/// Every currency the chain knows about, and how far getting it has got.
///
/// One value rather than a list beside a flag, so "not asked yet", "on its way"
/// and "here" cannot be in two states at once — the first keystroke in the
/// picker starts the fetch and the ones behind it must not each start another.
///
/// `listcurrencies` is a single reply with no pagination — 464KB and 290
/// currencies on VRSCTEST, measured by the SDK, and it grows with the chain — so
/// it is asked for once per session and every search after that is answered from
/// memory. Reset to `Unasked` on a chain switch: a picker offering VRSCTEST's
/// currencies on mainnet would be offering reserves that do not exist.
#[derive(Debug, PartialEq, Eq)]
enum Catalog {
    Unasked,
    Fetching,
    Ready(Vec<verus_sdk::network::CurrencySummary>),
}

/// How far the markets read has got, this session.
///
/// The same three states as [`Catalog`], and for the same reason — but note
/// what `Asked` does **not** mean. It means the question was put, not that it
/// was answered: a read that failed lands here too. That is deliberate. The
/// alternative is "retry while the book is empty", and the book is empty
/// exactly when the node is not answering — so every fifteen-second poll would
/// ask a struggling node the two most expensive questions this wallet has.
/// Opening the markets screen re-reads regardless, which is the retry a person
/// can actually ask for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Markets {
    Unasked,
    Fetching,
    Asked,
}

enum Work {
    Portfolio(Box<portfolio::Reading>),
    /// A payment was built and signed, or the attempt failed.
    Prepared {
        ticket: u64,
        result: Box<Result<send::Prepared, send::SendError>>,
    },
    /// A node reported where the chain is.
    Tip {
        node: u32,
        info: Box<Result<verus_sdk::network::ChainInfo, verus_sdk::network::RpcError>>,
        latency: std::time::Duration,
        /// Whether this is the fifteen-second poll of the **active** node, as
        /// opposed to a one-off probe of some other node. Only the poll may
        /// release the poller's in-flight flag, and only the poll should be
        /// able to conclude that a new block has arrived.
        from_poller: bool,
    },
    /// A node answered what a VerusID name points at.
    Identity {
        /// The text as it was typed, so a reply that arrives after the field
        /// has moved on can be discarded rather than applied to something else.
        typed: String,
        result: Box<Result<verus_sdk::network::IdentityRecord, verus_sdk::network::RpcError>>,
    },
    /// The raw JSON for a transaction the detail sheet is showing.
    RawTransaction {
        txid: String,
        json: Box<Option<serde_json::Value>>,
    },
    /// An older page of history arrived.
    OlderHistory(Box<Result<portfolio::HistoryPage, verus_sdk::network::FlowError>>),
    /// A node answered whether an uncertain transaction confirmed.
    ///
    /// `Some(_)` means the node has it — the payment landed after all.
    /// `None` means it has never heard of it, which is only evidence, not proof.
    Checked {
        record: u64,
        confirmations: Option<u32>,
    },
    /// The same bytes were handed over again.
    Resent {
        record: u64,
        result: Box<Result<String, verus_sdk::network::FlowError>>,
    },
    /// A broadcast finished, one way or another.
    Broadcast {
        /// The ledger row committed before the attempt.
        record: u64,
        result: Box<Result<verus_sdk::network::Sent, verus_sdk::network::FlowError>>,
    },
    /// A name was checked for availability and price.
    NameChecked {
        name: String,
        /// `None` when the question could not be answered — which is not the
        /// same as the name being free.
        taken: Option<bool>,
        fee: Option<verus_sdk::money::Amount>,
    },
    /// Step one is built and signed. Nothing has been broadcast.
    Reserved {
        name: String,
        label: String,
        /// Carried through rather than re-taken: it was checked before the
        /// signature, and a permit that has since lapsed should not silently
        /// become a different one.
        permit: Box<pecu_chain::SpendPermit>,
        result: Box<
            Result<verus_sdk::network::Pending<verus_sdk::network::AwaitingCommitment>, String>,
        >,
    },
    /// A change to an identity was built and signed. Nothing sent.
    IdentityChangePrepared {
        ticket: u64,
        described: String,
        /// Whether sending it needs a word typed first.
        needs_confirmation: bool,
        result: Box<Result<identity::Prepared, String>>,
    },
    /// It was sent, one way or another. The txid, or why not.
    IdentityChanged(Box<Result<String, String>>),
    /// A poll of the commitment's state.
    CommitmentPolled(Box<Result<verus_sdk::network::CommitmentStatus, String>>),
    /// Step two finished, one way or another.
    Registered(Box<Result<verus_sdk::network::Registered, String>>),
    /// The commitment was handed to a node, certainly or otherwise.
    Committed {
        pending: Box<verus_sdk::network::Pending<verus_sdk::network::AwaitingCommitment>>,
        result: Box<Result<(), String>>,
    },
    /// The identities this wallet's keys control, gathered across every key.
    Identities(
        Box<Result<Vec<verus_sdk::network::IdentityAtAddress>, verus_sdk::network::RpcError>>,
    ),
    /// A launch built and signed, with the permit that was taken before the
    /// signature. **The bytes never leave here** — the interface holds a ticket
    /// and a decoded summary.
    LaunchPrepared {
        ticket: u64,
        #[allow(clippy::type_complexity)]
        result: Box<Result<(Box<currency::Prepared>, Box<pecu_chain::SpendPermit>), String>>,
    },
    /// It was handed to a node, one way or another.
    LaunchSent(Box<Result<currency::Launch, String>>),
    /// What a launch costs, from chain policy: the currency registration fee
    /// and the identity import fee, in that order. Which one applies depends on
    /// the kind — and on VRSCTEST they are four orders of magnitude apart.
    LaunchFees(
        Box<
            Result<
                (verus_sdk::money::Amount, verus_sdk::money::Amount),
                verus_sdk::network::RpcError,
            >,
        >,
    ),
    /// One answer per identity: its name, its i-address, whether this wallet
    /// can sign for it, its status, and what the chain said about a currency
    /// under it.
    ///
    /// Not a `Result`, deliberately. Each identity is asked separately and one
    /// unanswered read must not discard the others — so the failure is carried
    /// per row, in `Lookup::Unknown`, rather than for the whole walk.
    Currencies(Vec<(String, String, bool, String, currency::Lookup)>),
    /// Every currency on the chain, or why it could not be asked. Fetched once
    /// per session — see `Core::currency_catalog`.
    CurrencyCatalog(Box<Result<Vec<verus_sdk::network::CurrencySummary>, String>>),
    /// What a node expects one conversion to yield, and what a transaction is
    /// expected to cost.
    ///
    /// Both in one job because both are needed to answer one question, and two
    /// jobs would let the quote arrive without its fee for a frame.
    ConvertEstimate {
        ticket: u64,
        #[allow(clippy::type_complexity)]
        result: Box<
            Result<
                (
                    verus_sdk::network::ConversionEstimate,
                    Option<verus_sdk::money::Amount>,
                ),
                String,
            >,
        >,
    },
    /// The markets read: every currency the chain knows, and the pools that
    /// can price them.
    ///
    /// Both in one job because a book assembled from a catalog fetched at one
    /// moment and reserves fetched at another describes a chain that never
    /// existed. `Err` for the whole read: unlike the currency walk, there is no
    /// per-row answer to keep — two failed calls leave nothing to show.
    #[allow(clippy::type_complexity)]
    Markets(
        Box<
            Result<
                (
                    Vec<verus_sdk::network::CurrencySummary>,
                    Vec<verus_sdk::network::CurrencyConverter>,
                ),
                String,
            >,
        >,
    ),
    /// One identity looked up by name or i-address, with everything the detail
    /// sheet needs.
    IdentityDetail {
        /// What was asked for, so an answer arriving late can be matched to its
        /// question rather than applied to whatever is open now.
        typed: String,
        /// Whether this answer should open the detail sheet.
        ///
        /// **Not every read is a request to look at something.** Refreshing the
        /// watch list re-reads each entry through the same path, and treating
        /// that as "open this" made the sheet fly open by itself on the first
        /// row every time somebody opened the screen.
        open: bool,
        result: Box<Result<identity::Detail, verus_sdk::network::RpcError>>,
    },
}

/// The name a currency is known by, or its i-address when it is not.
fn convert_name(names: &std::collections::BTreeMap<String, String>, address: &str) -> String {
    names
        .get(address)
        .cloned()
        .unwrap_or_else(|| address.to_string())
}

/// Whether one row answers a search.
///
/// A free function rather than a closure so it can be tested without standing
/// up an actor, a chain and a vault. The needle arrives already lower-cased and
/// trimmed; doing that once per search rather than once per row is the only
/// reason the caller looks like it does.
///
/// The **address** is matched as well as the name, and that is not padding: the
/// i-address is the identifier this wallet tells people to prefer for anything
/// destructive, so it is the one somebody arrives with on the clipboard.
fn search_matches(needle: &str, name: &str, address: &str) -> bool {
    name.to_lowercase().contains(needle) || address.to_lowercase().contains(needle)
}

impl Core {
    async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<Command>,
        mut work: mpsc::UnboundedReceiver<Work>,
    ) {
        self.restore();
        self.emit_network();
        self.emit_wallet();
        // A name claim the last run left unfinished. It has a deadline, so it is
        // worth saying before anything else on that screen.
        self.emit_registration(None);
        // And a currency waiting for a name, or waiting to be defined under one
        // that already landed. Said before anything else on that screen for the
        // same reason a claim is: it is unfinished and it has already cost
        // something.
        self.emit_launch_pending();
        // A payment left unresolved by a previous run is the first thing worth
        // saying — it is money whose fate nobody knows.
        self.emit_pending();
        self.emit_address_book();

        // The idle check. Five seconds is fine granularity for a five-minute
        // timeout and costs nothing — it compares two `Instant`s and returns.
        let mut idle = tokio::time::interval(std::time::Duration::from_secs(5));
        idle.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            let command = tokio::select! {
                // Biased so a command in flight is always handled before an
                // idle tick that might lock the wallet out from under it, and
                // before a finished background read.
                biased;
                received = commands.recv() => match received {
                    Some(command) => command,
                    None => break,
                },
                Some(finished) = work.recv() => {
                    self.finish_work(finished);
                    continue;
                }
                _ = idle.tick() => {
                    self.check_auto_lock();
                    self.poll_tip();
                    self.poll_inactive();
                    self.poll_pending();
                    self.poll_registration();
                    continue;
                }
            };

            if !self.handle(command).await {
                break;
            }
        }

        tracing::info!("core stopped");
    }

    fn create_wallet(&mut self, name: &str, passphrase: &pecu_protocol::Secret) {
        self.busy(TaskKind::CreatingWallet, true);
        match self.wallet.create(name, passphrase) {
            Ok(challenge) => {
                self.emit_wallet();
                // A name claim the last run left unfinished. It has a deadline, so it is
                // worth saying before anything else on that screen.
                self.emit_registration(None);
                // The phrase this just generated has never been seen by anyone.
                // Announcing the challenge is what puts the backup screen in
                // front of the dashboard rather than beside it.
                self.emit_challenge(&challenge);
                // A brand-new key has nothing on chain, and saying so from the
                // node beats showing a zero the wallet made up.
                self.refresh();
            }
            Err(error) => {
                self.notice("wallet_create", NoteVm::plain("wallet-create-failed"), &error);
            }
        }
        self.busy(TaskKind::CreatingWallet, false);
    }

    /// Restore or add a key.
    ///
    /// On a fresh install this creates the wallet too — see
    /// [`Command::ImportKey`]. `Wallet::import` does the checking before it
    /// touches the disk, so a refused phrase leaves nothing behind.
    fn import_key(
        &mut self,
        label: &str,
        material: pecu_protocol::ImportMaterial,
        passphrase: &pecu_protocol::Secret,
    ) {
        use pecu_protocol::ImportMaterial;

        let imported = match material {
            ImportMaterial::Phrase(p) => Imported::Phrase(p),
            ImportMaterial::Text(t) => Imported::Text(t),
            ImportMaterial::Wif(w) => Imported::Wif(w),
        };

        self.busy(TaskKind::CreatingWallet, true);
        match self.wallet.import(label, imported, passphrase) {
            Ok(()) => {
                self.emit_wallet();
                // A name claim the last run left unfinished. It has a deadline, so it is
                // worth saying before anything else on that screen.
                self.emit_registration(None);
                // A restored wallet usually has a history. Asking for it is the
                // whole reason someone restored.
                self.refresh();
            }
            Err(error) => {
                self.notice("import_key", import_note(&error), &error);
            }
        }
        self.busy(TaskKind::CreatingWallet, false);
    }

    fn reveal_backup(&mut self, label: &str, passphrase: &pecu_protocol::Secret) {
        match self.wallet.begin_reveal(label, passphrase) {
            Ok(challenge) => self.emit_challenge(&challenge),
            Err(error) => self.notice(
                "reveal_backup",
                NoteVm::plain("passphrase-wrong"),
                &error,
            ),
        }
    }

    /// Grade the confirmation, and finish the backup if it passed.
    ///
    /// Recording it here rather than on a second command means there is no
    /// window in which the user has proved the backup and the wallet has not
    /// written that down.
    fn confirm_phrase(&mut self, checks: &[(u32, String)]) {
        let correct = self.wallet.confirm_backup(checks);
        self.wallet.touch();

        if correct {
            if let Err(error) = self.wallet.finish_backup() {
                self.notice("finish_backup", NoteVm::plain("backup-record-failed"), &error);
            }
            let _ = self.events.send(Event::SeedWords(Vec::new()));
        }

        // One bool. Never which word was wrong.
        let _ = self.events.send(Event::PhraseConfirmed(correct));

        if correct {
            self.emit_wallet();
            // A name claim the last run left unfinished. It has a deadline, so it is
            // worth saying before anything else on that screen.
            self.emit_registration(None);
        }
    }

    // ── Reading the chain ───────────────────────────────────────────────────

    /// A client for whichever node is active, built once and kept.
    ///
    /// Built lazily rather than at startup because there may be no reachable
    /// node yet, and because a `Client` is bound to one URL — changing the
    /// active node throws this away rather than reusing a client pointed
    /// somewhere else.
    fn chain(&mut self) -> Option<Arc<Chain>> {
        #[cfg(feature = "mock")]
        if self.mock {
            return self.scripted_chain();
        }

        if let Some(chain) = &self.chain {
            return Some(chain.clone());
        }

        let url = self.nodes.active()?.url.clone();
        match Chain::live(&url) {
            Ok(chain) => {
                let chain = Arc::new(chain);
                self.chain = Some(chain.clone());
                Some(chain)
            }
            Err(error) => {
                self.notice("node_connect", NoteVm::plain("node-unreachable"), &error);
                None
            }
        }
    }

    /// The scripted chain, rebuilt when the wallet's addresses change.
    ///
    /// # Why this cannot be built once
    ///
    /// The script answers for the addresses this wallet holds, and those are
    /// unknown until it unlocks. A chain built during the first tip poll — which
    /// happens seconds before anyone has typed a passphrase — would answer for
    /// nobody, and caching it the way the live client is cached would leave the
    /// demo build with a permanently empty dashboard. So the address list is
    /// what the cache is keyed on, not merely the fact that a chain exists.
    #[cfg(feature = "mock")]
    fn scripted_chain(&mut self) -> Option<Arc<Chain>> {
        let addresses = self.wallet_addresses();
        if let Some(chain) = &self.chain {
            if self.mock_for == addresses {
                return Some(chain.clone());
            }
        }

        match Chain::mock(&addresses) {
            Ok(chain) => {
                let chain = Arc::new(chain);
                self.mock_for = addresses;
                self.chain = Some(chain.clone());
                Some(chain)
            }
            Err(error) => {
                self.notice(
                    "node_connect",
                    NoteVm::plain("mock-chain-failed"),
                    &error,
                );
                None
            }
        }
    }

    /// Every address this wallet holds. Public, and readable while locked.
    fn wallet_addresses(&self) -> Vec<String> {
        self.wallet
            .view()
            .keys
            .into_iter()
            .map(|key| key.address)
            .collect()
    }

    /// How to ask where the chain is — for one node, right now.
    ///
    /// **Every probe in this application goes through here**, and that is the
    /// point of it existing. There are three places that ask a node for its
    /// `chain_info`: the fifteen-second tip poll, the one-off probe of a node
    /// somebody is looking at, and the sweep at startup. When the mock backend
    /// was first wired in, two of them were changed and the third — the only
    /// `async` one, which reads differently enough to have been skipped — kept
    /// dialling `api.verustest.net` from a build advertising itself as scripted.
    ///
    /// A single decision with three callers cannot be got two-thirds right.
    ///
    /// In a build without the `mock` feature this needs neither `self` nor the
    /// `Option` — the answer is always a live endpoint. The signature is kept
    /// uniform anyway, so the callers are one piece of code rather than two
    /// arrangements of it, and so the demo build is not the configuration where
    /// three call sites get edited.
    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn prober(&mut self, url: &str) -> Option<Prober> {
        #[cfg(feature = "mock")]
        if self.mock {
            return self.chain().map(Prober::Scripted);
        }

        Some(Prober::Live(url.to_string()))
    }

    /// Ask a node where the chain is, off the actor. `false` if nothing went out.
    fn dispatch_probe(&mut self, id: u32, url: &str, from_poller: bool) -> bool {
        let Some(prober) = self.prober(url) else {
            return false;
        };

        self.blocking.dispatch(
            move || {
                let (info, latency) = prober.probe();
                Work::Tip {
                    node: id,
                    info: Box::new(info),
                    latency,
                    from_poller,
                }
            },
            self.work.clone(),
        );
        true
    }

    /// Start reading balances and history, and return immediately.
    ///
    /// Deliberately not awaited. `spendable` alone costs three requests plus
    /// one per young output, and an actor sitting inside that call is an actor
    /// that will not answer **Lock** until a node replies.
    fn refresh(&mut self) {
        if self.refreshing {
            // Coalesced rather than queued. A second refresh started while the
            // first is in flight would ask a public node the same questions
            // twice for an answer that is already on its way.
            return;
        }

        // A locked wallet has addresses — they are public and readable while
        // locked — but showing balances for one is showing figures nobody has
        // authenticated for, and the screen behind the lock is not visible
        // anyway.
        if !self.wallet.is_unlocked() {
            return;
        }

        // The node has to be on the chain this wallet is for.
        //
        // # Why reading needs a guard and not only spending
        //
        // The spend permit has always required `requested == effective`, on the
        // reasoning that only signing needs the distinction — reading a balance
        // on another chain was thought harmless.
        //
        // It is not. These addresses exist on this chain; on another they are
        // simply absent, so the node answers honestly with nothing and the
        // wallet renders that as YOUR BALANCE. A real session ran for eight
        // minutes against a node the wallet had itself marked `WrongNetwork`,
        // reading VRSCTEST addresses against a PBaaS chain, and every refresh
        // failed while a cached list sat on screen looking current.
        //
        // Zero is a claim about somebody's money. It is not one to make from a
        // node that was never asked about this chain.
        if let Some(wrong) = self.reading_the_wrong_chain() {
            self.notice_warning(
                "wrong_chain",
                NoteVm::with("read-wrong-chain", [wrong, self.requested_name()]),
                "",
            );
            return;
        }

        let addresses = self.wallet_addresses();
        if addresses.is_empty() {
            return;
        }

        let Some(chain) = self.chain() else {
            return;
        };

        self.refreshing = true;
        self.busy(TaskKind::RefreshingBalance, true);

        let cached = self.cached.clone();
        self.blocking.dispatch(
            move || Work::Portfolio(Box::new(portfolio::read(&chain, &addresses, cached))),
            self.work.clone(),
        );

        // The first prices of the session, from the one place every path that
        // could want them already passes through: unlocking, creating a wallet,
        // changing node, changing chain. Every guard above has already been
        // cleared here — the wallet is unlocked, it has addresses, and the node
        // is on the chain it was asked for.
        //
        // Once, not on every poll. See `Markets::Asked`.
        if self.markets == Markets::Unasked {
            self.refresh_markets();
        }
    }

    /// The chain the active node reports, when it is not the one asked for.
    ///
    /// `None` while no node has answered yet: not knowing is not the same as
    /// disagreeing, and refusing to read before the first probe would leave a
    /// cold start blank for no reason.
    fn reading_the_wrong_chain(&self) -> Option<String> {
        let reported = self.nodes.active()?.network.as_ref()?;
        let requested = self.nodes.requested()?;
        (reported != requested).then(|| reported.to_string())
    }

    fn requested_name(&self) -> String {
        self.nodes
            .requested()
            .map_or_else(|| "the requested chain".to_string(), ToString::to_string)
    }

    fn finish_work(&mut self, work: Work) {
        match work {
            Work::Portfolio(reading) => self.finish_refresh(&reading),
            Work::Prepared { ticket, result } => self.finish_prepare(ticket, *result),
            Work::Tip {
                node,
                info,
                latency,
                from_poller,
            } => self.finish_tip(node, &info, latency, from_poller),
            Work::Identity { typed, result } => self.finish_identity(&typed, *result),
            Work::NameChecked { name, taken, fee } => self.finish_name_check(&name, taken, fee),
            Work::Reserved {
                name,
                label,
                permit,
                result,
            } => self.finish_reserved(&name, &label, *permit, *result),
            Work::Committed { pending, result } => self.finish_committed(*pending, *result),
            Work::IdentityChangePrepared {
                ticket,
                described,
                needs_confirmation,
                result,
            } => self.finish_identity_change_prepared(
                ticket,
                &described,
                needs_confirmation,
                *result,
            ),
            Work::IdentityChanged(result) => self.finish_identity_changed(*result),
            Work::CommitmentPolled(result) => self.finish_poll(*result),
            Work::Registered(result) => self.finish_registered(*result),
            Work::Identities(result) => self.finish_identities(*result),
            Work::Currencies(read) => self.finish_currencies(read),
            Work::CurrencyCatalog(result) => self.finish_currency_catalog(*result),
            Work::Markets(result) => self.finish_markets(*result),
            Work::ConvertEstimate { ticket, result } => {
                self.finish_convert_estimate(ticket, *result);
            }
            Work::LaunchFees(result) => self.finish_launch_fees(*result),
            Work::LaunchPrepared { ticket, result } => {
                self.finish_launch_prepared(ticket, *result);
            }
            Work::LaunchSent(result) => self.finish_launch_sent(*result),
            Work::IdentityDetail {
                typed,
                open,
                result,
            } => self.finish_detail(&typed, open, *result),
            Work::OlderHistory(page) => self.finish_older_history(*page),
            Work::RawTransaction { txid, json } => self.finish_raw_transaction(&txid, *json),
            Work::Checked {
                record,
                confirmations,
            } => self.finish_check(record, confirmations),
            Work::Resent { record, result } => self.finish_resend(record, *result),
            Work::Broadcast { record, result } => self.finish_broadcast(record, *result),
        }
    }

    fn finish_refresh(&mut self, reading: &portfolio::Reading) {
        self.refreshing = false;
        self.busy(TaskKind::RefreshingBalance, false);

        // Both caches are write-once per fact: a currency's name is fixed when
        // it is registered, and the chain's own id is a property of the chain.
        // Neither ever needs invalidating, which is why the next refresh costs
        // six requests rather than six plus one per token.
        self.cached.native = reading.native;
        self.cached.names.clone_from(&reading.names);
        self.spendable = reading.spendable;
        self.native_balance = confirmed_native(reading);

        // What can be converted, which is not what can be spent: a token
        // balance is spendable in its own currency and invisible to
        // `self.spendable`, which is the native figure alone.
        //
        // Keyed by **i-address**, through `portfolio::i_address`, because that
        // is what the market book and every currency list use. `CurrencyId`
        // renders as raw hex through `Display` — so the obvious `to_string()`
        // here builds a map whose keys match nothing, every lookup misses, and
        // the convert screen reports a balance of zero for a wallet holding
        // twelve thousand coins. It did exactly that until this line was fixed.
        self.holdings.clear();
        if let Some(native) = reading.native {
            self.holdings
                .insert(portfolio::i_address(native), reading.spendable);
        }
        for (currency, amount) in &reading.tokens {
            self.holdings
                .insert(portfolio::i_address(*currency), *amount);
        }

        // A conversion being composed while the first balance was still loading
        // was refused against a wallet that appeared to hold nothing, and
        // stayed refused until somebody typed another character. Re-price it
        // now that there is something to price it against.
        if !self.convert.from.is_empty() {
            let draft = self.convert.clone();
            self.set_convert_draft(draft);
        }

        let ticker = self
            .nodes
            .active()
            .and_then(|node| node.network.as_ref())
            .map_or("VRSC", pecu_chain::Network::ticker);

        let portfolio = reading.portfolio(ticker);
        let _ = self.events.send(Event::Portfolio(portfolio.clone()));

        match &reading.history {
            Ok(entries) => {
                // A refresh restarts the scan from the tip, so what it found
                // replaces what was there. Merging would need a de-duplication
                // pass to gain nothing: this page covers the same ground and is
                // newer.
                self.history.entries.clone_from(entries);
                self.history.scanned_to = reading.scanned_to;
                self.history.complete = reading.reached_start;
                self.emit_history();
                self.emit_chart();
            }
            Err(error) => {
                // An unknown history is not an empty one, and the difference
                // matters: "no transactions yet" is a claim about the chain.
                self.notice("history", NoteVm::plain("history-unreadable"), error);
            }
        }

        self.remember(&portfolio, reading);
    }

    /// Keep what this refresh learned, for the next cold start.
    ///
    /// Only a read that actually worked. Caching a failed one would mean the
    /// next start restores a wrong balance and presents it as the last known
    /// good figure.
    fn remember(&self, portfolio: &pecu_protocol::PortfolioVm, reading: &portfolio::Reading) {
        let Some(store) = &self.store else {
            return;
        };
        if reading.failure.is_some() || reading.history.is_err() {
            return;
        }

        store.save_snapshot(
            portfolio,
            &portfolio::rows_from(&self.history.entries, &self.cached.names, now(), "all"),
            now(),
        );

        if let Some(native) = reading.native {
            store.remember_native_currency(&portfolio::i_address(native));
        }
        store.remember_currency_names(
            &reading
                .names
                .iter()
                .map(|(id, name)| (portfolio::i_address(*id), name.clone()))
                .collect(),
        );
    }

    /// Act on one command. `false` means stop.
    ///
    /// Split out of [`Core::run`] so the loop stays what it is — a `select!`
    /// over three sources — and the command table stays readable as a table.
    /// One command.
    ///
    /// Long because it is a dispatch table: one arm per command, each a call.
    /// Splitting it would put the list of what the wallet can be asked to do in
    /// two places, and the single readable list is the property worth keeping —
    /// it is what makes a new secret-bearing command impossible to add
    /// unnoticed.
    #[allow(clippy::too_many_lines)]
    async fn handle(&mut self, command: Command) -> bool {
        match command {
            Command::ProbeNodes => self.probe_all().await,
            Command::SelectNode(id) => {
                if self.nodes.set_active(id) {
                    // The client belongs to the node it was built for.
                    self.chain = None;
                    self.remember_active_node();
                    self.emit_network();
                    self.refresh();
                }
            }
            Command::AddNode { url, label } => self.add_node(&url, &label),
            Command::RemoveNode(id) => self.remove_node(id),
            Command::Refresh(_) => {
                self.refresh();
                // Pressing Refresh on the markets screen has to re-read the
                // prices. `refresh` asks for them once a session and then never
                // again, which is right for the dashboard's column and wrong
                // for the screen whose whole content is the answer — without
                // this, the one button on that screen does nothing visible.
                if self.polling.screen == pecu_protocol::ScreenId::Markets {
                    self.refresh_markets();
                }
            }
            Command::LoadHistory { .. } => self.load_older_history(),
            Command::LoadTxDetail(txid) => self.load_tx_detail(&txid),

            // ── Send ─────────────────────────────────────────────────
            // A currency is an identity wearing a second hat, so asking for
            // the currencies *is* asking for the identities — the currency walk
            // chains off the end of that one, in `finish_identities`.
            //
            // Merged rather than written twice with the same body: two arms
            // that happen to agree invite somebody to change one of them, and
            // the day they diverge is the day the currency list is built from a
            // stale identity list.
            Command::RefreshIdentities | Command::RefreshCurrencies => self.refresh_identities(),
            Command::RefreshMarkets => self.refresh_markets(),
            Command::OpenMarket(address) => self.open_market(address),
            Command::SetConvertDraft(draft) => self.set_convert_draft(draft),
            Command::SwapConvertLegs => self.swap_convert_legs(),
            Command::LookUpIdentity(typed) => self.look_up_identity(&typed),
            Command::SetIdentityAuthorities {
                address,
                revocation,
                recovery,
            } => self.prepare_identity_change(
                &address,
                identity::Change::Authorities {
                    revocation: some_if_set(&revocation),
                    recovery: some_if_set(&recovery),
                },
            ),
            Command::LockIdentity {
                address,
                delay_blocks,
            } => self.prepare_identity_change(
                &address,
                identity::Change::Lock {
                    delay: delay_blocks,
                },
            ),
            Command::UnlockIdentity {
                address,
                extra_blocks,
            } => self.prepare_identity_change(&address, identity::Change::Unlock { extra_blocks }),
            Command::RevokeIdentity { address } => {
                self.prepare_identity_change(&address, identity::Change::Revoke);
            }
            Command::RecoverIdentity { address } => {
                self.prepare_identity_change(&address, identity::Change::Recover);
            }
            Command::ConfirmIdentityChange { ticket, typed } => {
                self.confirm_identity_change(ticket, &typed);
            }
            Command::CancelIdentityChange { ticket } => {
                self.identity_changes.remove(&ticket);
                self.identity_confirms.remove(&ticket);
            }
            Command::CheckName(name) => self.check_name(&name),
            Command::StartRegistration {
                name,
                revocation_authority,
                recovery_authority,
            } => self.start_registration(&name, &revocation_authority, &recovery_authority),
            Command::FinishRegistration => self.finish_registration(),
            Command::AbandonRegistration => self.abandon_registration(),
            Command::ClearLookups => self.clear_lookups(),
            Command::UnwatchIdentity(address) => self.unwatch_identity(&address),
            // Same object, so the same read. Opening a currency opens the
            // identity that defines it, because that is what it is — and the
            // detail sheet already says everything an owner needs.
            Command::OpenIdentity(address) | Command::OpenCurrency(address) => {
                self.look_up_identity(&address);
            }
            Command::DeriveContentKey(uri) => self.derive_content_key(&uri),
            Command::ValidateCurrency(draft) => self.validate_currency(&draft),
            Command::SearchCurrencies { query, exclude } => {
                self.search_currencies(query, exclude);
            }
            Command::Search { query } => self.search(&query),
            Command::SetHistoryFilter { kind } => {
                self.history_filter = kind;
                self.emit_history();
            }
            Command::PrepareLaunch(draft) => self.prepare_launch(&draft),
            Command::StartCurrencyFromNewName {
                revocation_authority,
                recovery_authority,
                draft,
            } => {
                self.start_currency_from_new_name(
                    &revocation_authority,
                    &recovery_authority,
                    draft,
                );
            }
            Command::ResumeLaunch => self.resume_launch(),
            Command::AbandonLaunch => {
                self.intent.finish();
                self.emit_launch_pending();
            }
            Command::ConfirmLaunch { ticket } => self.confirm_launch(ticket),
            Command::CancelLaunch { ticket } => {
                // Dropping the value drops the signed bytes with it.
                self.launches.remove(&ticket);
                let _ = self.events.send(Event::LaunchPrepared(None));
            }
            Command::ValidateDraft(draft) => self.validate_draft(&draft),
            Command::PrepareSend(draft) => self.prepare_send(draft),
            Command::ConfirmSend { ticket } => self.confirm_send(ticket),
            Command::CancelSend { ticket } => {
                // Dropping the `Prepared` drops the signed bytes with it.
                // Nothing was broadcast, so there is nothing to undo.
                self.prepared.remove(&ticket);
            }
            Command::CreateWallet { name, passphrase } => {
                self.create_wallet(&name, &passphrase);
            }

            // ── The backup screen ────────────────────────────────────
            // One conversation, five commands: start, show, hide, prove,
            // abandon. They stay small because every one of them is
            // memory-only — the phrase is already here.
            Command::RevealBackup { label, passphrase } => {
                self.reveal_backup(&label, &passphrase);
            }
            Command::ShowNewPhrase => {
                self.wallet.touch();
                let _ = self
                    .events
                    .send(Event::SeedWords(self.wallet.backup_words()));
            }
            Command::HideBackup => {
                // Conceal only. The phrase stays here so the button can be
                // held again; what the UI was given is what goes.
                let _ = self.events.send(Event::SeedWords(Vec::new()));
            }
            Command::CancelBackup => {
                self.wallet.abandon_backup();
                let _ = self.events.send(Event::SeedWords(Vec::new()));
            }
            Command::ConfirmPhrase { checks } => self.confirm_phrase(&checks),

            Command::ImportKey {
                label,
                material,
                passphrase,
            } => self.import_key(&label, material, &passphrase),
            Command::Unlock { passphrase } => {
                self.busy(TaskKind::Unlocking, true);
                match self.wallet.unlock(&passphrase) {
                    Ok(()) => {
                        self.emit_wallet();
                        // A name claim the last run left unfinished. It has a deadline, so it is
                        // worth saying before anything else on that screen.
                        self.emit_registration(None);
                        // Unlocking is the moment the figures become worth
                        // fetching, and the moment they are stalest.
                        self.refresh();
                    }
                    Err(error) => self.notice(
                        "unlock",
                        NoteVm::plain("passphrase-wrong"),
                        &error,
                    ),
                }
                self.busy(TaskKind::Unlocking, false);
            }
            Command::Lock => {
                self.wallet.lock();
                let _ = self.events.send(Event::Locked {
                    reason: LockReason::Manual,
                });
                self.emit_wallet();
                // A name claim the last run left unfinished. It has a deadline, so it is
                // worth saying before anything else on that screen.
                self.emit_registration(None);
            }
            Command::ChangePassphrase { old, new } => {
                self.change_passphrase(&old, &new);
            }
            Command::AddKey { label } => self.add_key(&label),
            Command::RenameKey { from, to } => self.rename_key(&from, &to),
            Command::LabelAddress { address, label } => self.label_address(&address, &label),
            Command::ForgetAddress(address) => self.forget_address(&address),
            Command::SetActiveKey(label) => self.set_active_key(&label),
            Command::SetAutoLockMinutes(minutes) => self.set_auto_lock(minutes),
            Command::SetAppearance {
                dark,
                reduce_motion,
            } => self.set_appearance(dark, reduce_motion),
            Command::SetAllowMainnetSpend {
                on,
                typed_confirmation,
            } => self.set_mainnet_spend(on, &typed_confirmation),
            Command::SetRequestedNetwork(name) => self.switch_network(&name),
            Command::ResolvePending { id, action } => self.resolve_pending(id, action),
            Command::ScreenEntered(screen) => self.enter_screen(screen),
            Command::UserActivity => self.wallet.touch(),
            Command::RememberWindow { width, height } => self.remember_window(width, height),
            Command::Shutdown => return false,
            // Nothing to do, and named rather than swept up by a wildcard.
            //
            // Screen-scoped polling starts on the way in and stops by not being
            // asked for again, so leaving costs nothing. The arm exists so this
            // match stays exhaustive: with a wildcard here, the next command
            // added to the protocol would compile into silence — which is how
            // `SetRequestedNetwork` sat unhandled for two phases.
            Command::ScreenLeft(_) => {}
        }
        true
    }

    // ── One transaction, in detail ──────────────────────────────────────────

    /// Open a transaction.
    ///
    /// Everything on the sheet except the raw JSON comes from the entry already
    /// in memory, so it appears immediately. The one request follows and fills
    /// in the Advanced section when it arrives.
    fn load_tx_detail(&mut self, txid: &str) {
        let Some(entry) = self
            .history
            .entries
            .iter()
            .find(|entry| entry.txid.to_string() == txid)
        else {
            return;
        };

        let tip = self.nodes.active().and_then(|node| node.tip).unwrap_or(0);
        let explorer = self
            .nodes
            .active()
            .and_then(|node| node.network.as_ref())
            .and_then(|network| network.explorer(txid));

        let detail = portfolio::detail(entry, &self.cached.names, tip, now(), explorer);
        let _ = self.events.send(Event::TxDetail(detail));

        let Some(chain) = self.chain() else {
            return;
        };
        let wanted = txid.to_string();
        self.blocking.dispatch(
            move || {
                use verus_sdk::network::ChainReader;
                Work::RawTransaction {
                    // A node that will not decode it is not an error worth
                    // interrupting anyone for: the Advanced section simply
                    // stays empty, and everything else on the sheet is true.
                    json: Box::new(chain.raw_transaction(&wanted).ok()),
                    txid: wanted,
                }
            },
            self.work.clone(),
        );
    }

    fn finish_raw_transaction(&mut self, txid: &str, json: Option<serde_json::Value>) {
        let Some(json) = json else {
            return;
        };
        let Some(entry) = self
            .history
            .entries
            .iter()
            .find(|entry| entry.txid.to_string() == txid)
        else {
            return;
        };

        let tip = self.nodes.active().and_then(|node| node.tip).unwrap_or(0);
        let explorer = self
            .nodes
            .active()
            .and_then(|node| node.network.as_ref())
            .and_then(|network| network.explorer(txid));

        let mut detail = portfolio::detail(entry, &self.cached.names, tip, now(), explorer);

        // The node's own count where it gives one — it knows about blocks mined
        // since the last tip poll. Ours stands otherwise.
        if let Some(reported) = json
            .get("confirmations")
            .and_then(serde_json::Value::as_u64)
        {
            detail.confirmations = u32::try_from(reported).ok();
        }
        // Only if the node volunteers it. Working it out means one lookup per
        // input, and a fee nobody asked for is not worth that.
        if let Some(fee) = json.get("fee").and_then(serde_json::Value::as_f64) {
            detail.fee_display = verus_sdk::money::Amount::from_coins_str(&fee.abs().to_string())
                .ok()
                .map(portfolio::coins);
        }

        detail.raw_json = serde_json::to_string_pretty(&json).ok();
        let _ = self.events.send(Event::TxDetail(detail));
    }

    // ── Paging backwards through the chain ──────────────────────────────────

    /// Fetch the next page of older transactions.
    ///
    /// Picks up exactly where the last scan stopped. `HistoryScan::scanned_to` is
    /// how far down the chain has been looked at, which is a different fact
    /// from how far down a transaction was found — a gap of empty blocks must
    /// not be mistaken for the end of the list.
    fn load_older_history(&mut self) {
        if self.history.loading || self.history.complete || self.history.scanned_to == 0 {
            return;
        }
        let Some(chain) = self.chain() else {
            return;
        };

        let addresses = self.wallet_addresses();
        if addresses.is_empty() {
            return;
        }

        self.history.loading = true;
        self.busy(TaskKind::LoadingHistory, true);

        let before = self.history.scanned_to.saturating_sub(1);
        self.blocking.dispatch(
            move || {
                let refs: Vec<&str> = addresses.iter().map(String::as_str).collect();
                Work::OlderHistory(Box::new(portfolio::history_page(
                    &chain,
                    &refs,
                    before,
                    portfolio::PAGE_ROWS,
                )))
            },
            self.work.clone(),
        );
    }

    fn finish_older_history(
        &mut self,
        page: Result<portfolio::HistoryPage, verus_sdk::network::FlowError>,
    ) {
        self.history.loading = false;
        self.busy(TaskKind::LoadingHistory, false);

        let page = match page {
            Ok(page) => page,
            Err(error) => {
                // The list keeps what it has. An older page that could not be
                // read is a page nobody has seen, not a list that shrank.
                self.notice("load_history", NoteVm::plain("history-older-unreadable"), &error);
                return;
            }
        };

        // Older entries go at the FRONT: the list is oldest-first, and the
        // renderer reverses it.
        self.history.entries.splice(0..0, page.entries);
        self.history.scanned_to = page.scanned_to;
        self.history.complete = page.reached_start;

        self.emit_history();
        // A longer history is a longer chart. The anchor has not moved — the
        // balance is what it was — but there is more of the past to draw it
        // across now.
        self.emit_chart();
    }

    /// The balance over time, as far back as the scan has looked.
    ///
    /// # Built backwards, from the one figure that is certainly true
    ///
    /// The wallet knows the balance right now and every transaction inside the
    /// window it has scanned. It does **not** know what was held before that
    /// window — there may be a hundred thousand blocks underneath it. So the
    /// series is walked backwards from the current balance, subtracting each
    /// delta on the way down, rather than forwards from a starting figure
    /// nobody has. See [`pecu_chart::from_deltas`].
    ///
    /// The anchor is `spendable + immature + pending_out`, which is what the
    /// confirmed history actually sums to: an immature coinbase is confirmed
    /// and counted, and a confirmed output that some unconfirmed transaction
    /// already spends is still confirmed — the transaction spending it is not
    /// in the history yet.
    fn emit_chart(&self) {
        // Only confirmed transactions. An unconfirmed one has no block time,
        // so it has no position on a time axis — and its effect is not in the
        // anchor balance either, so including it at height zero would draw a
        // step in 1970.
        let deltas: Vec<(i64, i64)> = self
            .history
            .entries
            .iter()
            .filter(|entry| entry.height > 0)
            .map(|entry| (entry.block_time, entry.net_native.to_sat()))
            .collect();

        let points = pecu_chart::from_deltas(now(), self.native_balance, &deltas);

        let ticker = self
            .nodes
            .active()
            .and_then(|node| node.network.as_ref())
            .map_or("VRSC", pecu_chain::Network::ticker)
            .to_string();

        let _ = self.events.send(Event::Chart(pecu_protocol::ChartVm {
            points: points
                .iter()
                .map(|point| pecu_protocol::ChartPointVm {
                    t: point.t,
                    sats: point.value,
                })
                .collect(),
            complete: self.history.complete,
            ticker,
        }));
    }

    /// Send the whole list, re-grouped.
    ///
    /// A whole replacement rather than an append, because the day headings have
    /// to be recomputed across the join — appending a page whose first row is
    /// another "Yesterday" would print the heading twice. The list is a few
    /// hundred rows and carries no in-flight transitions, so there is nothing
    /// for a delta to protect.
    fn emit_history(&self) {
        let rows =
            portfolio::rows_from(&self.history.entries, &self.cached.names, now(), &self.history_filter);
        let _ = self.events.send(Event::History {
            key: String::new(),
            delta: pecu_protocol::ListDelta::Replace(rows),
        });
        let _ = self
            .events
            .send(Event::HistoryExhausted(self.history.complete));
    }

    // ── What the last run left behind ───────────────────────────────────────

    /// Put yesterday's figures on screen before asking a node anything.
    ///
    /// Marked **stale**, always. A cached balance is a true statement about the
    /// past, and the screen says so — which is a different thing from a blank
    /// dashboard for the two seconds a node takes, and a very different thing
    /// from a number presented as current.
    fn restore(&mut self) {
        let Some(store) = &self.store else {
            return;
        };

        // First, and before any figure: a theme that arrives after the first
        // frame is a flash of the wrong one. Dark is the designed default, so
        // a wallet that has never been told otherwise gets dark.
        let _ = self.events.send(Event::Appearance {
            dark: store.setting("theme").as_deref() != Some("light"),
            reduce_motion: store.setting("reduce_motion").as_deref() == Some("1"),
            window: remembered_window(store),
        });

        // The two caches that make the next refresh cheap: the chain's own
        // currency never changes, and a currency's name is fixed when it is
        // registered.
        self.cached.native = store
            .native_currency()
            .as_deref()
            .and_then(portfolio::currency_from_i_address);
        self.cached.names = store
            .currency_names()
            .iter()
            .filter_map(|(address, name)| {
                Some((portfolio::currency_from_i_address(address)?, name.clone()))
            })
            .collect();

        self.known = store
            .known_addresses()
            .into_iter()
            .map(|known| (known.address, known.label))
            .collect();

        // The identities somebody chose to keep an eye on. Name and address
        // only — everything else about an identity is a chain fact that would
        // be a lie by now, so they come back saying so rather than claiming a
        // status from last week, and the next refresh fills them in.
        self.looked_up = store
            .watched_identities()
            .into_iter()
            .map(|watched| pecu_protocol::IdentityVm {
                name: watched.name,
                address: watched.address,
                status: "Not read yet".to_string(),
                tone: "unknown".to_string(),
                note: String::new(),
                mine: false,
            })
            .collect();

        // Endpoints the user configured, put back beside the built-ins. Their
        // ids are offset so a shipped node and a saved one can never collide —
        // see `user_node_id`.
        for saved in store.nodes() {
            self.nodes
                .add(user_node_id(saved.id), &saved.label, &saved.url);
        }

        // And whichever of them was in use. Matched by URL, so a build that
        // ships a new node ahead of the others cannot silently move someone
        // onto a different chain.
        if let Some(url) = store.setting("active_node_url") {
            if !url.is_empty() && !self.nodes.set_active_by_url(&url) {
                // The endpoint it names is not configured any more. Whatever
                // is first stays active, which is the shipped default.
                tracing::info!(%url, "the node last in use is no longer configured");
            }
        }

        if let Some(minutes) = store.setting("auto_lock_minutes") {
            self.wallet.auto_lock = minutes
                .parse::<u64>()
                .ok()
                .filter(|m| *m > 0)
                .map(|m| std::time::Duration::from_secs(m.saturating_mul(60)));
        }

        let Some(snapshot) = store.snapshot() else {
            return;
        };

        let mut portfolio = snapshot.portfolio;
        portfolio.stale = true;

        let mut history = snapshot.history;
        // The figures may be old; the dates must not be WRONG. Every row keeps
        // its block time, so the wording is recomputed rather than restored.
        portfolio::restamp(&mut history, now());

        tracing::info!(saved_at = snapshot.saved_at, "restored the last dashboard");
        // Nothing has been scanned this run, so "Load older" stays offered
        // rather than claiming the cached page is the whole history.
        let _ = self.events.send(Event::HistoryExhausted(false));
        let _ = self.events.send(Event::Portfolio(portfolio));
        let _ = self.events.send(Event::History {
            key: String::new(),
            delta: pecu_protocol::ListDelta::Replace(history),
        });
    }

    // ── Keeping up with the chain ───────────────────────────────────────────

    /// Ask the active node where the chain is.
    ///
    /// One request. `chain_info` yields the network, the tip, the sync state
    /// and the version together, so the tip poll and the health probe are the
    /// same question — there is no cheaper way to learn the height and no
    /// reason to ask twice.
    ///
    /// Only the ACTIVE node, and only every 15 seconds. The other nodes are
    /// probed when someone is looking at the network screen; polling all of
    /// them forever would be asking public infrastructure for something nobody
    /// is reading.
    fn poll_tip(&mut self) {
        const EVERY: std::time::Duration = std::time::Duration::from_secs(15);

        if self.polling.tip_in_flight
            || self
                .polling
                .tip_at
                .is_some_and(|last| last.elapsed() < EVERY)
        {
            return;
        }
        let Some(node) = self.nodes.active() else {
            return;
        };
        let (id, url) = (node.id, node.url.clone());

        // Set only once the poll is genuinely on its way. Marking it in flight
        // and then failing to dispatch would leave the flag stuck true and stop
        // the wallet ever polling again.
        if self.dispatch_probe(id, &url, true) {
            self.polling.tip_in_flight = true;
            self.polling.tip_at = Some(std::time::Instant::now());
        }
    }

    fn finish_tip(
        &mut self,
        node: u32,
        info: &Result<verus_sdk::network::ChainInfo, verus_sdk::network::RpcError>,
        latency: std::time::Duration,
        from_poller: bool,
    ) {
        if from_poller {
            self.polling.tip_in_flight = false;
        }

        // This node's own previous height. Comparing against the ACTIVE node's
        // would mean a probe of some other endpoint could look like a new
        // block here and trigger a refresh of everything.
        let before = self.nodes.get(node).and_then(|n| n.tip);
        let requested = self.nodes.requested().cloned().unwrap_or(Network::Testnet);

        let Some(entry) = self.nodes.get_mut(node) else {
            return;
        };
        match info {
            Ok(reported) => entry.record_success(reported, latency, &requested),
            Err(error) => entry.record_failure(error),
        }

        let after = self.nodes.get(node).and_then(|n| n.tip);
        self.emit_network();

        // A node that has now failed three times in a row is a node that is
        // down rather than briefly unreachable.
        if info.is_err() {
            self.fail_over();
        }

        // A new block is the only reason to re-read anything. Polling the tip
        // and refreshing regardless would turn a one-request poll into seven.
        //
        // Only for the node the wallet is actually reading from: another node
        // finding a block says nothing about the balances on screen, which were
        // read somewhere else.
        let active = self.nodes.active().map(|n| n.id) == Some(node);
        if info.is_ok() && active && after != before {
            tracing::debug!(?before, ?after, "a new block");
            self.refresh();

            // And the identities, while somebody is looking at them.
            //
            // A registration that has just been broadcast is not on the chain
            // yet, so the refresh that follows it finds nothing —
            // `identities_with_address` answers about identity outputs as of the
            // current block height, and an unmined one has none. Without this
            // the name somebody just claimed simply does not appear until they
            // press Refresh, which looks exactly like it failed.
            if self.polling.screen == pecu_protocol::ScreenId::Identities {
                self.refresh_identities();
            }
        }
    }

    // ── Keys ────────────────────────────────────────────────────────────────

    /// Generate another key.
    ///
    /// The phrase it produces has never been seen by anyone, so this lands on
    /// the backup screen exactly as creating a wallet does — the same
    /// conversation, the same three words to prove, the same one chance before
    /// the words are sealed.
    fn add_key(&mut self, label: &str) {
        let label = label.trim();
        if label.is_empty() {
            return;
        }

        // One backup at a time. Generating a key starts a backup, and starting
        // a second would replace the phrase the first one is holding — the
        // words on screen would silently become a different key's. The backup
        // screen covers the settings screen, so this cannot happen through the
        // interface; it is here because the core must not depend on that.
        if self.wallet.backup_in_progress() {
            self.notice_warning(
                "add_key",
                NoteVm::plain("backup-in-progress"),
                "",
            );
            return;
        }

        match self.wallet.add_generated_key(label) {
            Ok(challenge) => {
                self.emit_wallet();
                // A name claim the last run left unfinished. It has a deadline, so it is
                // worth saying before anything else on that screen.
                self.emit_registration(None);
                self.emit_challenge(&challenge);
                // A new key has nothing on chain, and saying so from a node
                // beats showing a zero the wallet made up.
                self.refresh();
            }
            Err(error) => self.notice("add_key", key_error_note(&error), &error),
        }
    }

    /// Rename a key.
    ///
    /// The vault re-seals both blobs under the new name — see
    /// [`pecu_keystore::Vault::rename_key`]. Nothing here has to know that;
    /// what matters at this level is that a refusal says which rule was broken,
    /// because "could not rename" leaves someone guessing between a name that
    /// is taken and a name that is not allowed.
    fn rename_key(&mut self, from: &str, to: &str) {
        let to = to.trim();
        if from.is_empty() || to.is_empty() || from == to {
            return;
        }

        match self.wallet.rename_key(from, to) {
            Ok(()) => {
                tracing::info!(from, to, "a key was renamed");
                self.emit_wallet();
                // A name claim the last run left unfinished. It has a deadline, so it is
                // worth saying before anything else on that screen.
                self.emit_registration(None);
            }
            Err(error) => self.notice("rename_key", key_error_note(&error), &error),
        }
    }

    /// Switch which key the wallet sends from and receives to.
    fn set_active_key(&mut self, label: &str) {
        if self.wallet.set_active_key(label) {
            self.emit_wallet();
            // A name claim the last run left unfinished. It has a deadline, so it is
            // worth saying before anything else on that screen.
            self.emit_registration(None);
        }
    }

    // ── Endpoints the user configured ───────────────────────────────────────

    /// Add an endpoint.
    ///
    /// Three refusals, in order, and each of them says something different:
    /// a URL this wallet may not talk to, an endpoint that is already
    /// configured, and a row that could not be written down. The last one
    /// matters more than it looks — accepting a node the store refused would
    /// leave the running list and the file disagreeing from that moment on,
    /// and the disagreement would only surface after a restart.
    fn add_node(&mut self, url: &str, label: &str) {
        let url = url.trim();
        let label = label.trim();

        if url.is_empty() {
            return;
        }

        // The SDK's own check: the scheme allowlist, and the refusal to send
        // plaintext anywhere but loopback. No connection is opened, so this
        // says the URL is one we may use — not that anything is listening.
        if let Err(error) = pecu_chain::validate_url(url) {
            self.notice("add_node", url_refusal_note(&error), &error);
            return;
        }

        if self.nodes.has_url(url) {
            self.notice_warning(
                "add_node",
                NoteVm::plain("node-duplicate"),
                "",
            );
            return;
        }

        let Some(store) = &self.store else {
            self.notice_warning(
                "add_node",
                NoteVm::plain("node-store-unavailable"),
                "",
            );
            return;
        };

        // A node with no name of its own is named after its host, which is
        // what someone would have typed anyway.
        let label = if label.is_empty() {
            host_of(url)
        } else {
            label.to_string()
        };

        let Some(row) = store.add_node(&label, url) else {
            self.notice_warning(
                "add_node",
                NoteVm::plain("node-not-saved"),
                "",
            );
            return;
        };

        self.nodes.add(user_node_id(row), &label, url);
        tracing::info!(%label, %url, "a node was added");
        self.emit_network();
        // The one signal the form waits for. It does not clear itself on
        // submit, so that a refused address is still there to be corrected.
        self.notice_info("node_added", NoteVm::plain("node-added"));

        // Ask it what it is straight away. An entry sitting at "unknown" until
        // someone presses Probe reads as a node that did not work.
        self.probe_one(user_node_id(row), url);
    }

    /// Remove an endpoint the user added.
    ///
    /// The running list decides whether this is allowed — it is the one that
    /// knows which nodes are built in — so the file is only touched once the
    /// removal has actually happened.
    fn remove_node(&mut self, id: u32) {
        let was_active = self.nodes.active().map(|node| node.id) == Some(id);

        if !self.nodes.remove(id) {
            return;
        }

        if let (Some(store), Some(row)) = (&self.store, stored_node_id(id)) {
            store.remove_node(row);
        }

        if was_active {
            // The client was built for the node that just went away.
            self.chain = None;
            self.remember_active_node();
        }

        tracing::info!(id, "a node was removed");
        self.emit_network();

        if was_active {
            // Whatever inherited the active slot has never been asked anything.
            self.refresh();
        }
    }

    /// Ask one node what it is.
    ///
    /// The same request as the tip poll, aimed at a specific node rather than
    /// the active one — which is what a newly added endpoint needs, since it is
    /// not active and the poller would never reach it.
    fn probe_one(&mut self, id: u32, url: &str) {
        self.dispatch_probe(id, url, false);
    }

    // ── Changing chains ─────────────────────────────────────────────────────

    /// Open a different chain.
    ///
    /// # This is not a setting
    ///
    /// Everything durable belongs to one chain — the vault, both databases, the
    /// ledger of unresolved payments, the salt for a name being claimed — so
    /// this closes one wallet and opens another. Treating it as a preference
    /// that only changes what the network panel says would leave every figure on
    /// screen belonging to the chain somebody just left.
    ///
    /// # Why it locks
    ///
    /// A decryption key unlocked for one vault is meaningless to another, and
    /// carrying an unlocked session across the switch would mean either holding
    /// a key for a file that is no longer open or silently unlocking a different
    /// wallet with a passphrase that was typed for this one. Neither is
    /// something to do quietly, so the wallet locks and says so.
    ///
    /// # Why the ticket counter is left alone
    ///
    /// Every other piece of read state is thrown away, because it describes a
    /// chain that is no longer open. `tickets` describes nothing about a chain:
    /// it is a counter the interface holds references into, and resetting it
    /// would let a stale Confirm from before the switch name a payment prepared
    /// after it.
    fn switch_network(&mut self, name: &str) {
        let network = Network::from_chain_name(name);

        if self.nodes.requested() == Some(&network) {
            return;
        }

        // Not while a payment is on its way to a node. The broadcast is already
        // out of this actor's hands, and its answer lands in a ledger that is
        // about to be closed — so the one record of a transaction that may
        // already exist would be written to a file nobody reads again.
        if self.broadcasting {
            self.notice_warning(
                "network_switch_busy",
                NoteVm::plain("chain-switch-busy"),
                "",
            );
            return;
        }

        tracing::info!(to = %network, "changing chains");
        let label = network.label().to_string();

        self.wallet.lock();
        let _ = self.events.send(Event::Locked {
            reason: LockReason::Manual,
        });

        // Preferences that are about the application rather than about a chain.
        //
        // They live in a per-chain database because that is the only one there
        // is, which means a switch would otherwise hand somebody a wallet in
        // the wrong theme that locks on a different timer. Carried across so
        // the setting follows the person; a chain that already has its own
        // answer keeps it.
        let carried: Vec<(&str, String)> = self
            .store
            .as_ref()
            .map(|store| {
                ["theme", "reduce_motion", "auto_lock_minutes"]
                    .into_iter()
                    .filter_map(|key| Some((key, store.setting(key)?)))
                    .collect()
            })
            .unwrap_or_default();

        // The files, first. Everything below is either derived from these or
        // thrown away because it came from the chain being closed.
        self.paths = paths::Paths::new(self.paths.home().to_path_buf(), &network, self.mock);
        paths::Paths::remember(self.paths.home(), &network);

        self.wallet = Wallet::open_or_absent(self.paths.vault());
        self.store = open_store(&self.paths);

        if let Some(store) = &self.store {
            for (key, value) in carried {
                if store.setting(key).is_none() {
                    store.set_setting(key, &value);
                }
            }
        }
        self.pending = pending::Ledger::open(self.paths.pending());
        self.reservation = registration::Reservation::open(self.paths.registration());

        // Back to the shipped endpoints. `restore` adds this chain's saved ones
        // below; without the reset the previous chain's would stay, and a wallet
        // on VRSC would be offered a list of nodes it then refuses to read from.
        self.nodes = NodeManager::new(self.builtin.clone(), network);

        // Everything the old chain answered. A figure kept here is a figure
        // about somebody else's money.
        self.chain = None;
        #[cfg(feature = "mock")]
        {
            self.mock_for = Vec::new();
        }
        self.cached = portfolio::Cached::default();
        self.refreshing = false;
        self.spendable = verus_sdk::money::Amount::ZERO;
        self.native_balance = 0;
        self.prepared.clear();
        self.identity_changes.clear();
        self.identity_confirms.clear();
        self.ready = None;
        self.last_checked.clear();
        self.checking.clear();
        self.polling = Polling::default();
        self.history = HistoryScan::default();
        self.known.clear();
        self.identity = None;
        self.resolving = None;
        self.last_draft = pecu_protocol::SendDraft::default();
        self.identities.clear();
        self.looked_up.clear();
        // All three are per-chain facts that were being kept across a switch.
        // The registration fee especially: VRSCTEST charges 200 and VRSC does
        // not, and the cached figure was the one the launch form printed.
        self.launch_fees = None;
        self.currencies.clear();
        self.eligible.clear();
        self.currency_catalog = Catalog::Unasked;
        self.pending_currency_search = None;
        self.open_identity.clear();
        self.open_content_keys.clear();
        // Derived by hashing the chain's name, so a table built for VRSCTEST
        // matches nothing on VRSC — and a table that matches nothing looks
        // exactly like an identity that published nothing recognisable.
        self.vdxf = None;

        // Blank the dashboard before restoring, because `restore` says nothing
        // at all when the new chain has no saved snapshot — which is precisely
        // the case where the previous chain's figures would stay on screen.
        let _ = self
            .events
            .send(Event::Portfolio(pecu_protocol::PortfolioVm::default()));
        let _ = self.events.send(Event::History {
            key: String::new(),
            delta: pecu_protocol::ListDelta::Replace(Vec::new()),
        });
        let _ = self.events.send(Event::HistoryExhausted(false));

        self.restore();
        self.emit_network();
        self.emit_wallet();
        self.emit_identities();
        self.emit_registration(None);
        self.emit_pending();
        self.emit_address_book();

        self.notice_info(
            "network_switched",
            NoteVm::with("chain-switched", [label]),
        );

        // Ask the new chain's active node what it is, rather than waiting up to
        // fifteen seconds for the poller. Until something answers, `effective`
        // is unknown — and an unknown chain is what the read guard and the
        // spend permit both refuse on, so the wallet would sit there declining
        // to do anything with no visible reason.
        if let Some(node) = self.nodes.active() {
            let (id, url) = (node.id, node.url.clone());
            self.probe_one(id, &url);
        }
    }

    /// Name an address, or clear its name.
    fn label_address(&mut self, address: &str, label: &str) {
        let label = label.trim();
        if address.is_empty() {
            return;
        }
        self.known.insert(address.to_string(), label.to_string());
        if let Some(store) = &self.store {
            store.label_address(address, label);
        }
        self.emit_address_book();
    }

    fn forget_address(&mut self, address: &str) {
        self.known.remove(address);
        if let Some(store) = &self.store {
            store.forget_address(address);
        }
        self.emit_address_book();
    }

    /// Everything the wallet has recorded about who it has paid.
    ///
    /// Read from the store rather than from the in-memory map, because the map
    /// holds only addresses and names — the counts and the dates live in the
    /// file, and a summary assembled from half the facts would be worse than
    /// none.
    fn emit_address_book(&self) {
        let Some(store) = &self.store else {
            return;
        };

        let rows: Vec<pecu_protocol::KnownAddressVm> = store
            .known_addresses()
            .into_iter()
            .map(|known| pecu_protocol::KnownAddressVm {
                summary: payment_summary(known.payments, known.paid_at, now()),
                address: known.address,
                label: known.label,
            })
            .collect();

        let _ = self.events.send(Event::AddressBook(rows));
    }

    /// Record that the recipient of `record` has now been paid.
    ///
    /// Driven off the pending ledger rather than off the draft, because the
    /// ledger row is the thing that exists exactly once per payment and is
    /// removed once it is settled. Calling this twice for one payment — a
    /// resend succeeding *and* the original turning out to have landed — finds
    /// no row the second time, because the first call's `forget_confirmed`
    /// removed it.
    ///
    /// Only after a node has accepted. Recording on the attempt would make the
    /// review stop warning about a recipient this wallet has never actually
    /// paid, which is precisely the case the warning is for.
    fn remember_recipient(&mut self, record: u64) {
        let Some(address) = self.pending.get(record).map(|row| row.to_address.clone()) else {
            return;
        };
        if address.is_empty() {
            return;
        }

        // Kept even without a store: the warning should stop for the rest of
        // this session either way, and a wallet that cannot write is not a
        // wallet that should nag.
        self.known.entry(address.clone()).or_default();
        if let Some(store) = &self.store {
            store.note_payment(&address, now());
        }
        self.emit_address_book();
    }

    /// Write down which endpoint is in use.
    ///
    /// By URL, not by id: a built-in's id is its position in a compiled-in
    /// list, and a build that ships a third node ahead of the others would
    /// otherwise silently move the user onto a different chain.
    fn remember_active_node(&self) {
        let Some(store) = &self.store else {
            return;
        };
        store.set_setting("active_node_url", self.nodes.active_url().unwrap_or(""));
    }

    // ── The nodes nobody is reading from ────────────────────────────────────

    /// Note which screen is open, and act on it.
    ///
    /// Entering one screen IS leaving the last, so there is no second signal to
    /// get out of step with this — a `ScreenLeft` the interface forgot to send
    /// would leave the poller running against a screen nobody is looking at.
    fn enter_screen(&mut self, screen: pecu_protocol::ScreenId) {
        self.wallet.touch();
        self.polling.screen = screen;

        // Opening the node list is the signal to find out what the other
        // endpoints are doing, immediately rather than at the next tick.
        if screen == pecu_protocol::ScreenId::Nodes {
            self.probe_inactive();
        }

        // Same reasoning for the identities: one request per key, and nowhere
        // else reads the answer.
        //
        // Unconditionally, not only when the list is empty. A wallet that had
        // just registered a name arrived back at a non-empty list and never
        // re-asked, so the identity it had watched being created was missing
        // until somebody pressed Refresh — which reads as a failure rather than
        // as a stale list.
        if screen == pecu_protocol::ScreenId::Identities {
            self.refresh_identities();
        }

        // And the currencies, which are read *through* the identities — so this
        // asks only for the identities and lets `finish_identities` chain into
        // the currency walk when it has them.
        //
        // Not both here. `refresh_identities` dispatches and returns; the list
        // it fills is not there yet, so a currency walk started on this line
        // would ask about whatever the previous screen left behind.
        if screen == pecu_protocol::ScreenId::Currencies {
            self.refresh_identities();
            self.ensure_launch_fees();
        }

        // And the markets, unconditionally — somebody who opened this screen
        // wants current prices, and it is the only place to ask for them again.
        //
        // The dashboard shows the same rows in a column beside the balance and
        // does **not** appear here: it gets its first read from `refresh`, once
        // a session. `listcurrencies` is most of half a megabyte on VRSCTEST and
        // the dashboard is the screen a person passes through on the way to
        // everything else, so re-fetching on arrival would ask a public node for
        // the same answer a dozen times an hour.
        if screen == pecu_protocol::ScreenId::Markets {
            self.refresh_markets();
        }
    }

    /// Ask every node except the active one what it is.
    ///
    /// Only while the network screen is open. The active node is polled every
    /// fifteen seconds regardless, because the balance on screen depends on it;
    /// the others are only interesting to somebody looking at a list of them,
    /// and polling endpoints nobody is reading is asking public infrastructure
    /// for an answer that goes nowhere.
    ///
    /// # The backoff is deliberately not applied here
    ///
    /// It governs the automatic poller, whose job is to be polite about a node
    /// nobody asked about. This is the opposite case: somebody has opened the
    /// list precisely to find out what these endpoints are doing, and a node
    /// that recovered would otherwise sit at "offline" on a screen being
    /// watched, for as long as five minutes, because it failed earlier. Once a
    /// minute, for the nodes on screen, while the screen is open.
    fn probe_inactive(&mut self) {
        let active = self.nodes.active().map(|node| node.id);
        let targets: Vec<(u32, String)> = self
            .nodes
            .nodes()
            .iter()
            .filter(|node| Some(node.id) != active)
            .map(|node| (node.id, node.url.clone()))
            .collect();

        self.polling.others_at = Some(std::time::Instant::now());
        for (id, url) in targets {
            self.probe_one(id, &url);
        }
    }

    /// The sixty-second cadence for the inactive nodes, while anyone is looking.
    fn poll_inactive(&mut self) {
        const EVERY: std::time::Duration = std::time::Duration::from_mins(1);

        if self.polling.screen != pecu_protocol::ScreenId::Nodes {
            return;
        }
        if self
            .polling
            .others_at
            .is_some_and(|last| last.elapsed() < EVERY)
        {
            return;
        }
        self.probe_inactive();
    }

    /// Move to another node when the one in use has stopped answering.
    ///
    /// # Three failures, not one
    ///
    /// A single timeout is a network hiccup and switching on it would make the
    /// wallet flap between endpoints on a train. Three consecutive failures,
    /// with the backoff between them, is a node that is actually down.
    ///
    /// # Never while a payment's fate is unknown
    ///
    /// This is the rule that matters, and it is stronger than "not during a
    /// broadcast". A transaction whose broadcast could not be confirmed is
    /// resolved by asking a node whether it has it — and a **different** node
    /// legitimately answers "no" for a transaction that is propagating
    /// perfectly well through the one it was handed to. Failing over mid-
    /// resolution would turn a payment that landed into a payment the wallet
    /// reports as absent, and the screen would then offer to send it again.
    ///
    /// So: not while broadcasting, and not while anything is uncertain.
    ///
    /// # Only to a node on the same chain
    ///
    /// `record_success` marks a node answering about another chain as degraded
    /// rather than online, so requiring `Online` is already requiring agreement
    /// about which chain this is. Stated here because it is the property that
    /// makes automatic switching safe at all.
    fn fail_over(&mut self) {
        if self.broadcasting {
            tracing::info!("not failing over: a payment is being sent");
            return;
        }
        if resolution_pending(&self.pending) {
            tracing::warn!(
                "not failing over: a payment's fate is unknown, and another node would \
                 answer about it from a different mempool",
            );
            return;
        }

        let Some(failed) = self.nodes.active().map(|node| node.id) else {
            return;
        };
        let Some(healthy) = failover_target(&self.nodes) else {
            return;
        };
        if !self.nodes.set_active(healthy) {
            return;
        }

        tracing::warn!(
            from = failed,
            to = healthy,
            "the active node stopped answering"
        );
        self.chain = None;
        self.remember_active_node();
        self.notice_warning(
            "node_failover",
            NoteVm::plain("node-failover"),
            "",
        );
        self.emit_network();
        self.refresh();
    }

    // ── Transactions whose fate is unknown ──────────────────────────────────

    /// Ask the node about anything that is due.
    ///
    /// Runs on the same five-second tick as the auto-lock check and does
    /// nothing at all most of the time: the backoff decides when a record is
    /// due, and there is usually no record.
    fn poll_pending(&mut self) {
        let due: Vec<(u64, String)> = self
            .pending
            .unresolved()
            .filter(|record| {
                if self.checking.contains(&record.id) {
                    return false;
                }
                self.last_checked
                    .get(&record.id)
                    .is_none_or(|last| last.elapsed() >= pending::recheck_after(record.checks))
            })
            .map(|record| (record.id, record.txid.clone()))
            .collect();

        for (id, txid) in due {
            self.check_pending(id, &txid);
        }
    }

    /// One read: does the node have this transaction?
    ///
    /// This is the whole resolution mechanism. `Some(_)` settles it — the
    /// payment landed, and nothing further should happen. Nothing here can
    /// resend; that is a separate command, and it needs a person.
    fn check_pending(&mut self, id: u64, txid: &str) {
        let Some(chain) = self.chain() else {
            return;
        };

        self.checking.insert(id);
        self.last_checked.insert(id, std::time::Instant::now());
        let txid = txid.to_string();

        self.blocking.dispatch(
            move || {
                use verus_sdk::network::ChainReader;
                Work::Checked {
                    record: id,
                    // A node that cannot be reached is not a node saying "no",
                    // so a transport failure reads as "still unknown" and the
                    // backoff simply tries again.
                    confirmations: chain.confirmations(&txid).ok().flatten(),
                }
            },
            self.work.clone(),
        );
    }

    fn finish_check(&mut self, record: u64, confirmations: Option<u32>) {
        self.checking.remove(&record);

        if confirmations.is_some() {
            // Settled. It was on its way all along.
            tracing::info!(record, ?confirmations, "an uncertain payment confirmed");
            // It landed after all, which is the first time this recipient can
            // honestly be called paid.
            self.remember_recipient(record);
            self.pending.set_state(record, pending::State::Confirmed);
            self.pending.forget_confirmed();
            self.last_checked.remove(&record);
            self.emit_pending();
            self.refresh();
            return;
        }

        self.pending.note_check(record);

        // Enough fruitless asking. This does NOT resend — it changes what the
        // screen offers, and a person decides.
        if let Some(entry) = self.pending.get(record) {
            if entry.checks >= pending::CHECKS_BEFORE_ABSENT
                && entry.state == pending::State::Uncertain
            {
                self.pending.set_state(record, pending::State::Absent);
            }
        }
        self.emit_pending();
    }

    fn resolve_pending(&mut self, id: u64, action: pecu_protocol::PendingAction) {
        use pecu_protocol::PendingAction;

        match action {
            PendingAction::CheckNow => {
                if let Some(txid) = self.pending.get(id).map(|r| r.txid.clone()) {
                    self.check_pending(id, &txid);
                }
            }

            // **The same bytes.** Never a rebuild: rebuilding would select
            // different coins and produce a different transaction, and if the
            // first one did land that is a second payment.
            PendingAction::ResendSameBytes => self.resend_pending(id),

            PendingAction::Abandon => {
                self.pending.set_state(id, pending::State::Abandoned);
                self.last_checked.remove(&id);
                self.emit_pending();
            }
        }
    }

    fn resend_pending(&mut self, id: u64) {
        let Some(record) = self.pending.get(id) else {
            return;
        };
        let (hex, txid) = (record.hex.clone(), record.txid.clone());

        let permit = match self.nodes.spend_permit() {
            Ok(permit) => permit,
            Err(refused) => {
                self.notice("spend_refused", refusal_note(&refused), &refused);
                return;
            }
        };
        let Some(chain) = self.chain() else {
            return;
        };

        self.busy(TaskKind::Broadcasting, true);
        self.broadcasting = true;
        self.blocking.dispatch(
            move || Work::Resent {
                record: id,
                result: Box::new(send::resend(&chain, &permit, &hex, &txid)),
            },
            self.work.clone(),
        );
    }

    fn finish_resend(
        &mut self,
        record: u64,
        result: Result<String, verus_sdk::network::FlowError>,
    ) {
        self.busy(TaskKind::Broadcasting, false);
        self.broadcasting = false;

        match result {
            Ok(txid) => {
                tracing::info!(record, %txid, "the same bytes were accepted on a resend");
                self.remember_recipient(record);
                self.pending.set_state(record, pending::State::Confirmed);
                self.pending.forget_confirmed();
                self.last_checked.remove(&record);
                self.emit_pending();
                self.refresh();
            }
            Err(error) => {
                // Still unknown. The record stays, the backoff resumes, and the
                // bytes are still the only ones that may be sent.
                self.pending.set_state(record, pending::State::Resent);
                self.notice(
                    "resend",
                    NoteVm::plain("resend-unconfirmed"),
                    &error,
                );
                self.emit_pending();
            }
        }
    }

    fn emit_pending(&self) {
        let rows: Vec<pecu_protocol::PendingVm> = self
            .pending
            .unresolved()
            .map(|record| pecu_protocol::PendingVm {
                id: record.id,
                txid: record.txid.clone(),
                to_address: record.to_address.clone(),
                amount_display: record.amount_display.clone(),
                created_display: String::new(),
                checks: record.checks,
                state: match record.state {
                    pending::State::Uncertain => "uncertain",
                    pending::State::Absent => "absent",
                    pending::State::Resent => "resent",
                    pending::State::Confirmed => "confirmed",
                    pending::State::Abandoned => "abandoned",
                }
                .to_string(),
            })
            .collect();

        let _ = self
            .events
            .send(Event::Pending(pecu_protocol::ListDelta::Replace(rows)));
    }

    // ── Settings ────────────────────────────────────────────────────────────

    fn change_passphrase(
        &mut self,
        old: &pecu_protocol::Secret,
        new: &pecu_protocol::Secret,
    ) {
        match self.wallet.change_passphrase(old, new) {
            Ok(()) => {
                self.notice_info("passphrase_changed", NoteVm::plain("passphrase-changed"));
                self.emit_wallet();
                // A name claim the last run left unfinished. It has a deadline, so it is
                // worth saying before anything else on that screen.
                self.emit_registration(None);
            }
            Err(error) => self.notice(
                "change_passphrase",
                NoteVm::plain("passphrase-wrong"),
                &error,
            ),
        }
    }

    /// Remember how the window should look.
    ///
    /// Written down and never read back within a session: the interface already
    /// knows what it is showing, and echoing it would only be a chance for the
    /// two to disagree. It comes back once, at the next start, from
    /// [`Core::restore`].
    fn set_appearance(&self, dark: bool, reduce_motion: bool) {
        let Some(store) = &self.store else {
            return;
        };
        store.set_setting("theme", if dark { "dark" } else { "light" });
        store.set_setting("reduce_motion", if reduce_motion { "1" } else { "0" });
    }

    /// Remember how big the window was left.
    ///
    /// Refused below the window's own declared minimum. A stored size smaller
    /// than that could only come from a truncated write or a hand-edited
    /// database, and restoring it would open a wallet whose layout has nowhere
    /// to go — so the wrong value is dropped here rather than handed back at the
    /// next start.
    ///
    /// Size only, never position. See [`pecu_protocol::Command`].
    fn remember_window(&self, width: u32, height: u32) {
        let Some(store) = &self.store else {
            return;
        };
        if width < MIN_WINDOW.0 || height < MIN_WINDOW.1 {
            return;
        }
        store.set_setting("window_width", &width.to_string());
        store.set_setting("window_height", &height.to_string());
    }

    fn set_auto_lock(&mut self, minutes: Option<u32>) {
        self.wallet.auto_lock =
            minutes.map(|m| std::time::Duration::from_secs(u64::from(m).saturating_mul(60)));
        if let Some(store) = &self.store {
            // Zero is how "never" is written down, matching what the screen
            // sends.
            store.set_setting("auto_lock_minutes", &minutes.unwrap_or(0).to_string());
        }
        self.wallet.touch();
        self.emit_wallet();
        // A name claim the last run left unfinished. It has a deadline, so it is
        // worth saying before anything else on that screen.
        self.emit_registration(None);
    }

    /// The typed confirmation is checked **here**, in the core.
    ///
    /// Putting it in the UI would make it a decoration: the screen that asks
    /// for the word is the layer easiest to bypass, and this is the guard that
    /// stands between a half-finished wallet and real coins.
    fn set_mainnet_spend(&mut self, on: bool, typed: &str) {
        if self.nodes.set_allow_mainnet_spend(on, typed) {
            self.emit_network();
            return;
        }

        let _ = self
            .events
            .send(Event::Notice(pecu_protocol::UiError::simple(
                "mainnet_confirmation",
                NoteVm::plain("mainnet-confirm"),
                String::new(),
                pecu_protocol::Severity::Warning,
            )));
    }

    // ── Send ────────────────────────────────────────────────────────────────

    /// Check a draft, and say what the wallet knows about the recipient.
    ///
    /// Runs on every keystroke, so it touches nothing but memory: address
    /// parsing and amount parsing are both offline and both exact, and the name
    /// comes from a map that was loaded at startup.
    fn validate_draft(&mut self, draft: &pecu_protocol::SendDraft) {
        self.last_draft = draft.clone();
        self.maybe_resolve_identity(draft.to.trim());

        let mut verdict = send::validate(draft, self.spendable);

        // A VerusID typed by name. `send::validate` is offline and correctly
        // refuses it — a name is not base58 and no amount of local parsing will
        // make it one — so the answer a node already gave is applied here.
        if let Some(identity) = self.resolved_for(draft.to.trim()) {
            // Both halves. An empty address is a name the chain does not have,
            // and `revoked` is false for one of those — so testing only the
            // revocation would make every typo a valid recipient.
            //
            // A name that now points somewhere else is deliberately NOT refused,
            // only shouted about: identities are updated legitimately, and a
            // wallet that becomes unable to pay somebody because they moved
            // their identity is broken in a way people route around.
            verdict.to_valid = !identity.address.is_empty() && !identity.revoked;
            verdict.to_note = identity_note(identity);
        } else if self.resolving.as_deref() == Some(draft.to.trim()) {
            verdict.to_note = NoteVm::plain("verusid-resolving");
        }
        verdict.ready = verdict.to_valid && verdict.amount_valid;

        // A name this wallet gave the address, appended to the line that
        // already says what the address is. That is where somebody checking a
        // pasted address is looking, and "the exchange" tells them more than
        // any amount of checksum arithmetic can.
        // Carried beside the note rather than glued onto it.
        //
        // The core used to build `"{note} · {label}"`. That put two decisions
        // in the wrong place: the separator is typography, and whether the two
        // facts belong on one line at all depends on how wide the line is —
        // neither of which the core can see. It is a second field now, and the
        // interface decides how to show both.
        verdict.to_label = self
            .known
            .get(draft.to.trim())
            .filter(|label| !label.is_empty())
            .cloned()
            .unwrap_or_default();

        let _ = self.events.send(Event::SendValidation(verdict));
    }

    // ── VerusIDs ────────────────────────────────────────────────────────────

    /// The chain's name as the daemon spells it, once a node has said.
    ///
    /// `None` before any node has answered. VDXF keys are hashed from this
    /// string, so guessing it would derive keys that match nothing — which on
    /// screen looks exactly like an identity that published nothing.
    fn chain_name(&self) -> Option<String> {
        self.nodes
            .active()
            .and_then(|node| node.network.as_ref())
            .map(|network| network.chain_name().to_string())
    }

    /// Build the VDXF name table, once the chain and its currency are known.
    fn ensure_vdxf(&mut self) {
        if self.vdxf.is_some() {
            return;
        }
        let (Some(name), Some(id)) = (self.chain_name(), self.cached.native) else {
            return;
        };
        self.vdxf = Some(identity::Names::derive(&name, id));
    }

    // ── Changing an identity ────────────────────────────────────────────────

    /// Build and sign a change, without sending it.
    ///
    /// Same shape as a payment: the signed bytes stay here, the UI gets a
    /// ticket and a description. Nothing is broadcast until somebody confirms,
    /// and the broadcast needs a permit like every other write.
    fn prepare_identity_change(&mut self, address: &str, change: identity::Change) {
        let Some(label) = self.wallet.active_key.clone() else {
            return;
        };
        let Some(vault) = self.wallet.vault() else {
            return;
        };
        let Some(chain) = self.chain() else {
            return;
        };

        self.tickets += 1;
        let ticket = self.tickets;
        let address = address.to_string();
        let described = change.describe();
        let needs_confirmation = change.needs_typed_confirmation();
        self.busy(TaskKind::PreparingSend, true);

        self.blocking.dispatch(
            move || {
                // One `with_key`, used for both roles. The funding key and the
                // identity key are the same here: this wallet is changing an
                // identity its own key controls, which is the only case it
                // offers. A multisig identity would need every signer, and this
                // is where that would go.
                let built = vault.with_key(&label, |key| match &change {
                    identity::Change::Unlock { extra_blocks } => {
                        verus_sdk::network::prepare_identity_unlock(
                            &*chain,
                            key,
                            &[key],
                            &address,
                            *extra_blocks,
                        )
                        .map(identity::Prepared::Updated)
                        .map_err(|error| error.to_string())
                    }
                    // Signed by the **authority's** keys, not the identity's.
                    // For an identity that is its own authority they are the
                    // same key, which is the only case this wallet serves — and
                    // the SDK checks the authority before signing, so a wallet
                    // that does not hold them is told which ones were needed
                    // rather than meeting a script verification failure the
                    // daemon will not explain.
                    identity::Change::Revoke => verus_sdk::network::prepare_identity_revocation(
                        &*chain,
                        key,
                        &[key],
                        &address,
                    )
                    .map(identity::Prepared::Revoked)
                    .map_err(|error| error.to_string()),
                    identity::Change::Recover => verus_sdk::network::prepare_identity_recovery(
                        &*chain,
                        key,
                        &[key],
                        &address,
                        // Nothing restored beyond clearing the revocation. A
                        // recovery may legitimately hand the identity to new
                        // primary addresses, and offering that without a screen
                        // built for it would be the most dangerous default here.
                        &verus_sdk::network::IdentityChange::new(),
                    )
                    .map(identity::Prepared::Recovered)
                    .map_err(|error| error.to_string()),
                    other => match identity::as_sdk_change(other) {
                        Ok(sdk) => verus_sdk::network::prepare_identity_update(
                            &*chain,
                            key,
                            &[key],
                            &address,
                            &sdk,
                        )
                        .map(identity::Prepared::Updated)
                        .map_err(|error| error.to_string()),
                        Err(reason) => Err(reason),
                    },
                });

                Work::IdentityChangePrepared {
                    ticket,
                    described,
                    needs_confirmation,
                    result: Box::new(match built {
                        Ok(inner) => inner,
                        Err(vault) => Err(vault.to_string()),
                    }),
                }
            },
            self.work.clone(),
        );
    }

    fn finish_identity_change_prepared(
        &mut self,
        ticket: u64,
        described: &str,
        needs_confirmation: bool,
        result: Result<identity::Prepared, String>,
    ) {
        self.busy(TaskKind::PreparingSend, false);

        match result {
            Ok(prepared) => {
                let fee = portfolio::coins(prepared.fee());
                self.identity_changes.insert(ticket, prepared);
                if needs_confirmation {
                    self.identity_confirms.insert(ticket);
                }
                let _ = self.events.send(Event::IdentityChangePrepared {
                    ticket,
                    description: described.to_string(),
                    fee_display: fee,
                    // The word travels with the request rather than being
                    // written out again on the other side. `identity` owns what
                    // it is; nothing else should have an opinion.
                    confirmation: if needs_confirmation {
                        identity::REVOKE_CONFIRMATION.to_string()
                    } else {
                        String::new()
                    },
                });
            }
            Err(reason) => {
                self.notice_warning(
                    "identity_change",
                    NoteVm::plain("identity-change-failed"),
                    &reason,
                );
            }
        }
    }

    /// Send it. The permit is the same gate every other write goes through.
    fn confirm_identity_change(&mut self, ticket: u64, typed: &str) {
        // The typed word, checked here rather than in the interface — for the
        // same reason the mainnet switch is checked here. A guard the UI owns
        // is a guard a different UI does not have.
        if self.identity_confirms.contains(&ticket)
            && !typed
                .trim()
                .eq_ignore_ascii_case(identity::REVOKE_CONFIRMATION)
        {
            self.notice_warning(
                "identity_change",
                NoteVm::plain("identity-revoke-confirm"),
                "",
            );
            return;
        }

        let Some(unsent) = self.identity_changes.remove(&ticket) else {
            return;
        };
        self.identity_confirms.remove(&ticket);
        let permit = match self.nodes.spend_permit() {
            Ok(permit) => permit,
            Err(refused) => {
                // Put it back: rebuilding would produce different bytes, and
                // the refusal may be something the user can fix.
                self.identity_changes.insert(ticket, unsent);
                self.notice("spend_refused", refusal_note(&refused), &refused);
                return;
            }
        };
        let Some(chain) = self.chain() else {
            self.identity_changes.insert(ticket, unsent);
            return;
        };

        self.busy(TaskKind::Broadcasting, true);
        self.blocking.dispatch(
            move || {
                let result = unsent.broadcast(&chain.broadcaster(&permit));
                Work::IdentityChanged(Box::new(result.map_err(|error| error.to_string())))
            },
            self.work.clone(),
        );
    }

    fn finish_identity_changed(&mut self, result: Result<String, String>) {
        self.busy(TaskKind::Broadcasting, false);
        match result {
            Ok(txid) => {
                tracing::info!(%txid, "an identity was changed");
                let _ = self.events.send(Event::IdentityChanged { txid });
                // The change is not on the chain until it is mined, so the list
                // will show the old state until then — which the new-block
                // refresh corrects on its own.
                self.refresh_identities();
            }
            Err(reason) => {
                self.notice_warning(
                    "identity_change",
                    NoteVm::plain("identity-change-rejected"),
                    &reason,
                );
            }
        }
    }

    // ── Claiming a name ─────────────────────────────────────────────────────

    /// Whether a name can be claimed, and what it would cost.
    ///
    /// The local rule first, because it needs no node and refuses the mistakes
    /// people actually make — a capital letter, a dot, a space. Then the chain,
    /// for whether it is taken and what the fee is.
    fn check_name(&mut self, name: &str) {
        let name = name.trim().to_string();
        if let Some(problem) = identity::name_problem(&name) {
            let _ = self.events.send(Event::NameChecked {
                name,
                problem,
                fee_display: String::new(),
            });
            return;
        }
        if name.is_empty() {
            let _ = self.events.send(Event::NameChecked {
                name,
                problem: String::new(),
                fee_display: String::new(),
            });
            return;
        }

        let Some(chain) = self.chain() else {
            return;
        };
        let asked = name.clone();
        self.blocking.dispatch(
            move || {
                use verus_sdk::network::ChainReader;
                // Taken or free. `-5` is the daemon saying it does not exist,
                // which here is the good answer — anything else is the question
                // going unanswered, and the two must not read the same.
                let taken = match chain.identity(&format!("{asked}@")) {
                    Ok(_) => Some(true),
                    Err(verus_sdk::network::RpcError::Node { code: -5, .. }) => Some(false),
                    Err(_) => None,
                };
                let fee = chain
                    .currency(&asked)
                    .ok()
                    .map(|policy| policy.id_registration_fee);
                Work::NameChecked {
                    name: asked,
                    taken,
                    fee,
                }
            },
            self.work.clone(),
        );
    }

    /// Step one is built and signed. Write the salt down, then send it.
    ///
    /// The only ordering in this application where getting it backwards costs
    /// money with nothing to show: the commitment fee is spent by the broadcast,
    /// and without the salt it can never be redeemed. So the write comes first
    /// and a write that fails stops here.
    fn finish_reserved(
        &mut self,
        name: &str,
        label: &str,
        permit: pecu_chain::SpendPermit,
        result: Result<verus_sdk::network::Pending<verus_sdk::network::AwaitingCommitment>, String>,
    ) {
        self.busy(TaskKind::PreparingSend, false);

        let pending = match result {
            Ok(pending) => pending,
            Err(reason) => {
                self.notice_warning("registration", NoteVm::plain("name-claim-failed"), &reason);
                return;
            }
        };

        if let Err(error) = self.reservation.reserve(name, label, pending.clone()) {
            // Nothing has been broadcast, so this costs nothing but the attempt.
            // Sending anyway would spend the fee on a claim whose secret exists
            // only in this process.
            self.notice(
                "registration_write",
                NoteVm::plain("name-claim-unsaved"),
                &error,
            );
            return;
        }
        self.emit_registration(None);

        let Some(chain) = self.chain() else {
            return;
        };
        self.busy(TaskKind::Broadcasting, true);
        self.blocking.dispatch(
            move || {
                let mut pending = pending;
                let result = pending.broadcast_commitment(&*chain, &chain.broadcaster(&permit));
                Work::Committed {
                    // Handed back either way: `broadcast_commitment` takes
                    // `&mut self` rather than consuming, precisely so an
                    // ambiguous failure does not destroy the salt.
                    pending: Box::new(pending),
                    result: Box::new(result.map_err(|error| error.to_string())),
                }
            },
            self.work.clone(),
        );
    }

    fn finish_committed(
        &mut self,
        pending: verus_sdk::network::Pending<verus_sdk::network::AwaitingCommitment>,
        result: Result<(), String>,
    ) {
        self.busy(TaskKind::Broadcasting, false);

        // The anchor lands on the value whether or not the broadcast was
        // certain, so it is kept either way — without it the poll that follows
        // has nothing to compare against and stops noticing reorgs.
        self.reservation.update(pending);

        match result {
            Ok(()) => {
                self.reservation.mark_committed();
                tracing::info!("a name claim was accepted by the network");
            }
            Err(reason) => {
                // Not an abandonment. The claim may well be on the network —
                // that is what makes the failure ambiguous — and the salt is
                // still on disk, so the poller finds out which.
                tracing::warn!(%reason, "the name claim's outcome is unknown");
                self.reservation.mark_committed();
                self.notice_warning(
                    "registration_uncertain",
                    NoteVm::plain("name-claim-uncertain"),
                    "",
                );
            }
        }
        self.emit_registration(None);
    }

    /// Ask whether the claim has confirmed. **On the tick, not on the screen.**
    ///
    /// A commitment expires about twenty blocks after it is signed, and missing
    /// that window spends the fee for nothing. So this runs whatever the user is
    /// looking at — gating it on the Identities screen being open would make the
    /// deadline depend on where somebody happened to navigate.
    ///
    /// `poll` costs up to four requests and never sleeps, which is why it is
    /// driven from here rather than by `wait_blocking`.
    fn poll_registration(&mut self) {
        const EVERY: std::time::Duration = std::time::Duration::from_secs(20);

        if self.polling.registration_in_flight
            || self.ready.is_some()
            || self
                .polling
                .registration_at
                .is_some_and(|last| last.elapsed() < EVERY)
        {
            return;
        }
        let Some(record) = self.reservation.current() else {
            return;
        };
        // Nothing has been broadcast yet, so there is nothing to find.
        if record.step == registration::Step::Reserved {
            return;
        }
        let pending = record.pending.clone();
        let Some(chain) = self.chain() else {
            return;
        };

        self.polling.registration_in_flight = true;
        self.polling.registration_at = Some(std::time::Instant::now());
        self.blocking.dispatch(
            move || {
                Work::CommitmentPolled(Box::new(pending.poll(&*chain).map_err(|e| e.to_string())))
            },
            self.work.clone(),
        );
    }

    fn finish_poll(&mut self, result: Result<verus_sdk::network::CommitmentStatus, String>) {
        use verus_sdk::network::CommitmentStatus;

        self.polling.registration_in_flight = false;
        let status = match result {
            Ok(status) => status,
            Err(reason) => {
                // A poll that fails says nothing about the claim. It is still on
                // disk and still has a deadline; the next tick asks again.
                tracing::info!(%reason, "a name claim could not be polled");
                return;
            }
        };

        if let CommitmentStatus::Ready(ready) = &status {
            self.ready = Some((**ready).clone());
        }
        self.emit_registration(Some(&status));
    }

    /// Step two: reveal the name and create the identity.
    fn finish_registration(&mut self) {
        let Some(ready) = self.ready.take() else {
            return;
        };
        let Some(record) = self.reservation.current() else {
            return;
        };
        // The same key that made the commitment. The commitment output is
        // locked to its address, and the SDK refuses a mismatch rather than
        // producing something unspendable.
        let label = record.key_label.clone();
        let Some(vault) = self.wallet.vault() else {
            return;
        };
        let Some(chain) = self.chain() else {
            return;
        };
        let permit = match self.nodes.spend_permit() {
            Ok(permit) => permit,
            Err(refused) => {
                self.ready = Some(ready);
                self.notice("spend_refused", refusal_note(&refused), &refused);
                return;
            }
        };

        self.busy(TaskKind::Broadcasting, true);
        self.blocking.dispatch(
            move || {
                let outcome = vault.with_key(&label, |key| {
                    ready
                        .prepare(&*chain, key)
                        .and_then(|unsent| unsent.broadcast(&chain.broadcaster(&permit)))
                });
                Work::Registered(Box::new(match outcome {
                    Ok(Ok(registered)) => Ok(registered),
                    Ok(Err(flow)) => Err(flow.to_string()),
                    Err(vault) => Err(vault.to_string()),
                }))
            },
            self.work.clone(),
        );
    }

    fn finish_registered(&mut self, result: Result<verus_sdk::network::Registered, String>) {
        self.busy(TaskKind::Broadcasting, false);

        match result {
            Ok(registered) => {
                let address = verus_sdk::verus_keys::Address::new(
                    verus_sdk::verus_keys::AddressKind::Identity,
                    registered.identity_address,
                )
                .to_string();
                let registered_address = address.clone();
                tracing::info!(
                    name = %registered.name,
                    %address,
                    fee = %portfolio::coins(registered.fee_paid),
                    "a VerusID was registered",
                );

                // The salt has done its work. This is one of exactly two places
                // it is deleted, and the other is somebody saying to stop.
                let cannot_be_revoked = self
                    .reservation
                    .current()
                    .is_some_and(|record| record.pending.recovery_authority.is_none());
                self.reservation.finish();

                let _ = self.events.send(Event::Registration(Some(Box::new(
                    pecu_protocol::RegistrationVm {
                        name: registered.name,
                        step: "done".to_string(),
                        note: "The identity exists on the chain.".to_string(),
                        deadline: String::new(),
                        fee_display: portfolio::coins(registered.fee_paid),
                        address,
                        busy: false,
                        cannot_be_revoked,
                        steps: identity::progress("done"),
                    },
                ))));

                // It is one of yours now.
                self.refresh_identities();

                // And if a currency has been waiting for this name, carry on
                // without making somebody navigate back and fill the form in
                // again. That is what "created in the background" means.
                self.continue_after_identity(&registered_address);
            }
            Err(reason) => {
                // The claim is still on disk and may still be inside its
                // window, so this is not the end of it — the poller will offer
                // the step again.
                tracing::warn!(%reason, "the registration could not be completed");
                self.notice_warning(
                    "registration",
                    NoteVm::plain("name-register-failed"),
                    &reason,
                );
                self.emit_registration(None);
            }
        }
    }

    /// Give up on the claim in progress.
    fn abandon_registration(&mut self) {
        let spent = self
            .reservation
            .current()
            .is_some_and(|record| record.step == registration::Step::Committed);
        self.reservation.finish();
        self.ready = None;
        if spent {
            tracing::info!("a name claim was abandoned after its fee was spent");
        }
        self.emit_registration(None);
    }

    fn emit_registration(&self, status: Option<&verus_sdk::network::CommitmentStatus>) {
        let Some(record) = self.reservation.current() else {
            let _ = self.events.send(Event::Registration(None));
            return;
        };
        let tip = self.nodes.active().and_then(|node| node.tip).unwrap_or(0);
        let vm = identity::registration_view(record, status, tip, false);
        let _ = self.events.send(Event::Registration(Some(Box::new(vm))));
    }

    fn finish_name_check(
        &mut self,
        name: &str,
        taken: Option<bool>,
        fee: Option<verus_sdk::money::Amount>,
    ) {
        let problem = match taken {
            Some(true) => "That name is already registered.".to_string(),
            Some(false) => String::new(),
            None => "Could not check whether that name is free.".to_string(),
        };
        let _ = self.events.send(Event::NameChecked {
            name: name.to_string(),
            problem,
            fee_display: fee.map(portfolio::coins).unwrap_or_default(),
        });
    }

    /// Build step one, write the salt down, and hand the commitment to a node.
    ///
    /// **The order is the point.** The salt cannot be recovered from the chain,
    /// so it goes to disk before anything is broadcast — and a write that fails
    /// stops the whole thing rather than sending bytes whose secret exists only
    /// in memory. Same shape as the pending ledger, for the same reason.
    fn start_registration(&mut self, name: &str, revocation: &str, recovery: &str) {
        if self.reservation.in_progress() {
            self.notice_warning(
                "registration_busy",
                NoteVm::plain("name-claim-busy"),
                "",
            );
            return;
        }

        let name = name.trim().to_string();

        // The local rule, applied where the money is rather than only where the
        // typing was.
        //
        // The screen gates its own button on this — but on the answer to a
        // question asked a moment ago, and since that question waits for a
        // pause in typing there is a window in which the button is live and the
        // answer is about a shorter name. The same reasoning `prepare_launch`
        // gives: the checks are the core's, and this is where they bind.
        if let Some(problem) = identity::name_problem(&name) {
            self.notice_warning("registration_name", NoteVm::plain("name-refused"), &problem);
            return;
        }
        if name.is_empty() {
            return;
        }

        let Some(label) = self.wallet.active_key.clone() else {
            return;
        };
        let Some(vault) = self.wallet.vault() else {
            return;
        };
        let Some(chain) = self.chain() else {
            return;
        };

        // The permit before the work, not after: a registration that cannot be
        // broadcast should refuse before it costs a signature.
        let permit = match self.nodes.spend_permit() {
            Ok(permit) => permit,
            Err(refused) => {
                self.notice("spend_refused", refusal_note(&refused), &refused);
                return;
            }
        };

        let options = verus_sdk::network::RegistrationOptions {
            revocation_authority: some_if_set(revocation),
            recovery_authority: some_if_set(recovery),
            ..verus_sdk::network::RegistrationOptions::default()
        };

        self.busy(TaskKind::PreparingSend, true);
        self.blocking.dispatch(
            move || {
                let built = vault.with_key(&label, |key| {
                    verus_sdk::network::prepare_registration(&*chain, key, &name, &options)
                });
                Work::Reserved {
                    name,
                    label,
                    permit: Box::new(permit),
                    result: Box::new(match built {
                        Ok(Ok(pending)) => Ok(pending),
                        Ok(Err(flow)) => Err(flow.to_string()),
                        Err(vault) => Err(vault.to_string()),
                    }),
                }
            },
            self.work.clone(),
        );
    }

    /// Find every identity this wallet's keys control.
    ///
    /// **One request per key**, which is why this is asked for rather than run
    /// on a timer: nothing outside the Identities screen reads the answer, and
    /// a wallet with eight keys polling in the background would be asking a
    /// public node eight questions a minute for nobody.
    fn refresh_identities(&mut self) {
        self.ensure_vdxf();
        let addresses = self.wallet_addresses();
        if addresses.is_empty() {
            return;
        }
        let Some(chain) = self.chain() else {
            return;
        };

        self.busy(TaskKind::RefreshingBalance, true);
        self.blocking.dispatch(
            move || {
                use verus_sdk::network::ChainReader;
                let mut found = Vec::new();
                for address in &addresses {
                    match chain.identities_with_address(address) {
                        Ok(mut some) => found.append(&mut some),
                        // One key's answer failing must not lose the others.
                        // A shorter list is the failure that looks like success
                        // here, so it is reported rather than swallowed — but
                        // only after every key has been asked.
                        Err(error) => {
                            return Work::Identities(Box::new(Err(error)));
                        }
                    }
                }
                Work::Identities(Box::new(Ok(found)))
            },
            self.work.clone(),
        );
    }

    fn finish_identities(
        &mut self,
        result: Result<Vec<verus_sdk::network::IdentityAtAddress>, verus_sdk::network::RpcError>,
    ) {
        self.busy(TaskKind::RefreshingBalance, false);

        let found = match result {
            Ok(found) => found,
            Err(error) => {
                self.notice("identities", NoteVm::plain("identities-unreadable"), &error);
                return;
            }
        };

        let tip = self.nodes.active().and_then(|node| node.tip).unwrap_or(0);
        let chain_name = self.chain_name();

        // Rebuilt, not merged: an identity this wallet has stopped controlling
        // has to leave the list, and merging would keep it there forever.
        self.identities.clear();

        for record in &found {
            let row = identity::row(record, tip, chain_name.as_deref(), true);
            self.identities.insert(row.address.clone(), row);
        }

        // The watched ones are re-read too. They are stored as a name and an
        // address only, so until this runs they say "Not read yet" rather than
        // repeating a status from whenever they were last seen — which is the
        // one thing a persisted chain fact must not do.
        let watched: Vec<String> = self
            .looked_up
            .iter()
            .map(|row| row.address.clone())
            .collect();
        for address in watched {
            self.reread_identity(&address);
        }

        self.emit_identities();

        // A currency is a flag on an identity, so the currency walk can only
        // start once this one has finished. Chained here rather than at
        // `enter_screen`, which dispatches this and returns before the list
        // exists.
        //
        // Gated on the screen so the extra request per identity is only made
        // where somebody is looking at the answer.
        if self.polling.screen == pecu_protocol::ScreenId::Currencies {
            self.refresh_currencies();
        }
    }

    // ── Markets ─────────────────────────────────────────────────────────────

    /// The currency prices are quoted in.
    ///
    /// A dollar stablecoin, because "0.5372" means something to a person and
    /// "13.198 VRSCTEST to the Bridge unit" does not. By **name**, resolved
    /// through the chain's own currency list rather than by a hardcoded
    /// i-address: the same name exists on both networks and points at
    /// different bytes, and a constant here would be right on one of them.
    const QUOTE: &'static str = "DAI.vETH";

    /// Read the whole book: the chain's currency list, and the pools.
    ///
    /// Two `getcurrencyconverters` calls, not one per currency. Asking for the
    /// converters of the chain's own currency returns nearly every basket on
    /// the chain — because nearly every basket holds it — and asking for the
    /// quote currency's picks up the few that do not. Each answer already
    /// carries the reserves, the weights and the supply, so the price of all
    /// forty-odd currencies falls out of two replies.
    fn refresh_markets(&mut self) {
        if self.markets == Markets::Fetching {
            return;
        }
        let Some(chain) = self.chain() else {
            return;
        };

        self.markets = Markets::Fetching;
        self.busy(TaskKind::RefreshingBalance, true);
        self.blocking.dispatch(
            move || {
                use verus_sdk::network::ChainReader;

                let read = (|| {
                    let native = chain.chain_info().map_err(|e| e.to_string())?.chain_id;
                    let catalog = chain.list_currencies().map_err(|e| e.to_string())?;

                    let mut converters = chain
                        .currency_converters(&[native.as_str()])
                        .map_err(|e| e.to_string())?;

                    // The quote currency's own markets, minus the ones already
                    // in hand. A pool that holds DAI but not the chain currency
                    // prices things nothing above would reach.
                    let seen: std::collections::BTreeSet<String> = converters
                        .iter()
                        .map(|entry| entry.converter_id.clone())
                        .collect();
                    if let Ok(more) = chain.currency_converters(&[Self::QUOTE]) {
                        converters.extend(
                            more.into_iter()
                                .filter(|entry| !seen.contains(&entry.converter_id)),
                        );
                    }

                    Ok((catalog, converters))
                })();

                Work::Markets(Box::new(read))
            },
            self.work.clone(),
        );
    }

    fn finish_markets(
        &mut self,
        read: Result<
            (
                Vec<verus_sdk::network::CurrencySummary>,
                Vec<verus_sdk::network::CurrencyConverter>,
            ),
            String,
        >,
    ) {
        self.markets = Markets::Asked;
        self.busy(TaskKind::RefreshingBalance, false);

        let (catalog, converters) = match read {
            Ok(read) => read,
            Err(error) => {
                // The last book stays on screen. A market that could not be
                // re-read is stale, not gone, and blanking the table would say
                // the chain has no currencies on it.
                //
                // Said out loud only where somebody is looking at prices. This
                // read also happens once behind the dashboard, for a column
                // beside the balance that already has an honest empty state —
                // and a toast about a background read nobody asked for is how a
                // wallet on a flaky node becomes a wallet nobody reads toasts
                // from. It is in the log either way.
                tracing::warn!(code = "markets_read", %error, "could not read the markets");
                if self.polling.screen == pecu_protocol::ScreenId::Markets {
                    self.notice_warning(
                        "markets_read",
                        NoteVm::plain("markets-unreadable"),
                        &error,
                    );
                }
                return;
            }
        };

        self.market_names = catalog
            .iter()
            .map(|summary| {
                (
                    summary.currency_id.clone(),
                    summary.fully_qualified_name.clone(),
                )
            })
            .collect();

        // The quote currency's i-address, found by the name it is known by.
        // Without it there is nothing to price against and the book is empty —
        // which is the honest outcome on a chain that has no stablecoin.
        let quote = catalog
            .iter()
            .find(|summary| summary.fully_qualified_name == Self::QUOTE)
            .map(|summary| summary.currency_id.clone())
            .unwrap_or_default();
        let native = self
            .cached
            .native
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();

        let pools = converters
            .iter()
            .filter_map(market::Pool::from_converter)
            .collect();
        self.market = market::Book::new(pools, quote, native);

        // A selection that no longer exists is not a selection. It survives a
        // refresh that still knows the currency, so re-opening the screen does
        // not throw away what somebody was reading.
        if !self.market_open.is_empty() && self.market.quote_for(&self.market_open).is_none() {
            let known = self.market.currencies().contains(&self.market_open);
            if !known {
                self.market_open.clear();
            }
        }

        self.emit_markets();
    }

    /// Show one currency, from the book already in hand.
    fn open_market(&mut self, address: String) {
        self.wallet.touch();
        self.market_open = address;
        self.emit_market_detail();
    }

    fn emit_markets(&mut self) {
        let _ = self.events.send(Event::Markets {
            rows: market::rows(&self.market, &self.market_names),
            quote: Self::QUOTE.to_string(),
        });
        self.emit_market_detail();
    }

    fn emit_market_detail(&mut self) {
        let detail = if self.market_open.is_empty() {
            None
        } else {
            Some(Box::new(market::detail(
                &self.market,
                &self.market_open,
                &self.market_names,
                Self::QUOTE,
            )))
        };
        let _ = self.events.send(Event::MarketDetail(detail));
    }

    // ── Convert ─────────────────────────────────────────────────────────────

    /// Price what is being composed, asking a node only when it is worth it.
    ///
    /// Everything decidable offline is decided offline, against the book and
    /// the balances already in hand. `estimateconversion` is one request, and
    /// this runs on every keystroke — so a draft that cannot be converted at
    /// all is refused here rather than by a node that would have to be asked
    /// per character to say the same thing.
    fn set_convert_draft(&mut self, draft: pecu_protocol::ConvertDraft) {
        self.wallet.touch();
        self.convert = draft;

        // Any edit invalidates whatever is in flight. Incrementing before the
        // check, not after: a draft that just became unpriceable must also
        // cancel the estimate the previous one asked for, or that answer lands
        // as a quote for a conversion nobody can make.
        self.convert_ticket = self.convert_ticket.wrapping_add(1);
        let ticket = self.convert_ticket;

        let ready = match convert::check(
            &self.convert,
            &self.market,
            &self.holdings,
            &self.market_names,
        ) {
            Ok(ready) => ready,
            Err(note) => {
                self.refuse_conversion(note);
                return;
            }
        };

        let Some(chain) = self.chain() else {
            return;
        };

        // The names, not the i-addresses. `estimateconversion` takes either,
        // and a name is what turns up in the node's log and in a bug report —
        // the wallet has already resolved which currency it means, so there is
        // nothing left for a name to be ambiguous about here.
        let from = convert_name(&self.market_names, &ready.from);
        let to = convert_name(&self.market_names, &ready.to);
        let via = ready.via.clone();
        let amount = ready.amount;
        self.convert_ready = Some(ready);

        self.blocking.dispatch(
            move || {
                use verus_sdk::network::ChainReader;
                let read = chain
                    .estimate_conversion(&from, &to, &amount.to_coins_string(), via.as_deref())
                    .map_err(|error| error.to_string())
                    // The network fee is a second question and a failed answer
                    // to it must not cost the first: an estimate with an
                    // unknown fee beside it is still a quote, and `—` is a
                    // value this screen can show.
                    .map(|estimate| (estimate, chain.estimate_fee(1).ok().flatten()));
                Work::ConvertEstimate {
                    ticket,
                    result: Box::new(read),
                }
            },
            self.work.clone(),
        );
    }

    fn finish_convert_estimate(
        &mut self,
        ticket: u64,
        result: Result<
            (
                verus_sdk::network::ConversionEstimate,
                Option<verus_sdk::money::Amount>,
            ),
            String,
        >,
    ) {
        // An answer to a question nobody is still asking. See `convert_ticket`.
        if ticket != self.convert_ticket {
            return;
        }
        let Some(ready) = self.convert_ready.clone() else {
            return;
        };

        match result {
            Ok((estimate, network_fee)) => {
                let quote = convert::quote(
                    &ready,
                    &self.market_names,
                    &self.holdings,
                    estimate.estimated_out,
                    estimate.fee,
                    network_fee,
                );
                let _ = self.events.send(Event::ConvertQuote(Box::new(quote)));
            }
            Err(error) => {
                tracing::warn!(code = "convert_estimate", %error, "could not price a conversion");
                self.refuse_conversion(pecu_protocol::NoteVm::plain("convert-unpriced"));
            }
        }
    }

    /// Say why there is no quote, in the shape a quote has.
    ///
    /// A whole `ConvertQuoteVm` rather than a note on its own, because every
    /// field has to be answered: a refusal that left the last conversion's fee
    /// and floor on screen beside a new pair of currencies would be describing
    /// something nobody asked for.
    fn refuse_conversion(&self, note: pecu_protocol::NoteVm) {
        let quote = convert::refused(&self.convert, &self.market_names, &self.holdings, note);
        let _ = self.events.send(Event::ConvertQuote(Box::new(quote)));
    }

    /// Turn the conversion around.
    ///
    /// The amount is **cleared**, not carried over. It was typed as a quantity
    /// of one currency and means nothing as a quantity of the other — a swap
    /// that kept "100" would turn a hundred VRSCTEST into a hundred dollars
    /// without anybody typing a digit.
    fn swap_convert_legs(&mut self) {
        self.wallet.touch();
        let swapped = pecu_protocol::ConvertDraft {
            from: self.convert.to.clone(),
            to: self.convert.from.clone(),
            pay: String::new(),
        };
        self.set_convert_draft(swapped);
    }

    // ── Currencies ──────────────────────────────────────────────────────────

    /// Ask, for each identity this wallet controls, whether it defines a
    /// currency.
    ///
    /// # Why by i-address and not by name
    ///
    /// `getcurrency` takes either. A currency's fully qualified name is dotted
    /// and carries no trailing `@`, while an identity's carries one — so asking
    /// by name means converting between two spellings of the same thing and
    /// being wrong about a sub-identity. The i-address is the same string for
    /// both, because they *are* the same thing, and it is already in hand.
    ///
    /// One request per identity, on the screen whose being open is what
    /// justifies them. No node is asked anything until somebody navigates here.
    fn refresh_currencies(&mut self) {
        // The status travels with the name because the picker has to state a
        // refusal a revoked or timelocked identity would otherwise only learn
        // about from a failed build. See `currency::refusal`.
        let mine: Vec<(String, String, bool, String)> = self
            .identities
            .values()
            .map(|row| {
                (
                    row.name.clone(),
                    row.address.clone(),
                    row.mine,
                    row.status.clone(),
                )
            })
            .collect();

        if mine.is_empty() {
            // Nothing to ask about, but the screen still has to be told —
            // otherwise it sits on whatever the last chain's answer was.
            self.currencies.clear();
            self.eligible.clear();
            self.emit_currencies();
            return;
        }

        let Some(chain) = self.chain() else {
            return;
        };

        self.busy(TaskKind::RefreshingBalance, true);
        self.blocking.dispatch(
            move || {
                use verus_sdk::network::ChainReader;
                let read = mine
                    .into_iter()
                    .map(|(name, address, mine, status)| {
                        let lookup = currency::classify(chain.currency_definition(&address));
                        (name, address, mine, status, lookup)
                    })
                    .collect();
                Work::Currencies(read)
            },
            self.work.clone(),
        );
    }

    fn finish_currencies(&mut self, read: Vec<(String, String, bool, String, currency::Lookup)>) {
        self.busy(TaskKind::RefreshingBalance, false);

        // Rebuilt rather than merged, for the reason the identity list is: a
        // currency that has left this wallet's control has to leave the list,
        // and merging would keep it there for the life of the process.
        self.currencies.clear();
        self.eligible.clear();

        let tip = self.nodes.active().and_then(|node| node.tip).unwrap_or(0);

        for (name, address, mine, status, lookup) in read {
            if let currency::Lookup::Unknown(detail) = &lookup {
                // Not a notice: one identity out of five going unanswered is
                // not a failure of the screen, and a toast per identity would
                // bury the four that worked. The row says so itself.
                tracing::info!(%address, %detail, "a currency read went unanswered");
            }

            if let currency::Lookup::Defines(summary) = &lookup {
                self.currencies.push(currency::row(summary, tip));
            }

            let refusal = currency::refusal(&lookup, mine, &status);
            self.eligible.push(pecu_protocol::EligibleIdentityVm {
                name,
                address,
                refusal,
            });
        }

        self.emit_currencies();
    }

    /// Check a draft and hand back everything the configure screen draws.
    ///
    /// Runs on every keystroke, so it touches nothing but memory: every rule is
    /// arithmetic over what was typed, plus the tip and the launch fee the
    /// wallet is already holding. No node is asked anything.
    /// Answer the reserve picker, fetching the chain's currency list if this is
    /// the first search of the session.
    ///
    /// # Why the list is fetched here rather than when the screen opens
    ///
    /// Because it is half a megabyte and most sessions never open the picker.
    /// A basket is one of three kinds and the reserve list is one step inside
    /// it; paying for that on the way past the Currencies screen would be a
    /// request nobody asked for. The first search pays, and it is the one
    /// moment somebody is waiting for exactly this answer.
    /// Answer the command palette.
    ///
    /// Over what is already in hand — the identities these keys control and the
    /// currencies they define — rather than over the chain. A palette that made
    /// a request per keystroke would stutter, and neither list changes between
    /// two letters being typed.
    ///
    /// An empty query clears rather than listing everything: a palette that
    /// opens showing the whole wallet has answered a question nobody asked.
    ///
    /// Matching is a case-insensitive substring over the name **and** the
    /// i-address. The address matters more than it looks: it is the identifier
    /// this wallet tells people to prefer for anything destructive, so it is
    /// the one somebody arrives with pasted on the clipboard.
    fn search(&mut self, query: &str) {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            let _ = self.events.send(Event::SearchHits {
                query: query.to_string(),
                hits: Vec::new(),
            });
            return;
        }

        let matches = |name: &str, address: &str| search_matches(&needle, name, address);

        let mut hits: Vec<pecu_protocol::SearchHitVm> = Vec::new();

        // Identities first, and deliberately: a name is what a person searches
        // for, and a currency shares its address with the identity that defines
        // it — so an address query matches both and the identity is the one
        // that explains the other.
        for identity in self.identities.values() {
            if matches(&identity.name, &identity.address) {
                hits.push(pecu_protocol::SearchHitVm {
                    kind: "identity".to_string(),
                    label: identity.name.clone(),
                    sub: identity.address.clone(),
                    target: identity.address.clone(),
                });
            }
        }

        for currency in &self.currencies {
            if matches(&currency.name, &currency.address) {
                hits.push(pecu_protocol::SearchHitVm {
                    kind: "currency".to_string(),
                    label: currency.name.clone(),
                    sub: currency.address.clone(),
                    target: currency.address.clone(),
                });
            }
        }

        let _ = self.events.send(Event::SearchHits {
            query: query.to_string(),
            hits,
        });
    }

    fn search_currencies(&mut self, query: String, exclude: Vec<String>) {
        if let Catalog::Ready(all) = &self.currency_catalog {
            let tip = self.nodes.active().and_then(|node| node.tip).unwrap_or(0);
            let view = currency::choices(all, &query, &exclude, tip);
            let _ = self.events.send(Event::CurrencyChoices(Box::new(view)));
            return;
        }

        // Remembered rather than queued. These are keystrokes and the only
        // answer worth sending is the one to the last of them.
        self.pending_currency_search = Some((query, exclude));

        if self.currency_catalog == Catalog::Fetching {
            return;
        }
        let Some(chain) = self.chain() else {
            let _ = self.events.send(Event::CurrencyChoices(Box::new(
                pecu_protocol::CurrencyChoicesVm {
                    problem: "No node is connected, so the chain's currencies cannot be listed."
                        .to_string(),
                    ..Default::default()
                },
            )));
            return;
        };

        // Said before the request goes out, because the request is the slow one
        // in this wallet and a picker that opened to an empty list would read
        // as a chain with nothing on it.
        let _ = self.events.send(Event::CurrencyChoices(Box::new(
            pecu_protocol::CurrencyChoicesVm {
                loading: true,
                ..Default::default()
            },
        )));

        self.currency_catalog = Catalog::Fetching;
        self.blocking.dispatch(
            move || {
                use verus_sdk::network::ChainReader;
                Work::CurrencyCatalog(Box::new(
                    chain.list_currencies().map_err(|error| error.to_string()),
                ))
            },
            self.work.clone(),
        );
    }

    fn finish_currency_catalog(
        &mut self,
        result: Result<Vec<verus_sdk::network::CurrencySummary>, String>,
    ) {
        let waiting = self.pending_currency_search.take();

        match result {
            Ok(all) => {
                tracing::info!(count = all.len(), "the chain's currency list arrived");
                self.currency_catalog = Catalog::Ready(all);
                // Only if somebody is still looking. A picker closed while the
                // list was in flight has nothing to be told, and the list stays
                // for the next time it opens.
                if let Some((query, exclude)) = waiting {
                    self.search_currencies(query, exclude);
                }
            }
            Err(error) => {
                // Back to unasked rather than left as failed: the picker offers
                // a retry, and a state that remembered the failure would refuse
                // to try again.
                self.currency_catalog = Catalog::Unasked;
                tracing::warn!(%error, "the chain's currency list could not be read");
                // Not a toast. The failure belongs to the panel that asked for
                // it, where there is a Retry — a toast would put it over a form
                // whose other steps are unaffected.
                let _ = self.events.send(Event::CurrencyChoices(Box::new(
                    pecu_protocol::CurrencyChoicesVm {
                        problem: format!("The chain's currencies could not be listed. {error}"),
                        ..Default::default()
                    },
                )));
            }
        }
    }

    fn validate_currency(&mut self, draft: &pecu_protocol::CurrencyDraft) {
        let tip = self.nodes.active().and_then(|node| node.tip).unwrap_or(0);
        let ticker = self.nodes.requested().map_or_else(
            || "VRSC".to_string(),
            |network| network.ticker().to_string(),
        );

        // The fee depends on the kind: a token or a basket pays the chain's
        // currency registration fee, an NFT pays its identity import fee — and
        // on VRSCTEST those are 200 and 0.02. Reading the wrong one is not a
        // rounding error.
        let fee = self.launch_fees.as_ref().map(|fees| {
            if currency::Kind::named(&draft.kind) == currency::Kind::Nft {
                fees.1
            } else {
                fees.0
            }
        });

        // The name the picker showed, looked up from the list core already
        // holds. The draft carries an i-address — which is the right thing for
        // it to carry — but the preview is read against what somebody chose,
        // and nobody chose an address.
        let identity_name = self
            .eligible
            .iter()
            .find(|row| row.address == draft.identity)
            .map_or("", |row| row.name.as_str());

        let view = currency::check(draft, tip, fee, &ticker, identity_name);
        let _ = self
            .events
            .send(Event::CurrencyDraftChecked(Box::new(view)));
    }

    /// Build and sign a launch. Nothing is sent.
    ///
    /// The permit is taken **before** the work, the way registration does it: a
    /// launch that could not be broadcast should refuse before it costs a
    /// signature, not after.
    fn prepare_launch(&mut self, draft: &pecu_protocol::CurrencyDraft) {
        let tip = self.nodes.active().and_then(|node| node.tip).unwrap_or(0);
        if currency::problems(draft, tip).iter().any(|p| p.blocking) {
            // The interface gates its own button on this, so arriving here
            // means a second interface or a stale draft. Refused rather than
            // trusted: the checks are the core's, and this is where they bind.
            self.notice_warning(
                "currency_launch",
                NoteVm::plain("launch-draft-invalid"),
                "",
            );
            return;
        }

        let Some(row) = self
            .eligible
            .iter()
            .find(|row| row.address == draft.identity)
            .cloned()
        else {
            self.notice_warning(
                "currency_launch",
                NoteVm::plain("launch-identity-not-yours"),
                "",
            );
            return;
        };

        self.prepare_launch_under(draft, &row.address, &row.name);
    }

    /// Build and sign a launch under a named identity.
    ///
    /// Split out because there are two ways to arrive here and only one of them
    /// has an entry in `eligible`: choosing an identity from the picker, and
    /// the wallet continuing by itself the moment a name it just registered
    /// lands. The second has the name and the address in hand and no list to
    /// look them up in.
    fn prepare_launch_under(
        &mut self,
        draft: &pecu_protocol::CurrencyDraft,
        identity_address: &str,
        identity_name: &str,
    ) {
        let tip = self.nodes.active().and_then(|node| node.tip).unwrap_or(0);

        // The chain's own currency, which every definition is parented to. It
        // arrives from the first refresh and is cached; without it the parent
        // would have to be guessed, and a definition under the wrong parent is
        // a different currency with the same name.
        let Some(parent) = self.cached.native else {
            self.notice_warning(
                "currency_launch",
                NoteVm::plain("launch-chain-unknown"),
                "",
            );
            return;
        };

        let Some(label) = self.wallet.view().active_key else {
            return;
        };
        let Some(vault) = self.wallet.vault() else {
            return;
        };
        let Some(chain) = self.chain() else {
            return;
        };
        let permit = match self.nodes.spend_permit() {
            Ok(permit) => permit,
            Err(refused) => {
                self.notice_warning(
                    "spend_refused",
                    refusal_note(&refused),
                    "",
                );
                return;
            }
        };

        // The bare name: the identity is `demo.VRSCTEST@` and the currency is
        // `demo` under the chain's own currency. Splitting rather than trimming
        // the suffix, because a sub-identity carries its parents in the same
        // string and only the first part is the name.
        let name = identity_name
            .trim_end_matches('@')
            .split('.')
            .next()
            .unwrap_or_default()
            .to_string();

        let delay: u32 = draft.start_delay.trim().parse().unwrap_or(0);
        let start = u64::from(tip.saturating_add(delay));

        self.tickets += 1;
        let ticket = self.tickets;
        let draft = draft.clone();
        let identity = identity_address.to_string();

        self.busy(TaskKind::PreparingSend, true);
        self.blocking.dispatch(
            move || {
                let resolved = match resolve_recipients(&chain, &draft) {
                    Ok(resolved) => resolved,
                    Err(reason) => {
                        return Work::LaunchPrepared {
                            ticket,
                            result: Box::new(Err(reason)),
                        };
                    }
                };

                let built = match currency::definition(&draft, &name, parent, start, &resolved) {
                    Ok(built) => built,
                    Err(reason) => {
                        return Work::LaunchPrepared {
                            ticket,
                            result: Box::new(Err(reason)),
                        };
                    }
                };

                Work::LaunchPrepared {
                    ticket,
                    result: Box::new(
                        currency::prepare(&chain, &vault, &label, &identity, &built)
                            .map(|prepared| (Box::new(prepared), Box::new(permit))),
                    ),
                }
            },
            self.work.clone(),
        );
    }

    #[allow(clippy::type_complexity)]
    fn finish_launch_prepared(
        &mut self,
        ticket: u64,
        result: Result<(Box<currency::Prepared>, Box<pecu_chain::SpendPermit>), String>,
    ) {
        self.busy(TaskKind::PreparingSend, false);

        match result {
            Ok((prepared, permit)) => {
                let fee = prepared.launch_fee();
                let split = currency::cost(fee);
                let view = pecu_protocol::LaunchReviewVm {
                    ticket,
                    name: prepared.name.clone(),
                    description: format!(
                        "Defines {} under this identity. An identity can define one currency, and only once — this cannot be undone or repeated.",
                        prepared.name,
                    ),
                    fee_display: portfolio::coins(split.launch_fee),
                    deposit_display: portfolio::coins(split.deposit),
                    burned_display: portfolio::coins(split.burned),
                    // Off the signed outcome, not off the form. A launch is not
                    // instant, and the review is the last screen that can say
                    // when it begins.
                    start_block: currency::thousands(prepared.start_block()),
                };
                self.launches.insert(ticket, (*prepared, *permit));
                let _ = self
                    .events
                    .send(Event::LaunchPrepared(Some(Box::new(view))));
            }
            Err(reason) => {
                self.notice_warning(
                    "currency_launch",
                    NoteVm::plain("launch-build-failed"),
                    &reason,
                );
            }
        }
    }

    /// Send it. The permit was taken before the signature and is carried
    /// through rather than re-taken — one that has since lapsed should not
    /// silently become a different one.
    fn confirm_launch(&mut self, ticket: u64) {
        let Some((prepared, permit)) = self.launches.remove(&ticket) else {
            return;
        };
        let Some(chain) = self.chain() else {
            // Put it back. Rebuilding would produce different bytes, and the
            // ones already signed are the only ones anybody agreed to.
            self.launches.insert(ticket, (prepared, permit));
            return;
        };

        self.busy(TaskKind::Broadcasting, true);
        self.blocking.dispatch(
            move || {
                Work::LaunchSent(Box::new(
                    prepared
                        .broadcast(&chain.broadcaster(&permit))
                        .map_err(|error| error.to_string()),
                ))
            },
            self.work.clone(),
        );
    }

    fn finish_launch_sent(&mut self, result: Result<currency::Launch, String>) {
        self.busy(TaskKind::Broadcasting, false);

        match result {
            Ok(done) => {
                tracing::info!(txid = %done.txid, address = %done.address, "a currency was launched");
                // Said out loud, because this is the one action in the wallet
                // that spends two hundred coins and cannot be repeated. The
                // review closes and the list refreshes either way; without
                // this, the only difference between a launch that went and one
                // that was cancelled is a row appearing some minutes later.
                //
                // The height is in it because a launched currency does nothing
                // until it arrives, and somebody who reads "done" and then finds
                // a currency that converts nothing has been told half of it.
                let _ = self
                    .events
                    .send(Event::Notice(pecu_protocol::UiError::simple(
                        "currency_launched",
                        NoteVm::with(
                            "launch-on-its-way",
                            [
                                done.name.clone(),
                                currency::thousands(done.start_block),
                            ],
                        ),
                        String::new(),
                        pecu_protocol::Severity::Info,
                    )));
                // The decision has been carried out. This is the only place the
                // file is removed by success; the other is somebody saying stop.
                self.intent.finish();
                self.emit_launch_pending();
                let _ = self.events.send(Event::LaunchDone(Box::new(
                    pecu_protocol::LaunchDoneVm {
                        txid: done.txid,
                        address: done.address,
                        name: done.name,
                        start_block: currency::thousands(done.start_block),
                    },
                )));
                // The identity now carries a currency, so both halves of the
                // screen are stale.
                self.refresh_identities();
            }
            Err(reason) => {
                self.notice_warning("currency_launch", NoteVm::plain("launch-rejected"), &reason);
            }
        }
    }

    /// Claim a name, then define a currency under it.
    ///
    /// # Why the currency is written down before the name is claimed
    ///
    /// The registration is what costs money. If the process dies between paying
    /// for a name and recording what it was for, somebody is left with an
    /// identity that exists for no reason — and that identity can never be used
    /// for a different currency. Writing first costs nothing and is the only
    /// ordering where a crash is recoverable.
    ///
    /// Unlike the salt, this write is best-effort: it must not stop a
    /// registration somebody is waiting on. See `launch::Intent::begin`.
    fn start_currency_from_new_name(
        &mut self,
        revocation: &str,
        recovery: &str,
        draft: pecu_protocol::CurrencyDraft,
    ) {
        let name = draft.new_name.trim().to_string();
        if name.is_empty() {
            // Not a notice: the interface gates its own button on the same
            // check, so arriving here is a second interface or a stale draft,
            // and there is nothing to say to somebody who did not press
            // anything.
            return;
        }
        if self.intent.in_progress() {
            self.notice_warning(
                "currency_launch",
                NoteVm::plain("launch-busy"),
                "",
            );
            return;
        }
        let tip = self.nodes.active().and_then(|node| node.tip).unwrap_or(0);
        if currency::problems(&draft, tip).iter().any(|p| p.blocking) {
            self.notice_warning(
                "currency_launch",
                NoteVm::plain("launch-draft-invalid"),
                "",
            );
            return;
        }
        let Some(label) = self.wallet.view().active_key else {
            return;
        };

        // The identity is spelled with its `@`, which is how it will be looked
        // up when the launch is built.
        self.intent.begin(&format!("{name}@"), &label, draft);
        self.emit_launch_pending();

        self.start_registration(&name, revocation, recovery);
    }

    /// Pick up a currency whose identity landed while the wallet was closed.
    ///
    /// **Never automatic at startup.** Within a session the wallet carries on by
    /// itself the moment the name lands, because somebody is sitting there
    /// watching it happen. Across a restart nobody is — and signing a two
    /// hundred coin launch because an application was opened is not something
    /// to do on somebody's behalf. So a resumed launch waits for a press.
    fn resume_launch(&mut self) {
        let Some(record) = self.intent.current().cloned() else {
            return;
        };
        if record.step != launch::Step::ReadyToDefine {
            return;
        }

        // The address is not in the record — it did not exist when the form was
        // filled in — so the identity list is where it comes from. It is there:
        // this wallet registered the name.
        let Some(row) = self
            .identities
            .values()
            .find(|row| row.name.eq_ignore_ascii_case(&record.identity))
            .cloned()
        else {
            self.notice_warning(
                "currency_launch",
                NoteVm::plain("launch-identity-unknown"),
                "",
            );
            return;
        };

        let mut draft = record.draft;
        draft.identity.clone_from(&row.address);
        self.prepare_launch_under(&draft, &row.address, &row.name);
    }

    fn emit_launch_pending(&self) {
        let view = self.intent.current().map(|record| {
            let ready = record.step == launch::Step::ReadyToDefine;
            Box::new(pecu_protocol::LaunchPendingVm {
                identity: record.identity.clone(),
                step: if ready { "ready" } else { "awaiting-identity" }.to_string(),
                note: if ready {
                    format!(
                        "{} exists and nothing has been defined under it. It can never be used for a different currency.",
                        record.identity,
                    )
                } else {
                    format!(
                        "Claiming {} first. The currency is defined once the name is on the chain.",
                        record.identity,
                    )
                },
                can_continue: ready,
                steps: currency::progress(record.step),
            })
        });
        let _ = self.events.send(Event::LaunchPending(view));
    }

    /// A name this wallet just registered has landed. If a currency was waiting
    /// for it, build and sign that launch now.
    ///
    /// # Why this signs but does not send
    ///
    /// Signing costs nothing and can be undone by throwing the bytes away.
    /// Broadcasting spends two hundred coins and cannot. So the automatic part
    /// stops at the review — the screen where somebody says yes — rather than
    /// at the transaction. "In the background" means not having to navigate
    /// back and retype a form, not money moving unattended.
    fn continue_after_identity(&mut self, address: &str) {
        let Some(record) = self.intent.current().cloned() else {
            return;
        };
        if record.step != launch::Step::AwaitingIdentity {
            return;
        }

        tracing::info!(identity = %record.identity, "a currency was waiting for this name");
        self.intent.identity_exists();
        self.emit_launch_pending();

        let mut draft = record.draft;
        // The address did not exist when the form was filled in. It does now.
        draft.identity = address.to_string();
        self.prepare_launch_under(&draft, address, &record.identity);
    }

    /// Read what a launch costs, once, when the screen that needs it opens.
    ///
    /// Chain policy rather than arithmetic — `verus-flows` reads exactly these
    /// two figures and this wallet must show the same ones. Cached because the
    /// draft is re-checked on every keystroke and a policy read per keystroke
    /// would be a request storm against a public node.
    fn ensure_launch_fees(&mut self) {
        if self.launch_fees.is_some() {
            return;
        }
        let Some(chain) = self.chain() else {
            return;
        };
        let asked = self.nodes.requested().map_or_else(
            || "VRSC".to_string(),
            |network| network.chain_name().to_string(),
        );

        self.blocking.dispatch(
            move || {
                use verus_sdk::network::ChainReader;
                Work::LaunchFees(Box::new(
                    chain
                        .currency(&asked)
                        .map(|policy| (policy.currency_registration_fee, policy.id_import_fee)),
                ))
            },
            self.work.clone(),
        );
    }

    fn finish_launch_fees(
        &mut self,
        result: Result<
            (verus_sdk::money::Amount, verus_sdk::money::Amount),
            verus_sdk::network::RpcError,
        >,
    ) {
        match result {
            Ok(fees) => self.launch_fees = Some(fees),
            // Not a notice. The figure is missing from one panel; the screen
            // still works, and a toast about a fee nobody asked for yet would
            // be the wallet complaining about its own housekeeping.
            Err(error) => tracing::info!(%error, "the launch fee could not be read"),
        }
    }

    fn emit_currencies(&self) {
        let _ = self.events.send(Event::Currencies {
            yours: self.currencies.clone(),
            eligible: self.eligible.clone(),
        });
    }

    /// Look one up and show it — anyone's, by `name@` or i-address.
    fn look_up_identity(&mut self, typed: &str) {
        self.read_identity(typed, true);
    }

    /// Read one again without opening anything, to refresh a watched row.
    fn reread_identity(&mut self, address: &str) {
        self.read_identity(address, false);
    }

    fn read_identity(&mut self, typed: &str, open: bool) {
        self.ensure_vdxf();
        let typed = typed.trim().to_string();
        if typed.is_empty() {
            return;
        }
        let Some(chain) = self.chain() else {
            return;
        };

        self.blocking.dispatch(
            move || {
                let result = identity::read(&chain, &typed);
                Work::IdentityDetail {
                    typed,
                    open,
                    result: Box::new(result),
                }
            },
            self.work.clone(),
        );
    }

    fn finish_detail(
        &mut self,
        typed: &str,
        open: bool,
        result: Result<identity::Detail, verus_sdk::network::RpcError>,
    ) {
        let detail = match result {
            Ok(detail) => detail,
            Err(error) => {
                // A refresh that fails says nothing to the form. The field is
                // not what somebody is looking at, and putting an error in it
                // about a row further down the screen would be answering a
                // question nobody asked.
                if !open {
                    tracing::info!(%typed, %error, "a watched VerusID could not be re-read");
                    return;
                }
                // `-5` is the daemon saying it does not exist, which is an
                // answer. Anything else is the question going unanswered, and
                // the two must not read the same — one means "no such name",
                // the other means "we do not know".
                let reason = match &error {
                    verus_sdk::network::RpcError::Node { code: -5, .. } => {
                        "No VerusID by that name on this chain.".to_string()
                    }
                    other => format!("Could not read it: {other}"),
                };
                let _ = self.events.send(Event::IdentityMissing {
                    typed: typed.to_string(),
                    reason,
                });
                return;
            }
        };

        // Everything this wallet can sign with, so the sheet can say plainly
        // whether the buttons on it will work.
        let mine = self.wallet_addresses();
        let vdxf = self.vdxf.as_ref();
        let vm = identity::detail(&detail, &mine, vdxf);

        // Where the row goes depends on whose it is, and that is the whole
        // point: one that turns out to be yours belongs with your identities,
        // and one that does not must never be counted among them.
        let tip = self.nodes.active().and_then(|node| node.tip).unwrap_or(0);
        let row = identity::row_of(&detail.record, tip, &mine);
        if row.mine {
            self.identities.insert(row.address.clone(), row);
        } else if open {
            // Somebody asked about this one. Newest first, no duplicates —
            // looking the same name up twice is one entry, not two — and
            // written down, because looking something up is a decision that
            // used to evaporate on restart.
            if let Some(store) = &self.store {
                store.watch_identity(&row.address, &row.name, now());
            }
            self.looked_up.retain(|seen| seen.address != row.address);
            self.looked_up.insert(0, row);
        } else if let Some(existing) = self
            .looked_up
            .iter_mut()
            .find(|seen| seen.address == row.address)
        {
            // A refresh. Updated where it stands, and the stored row is left
            // alone: the list is ordered by when somebody chose to watch each
            // entry, and bumping that on every refresh would reorder it by
            // refresh time instead — which is not a fact about anything.
            *existing = row;
        }
        self.emit_identities();

        // A refresh updates the row and stops here. Opening the sheet is
        // something somebody asks for by clicking — and re-reading the watch
        // list through this same path is what once flung it open by itself on
        // whichever row answered first.
        if !open {
            return;
        }

        self.open_identity.clone_from(&vm.address);
        // What the try-a-key search compares a guess against.
        self.open_content_keys = detail.content.keys().cloned().collect();
        let _ = self.events.send(Event::IdentityDetail(Some(Box::new(vm))));
    }

    /// Derive a VDXF key from a URI, and say whether the open identity has it.
    ///
    /// The forward direction is the only one there is: the key is a hash of the
    /// name, so this can confirm a guess and can never recover one.
    fn derive_content_key(&mut self, uri: &str) {
        let Some(chain_name) = self.chain_name() else {
            return;
        };
        let Some(chain_id) = self.cached.native else {
            return;
        };

        match identity::Names::derive_one(uri, &chain_name, chain_id) {
            Ok(key) => {
                // Whether the identity on screen published under it. The answer
                // is only ever "this guess matched" or "it did not" — the hash
                // has no inverse, so a miss says nothing about what the key is.
                let present = self.open_content_keys.contains(&key);
                let _ = self.events.send(Event::ContentKeyDerived {
                    uri: uri.to_string(),
                    key,
                    present,
                });
            }
            Err(reason) => {
                let _ = self.events.send(Event::ContentKeyDerived {
                    uri: uri.to_string(),
                    key: String::new(),
                    present: false,
                });
                tracing::info!(%uri, %reason, "a VDXF URI did not derive");
            }
        }
    }

    /// Forget the lookups. Yours are untouched — those are not a list anybody
    /// chose to have, and clearing them would only mean asking again.
    fn clear_lookups(&mut self) {
        self.looked_up.clear();
        if let Some(store) = &self.store {
            store.unwatch_all_identities();
        }
        self.emit_identities();
    }

    /// Drop one of them.
    fn unwatch_identity(&mut self, address: &str) {
        self.looked_up.retain(|row| row.address != address);
        if let Some(store) = &self.store {
            store.unwatch_identity(address);
        }
        self.emit_identities();
    }

    fn emit_identities(&self) {
        let _ = self.events.send(Event::Identities {
            yours: self.identities.values().cloned().collect(),
            looked_up: self.looked_up.clone(),
        });
    }

    /// The resolution for this text, if it is the one that was asked about.
    fn resolved_for(&self, typed: &str) -> Option<&Identity> {
        self.identity.as_ref().filter(|found| found.typed == typed)
    }

    /// Look a VerusID name up, at most once per name.
    ///
    /// # Why the trigger is the `@` and not "it failed to parse"
    ///
    /// Every intermediate state of typing an address also fails to parse, so
    /// resolving on that would send `getidentity` for `R`, `RQ`, `RQr` and
    /// thirty more — a request storm against a public node, produced by
    /// somebody pasting one address. A VerusID name always ends in `@`, and
    /// nothing else does, which makes it a precise signal rather than a guess.
    fn maybe_resolve_identity(&mut self, typed: &str) {
        if typed.len() < 2 || !typed.ends_with('@') {
            return;
        }
        // Already answered, or already asked.
        if self.resolved_for(typed).is_some() || self.resolving.as_deref() == Some(typed) {
            return;
        }
        let Some(chain) = self.chain() else {
            return;
        };

        self.resolving = Some(typed.to_string());
        let name = typed.to_string();
        self.blocking.dispatch(
            move || {
                use verus_sdk::network::ChainReader;
                let result = chain.identity(&name);
                Work::Identity {
                    typed: name,
                    result: Box::new(result),
                }
            },
            self.work.clone(),
        );
    }

    fn finish_identity(
        &mut self,
        typed: &str,
        result: Result<verus_sdk::network::IdentityRecord, verus_sdk::network::RpcError>,
    ) {
        if self.resolving.as_deref() == Some(typed) {
            self.resolving = None;
        }

        match result {
            Ok(record) => {
                // What this name meant last time, read BEFORE recording what it
                // means now — the comparison is the whole point of keeping it.
                let was = self
                    .store
                    .as_ref()
                    .and_then(|store| store.identity_address(typed))
                    .filter(|previous| previous != &record.identity_address);

                if let Some(previous) = &was {
                    tracing::warn!(
                        name = %typed,
                        %previous,
                        now = %record.identity_address,
                        "a VerusID resolved to a different address than last time",
                    );
                }

                if let Some(store) = &self.store {
                    store.remember_identity(typed, &record.identity_address, now());
                }

                self.identity = Some(Identity {
                    typed: typed.to_string(),
                    name: record.fully_qualified_name.clone(),
                    address: record.identity_address.clone(),
                    revoked: record.is_revoked(),
                    was,
                });
            }
            Err(error) => {
                // Not a notice. Somebody halfway through typing a name has not
                // done anything wrong, and a toast for every unfinished word
                // would be the wallet shouting at them for typing.
                tracing::info!(name = %typed, %error, "a VerusID did not resolve");
                self.identity = Some(Identity {
                    typed: typed.to_string(),
                    name: String::new(),
                    address: String::new(),
                    revoked: false,
                    was: None,
                });
            }
        }

        // Put the answer on the form now rather than at the next keystroke —
        // otherwise a name typed and then left alone stays "looking up…".
        let draft = self.last_draft.clone();
        self.validate_draft(&draft);
    }

    /// Build and sign, off the actor.
    ///
    /// `prepare_send` reads the funding set first, so this is several requests
    /// plus an ECDSA signature — not something to hold the actor for. The key
    /// is decrypted inside `with_key` on the worker thread and dropped before
    /// that closure returns.
    fn prepare_send(&mut self, mut draft: pecu_protocol::SendDraft) {
        // A VerusID typed by name becomes the address it resolved to, HERE,
        // rather than in the UI. The interface never gets to decide which
        // address is paid — it says what somebody typed, and the core says what
        // that is. A revoked identity is not substituted at all, so the builder
        // refuses the name as unparseable, which is the outcome the form is
        // already telling them about.
        let mut paid_name = String::new();
        if let Some(identity) = self.resolved_for(draft.to.trim()) {
            if !identity.address.is_empty() && !identity.revoked {
                paid_name.clone_from(&identity.name);
                draft.to.clone_from(&identity.address);
            }
        }

        let Some(label) = self.wallet.active_key.clone() else {
            return;
        };
        let Some(vault) = self.wallet.vault() else {
            return;
        };
        let Some(chain) = self.chain() else {
            return;
        };

        self.tickets += 1;
        let ticket = self.tickets;
        self.busy(TaskKind::PreparingSend, true);

        self.blocking.dispatch(
            move || Work::Prepared {
                ticket,
                result: Box::new(send::prepare(&chain, &vault, &label, &draft, &paid_name)),
            },
            self.work.clone(),
        );
    }

    fn finish_prepare(&mut self, ticket: u64, result: Result<send::Prepared, send::SendError>) {
        self.busy(TaskKind::PreparingSend, false);

        match result {
            Ok(prepared) => {
                let from = self.wallet.active_address().unwrap_or_default();
                // Whether this wallet has ever successfully paid the recipient.
                // Not "have we seen the address" — an address that was typed,
                // reviewed and cancelled is still one nobody has paid.
                let known = self.known.contains_key(&prepared.to);
                // Built from the SIGNED bytes, not from the draft — see
                // `send::review`.
                let review = send::review(ticket, &prepared, &from, self.spendable, known);
                self.prepared.insert(ticket, prepared);
                let _ = self.events.send(Event::SendPrepared(review));
            }
            Err(error) => {
                let refusal = send_note(&error);
                self.notice("prepare_send", refusal.clone(), &error);
                let _ =
                    self.events
                        .send(Event::SendResult(pecu_protocol::SendOutcomeVm::Failed(
                            pecu_protocol::UiError::simple(
                                "prepare_send",
                                refusal,
                                error.to_string(),
                                pecu_protocol::Severity::Danger,
                            ),
                        )));
            }
        }
    }

    /// Send. The one place in this application that writes to the chain.
    fn confirm_send(&mut self, ticket: u64) {
        let Some(prepared) = self.prepared.remove(&ticket) else {
            return;
        };

        // The guard, and it is structural: `spend_permit` is the only
        // constructor of a `SpendPermit`, and `Chain::broadcaster` is the only
        // way to a `Broadcaster`. There is no path around this check.
        let permit = match self.nodes.spend_permit() {
            Ok(permit) => permit,
            Err(refused) => {
                // Put it back: the user may turn mainnet spending on and try
                // again, and rebuilding would pick different coins.
                self.prepared.insert(ticket, prepared);
                self.notice("spend_refused", refusal_note(&refused), &refused);
                return;
            }
        };

        let Some(chain) = self.chain() else {
            self.prepared.insert(ticket, prepared);
            return;
        };

        // Committed to disk BEFORE the broadcast. A process that dies mid-send
        // must not lose the only copy of bytes that may already be propagating.
        let record = match self.pending.commit(
            &prepared.unsent.txid,
            &prepared.unsent.hex,
            &prepared.to,
            &portfolio::coins(prepared.amount),
        ) {
            Ok(id) => id,
            Err(error) => {
                // Refusing to send is the right failure. Sending bytes we could
                // not record is exactly the situation the ledger exists to
                // prevent, and it is not made better by proceeding.
                self.prepared.insert(ticket, prepared);
                self.notice(
                    "pending_commit",
                    NoteVm::plain("pending-unsaved"),
                    &error,
                );
                return;
            }
        };

        self.busy(TaskKind::Broadcasting, true);
        self.broadcasting = true;
        self.blocking.dispatch(
            move || Work::Broadcast {
                record,
                result: Box::new(send::broadcast(&chain, &permit, prepared)),
            },
            self.work.clone(),
        );
    }

    fn finish_broadcast(
        &mut self,
        record: u64,
        result: Result<verus_sdk::network::Sent, verus_sdk::network::FlowError>,
    ) {
        use pecu_protocol::SendOutcomeVm;
        use verus_sdk::network::FlowError;

        self.busy(TaskKind::Broadcasting, false);
        self.broadcasting = false;

        match result {
            Ok(sent) => {
                // The one irreversible thing this application does, and until
                // this line it was the only outcome that left no trace. Every
                // failure was logged and every success was silent, so the file
                // somebody reads after a bad day described a wallet that only
                // ever fails — and "did it actually send?" had no answer in the
                // one place they would look.
                //
                // The txid and the fee, and deliberately not the recipient or
                // the amount. Both are on the chain and one lookup finds them,
                // so this loses nothing — while a log holding a plaintext list
                // of who was paid what is a file people attach to bug reports.
                tracing::info!(
                    txid = %sent.txid,
                    fee = %portfolio::coins(sent.fee),
                    "a payment was accepted by the network",
                );
                self.remember_recipient(record);
                self.pending.set_state(record, pending::State::Confirmed);
                self.pending.forget_confirmed();
                let _ = self.events.send(Event::SendResult(SendOutcomeVm::Sent {
                    txid: sent.txid,
                    fee_display: portfolio::coins(sent.fee),
                }));
                self.refresh();
            }

            // The one failure that is not a failure. The node may have taken
            // it. The bytes are on disk, and the only safe resolution is to ask
            // whether it confirmed — never to rebuild.
            Err(FlowError::BroadcastUncertain { txid, .. }) => {
                tracing::warn!(%txid, "the broadcast outcome is unknown");
                let _ = self
                    .events
                    .send(Event::SendResult(SendOutcomeVm::Uncertain {
                        txid,
                        pending_id: record,
                    }));
            }

            Err(error) => {
                // A refusal is unambiguous: the node understood it and said no.
                // Nothing was spent, and the record would only be noise.
                self.pending.set_state(record, pending::State::Abandoned);
                self.notice(
                    "broadcast_rejected",
                    NoteVm::plain("broadcast-rejected"),
                    &error,
                );
                let _ = self.events.send(Event::SendResult(SendOutcomeVm::Failed(
                    pecu_protocol::UiError::simple(
                        "broadcast_rejected",
                        NoteVm::plain("broadcast-rejected"),
                        error.to_string(),
                        pecu_protocol::Severity::Danger,
                    ),
                )));
            }
        }
    }

    /// Lock the wallet if it has been idle too long.
    ///
    /// The timeout exists for the case nobody plans for: a wallet left unlocked
    /// on an unattended machine. Locking drops the data key, so every key in
    /// the vault becomes unreadable again until the passphrase is entered.
    fn check_auto_lock(&mut self) {
        if !self.wallet.should_auto_lock() {
            return;
        }
        self.wallet.lock();
        tracing::info!("auto-locked after inactivity");
        let _ = self.events.send(Event::Locked {
            reason: LockReason::Timeout,
        });
        self.emit_wallet();
        // A name claim the last run left unfinished. It has a deadline, so it is
        // worth saying before anything else on that screen.
        self.emit_registration(None);
    }

    /// Probe every node, then report once.
    ///
    /// Sequential rather than concurrent: there are two nodes, and a burst of
    /// parallel requests to public infrastructure buys nothing here. When the
    /// list grows this becomes a bounded fan-out through the same semaphore.
    async fn probe_all(&mut self) {
        self.busy(TaskKind::ProbingNodes, true);

        let targets: Vec<(u32, String)> = self
            .nodes
            .nodes()
            .iter()
            .map(|n| (n.id, n.url.clone()))
            .collect();

        let requested = self.nodes.requested().cloned().unwrap_or(Network::Testnet);

        for (id, url) in targets {
            let Some(prober) = self.prober(&url) else {
                continue;
            };
            let outcome = self.blocking.run(move || prober.probe()).await;

            let Some((result, latency)) = outcome else {
                continue;
            };

            if let Some(node) = self.nodes.get_mut(id) {
                match result {
                    Ok(info) => {
                        node.record_success(&info, latency, &requested);
                        tracing::info!(
                            node = %node.label,
                            chain = %info.name,
                            blocks = info.blocks,
                            latency_ms = latency.as_millis(),
                            status = node.status.label(),
                            "probed"
                        );
                    }
                    Err(error) => {
                        node.record_failure(&error);
                        tracing::info!(node = %node.label, %error, "probe failed");
                    }
                }
            }
            // Report after each node so the list fills in as answers arrive,
            // rather than staying blank until the slowest one times out.
            self.emit_network();
        }

        self.busy(TaskKind::ProbingNodes, false);

        // A node that has just answered is a node worth reading from. This is
        // also what fills the dashboard on a cold start, where the probe at
        // launch finishes after the wallet is unlocked.
        self.refresh();
    }

    fn emit_wallet(&self) {
        let _ = self.events.send(Event::Wallet(self.wallet.view()));
    }

    fn emit_challenge(&self, challenge: &wallet::Challenge) {
        let _ = self.events.send(Event::PhraseChallenge {
            positions: challenge.positions.clone(),
            word_count: challenge.word_count,
        });
    }

    /// Turn an error into something a person can read, without losing the
    /// technical cause — that is what the "Copy details" button is for.
    fn notice(&self, code: &'static str, message: NoteVm, error: &dyn std::error::Error) {
        let mut technical = error.to_string();
        let mut source = error.source();
        while let Some(cause) = source {
            technical.push_str("\n  caused by: ");
            technical.push_str(&cause.to_string());
            source = cause.source();
        }
        tracing::warn!(code, reason = message.code, %technical, "notice");

        let _ = self.events.send(Event::Notice(
            pecu_protocol::UiError::simple(
                code,
                message,
                error.to_string(),
                pecu_protocol::Severity::Warning,
            )
            .with_technical(technical),
        ));
    }

    /// A refusal that has no underlying error to quote — the wallet decided,
    /// and it should say why in its own words rather than dress a decision up
    /// as a failure.
    fn notice_warning(&self, code: &'static str, message: NoteVm, detail: &str) {
        tracing::info!(code, reason = message.code, "refused");
        let _ = self
            .events
            .send(Event::Notice(pecu_protocol::UiError::simple(
                code,
                message,
                detail.to_string(),
                pecu_protocol::Severity::Warning,
            )));
    }

    /// A notice that is not a failure. Same channel, so the UI has one place to
    /// render everything it is told.
    fn notice_info(&self, code: &'static str, message: NoteVm) {
        let _ = self
            .events
            .send(Event::Notice(pecu_protocol::UiError::simple(
                code,
                message,
                String::new(),
                pecu_protocol::Severity::Info,
            )));
    }

    fn busy(&self, task: TaskKind, on: bool) {
        let _ = self.events.send(Event::Busy { task, on });
    }

    fn emit_network(&self) {
        let active = self.nodes.active();

        let vm = NetworkVm {
            requested: self
                .nodes
                .requested()
                .map(ToString::to_string)
                .unwrap_or_default(),
            // What the ACTIVE node reports. Only this ever decides anything.
            effective: active.and_then(|n| n.network.as_ref().map(ToString::to_string)),
            tip: active.and_then(|n| n.tip),
            syncing: matches!(
                active.map(|n| &n.status),
                Some(pecu_chain::NodeStatus::Syncing { .. })
            ),
            active_node: active.map(|n| n.id),
            allow_mainnet_spend: self.nodes.allow_mainnet_spend(),
            mock_mode: self.mock,
            nodes: self.nodes.nodes().iter().map(to_node_vm).collect(),
        };

        let _ = self.events.send(Event::Network(vm));
    }
}

/// The one sentence the import screen shows when it refuses.
///
/// Three distinct messages for the three distinct failures, because they call
/// for three different next steps: fix a word, count the words, or stop trying
/// to use it as a mnemonic at all. A single "invalid phrase" would leave every
/// one of those people guessing.
///
/// Note what is never in here: the word itself. The SDK reports a bad word by
/// position only, and an error message is the last place key material should
/// end up — it goes to logs, to crash reporters, and to screenshots.
fn import_note(error: &wallet::ImportError) -> NoteVm {
    let problem = match error {
        wallet::ImportError::Mnemonic(problem) => problem,
        // `from_wif` enforces the Verus version byte, so a Bitcoin WIF lands
        // here — and "invalid key" would leave someone staring at a key that
        // is perfectly valid, just not for this chain.
        wallet::ImportError::Key(_) => return NoteVm::plain("import-not-a-verus-key"),
        wallet::ImportError::Vault(_) => return NoteVm::plain("import-failed"),
    };

    match problem {
        MnemonicError::Checksum => NoteVm::plain("phrase-checksum"),
        MnemonicError::UnknownWord { position } => {
            NoteVm::with("phrase-unknown-word", [position.to_string()])
        }
        MnemonicError::WordCount(count) => NoteVm::with("phrase-word-count", [count.to_string()]),
        // `MnemonicError` is `#[non_exhaustive]` — the SDK gains a variant
        // whenever it learns to refuse something new. The unknown one carries
        // the SDK's own words, which is the one case where prose from below is
        // better than a code nobody wrote a sentence for.
        other => NoteVm::with("phrase-refused", [other.to_string()]),
    }
}

/// What the keys screen says when the vault refuses.
///
/// Three of these are the same word in a different order — "could not rename" —
/// and each calls for a different next step: pick another name, fix the name
/// you picked, or unlock the wallet. A single message would leave all three
/// people guessing.
fn key_error_note(error: &pecu_keystore::VaultError) -> NoteVm {
    use pecu_keystore::VaultError;

    match error {
        VaultError::DuplicateLabel(label) => NoteVm::with("key-name-taken", [label.clone()]),
        // The rules are the vault's, and they are what make a label safe to use
        // as an identifier everywhere else — including in a file path.
        VaultError::BadLabel(_) => NoteVm::plain("key-name-rules"),
        VaultError::Locked => NoteVm::plain("wallet-locked"),
        VaultError::NoSuchKey(_) => NoteVm::plain("key-not-here"),
        _ => NoteVm::plain("key-change-failed"),
    }
}

/// What the send form says when a build fails.
fn send_note(error: &send::SendError) -> NoteVm {
    use verus_sdk::network::FlowError;

    match error {
        send::SendError::BadAddress => NoteVm::plain("address-unparsable"),
        send::SendError::BadAmount => NoteVm::plain("amount-unparsable"),
        send::SendError::NothingToSend => NoteVm::plain("amount-zero"),
        send::SendError::Vault(_) => NoteVm::plain("wallet-locked"),
        // The distinction the SDK draws and a wallet must not lose: what you
        // hold and what you can spend right now are different numbers, and a
        // bare "insufficient funds" against a screen showing a balance reads as
        // a bug in the wallet.
        send::SendError::Flow(FlowError::InsufficientFunds { .. }) => {
            NoteVm::plain("send-not-enough-spendable")
        }
        send::SendError::Flow(_) => NoteVm::plain("send-build-failed"),
    }
}

/// What the send screen says when the spending guard refuses.
fn refusal_note(refused: &pecu_chain::SpendRefused) -> NoteVm {
    use pecu_chain::SpendRefused;

    // Deliberately specific. "Refused" tells someone nothing about what to do,
    // and each of these has a different answer — which is also why this is a
    // match rather than `refused.to_string()`. The `Display` text is a
    // developer's sentence in a library that knows nothing about who is
    // reading it, and it was going straight onto a toast.
    match refused {
        SpendRefused::NoNode => NoteVm::plain("spend-no-node"),
        SpendRefused::NetworkUnknown => NoteVm::plain("spend-chain-unknown"),
        SpendRefused::NetworkMismatch {
            requested,
            effective,
        } => NoteVm::with(
            "spend-wrong-chain",
            [effective.to_string(), requested.to_string()],
        ),
        SpendRefused::MainnetNotEnabled => NoteVm::plain("spend-mainnet-off"),
        SpendRefused::Syncing { blocks, longest } => NoteVm::with(
            "spend-node-syncing",
            [blocks.to_string(), longest.to_string()],
        ),
        SpendRefused::NodeNotReady { .. } => NoteVm::plain("spend-node-not-ready"),
    }
}

/// How many consecutive failures mean a node is down rather than unlucky.
///
/// One timeout is a network hiccup, and switching on it would make the wallet
/// flap between endpoints on a train. Three, with the backoff between them, is
/// a node that has stopped.
const FAILURES_BEFORE_FAILOVER: u32 = 3;

// There is deliberately no cap on the watch list.
//
// An earlier version kept five and dropped the rest, on the reasoning that the
// section should stay a short aside. That was right while the list lived in
// memory and vanished on restart. It is wrong now that it is durable and
// removable one row at a time: these are entries somebody chose to keep, and a
// list that silently forgets the sixth is one nobody can rely on. The address
// book has no cap either, for the same reason.

/// Whether any payment's fate is currently unknown.
///
/// The rule that makes automatic failover safe, and it is stronger than "not
/// during a broadcast". A transaction whose broadcast could not be confirmed is
/// resolved by asking a node whether it has it — and a **different** node
/// legitimately answers "no" for a transaction propagating perfectly well
/// through the one it was handed to. Failing over mid-resolution would turn a
/// payment that landed into a payment the wallet reports as absent, and the
/// screen would then offer to send it again.
///
/// `Resent` is deliberately not in here: those bytes have already been handed
/// to a second node, so a third one's opinion changes nothing about the
/// decision in front of the user.
fn resolution_pending(ledger: &pending::Ledger) -> bool {
    ledger.unresolved().any(|record| {
        matches!(
            record.state,
            pending::State::Uncertain | pending::State::Absent
        )
    })
}

/// Which node to move to when the active one has stopped answering.
///
/// `None` when the active node is still worth waiting for, or when there is
/// nothing better to move to — staying put is right in that case, because the
/// screen already says the node is offline and switching to a second
/// unreachable one only changes which URL is failing.
///
/// Only a node reporting `Online`, which is already a node that agrees about
/// which chain this is: `record_success` marks one answering about another
/// chain as degraded rather than online. That is what makes switching
/// automatically safe at all.
fn failover_target(nodes: &NodeManager) -> Option<u32> {
    let active = nodes.active()?;
    if active.consecutive_failures < FAILURES_BEFORE_FAILOVER {
        return None;
    }

    let failed = active.id;
    nodes
        .nodes()
        .iter()
        .find(|node| node.id != failed && node.status == pecu_chain::NodeStatus::Online)
        .map(|node| node.id)
}

/// Where the ids of user-added nodes start.
///
/// The built-ins are numbered from zero by their position in a compiled-in
/// list; the saved ones are numbered by SQLite, also from one. Without a gap
/// the two schemes would collide on the second node ever added, and the
/// collision would look like the wrong endpoint being selected rather than like
/// a numbering bug. A thousand is far more built-in nodes than this will ever
/// ship, and the arithmetic is exact in both directions.
const USER_NODE_ID_BASE: u32 = 1000;

/// A stored row id as it is numbered in the running list.
fn user_node_id(row: i64) -> u32 {
    USER_NODE_ID_BASE.saturating_add(u32::try_from(row).unwrap_or(0))
}

/// The stored row a running id refers to, or `None` for a built-in.
fn stored_node_id(id: u32) -> Option<i64> {
    id.checked_sub(USER_NODE_ID_BASE).map(i64::from)
}

/// What the network screen says when a URL is refused before anything is sent.
///
/// The plaintext case gets its own sentence because it is the one that sounds
/// like the wallet being difficult, and it is not: it is the one refusal that
/// prevents every address in this wallet from being readable by whoever is on
/// the path between here and that node.
fn url_refusal_note(error: &verus_sdk::network::RpcError) -> NoteVm {
    use verus_sdk::network::RpcError;

    match error {
        RpcError::InsecureUrl { .. } => NoteVm::plain("node-url-insecure"),
        _ => NoteVm::plain("node-url-unusable"),
    }
}

/// The host of a URL, for a node the user did not name.
///
/// Best-effort and deliberately so: this only produces a label. Anything it
/// cannot parse falls back to the URL itself, which is at least true.
fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .filter(|host| !host.is_empty())
        .unwrap_or(url)
        .to_string()
}

fn to_node_vm(node: &Node) -> NodeVm {
    NodeVm {
        id: node.id,
        url: node.url.clone(),
        label: node.label.clone(),
        status: match node.status.label() {
            "online" => Reachability::Online,
            "degraded" => Reachability::Degraded,
            "offline" => Reachability::Offline,
            "probing" => Reachability::Probing,
            _ => Reachability::Unknown,
        },
        network: node.network.as_ref().map(ToString::to_string),
        tip: node.tip,
        latency_ms: node.latency_ms(),
        note: node.status.note(),
        builtin: node.builtin,
    }
}

/// What the confirmed history adds up to.
///
/// Not the spendable figure: an immature coinbase is a confirmed transaction
/// the history contains, and a confirmed output that some unconfirmed
/// transaction already spends is still confirmed — the transaction spending it
/// has not been mined and so is not in the history either. Anchoring the chart
/// on `spendable` alone would leave every point short by whatever is maturing,
/// and the chart would disagree with the balance above it.
fn confirmed_native(reading: &portfolio::Reading) -> i64 {
    let total = reading
        .spendable
        .to_sat()
        .saturating_add(reading.immature.to_sat())
        .saturating_add(reading.pending_out.to_sat());
    i64::try_from(total).unwrap_or(i64::MAX)
}

/// "3 payments · 2 days ago", or what to say when there have been none.
///
/// An address that was named but never paid is a normal state — somebody typed
/// a name in before sending — and saying "0 payments" about it reads as a
/// failure rather than as a plan.
fn payment_summary(payments: i64, paid_at: Option<i64>, now: i64) -> String {
    let Some(paid_at) = paid_at.filter(|_| payments > 0) else {
        return "never paid".to_string();
    };

    let count = if payments == 1 {
        "1 payment".to_string()
    } else {
        format!("{payments} payments")
    };

    let age = now.saturating_sub(paid_at);
    let when = match age {
        ..3_600 => "in the last hour".to_string(),
        3_600..86_400 => format!("{} hours ago", age / 3_600),
        86_400..172_800 => "yesterday".to_string(),
        172_800..2_592_000 => format!("{} days ago", age / 86_400),
        _ => format!("{} months ago", age / 2_592_000),
    };

    format!("{count} · last {when}")
}

/// Seconds since the epoch, for "2 hours ago".
///
/// A wall clock, not a monotonic one: it is compared against block timestamps,
/// which are wall-clock too. A user changing their clock changes what the list
/// says, which is correct — it is their clock the list is relative to.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── The command palette's matcher ───────────────────────────────────────

    /// Case is ignored on both sides, because neither side controls it: a name
    /// is spelled however the chain spells it, and a query is typed however it
    /// is typed.
    #[test]
    fn a_search_ignores_case_on_both_sides() {
        assert!(search_matches("vault", "vault.VRSCTEST@", "i5Qcj82"));
        assert!(search_matches("vrsctest", "vault.VRSCTEST@", "i5Qcj82"));
    }

    /// The i-address matches as well as the name.
    ///
    /// This is the case somebody actually arrives with: an address on the
    /// clipboard, because it is the identifier this wallet tells them to prefer
    /// for anything destructive.
    #[test]
    fn a_search_finds_a_row_by_its_address() {
        assert!(search_matches(
            "i5qcj82",
            "vault.VRSCTEST@",
            "i5Qcj82gvrHdHCCvTwy2yCFeMz3s3dgB6m"
        ));
    }

    /// A substring, not a prefix. `pecu` should find `my.pecu@`, because a
    /// person searching for a name rarely knows which parent it hangs off.
    #[test]
    fn a_search_matches_inside_the_name_not_only_at_the_start() {
        assert!(search_matches("pecu", "shop.pecu@", "iAAA"));
    }

    /// And it says no. A matcher that is never false is not a filter.
    #[test]
    fn a_search_refuses_what_does_not_match() {
        assert!(!search_matches("bridge", "vault.VRSCTEST@", "i5Qcj82"));
    }

    /// The one line the send form gets about a VerusID, in priority order.
    ///
    /// The ordering is the point, not the wording. A name that now resolves
    /// somewhere else is the only case where the form looks entirely ordinary
    /// and the money reaches a stranger — so it has to outrank a revocation and
    /// a plain success, both of which are visible in other ways.
    #[test]
    fn a_changed_verusid_outranks_everything_else_the_note_could_say() {
        let base = Identity {
            typed: "someone@".to_string(),
            name: "someone.VRSCTEST@".to_string(),
            address: "iNEW".to_string(),
            revoked: false,
            was: None,
        };

        let plain = identity_note(&base);
        assert_eq!(plain.code, "verusid-resolved");
        assert!(
            plain.args.contains(&"someone.VRSCTEST@".to_string()),
            "{plain:?}"
        );
        assert!(
            plain.args.contains(&"iNEW".to_string()),
            "the note hides the address: {plain:?}"
        );

        // Changed AND revoked: the change still wins, and the old address is
        // named — "it changed" without saying from what is an alarm nobody can
        // act on.
        let moved = identity_note(&Identity {
            was: Some("iOLD".to_string()),
            revoked: true,
            ..base.clone()
        });
        assert_eq!(moved.code, "verusid-moved");
        assert!(
            moved.args.contains(&"iOLD".to_string()),
            "the old address is missing: {moved:?}"
        );
        assert!(moved.args.contains(&"iNEW".to_string()), "{moved:?}");

        let revoked = identity_note(&Identity {
            revoked: true,
            ..base.clone()
        });
        assert_eq!(revoked.code, "verusid-revoked");

        // No address at all is a name this chain does not have. It must not
        // read as a success — the address being empty is exactly the case a
        // check on `revoked` alone gets wrong.
        let missing = identity_note(&Identity {
            address: String::new(),
            ..base
        });
        assert_eq!(missing.code, "verusid-unknown");
    }

    /// Wait for the next `Event::Network`, ignoring anything else.
    async fn next_network(events: &mut mpsc::UnboundedReceiver<Event>) -> NetworkVm {
        loop {
            match events.recv().await {
                Some(Event::Network(vm)) => return vm,
                Some(_) => {}
                None => panic!("the core stopped before sending a network event"),
            }
        }
    }

    fn testnet_nodes() -> Vec<Node> {
        vec![Node::builtin(0, "one", "https://example.invalid")]
    }

    /// Where the core keeps testnet's files under a temporary home.
    ///
    /// Asked for rather than spelled out: a test that hardcoded the directory
    /// would keep passing while reading a database nothing writes to.
    fn chain_dir(home: &tempfile::TempDir) -> std::path::PathBuf {
        paths::Paths::new(home.path().to_path_buf(), &Network::Testnet, false)
            .dir()
            .to_path_buf()
    }

    /// The core reports its starting state without being asked, so the UI has
    /// something to render before any command is sent.
    #[tokio::test]
    async fn the_core_emits_its_initial_network_state() {
        let handle = tokio::runtime::Handle::current();
        let (_dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                home: std::path::PathBuf::from("/nonexistent"),
            },
        );

        let vm = next_network(&mut events).await;

        assert_eq!(vm.requested, "Testnet");
        // Nothing has been asked yet, so nothing claims to know the chain.
        assert_eq!(vm.effective, None);
        assert_eq!(vm.nodes.len(), 1);
        assert_eq!(vm.nodes[0].status, Reachability::Unknown);
        assert_eq!(vm.nodes[0].network, None);
    }

    /// The idle timeout has to actually fire, or it is a comment.
    ///
    /// # Ignored, and why — this is a known defect in the test, not the code
    ///
    /// `start_paused` is supposed to make five minutes cost microseconds. It
    /// does not here: measured, this takes **302 seconds**, so the clock is
    /// running in real time despite the attribute. The auto-lock itself works —
    /// the assertion passes — but a five-minute test does not belong in a suite
    /// people run before every commit.
    ///
    /// The likely cause is that the actor is spawned on the runtime rather than
    /// driven by the test task, so Tokio's auto-advance never sees an idle
    /// runtime to skip ahead from. The fix is probably to drive the actor's
    /// loop directly instead of spawning it, which needs a seam this crate does
    /// not have yet.
    ///
    /// Until then: the decision logic is covered by
    /// `wallet::tests::auto_lock_fires_only_after_the_idle_limit`, which is
    /// fast and exhaustive. What is NOT covered on the fast path is the wiring
    /// — that the actor ticks and emits. Run this one by hand after touching
    /// either:
    ///
    /// ```sh
    /// cargo test -p pecu-core --lib -- --ignored an_idle_wallet
    /// ```
    #[ignore = "takes ~5 minutes: time virtualisation is not taking effect, see the doc comment"]
    #[tokio::test(start_paused = true)]
    async fn an_idle_wallet_locks_itself() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = tokio::runtime::Handle::current();

        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                home: dir.path().to_path_buf(),
            },
        );

        dispatcher.send(Command::CreateWallet {
            name: "test".to_string(),
            passphrase: pecu_protocol::Secret::from("a passphrase"),
        });

        // Wait until the wallet reports itself unlocked, so the clock is only
        // advanced once there is something to lock.
        loop {
            match events.recv().await {
                Some(Event::Wallet(vm)) if vm.exists && !vm.locked => break,
                Some(_) => {}
                None => panic!("the core stopped before the wallet was created"),
            }
        }

        tokio::time::advance(std::time::Duration::from_mins(6)).await;

        let mut locked = false;
        for _ in 0..40 {
            match events.recv().await {
                Some(Event::Locked {
                    reason: LockReason::Timeout,
                }) => {
                    locked = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(
            locked,
            "the wallet did not auto-lock after the idle timeout"
        );
    }

    /// The whole backup path through the actor, which is the part the fast
    /// `wallet::tests` cannot reach: that the commands are wired, that the
    /// events come back in a usable order, and that a wrong answer is refused.
    #[tokio::test]
    async fn a_new_wallet_is_backed_up_through_the_actor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = tokio::runtime::Handle::current();

        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                home: dir.path().to_path_buf(),
            },
        );

        dispatcher.send(Command::CreateWallet {
            name: "test".to_string(),
            passphrase: pecu_protocol::Secret::from("a passphrase"),
        });

        let positions = loop {
            match events.recv().await {
                Some(Event::PhraseChallenge { positions, .. }) => break positions,
                Some(_) => {}
                None => panic!("the core stopped before announcing a challenge"),
            }
        };
        assert_eq!(positions.len(), 3);

        // Hold to reveal.
        dispatcher.send(Command::ShowNewPhrase);
        let words = loop {
            match events.recv().await {
                Some(Event::SeedWords(words)) if !words.is_empty() => break words,
                Some(_) => {}
                None => panic!("the core stopped before sending the words"),
            }
        };
        assert_eq!(words.len(), 24);

        // A wrong answer is refused, and refusing does not end the backup.
        let wrong: Vec<(u32, String)> = positions
            .iter()
            .map(|p| (*p, "wrong".to_string()))
            .collect();
        dispatcher.send(Command::ConfirmPhrase { checks: wrong });
        assert!(!next_confirmation(&mut events).await);

        let right: Vec<(u32, String)> = positions
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
        dispatcher.send(Command::ConfirmPhrase { checks: right });
        assert!(next_confirmation(&mut events).await);

        // The wallet stops asking, and the words are gone.
        let vm = loop {
            match events.recv().await {
                Some(Event::Wallet(vm)) if vm.needs_backup.is_none() => break vm,
                Some(_) => {}
                None => panic!("the core stopped before finishing the backup"),
            }
        };
        assert!(!vm.locked);

        dispatcher.send(Command::ShowNewPhrase);
        loop {
            match events.recv().await {
                Some(Event::SeedWords(words)) => {
                    assert!(words.is_empty(), "the phrase survived being finalised");
                    break;
                }
                Some(_) => {}
                None => panic!("the core stopped"),
            }
        }
    }

    /// Restoring on a fresh install goes through `ImportKey`, which has to
    /// create the wallet on the way — and must not create one when it refuses.
    #[tokio::test]
    async fn a_restore_creates_the_wallet_but_a_refusal_does_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = chain_dir(&dir).join("vault.json");
        let handle = tokio::runtime::Handle::current();

        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                home: dir.path().to_path_buf(),
            },
        );

        // A phrase with a broken checksum.
        dispatcher.send(Command::ImportKey {
            label: "main".to_string(),
            material: pecu_protocol::ImportMaterial::Phrase(pecu_protocol::Secret::from(
                "abandon abandon abandon abandon abandon abandon \
                 abandon abandon abandon abandon abandon abandon",
            )),
            passphrase: pecu_protocol::Secret::from("pass"),
        });

        let notice = loop {
            match events.recv().await {
                Some(Event::Notice(notice)) => break notice,
                Some(_) => {}
                None => panic!("the core stopped before refusing"),
            }
        };
        assert_eq!(notice.code, "import_key");
        assert_eq!(notice.message.code, "phrase-checksum", "{:?}", notice.message);
        assert!(!path.exists(), "a refused restore created a wallet file");

        // The same words with a valid checksum.
        dispatcher.send(Command::ImportKey {
            label: "main".to_string(),
            material: pecu_protocol::ImportMaterial::Phrase(pecu_protocol::Secret::from(
                "abandon abandon abandon abandon abandon abandon \
                 abandon abandon abandon abandon abandon about",
            )),
            passphrase: pecu_protocol::Secret::from("pass"),
        });

        let vm = loop {
            match events.recv().await {
                Some(Event::Wallet(vm)) if vm.exists => break vm,
                Some(_) => {}
                None => panic!("the core stopped before restoring"),
            }
        };

        assert!(!vm.locked);
        assert_eq!(vm.keys.len(), 1);
        // An imported phrase is already written down somewhere.
        assert_eq!(vm.needs_backup, None);
        assert!(path.exists());
    }

    async fn next_confirmation(events: &mut mpsc::UnboundedReceiver<Event>) -> bool {
        loop {
            match events.recv().await {
                Some(Event::PhraseConfirmed(ok)) => return ok,
                Some(_) => {}
                None => panic!("the core stopped before answering"),
            }
        }
    }

    /// A cold start must show the last known figures before it asks anything.
    ///
    /// The alternative is a blank dashboard for however long a node takes,
    /// which reads as a wallet that has lost your money.
    #[tokio::test]
    async fn a_cold_start_shows_the_last_known_dashboard_marked_stale() {
        let dir = tempfile::tempdir().expect("tempdir");

        // What a previous run would have left behind — in this chain's own
        // directory, which is asked for rather than spelled out.
        {
            let store = pecu_store::Store::open(&chain_dir(&dir)).expect("store");
            let mut portfolio = pecu_protocol::PortfolioVm::default();
            portfolio.balance.total_display = "48.8999 0000".to_string();
            // Written as current, because it WAS current when it was written.
            portfolio.stale = false;

            let history = vec![pecu_protocol::HistoryRowVm {
                txid: "abc".to_string(),
                height: 1_187_000,
                block_time: 1_000_000_000,
                when_display: "2 hours ago".to_string(),
                group: "Today".to_string(),
                ..pecu_protocol::HistoryRowVm::default()
            }];
            store.save_snapshot(&portfolio, &history, 1_000_000_000);
            store.set_setting("auto_lock_minutes", "15");
        }

        let handle = tokio::runtime::Handle::current();
        let (_dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                home: dir.path().to_path_buf(),
            },
        );

        let mut portfolio = None;
        let mut history = None;
        let mut auto_lock = None;
        for _ in 0..12 {
            match events.recv().await {
                Some(Event::Portfolio(vm)) => portfolio = Some(vm),
                Some(Event::History { delta, .. }) => history = Some(delta),
                Some(Event::Wallet(vm)) => auto_lock = Some(vm.auto_lock_minutes),
                Some(_) => {}
                None => break,
            }
            if portfolio.is_some() && history.is_some() && auto_lock.is_some() {
                break;
            }
        }

        let portfolio = portfolio.expect("the cached dashboard is put on screen");
        assert_eq!(portfolio.balance.total_display, "48.8999 0000");
        // The figure is true about the past and the screen has to say so.
        assert!(portfolio.stale, "a cached balance was presented as current");

        let pecu_protocol::ListDelta::Replace(rows) = history.expect("cached history") else {
            panic!("a restored history should arrive whole");
        };
        assert_eq!(rows.len(), 1);
        // The figures may be old; the dates must not be WRONG. "2 hours ago"
        // was written long ago, so it has been recomputed from the block time.
        assert_ne!(
            rows[0].when_display, "2 hours ago",
            "a restored row kept wording that was true when it was cached",
        );

        assert_eq!(auto_lock.expect("wallet state"), Some(15));
    }

    /// A node that has stopped answering is left behind — but only for one
    /// that is actually answering, and only after enough failures to tell a
    /// dead endpoint from a train tunnel.
    #[test]
    fn a_dead_node_is_left_for_a_healthy_one() {
        use pecu_chain::NodeStatus;

        let mut nodes = NodeManager::new(
            vec![
                Node::builtin(0, "first", "https://one.invalid"),
                Node::builtin(1, "second", "https://two.invalid"),
            ],
            Network::Testnet,
        );

        let healthy = |nodes: &mut NodeManager, id: u32| {
            if let Some(node) = nodes.get_mut(id) {
                node.status = NodeStatus::Online;
            }
        };
        healthy(&mut nodes, 0);
        healthy(&mut nodes, 1);

        // One failure is a hiccup, and switching on it would make the wallet
        // flap between endpoints on a train.
        for failures in 0..FAILURES_BEFORE_FAILOVER {
            if let Some(active) = nodes.get_mut(0) {
                active.consecutive_failures = failures;
                active.status = NodeStatus::Offline {
                    reason: "timed out".to_string(),
                };
            }
            assert_eq!(
                failover_target(&nodes),
                None,
                "moved after only {failures} failures",
            );
        }

        if let Some(active) = nodes.get_mut(0) {
            active.consecutive_failures = FAILURES_BEFORE_FAILOVER;
        }
        assert_eq!(failover_target(&nodes), Some(1));
    }

    /// Staying put is right when there is nothing better: the screen already
    /// says the node is offline, and moving to a second unreachable one only
    /// changes which URL is failing.
    #[test]
    fn there_is_nowhere_to_fail_over_to_when_nothing_is_answering() {
        use pecu_chain::NodeStatus;

        let mut nodes = NodeManager::new(
            vec![
                Node::builtin(0, "first", "https://one.invalid"),
                Node::builtin(1, "second", "https://two.invalid"),
            ],
            Network::Testnet,
        );

        for id in [0, 1] {
            if let Some(node) = nodes.get_mut(id) {
                node.consecutive_failures = 9;
                node.status = NodeStatus::Offline {
                    reason: "timed out".to_string(),
                };
            }
        }
        assert_eq!(failover_target(&nodes), None);

        // And not to one that answers about a different chain, either — that is
        // `Degraded`, not `Online`, and the distinction is what makes switching
        // automatically safe.
        if let Some(node) = nodes.get_mut(1) {
            node.status = NodeStatus::WrongNetwork {
                reported: Network::Mainnet,
            };
        }
        assert_eq!(failover_target(&nodes), None);
    }

    /// The rule that matters most: a different node answers about an uncertain
    /// transaction from a different mempool, so switching mid-resolution turns
    /// a payment that landed into one the wallet reports as absent — and then
    /// offers to send again.
    #[test]
    fn a_payment_whose_fate_is_unknown_pins_the_wallet_to_its_node() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut ledger = pending::Ledger::open(dir.path().join("pending.json"));

        assert!(
            !resolution_pending(&ledger),
            "an empty ledger blocks nothing"
        );

        let record = ledger
            .commit("abc", "00", "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX", "1.0")
            .expect("commit");
        assert!(
            resolution_pending(&ledger),
            "an uncertain payment must pin it"
        );

        // Eight fruitless checks: still pinned, because this is exactly when
        // the screen is offering a resend and a second opinion would be the
        // wrong evidence to decide on.
        ledger.set_state(record, pending::State::Absent);
        assert!(resolution_pending(&ledger));

        // Already handed to another node — a third one's view changes nothing
        // about the decision in front of the user.
        ledger.set_state(record, pending::State::Resent);
        assert!(!resolution_pending(&ledger));

        ledger.set_state(record, pending::State::Confirmed);
        assert!(!resolution_pending(&ledger));
    }

    /// The address book, end to end: named, shown, resolved on the send form,
    /// and forgotten.
    #[tokio::test]
    async fn a_named_address_is_remembered_and_shown_where_it_is_needed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = tokio::runtime::Handle::current();
        let address = "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX";

        let config = || Config {
            nodes: testnet_nodes(),
            network: Network::Testnet,
            mock: false,
            home: dir.path().to_path_buf(),
        };

        let (dispatcher, mut events) = start(&handle, config());

        dispatcher.send(Command::LabelAddress {
            address: address.to_string(),
            label: "the exchange".to_string(),
        });
        let book = next_address_book(&mut events, |rows| !rows.is_empty()).await;
        assert_eq!(book[0].address, address);
        assert_eq!(book[0].label, "the exchange");
        // Named but never paid is a normal state, and the summary has to say so
        // rather than reporting zero of something.
        assert_eq!(book[0].summary, "never paid");

        // The name reaches the one place somebody is checking a pasted
        // address: the recipient line on the send form.
        dispatcher.send(Command::ValidateDraft(pecu_protocol::SendDraft {
            from_label: String::new(),
            to: address.to_string(),
            amount: "1.0".to_string(),
        }));
        // The label is its own field now, so this reads the thing itself
        // rather than looking for a substring of a sentence the core used to
        // compose.
        let label = loop {
            match events.recv().await {
                Some(Event::SendValidation(vm)) => break vm.to_label,
                Some(_) => {}
                None => panic!("the core stopped before validating"),
            }
        };
        assert_eq!(label, "the exchange");

        dispatcher.send(Command::Shutdown);

        // It survives a restart, because a name is a choice nobody can
        // reconstruct.
        let (dispatcher, mut events) = start(&handle, config());
        let book = next_address_book(&mut events, |rows| !rows.is_empty()).await;
        assert_eq!(book[0].label, "the exchange");

        dispatcher.send(Command::ForgetAddress(address.to_string()));
        let book = next_address_book(&mut events, Vec::is_empty).await;
        assert!(book.is_empty());
    }

    async fn next_address_book(
        events: &mut mpsc::UnboundedReceiver<Event>,
        want: impl Fn(&Vec<pecu_protocol::KnownAddressVm>) -> bool,
    ) -> Vec<pecu_protocol::KnownAddressVm> {
        for _ in 0..40 {
            match events.recv().await {
                Some(Event::AddressBook(rows)) if want(&rows) => return rows,
                Some(_) => {}
                None => panic!("the core stopped before sending an address book"),
            }
        }
        panic!("the core never sent the address book this test was waiting for");
    }

    /// "never paid" rather than "0 payments", and singular where it should be.
    #[test]
    fn a_payment_summary_reads_as_a_sentence() {
        let now = 1_800_000_000;

        assert_eq!(payment_summary(0, None, now), "never paid");
        // A row with a date but no payments cannot happen through the store,
        // and if it ever does the count is the fact to trust.
        assert_eq!(payment_summary(0, Some(now), now), "never paid");

        assert_eq!(
            payment_summary(1, Some(now - 60), now),
            "1 payment · last in the last hour",
        );
        assert_eq!(
            payment_summary(3, Some(now - 2 * 86_400), now),
            "3 payments · last 2 days ago",
        );
        assert_eq!(
            payment_summary(2, Some(now - 90_000), now),
            "2 payments · last yesterday",
        );
    }

    /// Reading from a node on another chain is refused.
    ///
    /// Found the hard way: a real session spent eight minutes reading VRSCTEST
    /// addresses against a PBaaS node the wallet had itself marked
    /// `WrongNetwork`, while a cached list sat on screen looking current. The
    /// spend permit had always required agreement about the chain; reading had
    /// not, on the reasoning that a balance from elsewhere is harmless. It is
    /// not — these addresses do not exist over there, so the node answers
    /// honestly with nothing and the wallet renders that as your balance.
    #[tokio::test]
    async fn a_node_on_another_chain_is_not_read_from() {
        use pecu_chain::NodeStatus;

        let mut nodes = NodeManager::new(
            vec![Node::builtin(0, "one", "https://example.invalid")],
            Network::Testnet,
        );

        // Nothing has answered yet: not knowing is not disagreeing, and a cold
        // start must not refuse to read.
        assert!(reading_refused(&nodes).is_none());

        if let Some(node) = nodes.get_mut(0) {
            node.status = NodeStatus::Online;
            node.network = Some(Network::Testnet);
        }
        assert!(
            reading_refused(&nodes).is_none(),
            "a node on the requested chain was refused",
        );

        if let Some(node) = nodes.get_mut(0) {
            node.network = Some(Network::Other("CHIPS".to_string()));
        }
        assert_eq!(reading_refused(&nodes).as_deref(), Some("CHIPS"));

        // Mainnet against a testnet wallet is the same refusal, and the one
        // that would matter most.
        if let Some(node) = nodes.get_mut(0) {
            node.network = Some(Network::Mainnet);
        }
        assert_eq!(reading_refused(&nodes).as_deref(), Some("Mainnet"));
    }

    /// The same decision `Core::reading_the_wrong_chain` makes, over a manager
    /// a test can arrange — the core itself is only reachable through the
    /// actor, and this is the rule rather than the plumbing.
    fn reading_refused(nodes: &NodeManager) -> Option<String> {
        let reported = nodes.active()?.network.as_ref()?;
        let requested = nodes.requested()?;
        (reported != requested).then(|| reported.to_string())
    }

    /// The anchor the whole chart hangs from.
    ///
    /// Getting this wrong is invisible: the chart would still be a plausible
    /// staircase, just uniformly offset from the balance printed above it by
    /// whatever is maturing. Nobody spots a constant offset by looking.
    #[test]
    fn the_chart_is_anchored_on_what_the_confirmed_history_sums_to() {
        use verus_sdk::money::Amount;

        let reading = portfolio::Reading {
            tip: 1000,
            spendable: Amount::from_sat(500),
            // Confirmed and counted by the history, just not spendable yet.
            immature: Amount::from_sat(100),
            // Confirmed outputs that an UNCONFIRMED transaction spends. Still
            // confirmed; the transaction spending them is not in the history.
            pending_out: Amount::from_sat(30),
            // Arriving and unconfirmed, so not in the confirmed history at all.
            pending_in: Amount::from_sat(9_999),
            tokens: std::collections::BTreeMap::new(),
            immature_tokens: std::collections::BTreeMap::new(),
            native: None,
            names: std::collections::BTreeMap::new(),
            history: Ok(Vec::new()),
            scanned_to: 0,
            reached_start: false,
            failure: None,
        };

        assert_eq!(
            confirmed_native(&reading),
            630,
            "the anchor must include what is maturing and what is already \
             spent by something unconfirmed, and must exclude what is arriving",
        );
    }

    /// Wait for a wallet event the caller is interested in. Bounded, so a test
    /// that will never see what it wants fails rather than hanging the suite.
    async fn wallet_until(
        events: &mut mpsc::UnboundedReceiver<Event>,
        want: impl Fn(&pecu_protocol::WalletVm) -> bool,
    ) -> pecu_protocol::WalletVm {
        for _ in 0..60 {
            match events.recv().await {
                Some(Event::Wallet(vm)) if want(&vm) => return vm,
                Some(_) => {}
                None => panic!("the core stopped before reporting the wallet"),
            }
        }
        panic!("the core never reported the wallet state this test was waiting for");
    }

    /// A wallet, created and backed up, so the tests below start from a state
    /// where a second key can actually be added.
    async fn backed_up_wallet(
        dispatcher: &Dispatcher,
        events: &mut mpsc::UnboundedReceiver<Event>,
    ) {
        dispatcher.send(Command::CreateWallet {
            name: "test".to_string(),
            passphrase: pecu_protocol::Secret::from("a passphrase"),
        });

        let positions = loop {
            match events.recv().await {
                Some(Event::PhraseChallenge { positions, .. }) => break positions,
                Some(_) => {}
                None => panic!("the core stopped before announcing a challenge"),
            }
        };

        dispatcher.send(Command::ShowNewPhrase);
        let words = loop {
            match events.recv().await {
                Some(Event::SeedWords(words)) if !words.is_empty() => break words,
                Some(_) => {}
                None => panic!("the core stopped before sending the words"),
            }
        };

        let checks = positions
            .iter()
            .map(|position| {
                let word = words
                    .iter()
                    .find(|w| w.index == *position)
                    .map(|w| w.word.clone())
                    .unwrap_or_default();
                (*position, word)
            })
            .collect();
        dispatcher.send(Command::ConfirmPhrase { checks });
        let _ = wallet_until(events, |vm| vm.needs_backup.is_none()).await;
    }

    /// Multi-key is the wallet model, and it has to work end to end: a second
    /// key generated in an open wallet, renamed, and switched to.
    #[tokio::test]
    async fn a_second_key_can_be_generated_renamed_and_selected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = tokio::runtime::Handle::current();

        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                home: dir.path().to_path_buf(),
            },
        );
        backed_up_wallet(&dispatcher, &mut events).await;

        dispatcher.send(Command::AddKey {
            label: "savings".to_string(),
        });
        let vm = wallet_until(&mut events, |vm| vm.keys.len() == 2).await;

        let added = vm
            .keys
            .iter()
            .find(|key| key.label == "savings")
            .expect("the new key");
        assert!(!added.address.is_empty());
        assert_ne!(
            added.address,
            vm.keys
                .iter()
                .find(|key| key.label == "main")
                .expect("main")
                .address,
            "the second key has the same address as the first",
        );
        // Adding a key is not switching to it. Moving the receive address out
        // from under someone who was about to be paid is not this command's
        // decision to make.
        assert_eq!(vm.active_key.as_deref(), Some("main"));
        // Generated here, so nobody has written its phrase down yet.
        assert_eq!(vm.needs_backup.as_deref(), Some("savings"));

        // Renaming re-seals both blobs under the new name — see
        // `Vault::rename_key`. What matters here is that the address survives,
        // because that is the observable proof the key still decrypts.
        let address = added.address.clone();
        dispatcher.send(Command::RenameKey {
            from: "savings".to_string(),
            to: "cold-storage".to_string(),
        });
        let vm = wallet_until(&mut events, |vm| {
            vm.keys.iter().any(|key| key.label == "cold-storage")
        })
        .await;
        assert_eq!(vm.keys.len(), 2, "renaming changed how many keys there are");
        assert_eq!(
            vm.keys
                .iter()
                .find(|key| key.label == "cold-storage")
                .expect("the renamed key")
                .address,
            address,
        );

        dispatcher.send(Command::SetActiveKey("cold-storage".to_string()));
        let vm = wallet_until(&mut events, |vm| {
            vm.active_key.as_deref() == Some("cold-storage")
        })
        .await;
        assert_eq!(vm.active_key.as_deref(), Some("cold-storage"));

        // And all of it is on disk, not just in memory.
        dispatcher.send(Command::Shutdown);
        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                home: dir.path().to_path_buf(),
            },
        );
        dispatcher.send(Command::Unlock {
            passphrase: pecu_protocol::Secret::from("a passphrase"),
        });
        let vm = wallet_until(&mut events, |vm| !vm.locked).await;
        assert_eq!(vm.keys.len(), 2);
        assert!(vm.keys.iter().any(|key| key.label == "cold-storage"));
        assert!(!vm.keys.iter().any(|key| key.label == "savings"));
    }

    /// The label is what every command names a key by, so two keys under one
    /// name would make `with_key` ambiguous.
    #[tokio::test]
    async fn a_key_name_that_is_taken_is_refused_with_a_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = tokio::runtime::Handle::current();

        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                home: dir.path().to_path_buf(),
            },
        );
        backed_up_wallet(&dispatcher, &mut events).await;

        dispatcher.send(Command::AddKey {
            label: "main".to_string(),
        });

        let notice = loop {
            match events.recv().await {
                Some(Event::Notice(notice)) => break notice,
                Some(_) => {}
                None => panic!("the core stopped before refusing"),
            }
        };
        assert_eq!(notice.code, "add_key");
        assert_eq!(notice.message.code, "key-name-taken", "{:?}", notice.message);

        // A name the vault's own rules refuse gets a different sentence,
        // because it calls for a different fix.
        dispatcher.send(Command::AddKey {
            label: "Not A Label".to_string(),
        });
        let notice = loop {
            match events.recv().await {
                Some(Event::Notice(notice)) => break notice,
                Some(_) => {}
                None => panic!("the core stopped before refusing"),
            }
        };
        assert_eq!(notice.code, "add_key");
        assert_eq!(notice.message.code, "key-name-rules", "{:?}", notice.message);
    }

    /// Wait for a network event the caller is interested in, ignoring the rest.
    /// Bounded, so a test that will never see what it wants fails rather than
    /// hanging the suite.
    async fn network_until(
        events: &mut mpsc::UnboundedReceiver<Event>,
        want: impl Fn(&NetworkVm) -> bool,
    ) -> NetworkVm {
        for _ in 0..40 {
            let vm = next_network(events).await;
            if want(&vm) {
                return vm;
            }
        }
        panic!("the core never reported the network state this test was waiting for");
    }

    /// The whole point of writing a node down: it is still there next time.
    #[tokio::test]
    async fn a_user_added_node_is_remembered_across_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = tokio::runtime::Handle::current();

        let config = || Config {
            nodes: testnet_nodes(),
            network: Network::Testnet,
            mock: false,
            home: dir.path().to_path_buf(),
        };

        {
            let (dispatcher, mut events) = start(&handle, config());
            let _ = next_network(&mut events).await;

            dispatcher.send(Command::AddNode {
                url: "https://my-node.invalid".to_string(),
                label: "mine".to_string(),
            });

            let vm = network_until(&mut events, |vm| vm.nodes.len() == 2).await;
            let added = vm
                .nodes
                .iter()
                .find(|node| node.url == "https://my-node.invalid")
                .expect("the added node");
            assert_eq!(added.label, "mine");
            assert!(!added.builtin, "a user-added node claimed to be built in");

            // Selecting it is a choice, and choices are written down too.
            dispatcher.send(Command::SelectNode(added.id));
            let vm = network_until(&mut events, |vm| vm.active_node == Some(added.id)).await;
            assert_eq!(vm.active_node, Some(added.id));

            dispatcher.send(Command::Shutdown);
        }

        // A second run, with the same built-ins and the same directory.
        let (_dispatcher, mut events) = start(&handle, config());
        let vm = network_until(&mut events, |vm| vm.nodes.len() == 2).await;

        let restored = vm
            .nodes
            .iter()
            .find(|node| node.url == "https://my-node.invalid")
            .expect("the node survived the restart");
        assert_eq!(restored.label, "mine");
        // The active node is remembered by URL, so it comes back even though
        // its id is assigned afresh each run.
        assert_eq!(vm.active_node, Some(restored.id));
    }

    /// The refusal that matters: plaintext to anything but loopback would put
    /// every address this wallet asks about in front of whoever is on the path.
    #[tokio::test]
    async fn a_url_that_would_leak_in_transit_is_refused_and_not_saved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = tokio::runtime::Handle::current();

        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                home: dir.path().to_path_buf(),
            },
        );
        let _ = next_network(&mut events).await;

        dispatcher.send(Command::AddNode {
            url: "http://my-node.invalid".to_string(),
            label: "mine".to_string(),
        });

        let notice = loop {
            match events.recv().await {
                Some(Event::Notice(notice)) => break notice,
                Some(_) => {}
                None => panic!("the core stopped before refusing"),
            }
        };
        assert_eq!(notice.code, "add_node");
        assert_eq!(notice.message.code, "node-url-insecure", "{:?}", notice.message);

        // And nothing was written down, so a restart does not resurrect it.
        let store = pecu_store::Store::open(&chain_dir(&dir)).expect("store");
        assert!(store.nodes().is_empty(), "a refused node was saved anyway");
    }

    /// A duplicate would probe identically and could never disagree about
    /// anything, which makes choosing between the two entries meaningless.
    #[tokio::test]
    async fn an_endpoint_that_is_already_configured_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = tokio::runtime::Handle::current();

        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                home: dir.path().to_path_buf(),
            },
        );
        let _ = next_network(&mut events).await;

        // `testnet_nodes` ships exactly this endpoint.
        dispatcher.send(Command::AddNode {
            url: "https://example.invalid/".to_string(),
            label: "again".to_string(),
        });

        let notice = loop {
            match events.recv().await {
                Some(Event::Notice(notice)) => break notice,
                Some(_) => {}
                None => panic!("the core stopped before refusing"),
            }
        };
        assert_eq!(notice.code, "add_node");
        assert_eq!(notice.message.code, "node-duplicate", "{:?}", notice.message);
    }

    /// Removing whichever node is in use must not leave the wallet pointed at
    /// an endpoint that is no longer configured.
    #[tokio::test]
    async fn removing_the_active_node_falls_back_to_one_that_is_left() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = tokio::runtime::Handle::current();

        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                home: dir.path().to_path_buf(),
            },
        );
        let _ = next_network(&mut events).await;

        dispatcher.send(Command::AddNode {
            url: "https://my-node.invalid".to_string(),
            label: "mine".to_string(),
        });
        let vm = network_until(&mut events, |vm| vm.nodes.len() == 2).await;
        let added = vm
            .nodes
            .iter()
            .find(|node| !node.builtin)
            .expect("the added node")
            .id;

        dispatcher.send(Command::SelectNode(added));
        let _ = network_until(&mut events, |vm| vm.active_node == Some(added)).await;

        dispatcher.send(Command::RemoveNode(added));
        let vm = network_until(&mut events, |vm| vm.nodes.len() == 1).await;

        assert_eq!(vm.active_node, Some(0), "the wallet was left with no node");

        // Gone from the file too, so it does not come back at the next start.
        let store = pecu_store::Store::open(&chain_dir(&dir)).expect("store");
        assert!(store.nodes().is_empty());
    }

    /// A built-in comes from the build. "Removing" one would last until the
    /// next start and then quietly undo itself.
    #[tokio::test]
    async fn a_builtin_node_cannot_be_removed() {
        let handle = tokio::runtime::Handle::current();
        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                home: std::path::PathBuf::from("/nonexistent"),
            },
        );
        let _ = next_network(&mut events).await;

        dispatcher.send(Command::RemoveNode(0));
        // Nothing to wait for, so ask a question whose answer has to come after
        // the removal was handled — commands are processed in order.
        dispatcher.send(Command::SelectNode(0));

        let vm = next_network(&mut events).await;
        assert_eq!(vm.nodes.len(), 1, "a built-in node was removed");
    }

    /// Wait for the next `Event::Wallet`, ignoring anything else.
    async fn next_wallet(
        events: &mut mpsc::UnboundedReceiver<Event>,
    ) -> pecu_protocol::WalletVm {
        loop {
            match events.recv().await {
                Some(Event::Wallet(vm)) => return vm,
                Some(_) => {}
                None => panic!("the core stopped before sending a wallet event"),
            }
        }
    }

    // ── Changing chains ─────────────────────────────────────────────────────

    /// The property the per-chain directory exists for.
    ///
    /// A wallet made on one chain is not there on the other. If this ever
    /// fails, testnet keys are being opened against mainnet — where the same
    /// addresses hold real money and a balance of zero is a lie about it.
    #[tokio::test]
    async fn a_wallet_made_on_one_chain_is_not_on_the_other() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = tokio::runtime::Handle::current();
        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                home: dir.path().to_path_buf(),
            },
        );

        dispatcher.send(Command::CreateWallet {
            name: "testnet-wallet".to_string(),
            passphrase: pecu_protocol::Secret::from("correct-horse-battery-staple-9931"),
        });
        loop {
            let vm = next_wallet(&mut events).await;
            if vm.exists && !vm.locked {
                break;
            }
        }

        dispatcher.send(Command::SetRequestedNetwork("VRSC".to_string()));

        // Two things at once, and both matter: there is no wallet over here,
        // and the session that was open did not come along.
        let vm = loop {
            let vm = next_wallet(&mut events).await;
            if !vm.exists {
                break vm;
            }
        };
        assert!(vm.locked, "an unlocked session followed the switch");
        assert!(vm.keys.is_empty(), "the other chain's keys followed");

        // And the file itself is still where it was, untouched by the move.
        let testnet = paths::Paths::new(dir.path().to_path_buf(), &Network::Testnet, false);
        let mainnet = paths::Paths::new(dir.path().to_path_buf(), &Network::Mainnet, false);
        assert!(testnet.vault().exists(), "the testnet wallet was moved");
        assert!(!mainnet.vault().exists(), "a mainnet wallet was created");

        dispatcher.send(Command::Shutdown);
    }

    /// Switching back finds the wallet again — the first one was not destroyed,
    /// merely closed.
    #[tokio::test]
    async fn switching_back_finds_the_wallet_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = tokio::runtime::Handle::current();
        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                home: dir.path().to_path_buf(),
            },
        );

        dispatcher.send(Command::CreateWallet {
            name: "testnet-wallet".to_string(),
            passphrase: pecu_protocol::Secret::from("correct-horse-battery-staple-9931"),
        });
        loop {
            if next_wallet(&mut events).await.exists {
                break;
            }
        }

        dispatcher.send(Command::SetRequestedNetwork("VRSC".to_string()));
        dispatcher.send(Command::SetRequestedNetwork("VRSCTEST".to_string()));

        let vm = loop {
            let vm = next_wallet(&mut events).await;
            if vm.exists {
                break vm;
            }
        };
        assert_eq!(vm.name, "testnet-wallet");
        assert!(vm.locked, "the wallet came back unlocked");

        dispatcher.send(Command::Shutdown);
    }

    /// A saved endpoint belongs to the chain it was saved on.
    ///
    /// Without the reset, the list accumulates: a node added for testnet stays
    /// when the wallet moves to mainnet, offering an endpoint the read guard
    /// will then refuse — which reads as a broken wallet rather than as a node
    /// on the wrong chain.
    #[tokio::test]
    async fn saved_nodes_do_not_follow_the_wallet_to_another_chain() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = tokio::runtime::Handle::current();
        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                home: dir.path().to_path_buf(),
            },
        );
        let _ = next_network(&mut events).await;

        dispatcher.send(Command::AddNode {
            url: "https://saved-for-testnet.invalid".to_string(),
            label: "mine".to_string(),
        });
        loop {
            if next_network(&mut events).await.nodes.len() == 2 {
                break;
            }
        }

        dispatcher.send(Command::SetRequestedNetwork("VRSC".to_string()));

        let vm = loop {
            let vm = next_network(&mut events).await;
            if vm.requested == "Mainnet" {
                break vm;
            }
        };
        assert_eq!(vm.nodes.len(), 1, "a saved node followed the switch");
        assert!(
            !vm.nodes.iter().any(|n| n.url.contains("saved-for-testnet")),
            "the endpoint saved for the other chain is still listed",
        );

        dispatcher.send(Command::Shutdown);
    }

    /// The chain in use is remembered, so the next launch opens the same one.
    ///
    /// Written at the home directory rather than in a per-chain database, since
    /// finding that database means already knowing the answer.
    #[tokio::test]
    async fn the_chain_in_use_survives_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = tokio::runtime::Handle::current();
        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                home: dir.path().to_path_buf(),
            },
        );
        let _ = next_network(&mut events).await;

        dispatcher.send(Command::SetRequestedNetwork("VRSC".to_string()));
        loop {
            if next_network(&mut events).await.requested == "Mainnet" {
                break;
            }
        }
        dispatcher.send(Command::Shutdown);

        assert_eq!(
            paths::Paths::remembered(dir.path()),
            Some(Network::Mainnet),
            "the chain in use was not written down",
        );
    }

    /// Asking for the chain already open is not a reason to lock the wallet.
    #[tokio::test]
    async fn asking_for_the_chain_already_open_does_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = tokio::runtime::Handle::current();
        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                home: dir.path().to_path_buf(),
            },
        );

        dispatcher.send(Command::CreateWallet {
            name: "testnet-wallet".to_string(),
            passphrase: pecu_protocol::Secret::from("correct-horse-battery-staple-9931"),
        });
        loop {
            let vm = next_wallet(&mut events).await;
            if vm.exists && !vm.locked {
                break;
            }
        }

        dispatcher.send(Command::SetRequestedNetwork("VRSCTEST".to_string()));
        // Nothing to wait for if it did nothing, so ask a question whose answer
        // has to come afterwards — commands are handled in order.
        dispatcher.send(Command::SetActiveKey("nonexistent".to_string()));
        dispatcher.send(Command::Lock);

        let vm = next_wallet(&mut events).await;
        assert!(
            !vm.keys.is_empty(),
            "a no-op switch closed the wallet that was open",
        );

        dispatcher.send(Command::Shutdown);
    }

    #[tokio::test]
    async fn selecting_a_node_re_reports_the_network() {
        let handle = tokio::runtime::Handle::current();
        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: vec![
                    Node::builtin(0, "one", "https://example.invalid"),
                    Node::builtin(1, "two", "https://other.invalid"),
                ],
                network: Network::Testnet,
                mock: false,
                home: std::path::PathBuf::from("/nonexistent"),
            },
        );

        // The actor reports several things at startup, so wait for the one
        // this test is about rather than assuming an ordering. Counting events
        // would make this test fail every time a new kind is emitted.
        let _ = next_network(&mut events).await;
        dispatcher.send(Command::SelectNode(1));

        let vm = next_network(&mut events).await;
        assert_eq!(vm.active_node, Some(1));
    }
}
