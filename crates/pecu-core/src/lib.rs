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
pub mod params;
pub mod paths;
pub mod pending;
pub mod portfolio;
pub mod registration;
pub mod runtime;
pub mod send;
pub mod shield;
pub mod shielded;
pub mod upgrade;
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

/// The endpoints this build ships for one chain.
///
/// One function, called at startup and again on every chain switch, so the two
/// cannot drift into offering different lists for the same chain.
///
/// The demo build gets its one scripted entry whatever the chain is. Every
/// probe there is answered by `pecu-mock` rather than by a node, so shipping
/// the real list would put `api.verus.services` on screen reporting a tip it
/// never mined — and a URL scheme nothing in this application knows how to
/// dial is the honest way to say that.
#[must_use]
pub fn shipped_nodes(network: &Network, mock: bool) -> Vec<Node> {
    if mock {
        return vec![Node::builtin(0, "Scripted chain", "mock://scripted")];
    }
    network
        .builtin_nodes()
        .iter()
        .enumerate()
        .map(|(index, (label, url))| Node::builtin(u32::try_from(index).unwrap_or(0), label, url))
        .collect()
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
        halt: None,
        reading_halt: false,
        named_addresses: false,
        refreshing: false,
        work: work_tx,
        spendable: verus_sdk::money::Amount::ZERO,
        key_funds: std::collections::BTreeMap::new(),
        shielded: None,
        light_server: None,
        scanning: false,
        scan_share: None,
        native_balance: 0,
        prepared: std::collections::HashMap::new(),
        conversions: std::collections::HashMap::new(),
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
        convert_floor: None,
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

/// A refusal that has already been reported to the interface.
///
/// The helpers that turn a draft away call `refuse_send` themselves, because
/// each of them knows the sentence that fits. What the caller needs back is
/// only "stop" — an error carrying a message would invite a second, worse one
/// being written at the call site.
struct Refused;

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
    /// The floor from the quote last put on screen, with the ticket it belongs
    /// to.
    ///
    /// Both, because the floor is a record of what somebody was *shown* and
    /// agreed to. Recomputing it when they press Review would enforce a number
    /// nobody saw, and carrying it without its ticket would let a floor
    /// calculated for one pair of currencies be checked against another.
    convert_floor: Option<(u64, verus_sdk::money::Amount)>,
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
    /// number, and the authorisation is taken again at the moment of broadcast
    /// rather than kept here beside them — see `confirm_launch`.
    launches: std::collections::HashMap<u64, currency::Prepared>,
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
    /// The last thing the chain's oracle said, and when it was read.
    ///
    /// Kept so a failed read can stand in with it for a bounded window rather
    /// than reporting `unknown` — see [`Core::read_chain_halt`]. **A failure is
    /// never stored here**, which is the whole point: caching one as if it were
    /// an answer pins the banner to "unavailable" for as long as the window
    /// lasts, and every failed refresh extends it.
    halt: Option<(std::time::Instant, upgrade::Status)>,
    /// Whether a read is out, so a slow node cannot pile them up.
    reading_halt: bool,
    /// Whether the address book's identities have been named this session.
    /// See [`Core::name_known_identities`].
    named_addresses: bool,
    /// One refresh at a time. Without this, holding the Refresh button would
    /// queue a request storm against a public node.
    refreshing: bool,
    work: mpsc::UnboundedSender<Work>,

    /// What the last refresh said this wallet can spend. Used to validate a
    /// draft offline; the builder is still the authority, and it refuses on its
    /// own terms if this turns out to be stale.
    spendable: verus_sdk::money::Amount,
    /// The same figure, and the maturing one, per address.
    ///
    /// The wallet-wide `spendable` above is what the dashboard shows. This is
    /// what the *send* screens need: a payment is signed by the active key and
    /// spends its coins alone, so validating a draft against the wallet total
    /// tells somebody they can afford something one key cannot pay for, and the
    /// refusal then arrives from the builder — after the form said it was fine.
    ///
    /// An address missing from here has not been read, which is not the same as
    /// holding nothing. See [`portfolio::Reading::by_address`].
    key_funds: std::collections::BTreeMap<String, portfolio::KeyFunds>,
    /// The shielded side of the active key, once the wallet is open.
    ///
    /// `None` while locked, for a key that cannot have one, and before the
    /// first unlock — which are three different situations that all mean "no
    /// shielded balance to show". `WalletVm::shielded_note` is what tells them
    /// apart on screen.
    ///
    /// Holds the viewing key and whatever has been scanned. It is dropped on
    /// lock along with everything else the data key reaches.
    shielded: Option<shielded::Shielded>,
    /// The lightwalletd shielded notes are read through, if one is overridden.
    ///
    /// Per chain, like every other node setting. `None` means the chain's
    /// shipped address is used — see `Network::light_server`, which names one
    /// only for chains where connecting to it has actually been tested.
    light_server: Option<String>,
    /// Whether a shielded scan is already in flight, so a second refresh does
    /// not start another. Scans are slow and idempotent; two at once is waste.
    scanning: bool,
    /// How far a scan in flight has got, as a percentage, or `None`.
    ///
    /// Only meaningful while `scanning`. A first scan with no birthday covers
    /// the whole chain, so this is the difference between a wallet that is
    /// working and a wallet that appears to have died.
    scan_share: Option<u32>,
    /// What the confirmed history sums to: spendable + immature + the confirmed
    /// coins an unconfirmed transaction already spends. The anchor the balance
    /// chart is built backwards from — see `emit_chart`.
    native_balance: i64,
    /// Signed payments waiting for a Confirm. **The bytes never leave here** —
    /// the UI holds a ticket number and a decoded summary.
    prepared: std::collections::HashMap<u64, send::Prepared>,
    /// Signed conversions waiting for a Confirm. Same shape and same rule as
    /// `prepared`: the bytes never leave here, and the interface holds a
    /// number.
    ///
    /// A second map rather than a shared one, because the two are confirmed by
    /// different commands and a ticket that could be either would let the send
    /// screen's Confirm broadcast a conversion.
    conversions: std::collections::HashMap<u64, convert::Prepared>,
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
    /// What the chain's oracle published, or nothing at all.
    ///
    /// `None` is a read that failed. It is deliberately not an empty vector:
    /// "the key is absent" and "I could not ask" are different answers and
    /// collapsing them is how a halt goes unannounced.
    ChainHalt(Option<Vec<Vec<u8>>>),
    /// What the chain calls the identities in the address book.
    AddressNames(Vec<(String, String)>),
    /// A conversion was built and signed, or the attempt failed.
    Converted {
        ticket: u64,
        result: Box<Result<convert::Prepared, convert::ConvertError>>,
    },
    /// A conversion's broadcast finished, one way or another.
    ConversionSent {
        /// The ledger row committed before the attempt.
        record: u64,
        result: Box<Result<verus_sdk::network::Sent, verus_sdk::network::FlowError>>,
    },
    /// A shielded scan has reached this height and is still going.
    ScanProgress {
        scanned_to: u64,
        tip: u64,
        /// Where this scan began, so a share can be worked out. Without it a
        /// wallet scanning the last thousand blocks and one scanning the whole
        /// chain look identical at the same height.
        start: u64,
    },
    /// A shielded scan finished, or gave up part of the way.
    Scanned {
        /// The account as far as it got. **Kept even on failure.**
        ///
        /// A full scan is about twelve hundred requests over the open
        /// internet. Throwing the whole thing away because the last one failed
        /// means starting at block two again — four minutes of work, discarded
        /// for a hiccup, repeatedly.
        ///
        /// `None` only when the server could not be reached at all, so there is
        /// no partial state to keep.
        watching: Box<Option<shielded::Shielded>>,
        /// Why it stopped early, if it did.
        ///
        /// A `String` rather than a typed error: three different kinds can end
        /// up here — the address was refused, the server was, or the scan was —
        /// and every one of them ends as the same sentence on the same line.
        failed: Option<String>,
        /// Whether what was kept has to be thrown away and scanned again.
        ///
        /// The one failure that is not "try again in a minute". A reorg deeper
        /// than the scan can verify a rollback to leaves state that will be
        /// refused identically on every future call — so with a kept scan on
        /// disk it is not a bad minute, it is a wallet that never scans again.
        /// This is what breaks that loop.
        restart: bool,
    },
    /// A broadcast finished, one way or another.
    Broadcast {
        /// The ledger row committed before the attempt.
        record: u64,
        /// Shielded nullifiers this transaction publishes, empty otherwise.
        ///
        /// Carried through the worker rather than recorded before it, because a
        /// refused broadcast has spent nothing — marking its notes would strand
        /// them until the next scan finds them again.
        spends: Vec<[u8; 32]>,
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
        described: NoteVm,
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
    /// A launch built and signed. **The bytes never leave here** — the
    /// interface holds a ticket and a decoded summary.
    LaunchPrepared {
        ticket: u64,
        result: Box<Result<Box<currency::Prepared>, String>>,
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
    /// The type is long because the read is one round of questions and its
    /// answers belong together — see the variant's own note on why the chain id
    /// travels with them rather than being read off the balance.
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
                    // What each started pool published over the chart's window,
                    // by converter i-address.
                    Vec<(String, Vec<verus_sdk::network::CurrencyStateAt>)>,
                    // The chain's own currency id, from the same read.
                    //
                    // Not `Core::cached.native`: that is filled by a balance
                    // read, and the markets read can finish first. An empty
                    // chain id costs every two-hop price on the screen — the
                    // direct ones still work, so the failure looks like a few
                    // currencies being unpriceable rather than like a bug.
                    String,
                    // Names for the currencies `listcurrencies` does not
                    // return — the bridged ones, which include the currency
                    // every price here is quoted in. See `refresh_markets`.
                    Vec<(String, String)>,
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

/// A node's condition, as a named reason.
///
/// `NodeStatus::note` in `pecu-chain` builds English, and it cannot do
/// otherwise: that crate does not know `NoteVm` exists and should not — it is
/// the layer that talks to a daemon. So the mapping is here, where the words
/// are already the interface's business. The one string carried through is the
/// **node's own** reason for being unreachable, which is a machine's words and
/// is not ours to translate.
fn node_note(status: &pecu_chain::NodeStatus) -> NoteVm {
    use pecu_chain::NodeStatus;

    match status {
        NodeStatus::Syncing { blocks, longest } => NoteVm::with(
            "node-catching-up",
            [blocks.to_string(), longest.to_string()],
        ),
        NodeStatus::WrongNetwork { reported } => {
            NoteVm::with("node-other-chain", [reported.to_string()])
        }
        // Both of the node's own strings are carried through rather than
        // summarised, for the same reason the offline reason is: which two
        // things disagreed is the whole of what a person can act on here.
        NodeStatus::Unidentified { name, chain_id } => {
            NoteVm::with("node-unidentified", [name.clone(), chain_id.clone()])
        }
        NodeStatus::MethodRefused { method } => {
            NoteVm::with("node-refused-method", [method.clone()])
        }
        NodeStatus::Offline { reason } => NoteVm::with("node-offline", [reason.clone()]),
        NodeStatus::Unknown | NodeStatus::Probing | NodeStatus::Online => NoteVm::none(),
    }
}

/// Why the active node must not be read from, if it must not be.
///
/// # Two refusals, because a person can act on the difference
///
/// A node on another chain is fixed by switching chain or switching node, and
/// the note names the chain it is actually on. A node whose own two claims
/// about its identity disagree is not on a chain the wallet could switch to at
/// all, and the only useful thing to say is which two claims those were.
///
/// # Identity is asked about first, and that ordering is the guard
///
/// The disagreement test below reads `node.network`, and `None` there means
/// "read anyway": not knowing is not the same as disagreeing, and refusing
/// before the first probe would leave a cold start blank for no reason. That
/// reasoning was written when a cold start was the only way to have no network.
/// It no longer is — `Node::record_success` leaves `network` unset for a node
/// whose name and chain id contradict each other — so asking about the network
/// first would read that silence as a cold start and let the refresh straight
/// through to balances, history and quotes from the one class of endpoint the
/// cross-check exists to distrust. The check would have cost this wallet a
/// refusal it already had rather than bought it one.
///
/// A free function rather than a method because the rule is worth testing on
/// its own and `Core` is only reachable through the actor. It takes the manager
/// the actor holds, so a test exercises this and not a copy of it.
///
/// The `&'static str` is the code the notice is filed and logged under, and the
/// two refusals do not share one: `code=wrong_chain reason=read-unidentified`
/// would send whoever reads that line looking for a chain mismatch that never
/// happened.
fn reading_refused(nodes: &NodeManager) -> Option<(&'static str, NoteVm)> {
    use pecu_chain::NodeStatus;

    let active = nodes.active()?;

    if let NodeStatus::Unidentified { name, chain_id } = &active.status {
        return Some((
            "unidentified_node",
            NoteVm::with("read-unidentified", [name.clone(), chain_id.clone()]),
        ));
    }

    let reported = active.network.as_ref()?;
    let requested = nodes.requested()?;
    (reported != requested).then(|| {
        (
            "wrong_chain",
            NoteVm::with(
                "read-wrong-chain",
                [reported.to_string(), requested.to_string()],
            ),
        )
    })
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

/// Everything the command palette shows for one query.
///
/// Free rather than a method so it can be tested: `Core` owns channels, a node
/// manager and an open database, and none of those have anything to do with
/// which rows a substring matches.
///
/// `known` comes back from the store most-recently-paid first, and the order is
/// load-bearing — see `MOST`.
fn palette_hits(
    query: &str,
    known: &[pecu_store::KnownAddress],
    currencies: &std::collections::BTreeMap<String, String>,
) -> Vec<pecu_protocol::SearchHitVm> {
    /// How many results the panel will show.
    ///
    /// It has no scroll and sizes itself to its content, so an uncapped list
    /// would grow the panel off the bottom of the window. Eight fits an
    /// 800px-tall window and leaves the field visible, which is the one thing
    /// that must never be pushed off the top.
    ///
    /// Truncation is silent, and is the reason both lists are walked in an
    /// order somebody would choose: addresses most-recently-paid first, and
    /// currencies alphabetically because a `BTreeMap` is. A cap over an
    /// arbitrary order would drop arbitrary rows.
    const MOST: usize = 8;

    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }

    let mut hits: Vec<pecu_protocol::SearchHitVm> = Vec::new();

    // Addresses first, and deliberately: in a wallet whose subject is sending
    // money, what somebody opens a search box to find is usually somebody to
    // pay.
    //
    // An address that has been paid but never named shows its address on both
    // lines rather than leaving the first empty: the row is two lines tall
    // whatever it holds, and a blank top line reads as a broken row instead of
    // an unnamed one.
    for entry in known {
        if hits.len() >= MOST {
            break;
        }
        // Three things to match on, not two: the owner's label, the chain's
        // name, and the address. Somebody who paid `dude.VRSCTEST@` looks for
        // it by that name — it is what the send screen shows them — and a
        // palette that only knew the i-address would find nothing.
        if !search_matches(&needle, &entry.label, &entry.address)
            && !search_matches(&needle, &entry.name, &entry.address)
        {
            continue;
        }
        hits.push(pecu_protocol::SearchHitVm {
            kind: "address".to_string(),
            // Most recognisable first, the same order the send screen uses.
            label: if !entry.name.is_empty() {
                entry.name.clone()
            } else if entry.label.is_empty() {
                entry.address.clone()
            } else {
                entry.label.clone()
            },
            sub: entry.address.clone(),
            target: entry.address.clone(),
        });
    }

    // Then currencies, from the names the markets read brought back.
    //
    // That list and not `Core::cached.names`, which is the other set of
    // currency names the core holds: its keys are `CurrencyId`s, and a
    // `CurrencyId` prints as its bytes rather than as the i-address
    // `OpenMarket` takes. A target built from one would look right and open
    // nothing. So a session that has not been to the markets screen finds no
    // currencies here, which is the honest answer to "what is in hand".
    for (address, name) in currencies {
        if hits.len() >= MOST {
            break;
        }
        if !search_matches(&needle, name, address) {
            continue;
        }
        hits.push(pecu_protocol::SearchHitVm {
            kind: "currency".to_string(),
            label: name.clone(),
            sub: address.clone(),
            target: address.clone(),
        });
    }

    hits
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
                if let Some(label) = self.wallet.active_key.clone() {
                    self.remember_birthday(&label);
                }
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
                self.notice(
                    "wallet_create",
                    NoteVm::plain("wallet-create-failed"),
                    &error,
                );
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
            Err(error) => self.notice("reveal_backup", reveal_error_note(&error), &error),
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
                self.notice(
                    "finish_backup",
                    NoteVm::plain("backup-record-failed"),
                    &error,
                );
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
                self.notice("node_connect", NoteVm::plain("mock-chain-failed"), &error);
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
        // node that was never asked about this chain — nor from one that was
        // asked and could not answer consistently. See `reading_refused`.
        if let Some((code, refusal)) = reading_refused(&self.nodes) {
            self.notice_warning(code, refusal, "");
            return;
        }

        // The shielded half, on the same timer and on its own worker. It uses a
        // different server and a different protocol, so it is deliberately not
        // folded into the transparent refresh: one being down must not stop the
        // other, and a wallet with no light server configured does nothing here.
        self.scan_shielded();

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
            } => {
                self.finish_identity_change_prepared(ticket, described, needs_confirmation, *result);
            }
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
            Work::ScanProgress {
                scanned_to,
                tip,
                start,
            } => self.scan_progress(scanned_to, tip, start),
            Work::Scanned {
                watching,
                failed,
                restart,
            } => self.finish_scan(*watching, failed, restart),
            Work::Broadcast {
                record,
                spends,
                result,
            } => self.finish_broadcast(record, &spends, *result),
            Work::ChainHalt(found) => self.finish_chain_halt(found.as_deref()),
            Work::AddressNames(found) => self.finish_address_names(&found),
            Work::Converted { ticket, result } => self.finish_convert_prepare(ticket, *result),
            Work::ConversionSent { record, result } => {
                self.finish_convert_broadcast(record, *result);
            }
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
        self.key_funds.clone_from(&reading.by_address);
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

        // The per-key figures moved with this read, and they travel on the
        // wallet rather than on the portfolio — see `WalletVm::key_funds`. So
        // the wallet is republished here, as it already is after a shielded
        // scan, for the same reason: the balance changed and the send form is
        // drawn from it.
        self.emit_wallet();

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
            Command::PrepareConversion => self.prepare_conversion(),
            Command::ConfirmConversion { ticket } => self.confirm_conversion(ticket),
            Command::CancelConversion { ticket } => self.cancel_conversion(ticket),
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
                    Err(error) => self.notice("unlock", NoteVm::plain("passphrase-wrong"), &error),
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
            Command::SetLightServer(url) => self.set_light_server(&url),
            Command::SetAllowSpending {
                on,
                typed_confirmation,
            } => self.set_allow_spending(on, &typed_confirmation),
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
                self.notice(
                    "load_history",
                    NoteVm::plain("history-older-unreadable"),
                    &error,
                );
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
        let rows = portfolio::rows_from(
            &self.history.entries,
            &self.cached.names,
            now(),
            &self.history_filter,
        );
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
                status: "not-read-yet".to_string(),
                tone: "unknown".to_string(),
                note: NoteVm::none(),
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

        // A stored choice wins; otherwise whatever this chain ships.
        //
        // Stored-empty is a *choice* — somebody pressed "Forget it" — and must
        // not be overridden by the default, or the wallet would silently point
        // itself back at a server the person had just removed. So the absence
        // of the key and an empty value mean different things here.
        self.light_server = match store.setting("light_server") {
            Some(url) if url.is_empty() => None,
            Some(url) => Some(url),
            None => self
                .nodes
                .requested()
                .and_then(|network| network.light_server().map(str::to_string)),
        };

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

        // The first time this chain answers, find out whether the protocol is
        // taking conversions at all. Once — `halt` is cleared on a chain
        // switch and on nothing else, because this state moves on the order of
        // weeks and polling it would be a request per tick for the same answer.
        if info.is_ok() && self.halt.is_none() {
            self.read_chain_halt();
        }

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
            self.notice_warning("add_key", NoteVm::plain("backup-in-progress"), "");
            return;
        }

        match self.wallet.add_generated_key(label) {
            Ok(challenge) => {
                // Before anything else: this account came into existence a
                // moment ago, and now is the only time that can be said with a
                // straight face.
                self.remember_birthday(label);
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
            self.notice_warning("add_node", NoteVm::plain("node-duplicate"), "");
            return;
        }

        let Some(store) = &self.store else {
            self.notice_warning("add_node", NoteVm::plain("node-store-unavailable"), "");
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
            self.notice_warning("add_node", NoteVm::plain("node-not-saved"), "");
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
        // A fresh manager is also what disarms spending: the opt-in is a field
        // on it, so the arm cannot follow somebody onto another chain. See
        // `NodeManager`'s `allow_spending` for why that has to stay true.
        self.nodes = NodeManager::new(shipped_nodes(&network, self.mock), network);

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
        self.key_funds.clear();
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
        // The other chain's address book has its own identities in it, and an
        // i-address means a different name — or nothing at all — over there.
        self.named_addresses = false;
        // And its own oracle, its own key, and its own idea of what is switched
        // off. Carrying one chain's answer onto another is how a wallet offers
        // a conversion on a halted chain.
        self.halt = None;
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

        self.notice_info("network_switched", NoteVm::with("chain-switched", [label]));

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
                name: known.name,
            })
            .collect();

        let _ = self.events.send(Event::AddressBook(rows));
    }

    /// How long a reading stands in for itself after a read fails.
    ///
    /// This state changes on the order of weeks — the mainnet halt at the time
    /// of writing had stood since July — so a reading from four minutes ago is
    /// the same fact measured slightly earlier, not a guess.
    const HALT_STANDBY: std::time::Duration = std::time::Duration::from_mins(15);

    /// Ask the chain's oracle whether the protocol has switched anything off.
    ///
    /// Two reads and no more: the tip, which is already in hand, and one
    /// `getidentity`. It is asked when a chain first answers and again when the
    /// chain changes, because nothing else moves it.
    fn read_chain_halt(&mut self) {
        if self.reading_halt {
            return;
        }
        let Some(chain) = self.chain() else {
            return;
        };
        let Some(network) = self.nodes.active().and_then(|node| node.network.clone()) else {
            return;
        };
        // No oracle for this chain. Not a failure and not a clear answer — see
        // `Status::unconfigured`.
        let Some(oracle) = network.oracle() else {
            self.halt = None;
            let _ = self.events.send(Event::ChainHalt(halt_vm(
                &upgrade::Status::unconfigured(),
                false,
            )));
            return;
        };

        self.reading_halt = true;
        self.blocking.dispatch(
            move || {
                use verus_sdk::network::ChainReader;

                let found = chain.identity_content(oracle.identity).ok().map(|content| {
                    content
                        .content_multimap
                        .get(oracle.content_key)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(|value| value.as_bytes().map(<[u8]>::to_vec))
                                .collect()
                        })
                        // The key is absent, which is the healthy state and is
                        // an **answer** — an empty list, not a failed read.
                        .unwrap_or_default()
                });
                Work::ChainHalt(found)
            },
            self.work.clone(),
        );
    }

    fn finish_chain_halt(&mut self, found: Option<&[Vec<u8>]>) {
        self.reading_halt = false;
        let tip = self.nodes.active().and_then(|node| node.tip).unwrap_or(0);

        let (status, stale) = match found {
            Some(values) => {
                let status = upgrade::read(values, tip);
                self.halt = Some((std::time::Instant::now(), status.clone()));
                (status, false)
            }
            // The read failed. Stand in with the last good one for a bounded
            // window — and **do not store this**, or one dropped connection
            // pins the banner for as long as the window lasts and every failed
            // refresh extends it.
            None => match &self.halt {
                Some((at, last)) if at.elapsed() < Self::HALT_STANDBY => (last.clone(), true),
                // Nothing to stand in with, or too old. Unknown, which outranks
                // info and is never the same as clear.
                _ => (upgrade::Status::unknown(), false),
            },
        };

        if status.conversions_halted {
            tracing::warn!(
                reason = %status.note.code,
                "the protocol has conversions switched off on this chain",
            );
        }
        let _ = self.events.send(Event::ChainHalt(halt_vm(&status, stale)));
    }

    /// Ask the chain what the unnamed identities in the address book are called.
    ///
    /// # Why this exists at all
    ///
    /// Because a payment to a VerusID records the **i-address** it resolved to
    /// — that is what the transaction pays, and what anybody can check
    /// afterwards — so the list of people this wallet has paid is a list of
    /// `i4YzoP8Z…`. Which is nobody.
    ///
    /// The name is usually free: `remember_recipient` writes it at the moment
    /// the payment lands, from the lookup that resolved it. This is for the
    /// rest — rows written before that existed, and i-addresses somebody pasted
    /// rather than typed as a name.
    ///
    /// **Once per session, and only for rows that have no name.** A name does
    /// not change; an identity can be *re-pointed*, but the address book's job
    /// here is to say who a row is, and re-asking on a timer would be a request
    /// per contact per refresh for an answer that is nearly always the same.
    /// The send review is where a changed identity is caught, against the
    /// `identity_name` table, which is a different question asked at the moment
    /// it matters.
    fn name_known_identities(&mut self) {
        if self.named_addresses {
            return;
        }
        let Some(store) = &self.store else {
            return;
        };
        // Only i-addresses, and only ones with nothing recorded. A transparent
        // R-address is not an identity and asking about one is a request whose
        // answer is always "no such thing".
        let wanted: Vec<String> = store
            .known_addresses()
            .into_iter()
            .filter(|known| known.name.is_empty() && known.address.starts_with('i'))
            .map(|known| known.address)
            .collect();
        if wanted.is_empty() {
            self.named_addresses = true;
            return;
        }
        let Some(chain) = self.chain() else {
            return;
        };

        self.named_addresses = true;
        self.blocking.dispatch(
            move || {
                use verus_sdk::network::ChainReader;
                let found = wanted
                    .into_iter()
                    .filter_map(|address| {
                        let record = chain.identity(&address).ok()?;
                        Some((
                            address,
                            pecu_protocol::format::safe_name(&record.fully_qualified_name),
                        ))
                    })
                    .collect();
                Work::AddressNames(found)
            },
            self.work.clone(),
        );
    }

    fn finish_address_names(&mut self, found: &[(String, String)]) {
        if found.is_empty() {
            return;
        }
        if let Some(store) = &self.store {
            for (address, name) in found {
                store.name_address(address, name);
            }
        }
        self.emit_address_book();
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

        // The name, when this payment is the thing that resolved it. Free —
        // the lookup already happened, on the way to building the transaction —
        // and it is the case that matters most, because a VerusID paid by name
        // is exactly the row that would otherwise show an i-address.
        let name = self
            .identity
            .as_ref()
            .filter(|found| found.address == address && !found.revoked)
            .map(|found| found.name.clone())
            .or_else(|| self.identities.get(&address).map(|mine| mine.name.clone()));

        if let Some(store) = &self.store {
            store.note_payment(&address, now());
            if let Some(name) = &name {
                store.name_address(&address, name);
            }
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

        // The send screen's list of people this wallet has paid. Names for the
        // i-addresses in it, once, when somebody is actually looking at it —
        // see `name_known_identities`.
        if screen == pecu_protocol::ScreenId::Send {
            self.name_known_identities();
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
        self.notice_warning("node_failover", NoteVm::plain("node-failover"), "");
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

        // Uncorroborated, and these bytes are the one case where that is not a
        // gap: a resend hands over a transaction that was already built and
        // signed, so there is no funding set to hold against anything.
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
                self.notice("resend", NoteVm::plain("resend-unconfirmed"), &error);
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

    fn change_passphrase(&mut self, old: &pecu_protocol::Secret, new: &pecu_protocol::Secret) {
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
    fn set_allow_spending(&mut self, on: bool, typed: &str) {
        if self.nodes.set_allow_spending(on, typed) {
            self.emit_network();
            return;
        }

        let _ = self
            .events
            .send(Event::Notice(pecu_protocol::UiError::simple(
                "spend_confirmation",
                // The word that was expected, named in the refusal. It differs
                // per chain, so "that was not the word" without saying which
                // word would leave somebody guessing.
                NoteVm::with("spend-confirm-word", [self.confirm_word()]),
                String::new(),
                pecu_protocol::Severity::Warning,
            )));
    }

    /// What the spending guard is doing on the chain the wallet is set to.
    ///
    /// Decided here rather than sent as two flags, so the interface cannot draw
    /// the fourth combination — "no gate, and it is open" — that a pair of
    /// bools would allow.
    fn spend_gate(&self) -> pecu_protocol::SpendGate {
        use pecu_protocol::SpendGate;

        match self.nodes.requested() {
            Some(network) if network.may_be_real_money() => {
                if self.nodes.allow_spending() {
                    SpendGate::Open
                } else {
                    SpendGate::Closed
                }
            }
            // No chain chosen is not a chain that costs nothing — but there is
            // also nothing to name in a confirmation, and `set_allow_spending`
            // refuses outright in that state, so the control has nothing to do.
            _ => SpendGate::NotNeeded,
        }
    }

    /// The word the core will accept to arm spending: the requested chain's own
    /// name. Empty when no chain has been chosen, which `set_allow_spending`
    /// refuses outright.
    fn confirm_word(&self) -> String {
        self.nodes
            .requested()
            .map(|network| network.chain_name().to_string())
            .unwrap_or_default()
    }

    // ── Send ────────────────────────────────────────────────────────────────

    /// Check a draft, and say what the wallet knows about the recipient.
    ///
    /// Runs on every keystroke, so it touches nothing but memory: address
    /// parsing and amount parsing are both offline and both exact, and the name
    /// comes from a map that was loaded at startup.
    /// The shielded balance in satoshis, or zero when there is none to speak of.
    ///
    /// Zero and "there is no shielded account" are deliberately the same number
    /// here, because for arithmetic they are: neither can pay for anything. The
    /// difference is carried by `WalletVm::shielded_note`, which is what the
    /// interface reads to decide whether to offer the choice at all.
    /// Build, keep or drop the shielded account so it matches the wallet.
    ///
    /// Three transitions, and each has to be right:
    ///
    /// * the wallet has a shielded address and this does not know it yet — make
    ///   one, which costs a viewing-key reconstruction and nothing else;
    /// * the address is the same one — keep what is there, **including whatever
    ///   has been scanned**. Rebuilding here would silently discard the scan on
    ///   every publish, which happens several times a second;
    /// * the wallet has none — drop it. That covers locking, switching to a key
    ///   that cannot have one, and a key whose phrase is not BIP-39.
    fn sync_shielded_state(&mut self) {
        let view = self.wallet.view();

        if view.shielded_address.is_empty() {
            self.shielded = None;
            return;
        }

        if self
            .shielded
            .as_ref()
            .is_some_and(|held| held.address() == view.shielded_address)
        {
            return;
        }

        match self.wallet.shielded_view() {
            Some(view) => match shielded::Shielded::watching(&view) {
                Ok(watching) => self.shielded = Some(self.with_kept_scan(&view, watching)),
                Err(error) => {
                    tracing::warn!(%error, "the shielded account could not be watched");
                    self.shielded = None;
                }
            },
            None => self.shielded = None,
        }
    }

    /// Fold a kept scan into a freshly built account, when there is one to fold.
    ///
    /// # Why every failure here is silent
    ///
    /// There is exactly one consequence to any of them — no kept scan, so the
    /// next one starts from the birthday — and it is not a consequence anybody
    /// needs to be told about in a toast. A wallet that has just been unlocked
    /// with a *different* key reaches the `WrongAccount` arm every time, and
    /// that is the system working.
    ///
    /// The one that is logged at warning is a blob that will not open: that
    /// means the file was edited or the wallet id changed underneath it, which
    /// is worth a line in a log even though the answer is still "scan again".
    fn with_kept_scan(
        &self,
        view: &pecu_keystore::ShieldedView,
        fresh: shielded::Shielded,
    ) -> shielded::Shielded {
        let (Some(store), Some(vault)) = (self.store.as_ref(), self.wallet.vault()) else {
            return fresh;
        };
        let Some(sealed) = store.shielded_scan() else {
            return fresh;
        };

        let opened = match vault.open_blob(Self::SHIELDED_SCAN_BLOB, &sealed) {
            Ok(opened) => opened,
            Err(error) => {
                tracing::warn!(%error, "the kept shielded scan would not open");
                return fresh;
            }
        };

        let kept: shielded::Kept = match serde_json::from_slice(&opened) {
            Ok(kept) => kept,
            Err(error) => {
                tracing::warn!(%error, "the kept shielded scan would not decode");
                return fresh;
            }
        };

        match shielded::Shielded::restore(view, kept) {
            Ok(restored) => {
                tracing::info!(
                    scanned_to = ?restored.scanned_to(),
                    balance = restored.balance(),
                    notes = restored.note_count(),
                    "a kept shielded scan was taken up where it stopped",
                );
                restored
            }
            Err(error) => {
                tracing::debug!(%error, "the kept shielded scan is not this account");
                fresh
            }
        }
    }

    /// Write the scan down, sealed, so the next launch continues it.
    ///
    /// Best effort throughout, and every early return means the same thing: the
    /// next launch scans from the birthday instead. That is slow, never wrong,
    /// and not worth failing an operation over — which is why nothing here
    /// returns a `Result`.
    ///
    /// Locked is one of those cases and is the reason this is called where it
    /// is: a scan that comes back after an auto-lock has nowhere to put itself,
    /// because the data key is gone. It is dropped rather than held for later.
    fn keep_shielded_scan(&self) {
        let (Some(store), Some(vault), Some(watching)) = (
            self.store.as_ref(),
            self.wallet.vault(),
            self.shielded.as_ref(),
        ) else {
            return;
        };
        let Some(kept) = watching.keep() else {
            return;
        };

        let Ok(plain) = serde_json::to_vec(&kept) else {
            tracing::warn!("the shielded scan would not encode");
            return;
        };

        match vault.seal_blob(Self::SHIELDED_SCAN_BLOB, &plain) {
            Ok(sealed) => store.save_shielded_scan(&sealed, now()),
            Err(error) => tracing::warn!(%error, "the shielded scan could not be sealed"),
        }
    }

    /// Remember where shielded notes are read from, or forget it.
    ///
    /// Checked before it is saved: plaintext is refused to anything but
    /// loopback, and credentials in the address are refused outright. Finding
    /// that out at the next scan would put the complaint minutes away from the
    /// typing that caused it.
    ///
    /// Not checked here: which gRPC dialect the address speaks. `LightServer`
    /// probes both when it connects, so a lightwalletd and a grpc-web proxy in
    /// front of one are equally valid things to type.
    fn set_light_server(&mut self, url: &str) {
        let url = url.trim();

        if url.is_empty() {
            self.light_server = None;
            self.shielded_scan_forgotten();
        } else if let Err(refused) = pecu_chain::validate_light_url(url) {
            self.notice(
                "light_server",
                NoteVm::plain("light-server-unusable"),
                &refused,
            );
            return;
        } else {
            self.light_server = Some(url.to_string());
        }

        if let Some(store) = self.store.as_ref() {
            store.set_setting("light_server", self.light_server.as_deref().unwrap_or(""));
        }
        self.emit_network();
        self.scan_shielded();
    }

    /// Drop what was scanned, because it came from a server nobody is asking
    /// any more.
    ///
    /// A balance from an endpoint that has been removed is a figure with no
    /// source. Better to have none.
    fn shielded_scan_forgotten(&mut self) {
        if let Some(store) = self.store.as_ref() {
            store.forget_shielded_scan();
        }
        if let Some(view) = self.wallet.shielded_view() {
            if let Ok(fresh) = shielded::Shielded::watching(&view) {
                self.shielded = Some(fresh);
            }
        }
    }

/// What a kept shielded scan is sealed as.
///
/// One name, used by both the write and the read, because the vault
/// authenticates it: a mismatch is a decryption failure rather than a
/// mysterious empty result. See `Vault::seal_blob`.
const SHIELDED_SCAN_BLOB: &str = "shielded-scan";

/// The shortest a Verus block is assumed to take, in seconds.
///
/// Half the one-minute target, deliberately. This divides an elapsed time to
/// estimate a number of blocks, so a smaller number estimates *more* blocks and
/// starts a scan *earlier* — which is the side to be wrong on.
const FASTEST_BLOCK_SECONDS: u64 = 30;

/// How far before an estimated birthday a scan starts anyway.
///
/// A floor under the estimate above, so even a birthday settled the instant
/// after a key is generated reaches back a hundred blocks. At Verus' block time
/// that is under two hours, and it costs a fraction of a second to scan.
const BIRTHDAY_MARGIN: u64 = 100;

/// How many blocks one stride of a shielded scan covers.
///
/// A first scan with no birthday runs from Sapling activation, which on
/// VRSCTEST is the whole chain. Doing that in a single call means minutes of
/// silence; doing it in strides means the interface can say how far it has got.
///
/// 50 000 blocks measured at roughly ten seconds — the throughput is about
/// 0.2s per thousand, and it improves with the stride because the per-request
/// cost spreads over more blocks. Smaller strides would report more often and
/// finish later.
const SCAN_STRIDE: u64 = 50_000;

/// How many times one stride of a scan is retried before it stops.
///
/// Transport failures on a scan this long are ordinary rather than
/// exceptional — a full pass is about twelve hundred requests over the open
/// internet. One measured here was "Error while decoding chunks" on a range
/// that answered perfectly a minute later, and it cost the whole scan. Three
/// attempts turn a hiccup into a pause.
const SCAN_ATTEMPTS: u32 = 3;

    /// Look for shielded notes, off the actor.
    ///
    /// # Where a scan starts, in the three cases there are
    ///
    /// * **A scan is already under way in this wallet's memory** — continue it.
    ///   `sync` picks up after the last block it finished, and proves the new
    ///   range descends from it rather than assuming so.
    /// * **A scan was kept from a previous run** — the same thing. It was
    ///   restored when the account was built, so by the time this runs it is
    ///   indistinguishable from the case above, which is the point of keeping
    ///   it.
    /// * **Neither** — start at this key's birthday, if it has one. A key
    ///   generated by this wallet does: the tip at the moment it was created.
    ///   An imported phrase does not and cannot, so it starts at Sapling
    ///   activation and walks the chain once. See `remember_birthday` for why
    ///   guessing there is the one mistake that loses money quietly.
    fn scan_shielded(&mut self) {
        if self.scanning {
            return;
        }
        let Some(url) = self.light_server.clone() else {
            return;
        };
        if !self.wallet.is_unlocked() {
            return;
        }
        // Before the birthday is read, not after: this is the tick that turns a
        // pending one into a height, and reading first would send exactly one
        // full-chain scan for every wallet created before its node answered.
        self.settle_birthday();
        // Cloned rather than taken, so the balance on screen survives the scan
        // instead of blinking to zero for the length of it.
        let Some(watching) = self.shielded.clone() else {
            return;
        };

        // `None` for a continuation — `sync` picks up where it stopped — and
        // for a first scan when nobody has named a height, which the worker
        // then resolves to Sapling activation.
        let from = if watching.scanned_to().is_some() {
            None
        } else {
            match self.light_birthday() {
                Birthday::Known(height) => Some(height),
                // Wait. The height is one tip away and the account is minutes
                // old, so there is nothing to miss by waiting and a whole chain
                // to walk by not.
                Birthday::Pending => {
                    tracing::debug!("holding the first shielded scan until the birthday settles");
                    return;
                }
                Birthday::Unknown => None,
            }
        };

        let network = self
            .nodes
            .requested()
            .cloned()
            .unwrap_or(pecu_chain::Network::Testnet);

        self.scanning = true;
        // A second sender, so the worker can report progress on the way rather
        // than only its result at the end.
        let progress = self.work.clone();
        // Bound out here: the worker closure is `move` and has no `Self` to
        // reach an associated constant through.
        let stride = Self::SCAN_STRIDE;
        let attempts = Self::SCAN_ATTEMPTS;
        self.blocking.dispatch(
            move || {
                let mut watching = watching;

                let server = match pecu_chain::LightServer::connect(&url, &network) {
                    Ok(server) => server,
                    // Nothing was scanned, so there is no partial state to keep.
                    Err(e) => {
                        return Work::Scanned {
                            watching: Box::new(None),
                            failed: Some(e.to_string()),
                            restart: false,
                        }
                    }
                };

                let outcome =
                    walk_the_chain(&mut watching, &server, from, stride, attempts, &progress);
                let restart = matches!(outcome, Err(ScanStop::StartAgain(_)));
                let outcome = match outcome {
                    Ok(()) => Ok(()),
                    Err(ScanStop::Stopped(why) | ScanStop::StartAgain(why)) => Err(why),
                };

                // The scan comes back either way. What it managed is worth
                // keeping even when the last stride failed — the alternative is
                // starting at block two again, for a hiccup.
                Work::Scanned {
                    // Nothing to keep when the whole thing is being discarded,
                    // and handing it back anyway would invite somebody to store
                    // it out of habit.
                    watching: Box::new((!restart).then_some(watching)),
                    failed: outcome.err(),
                    restart,
                }
            },
            self.work.clone(),
        );
    }

    /// Where a first scan starts, when somebody has said.
    ///
    /// `None` means nobody has, and the scan then begins at **Sapling
    /// activation** — see `scan_shielded`.
    ///
    /// # Why this is no longer defaulted to the tip
    ///
    /// It was, and it was wrong in the one way that loses money quietly: it
    /// recorded the height at the moment a light server was configured, so a
    /// wallet found nothing that arrived before somebody happened to fill in a
    /// setting. That is not a hypothetical — it hid a real payment of 10
    /// VRSCTEST, sixty-three blocks the wrong side of the line.
    ///
    /// There is no honest way to guess it either. The same recovery phrase may
    /// have been used in another wallet years earlier, so when *this* wallet
    /// derived the account says nothing about when the account was first paid.
    /// A key's creation date is a fact about this installation, not about the
    /// account.
    fn light_birthday(&self) -> Birthday {
        let (Some(store), Some(label)) = (self.store.as_ref(), self.wallet.active_key.as_ref())
        else {
            return Birthday::Unknown;
        };

        if let Some(height) = store
            .setting(&Self::birthday_key(label))
            .and_then(|height| height.parse().ok())
        {
            return Birthday::Known(height);
        }
        if store.setting(&Self::birthday_pending_key(label)).is_some() {
            return Birthday::Pending;
        }
        Birthday::Unknown
    }

    /// Where a key's *unsettled* birthday is written.
    ///
    /// Holds the wall-clock second the key was generated, because the height it
    /// wants is not knowable yet — see [`Self::settle_birthday`].
    fn birthday_pending_key(label: &str) -> String {
        format!("light_birthday_pending:{label}")
    }

    /// Turn a pending birthday into a height, once a node has reported one.
    ///
    /// # Why this exists at all
    ///
    /// A key is generated during onboarding, seconds after launch, and the node
    /// probe has usually not come back yet. Writing nothing in that case looked
    /// safe and was nearly useless: measured against a real testnet node, the
    /// tip was unknown at creation almost every time, so the birthday was
    /// recorded almost never and every new wallet still walked the whole chain.
    ///
    /// # Why the height is estimated backwards rather than taken as read
    ///
    /// The tip that finally arrives is the tip *now*, not the tip when the key
    /// was made, and the gap between them is blocks this wallet would skip. So
    /// the gap is estimated from wall-clock and subtracted, and every choice in
    /// that estimate leans the same way — earlier, meaning more scanning:
    ///
    /// * [`Self::FASTEST_BLOCK_SECONDS`] is half Verus' one-minute target, so a
    ///   chain running fast is still overestimated rather than under.
    /// * [`Self::BIRTHDAY_MARGIN`] is a floor, so even an instantaneous settle
    ///   starts a hundred blocks early.
    /// * A clock that has gone backwards yields zero elapsed, and the margin
    ///   carries it.
    ///
    /// Scanning more than needed costs a fraction of a second. Scanning less
    /// costs somebody their money, quietly, and this wallet has done that once.
    fn settle_birthday(&mut self) {
        let (Some(store), Some(label)) = (self.store.as_ref(), self.wallet.active_key.clone())
        else {
            return;
        };
        let pending = Self::birthday_pending_key(&label);
        let Some(generated_at) = store
            .setting(&pending)
            .and_then(|second| second.parse::<i64>().ok())
        else {
            return;
        };
        let Some(tip) = self.nodes.active().and_then(|node| node.tip) else {
            return;
        };

        let elapsed = u64::try_from(now().saturating_sub(generated_at)).unwrap_or(0);
        let blocks = (elapsed / Self::FASTEST_BLOCK_SECONDS).max(Self::BIRTHDAY_MARGIN);
        // Two at the earliest, for the same protocol reason a scan starts
        // there: height zero cannot be asked for.
        let birthday = u64::from(tip).saturating_sub(blocks).max(2);

        tracing::info!(
            label,
            tip,
            elapsed,
            birthday,
            "a pending birthday was settled against the first tip this wallet saw",
        );
        store.set_setting(&Self::birthday_key(&label), &birthday.to_string());
        store.forget_setting(&pending);
    }

    /// Where a key's birthday is written, per key rather than per wallet.
    ///
    /// A wallet holds several keys and each is its own shielded account with
    /// its own history, so one height for all of them would be one account's
    /// answer applied to another's — and applied in the direction that skips
    /// blocks, which is the direction that loses money.
    ///
    /// Keyed by label, which is what names a key everywhere else here. A rename
    /// therefore loses the birthday and the next scan starts at Sapling
    /// activation: slower, never wrong, and the safe way round for something a
    /// wallet cannot ask anybody to confirm.
    fn birthday_key(label: &str) -> String {
        format!("light_birthday:{label}")
    }

    /// Record that a key generated here cannot have been paid before now.
    ///
    /// # Why this is only ever called for a key this wallet generated
    ///
    /// The claim being written down is a real one, and it is only true for
    /// fresh entropy: an account that came into existence a moment ago has no
    /// history, so the chain tip is a correct floor for it.
    ///
    /// An **imported** phrase gets nothing. The same words may have been in
    /// another wallet for years, and when *this* wallet derived the account
    /// says nothing whatever about when the account was first paid. That is not
    /// a hypothetical either: an earlier version recorded the tip at the moment
    /// a light server was configured, and hid a real payment of 10 VRSCTEST
    /// sixty-three blocks the wrong side of the line.
    ///
    /// # And why an unknown tip records nothing
    ///
    /// The wallet has to know a height for the claim to be about anything. If
    /// no node has answered yet, there is no honest floor to write, and the
    /// first scan covers the chain — minutes, once. Guessing here would trade a
    /// wait nobody minds for a balance that is quietly short.
    fn remember_birthday(&self, label: &str) {
        let Some(store) = self.store.as_ref() else {
            return;
        };

        if let Some(tip) = self.nodes.active().and_then(|node| node.tip) {
            tracing::info!(label, tip, "a freshly generated key was given its birthday");
            store.set_setting(&Self::birthday_key(label), &tip.to_string());
            return;
        }

        // The ordinary case, not the exception: a key is generated during
        // onboarding and the node probe has usually not come back yet. Measured
        // against a real testnet node, it had not come back *every* time. So
        // the moment is recorded instead, and `settle_birthday` turns it into a
        // height as soon as there is one to work from. Until it does, the
        // account is not scanned at all — there is nothing to find in an
        // account that came into existence a moment ago, and a scan started
        // before the birthday is known would be the whole chain, which is
        // exactly what the birthday exists to avoid.
        tracing::info!(
            label,
            "no node has reported a tip yet; this key's birthday is pending",
        );
        store.set_setting(&Self::birthday_pending_key(label), &now().to_string());
    }

    /// A scan is under way and has got this far.
    ///
    /// Reported because a first scan with no birthday covers the whole chain —
    /// minutes, measured — and a wallet that goes quiet for minutes is a wallet
    /// somebody restarts.
    fn scan_progress(&mut self, scanned_to: u64, tip: u64, start: u64) {
        let span = tip.saturating_sub(start).max(1);
        let done = scanned_to.saturating_sub(start);
        // Integer arithmetic: a percentage needs no floating point, and this
        // number is read by a person rather than computed with.
        let percent = u32::try_from(done.saturating_mul(100) / span).unwrap_or(100);

        self.scan_share = Some(percent.min(100));
        self.emit_wallet();
    }

    /// A scan came back — completely, or as far as it got.
    ///
    /// Whatever was scanned is kept in both cases. A partial result is not a
    /// failed one: it is fewer blocks than asked for, and the next scan
    /// continues from there instead of beginning at block two again.
    fn finish_scan(
        &mut self,
        watching: Option<shielded::Shielded>,
        failed: Option<String>,
        restart: bool,
    ) {
        self.scanning = false;
        self.scan_share = None;

        if restart {
            tracing::warn!(
                "the chain moved further back than the kept scan can verify; discarding it",
            );
            // Forgets the stored blob and resets what is in memory, so the next
            // tick is a first scan from the birthday rather than a continuation
            // of something the chain no longer agrees with.
            self.shielded_scan_forgotten();
            self.notice_warning(
                "shielded_scan",
                NoteVm::plain("shielded-scan-restarting"),
                failed.as_deref().unwrap_or_default(),
            );
            self.emit_wallet();
            return;
        }

        if let Some(watching) = watching {
            tracing::info!(
                balance = watching.balance(),
                notes = watching.note_count(),
                scanned_to = ?watching.scanned_to(),
                complete = failed.is_none(),
                "shielded scan came back",
            );
            self.shielded = Some(watching);
            // Written down before anything is reported, and on a partial scan
            // as well as a complete one. What it managed is exactly what the
            // next launch should not have to do again — a hiccup at block
            // 900 000 must not cost the 900 000 blocks in front of it.
            self.keep_shielded_scan();
            self.emit_wallet();
        }

        if let Some(why) = failed {
            // Warned rather than shouted about: a light server that is down
            // costs a balance, not money, and the transparent half of the
            // wallet is unaffected.
            tracing::warn!(%why, "the shielded scan stopped early");
            self.notice_warning("shielded_scan", NoteVm::plain("shielded-scan-failed"), &why);
        }
    }

    fn shielded_balance(&self) -> u64 {
        self.shielded
            .as_ref()
            .map_or(0, shielded::Shielded::balance)
    }

    /// What the key that would sign a payment holds, on its own.
    ///
    /// Zero for an address no refresh has covered yet, which is the same answer
    /// the wallet-wide figure gives before its first read — the send form is
    /// already written to treat a balance it has not been told about as a
    /// balance it cannot promise. What it must not do is quote the *wallet's*
    /// figure for one key: two keys with 5 coins each is not one key with 10,
    /// and no transaction this wallet can build spends both.
    fn active_key_funds(&self) -> portfolio::KeyFunds {
        self.wallet
            .active_address()
            .and_then(|address| self.key_funds.get(&address).copied())
            .unwrap_or_default()
    }

    fn validate_draft(&mut self, draft: &pecu_protocol::SendDraft) {
        self.last_draft = draft.clone();
        self.maybe_resolve_identity(draft.to.trim());

        // Checked against the balance the money is coming out of, not against
        // whichever one happens to be larger. A shielded payment measured
        // against the transparent balance would tell somebody they can afford
        // something they cannot, and the refusal would then arrive from the
        // builder — after the form had said it was fine.
        let against = match draft.from_pool {
            // The **active key's** transparent balance, not the wallet's. The
            // shielded figure is already per-key — there is only ever one
            // shielded account open — so this is the half that was wrong.
            pecu_protocol::Pool::Transparent => self.active_key_funds().spendable,
            pecu_protocol::Pool::Shielded => {
                verus_sdk::money::Amount::from_sat(self.shielded_balance())
            }
        };

        let mut verdict = send::validate(draft, against);

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
            move || Work::IdentityChangePrepared {
                ticket,
                described,
                needs_confirmation,
                result: Box::new(identity::prepare(&chain, &vault, &label, &address, &change)),
            },
            self.work.clone(),
        );
    }

    fn finish_identity_change_prepared(
        &mut self,
        ticket: u64,
        described: NoteVm,
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
                    description: described,
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
        // same reason the spending switch is checked here. A guard the UI owns
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
        // Uncorroborated. This spends the same transparent coins a payment
        // does, and nothing asks a second node about them — see
        // `pecu_chain::corroborate` for which two paths are covered and why the
        // rest need a token on `Chain::broadcaster` to be enumerated.
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
                problem: NoteVm::none(),
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
        // Uncorroborated, for the reason `confirm_identity_change` states: a
        // name commitment is funded from the same transparent set a payment is,
        // and only the send path asks a second node about it.
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
                        note: NoteVm::plain("claim-registered"),
                        deadline: NoteVm::none(),
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
            Some(true) => NoteVm::plain("name-taken"),
            Some(false) => NoteVm::none(),
            None => NoteVm::plain("name-unchecked"),
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
            self.notice_warning("registration_busy", NoteVm::plain("name-claim-busy"), "");
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
            self.notice_warning("registration_name", problem, "");
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
        // Uncorroborated too. The registration fee comes out of the same
        // transparent coins, and no second node is asked about them.
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

                    // One history per pool that has started, because a pool
                    // that has not is not a market and a chart of it is a
                    // picture of nothing. Bounded by the number of converters
                    // rather than by the number of currencies — a pool's series
                    // gives the change for every currency priced through it, so
                    // this is around twenty calls on VRSCTEST rather than fifty.
                    //
                    // Sequential and slow, deliberately: it happens when
                    // somebody opens the markets screen, once, and asking a
                    // public node twenty questions at once is worse manners
                    // than making them wait two seconds.
                    let tip = chain.block_count().unwrap_or_default();
                    let from = tip.saturating_sub(market::WINDOW_DAYS * market::BLOCKS_PER_DAY);
                    let histories = converters
                        .iter()
                        .filter(|entry| {
                            !entry.last_notarization["prelaunch"]
                                .as_bool()
                                .unwrap_or(false)
                        })
                        .map(|entry| {
                            let samples = chain
                                .currency_state_range(
                                    &entry.converter_id,
                                    from,
                                    tip,
                                    market::BLOCKS_PER_DAY,
                                )
                                .unwrap_or_default();
                            (entry.converter_id.clone(), samples)
                        })
                        .collect();

                    // The names `listcurrencies` cannot give.
                    //
                    // With no query it answers `systemtype: "local"` — every
                    // currency launched *from this chain* — and a bridged one
                    // was launched from the system it came across, so it is
                    // simply not in the reply. On VRSCTEST that is 316 entries
                    // that do not include DAI.vETH, which is the currency this
                    // wallet quotes every price in. The effect was total: no
                    // quote id, so no price on any row, no change column and no
                    // chart, on a screen that looked merely empty rather than
                    // broken.
                    //
                    // Asked one at a time because the SDK's `list_currencies`
                    // takes no query and this workspace does not reach past it.
                    // Bounded by what the book actually holds — eight of
                    // forty-nine on VRSCTEST — and it happens once, when
                    // somebody opens the screen, beside the twenty history
                    // reads already above it.
                    //
                    // A lookup that fails is dropped rather than retried: the
                    // currency keeps its i-address on screen, which is `name_of`
                    // already doing the right thing with an unknown id.
                    let known: std::collections::BTreeSet<&str> = catalog
                        .iter()
                        .map(|summary| summary.currency_id.as_str())
                        .collect();
                    let imported: Vec<(String, String)> = market::currencies_in(&converters)
                        .into_iter()
                        .filter(|id| !known.contains(id.as_str()))
                        .filter_map(|id| {
                            let found = chain.currency_definition(&id).ok()?;
                            let name =
                                pecu_protocol::format::safe_name(&found.fully_qualified_name);
                            Some((id, name))
                        })
                        .collect();

                    Ok((catalog, converters, histories, native, imported))
                })();

                Work::Markets(Box::new(read))
            },
            self.work.clone(),
        );
    }

    #[allow(clippy::type_complexity)]
    fn finish_markets(
        &mut self,
        read: Result<
            (
                Vec<verus_sdk::network::CurrencySummary>,
                Vec<verus_sdk::network::CurrencyConverter>,
                Vec<(String, Vec<verus_sdk::network::CurrencyStateAt>)>,
                String,
                Vec<(String, String)>,
            ),
            String,
        >,
    ) {
        self.markets = Markets::Asked;
        self.busy(TaskKind::RefreshingBalance, false);

        let (catalog, converters, histories, native, imported) = match read {
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
                    pecu_protocol::format::safe_name(&summary.fully_qualified_name),
                )
            })
            .collect();
        // The bridged ones, which `listcurrencies` never returned. See the
        // closure above.
        self.market_names.extend(imported);

        // The quote currency's i-address, found by the name it is known by.
        // Without it there is nothing to price against and the book is empty —
        // which is the honest outcome on a chain that has no stablecoin, and
        // was the *dishonest* one on a chain that has a stablecoin the catalog
        // does not list.
        //
        // From the merged map rather than from `catalog`, which is the whole
        // point: on VRSCTEST the quote currency is bridged, so it is only ever
        // in the half that had to be asked for by name.
        let quote = self
            .market_names
            .iter()
            .find(|(_, name)| name.as_str() == Self::QUOTE)
            .map(|(id, _)| id.clone())
            .unwrap_or_default();
        let mut pools: Vec<market::Pool> = converters
            .iter()
            .filter_map(market::Pool::from_converter)
            .collect();
        for (id, samples) in &histories {
            if let Some(pool) = pools.iter_mut().find(|pool| pool.id == *id) {
                pool.remember(samples);
            }
        }
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
            rows: market::rows(&self.market, &self.market_names, now()),
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
                now(),
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
                // Kept with the ticket, so pressing Review signs against the
                // floor that was on screen rather than one worked out
                // afterwards. See `Core::convert_floor`.
                self.convert_floor = Some((ticket, convert::floor(estimate.estimated_out)));
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

    /// Build and sign the conversion the form is showing. **Sends nothing.**
    ///
    /// The draft is re-checked here rather than the last `Ready` being reused.
    /// It costs one pass over data already in memory, and it closes the window
    /// where a balance refresh landed between the last keystroke and this
    /// press — a conversion whose amount is no longer held would otherwise be
    /// signed against a balance nobody has.
    fn prepare_conversion(&mut self) {
        self.wallet.touch();

        // The protocol is not taking conversions. Refused here rather than let
        // through to a node that would reject it in validation: the quote came
        // back and describes a market nobody can trade in, and signing it costs
        // a key, a round trip and somebody's confidence for nothing.
        if self
            .halt
            .as_ref()
            .is_some_and(|(_, status)| status.conversions_halted)
        {
            self.refuse_conversion(NoteVm::plain("halt-conversions"));
            return;
        }

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

        // The floor from the quote that is on screen, and only from that quote.
        // A stale one is unreachable because its ticket no longer matches —
        // see `convert_floor`. Without a current one there is no floor anybody
        // has agreed to, so there is nothing to sign.
        let Some(floor) = self
            .convert_floor
            .filter(|(ticket, _)| *ticket == self.convert_ticket)
            .map(|(_, floor)| floor)
        else {
            self.refuse_conversion(NoteVm::plain("convert-unpriced"));
            return;
        };

        let Some(label) = self.wallet.active_key.clone() else {
            return;
        };
        let Some(vault) = self.wallet.vault() else {
            return;
        };
        let Some(chain) = self.chain() else {
            return;
        };
        // The conversion is delivered to this wallet's own address, which is
        // also the refund address. Both are the active key's, and neither is
        // anything the interface got to choose.
        let recipient = self.wallet.active_address().unwrap_or_default();
        if recipient.is_empty() {
            return;
        }

        let names = self.market_names.clone();

        self.tickets += 1;
        let ticket = self.tickets;
        self.busy(TaskKind::Converting, true);

        self.blocking.dispatch(
            move || Work::Converted {
                ticket,
                result: Box::new(convert::prepare(
                    &chain, &vault, &label, &ready, &names, &recipient, floor,
                )),
            },
            self.work.clone(),
        );
    }

    fn finish_convert_prepare(
        &mut self,
        ticket: u64,
        result: Result<convert::Prepared, convert::ConvertError>,
    ) {
        self.busy(TaskKind::Converting, false);

        match result {
            Ok(prepared) => {
                let from = self.wallet.active_address().unwrap_or_default();
                // From the SIGNED bytes — see `convert::review`.
                let review = convert::review(ticket, &prepared, &from, self.spendable);
                self.conversions.insert(ticket, prepared);
                let _ = self.events.send(Event::ConvertPrepared(Box::new(review)));
            }
            Err(error) => {
                let refusal = convert_note(&error);
                self.notice("prepare_conversion", refusal.clone(), &error);
                let _ =
                    self.events
                        .send(Event::ConvertResult(pecu_protocol::SendOutcomeVm::Failed(
                            pecu_protocol::UiError::simple(
                                "prepare_conversion",
                                refusal,
                                error.to_string(),
                                pecu_protocol::Severity::Danger,
                            ),
                        )));
            }
        }
    }

    /// Send a conversion. The second — and last — place this application writes
    /// to the chain.
    ///
    /// Every guard `confirm_send` has, for the same reasons, because they are
    /// the same guards: the permit is the only route to a `Broadcaster`, and
    /// the bytes are on disk before anything is handed over. A conversion is
    /// not less irreversible than a payment; it is a payment that also changes
    /// what you hold.
    fn confirm_conversion(&mut self, ticket: u64) {
        let Some(prepared) = self.conversions.remove(&ticket) else {
            return;
        };

        // Uncorroborated. A conversion funds through `network::spendable` on
        // the same address from the same node as a payment does, and nothing
        // holds that answer against a second endpoint.
        let permit = match self.nodes.spend_permit() {
            Ok(permit) => permit,
            Err(refused) => {
                // Put it back: the user may turn spending on and try again, and
                // rebuilding would select different coins.
                self.conversions.insert(ticket, prepared);
                self.notice("spend_refused", refusal_note(&refused), &refused);
                return;
            }
        };

        let Some(chain) = self.chain() else {
            self.conversions.insert(ticket, prepared);
            return;
        };

        // The same ledger a payment uses, and deliberately.
        //
        // What it exists to protect is bytes that may already be propagating,
        // and that is exactly as true here. The recipient recorded is this
        // wallet's own address, which is the truth — a conversion pays you —
        // and it means an uncertain conversion turns up in the same list, is
        // resolved by the same "ask the node whether it confirmed", and is
        // re-sent as the same bytes rather than rebuilt.
        let record = match self.pending.commit(
            &prepared.unsent.txid,
            &prepared.unsent.hex,
            &prepared.to,
            &portfolio::coins(prepared.amount),
        ) {
            Ok(id) => id,
            Err(error) => {
                self.conversions.insert(ticket, prepared);
                self.notice("pending_commit", NoteVm::plain("pending-unsaved"), &error);
                return;
            }
        };

        self.busy(TaskKind::Converting, true);
        self.broadcasting = true;
        self.blocking.dispatch(
            move || Work::ConversionSent {
                record,
                result: Box::new(convert::broadcast(&chain, &permit, prepared)),
            },
            self.work.clone(),
        );
    }

    fn finish_convert_broadcast(
        &mut self,
        record: u64,
        result: Result<verus_sdk::network::Sent, verus_sdk::network::FlowError>,
    ) {
        use pecu_protocol::SendOutcomeVm;
        use verus_sdk::network::FlowError;

        self.busy(TaskKind::Converting, false);
        self.broadcasting = false;

        match result {
            Ok(sent) => {
                // The txid and the fee, and deliberately not which currencies
                // or how much — the same rule the payment log follows. Both are
                // on the chain and one lookup finds them, while a log holding a
                // list of what somebody converted is a file people attach to
                // bug reports.
                tracing::info!(
                    txid = %sent.txid,
                    fee = %portfolio::coins(sent.fee),
                    "a conversion was accepted by the network",
                );
                self.pending.set_state(record, pending::State::Confirmed);
                self.pending.forget_confirmed();
                let explorer_url = self.explorer_for(&sent.txid);
                let _ = self.events.send(Event::ConvertResult(SendOutcomeVm::Sent {
                    txid: sent.txid,
                    fee_display: portfolio::coins(sent.fee),
                    explorer_url,
                }));
                self.refresh();
            }

            // The node may have taken it. The bytes are on disk and the only
            // safe resolution is to ask whether it confirmed — never to
            // rebuild, which would convert twice.
            Err(FlowError::BroadcastUncertain { txid, .. }) => {
                tracing::warn!(%txid, "the outcome of a conversion broadcast is unknown");
                let explorer_url = self.explorer_for(&txid);
                let _ = self
                    .events
                    .send(Event::ConvertResult(SendOutcomeVm::Uncertain {
                        txid,
                        pending_id: record,
                        explorer_url,
                    }));
            }

            Err(error) => {
                // A refusal is unambiguous: the node understood it and said no.
                // Nothing was spent, and the record would only be noise.
                //
                // **This is the ordinary outcome while the network has DeFi
                // paused.** Every conversion is rejected, and it is rejected
                // here — by a node, in a sentence, after the wallet has done
                // everything right. That is why the message travels rather than
                // being flattened to "it did not work".
                self.pending.set_state(record, pending::State::Abandoned);
                self.pending.forget_confirmed();
                let refusal = NoteVm::plain("convert-refused-by-node");
                self.notice("convert_broadcast", refusal.clone(), &error);
                let _ = self.events.send(Event::ConvertResult(SendOutcomeVm::Failed(
                    pecu_protocol::UiError::simple(
                        "convert_broadcast",
                        refusal,
                        error.to_string(),
                        pecu_protocol::Severity::Danger,
                    ),
                )));
            }
        }
    }

    /// Throw away a signed conversion nobody agreed to send.
    fn cancel_conversion(&mut self, ticket: u64) {
        self.conversions.remove(&ticket);
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

    /// Answer the command palette.
    ///
    /// Over what is already in hand — the addresses this wallet has paid or
    /// named, and the currencies the markets read has named — rather than over
    /// the chain. A palette that made a request per keystroke would stutter,
    /// and neither list changes between two letters being typed.
    ///
    /// An empty query clears rather than listing everything: a palette that
    /// opens showing the whole wallet has answered a question nobody asked.
    ///
    /// # Why it no longer searches identities
    ///
    /// It searched the identities these keys control, and that screen is out of
    /// the rail for this build. A result that lands on a screen the wallet is
    /// otherwise not offering is worse than no result — there is no visible way
    /// back from it. The loop is deleted rather than filtered, because a filter
    /// here would have to encode which screens the interface is currently
    /// showing, and the core does not know that and should not learn it.
    ///
    /// Why the screen is out of the rail is **not** a fact about this function
    /// and is not restated here: `docs/LATER.md` §0b is the record of it,
    /// along with what it would take to bring it back. Two code
    /// comments describing the same decision are two chances to describe it
    /// differently, and this decision already had five.
    ///
    /// `kind` is the whole of the contract: it picks the icon and the screen.
    /// Adding identities back is one loop in [`palette_hits`], one arm in
    /// `wire_search`, the icon branch in `overlay.slint` — which is binary
    /// today — the palette's placeholder, which names the kinds it searches,
    /// and the assertion below that pins them to exactly two.
    fn search(&mut self, query: &str) {
        let hits = match &self.store {
            Some(store) => palette_hits(query, &store.known_addresses(), &self.market_names),
            None => palette_hits(query, &[], &self.market_names),
        };
        let _ = self.events.send(Event::SearchHits {
            query: query.to_string(),
            hits,
        });
    }

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
                    problem: NoteVm::plain("catalog-no-node"),
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
                        problem: NoteVm::with("catalog-unreadable", [error.clone()]),
                        ..Default::default()
                    },
                )));
            }
        }
    }

    /// Check a draft and hand back everything the configure screen draws.
    ///
    /// Runs on every keystroke, so it touches nothing but memory: every rule is
    /// arithmetic over what was typed, plus the tip and the launch fee the
    /// wallet is already holding. No node is asked anything.
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
    /// The permit is asked for **before** the work, the way registration does
    /// it: a launch that could not be broadcast should refuse before it costs a
    /// signature, not after. It is asked for again at the broadcast and not
    /// carried across — see `confirm_launch` for why the two are different
    /// questions.
    fn prepare_launch(&mut self, draft: &pecu_protocol::CurrencyDraft) {
        let tip = self.nodes.active().and_then(|node| node.tip).unwrap_or(0);
        if currency::problems(draft, tip).iter().any(|p| p.blocking) {
            // The interface gates its own button on this, so arriving here
            // means a second interface or a stale draft. Refused rather than
            // trusted: the checks are the core's, and this is where they bind.
            self.notice_warning("currency_launch", NoteVm::plain("launch-draft-invalid"), "");
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
            self.notice_warning("currency_launch", NoteVm::plain("launch-chain-unknown"), "");
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
        // Checked, not kept. A launch that could not be broadcast should refuse
        // before it costs a signature — but the permit that answers *now* is
        // not the authorisation the broadcast runs under. That one is taken
        // again in `confirm_launch`, against whatever the wallet is set to by
        // the time somebody presses the button.
        // Uncorroborated, like every spend outside the send path. This one is
        // only the early check; `confirm_launch` takes the permit that the
        // broadcast runs under, and it is uncorroborated as well.
        if let Err(refused) = self.nodes.spend_permit() {
            self.notice_warning("spend_refused", refusal_note(&refused), "");
            return;
        }

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
                        currency::prepare(&chain, &vault, &label, &identity, &built).map(Box::new),
                    ),
                }
            },
            self.work.clone(),
        );
    }

    fn finish_launch_prepared(
        &mut self,
        ticket: u64,
        result: Result<Box<currency::Prepared>, String>,
    ) {
        self.busy(TaskKind::PreparingSend, false);

        match result {
            Ok(prepared) => {
                let fee = prepared.launch_fee();
                let split = currency::cost(fee);
                let view = pecu_protocol::LaunchReviewVm {
                    ticket,
                    name: prepared.name.clone(),
                    description: pecu_protocol::NoteVm::with(
                        "launch-defines-once",
                        [prepared.name.clone()],
                    ),
                    fee_display: portfolio::coins(split.launch_fee),
                    deposit_display: portfolio::coins(split.deposit),
                    burned_display: portfolio::coins(split.burned),
                    // Off the signed outcome, not off the form. A launch is not
                    // instant, and the review is the last screen that can say
                    // when it begins.
                    start_block: currency::thousands(prepared.start_block()),
                };
                self.launches.insert(ticket, *prepared);
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

    /// Send it.
    ///
    /// The permit is taken **here**, not carried from `prepare_launch_under`,
    /// and the distinction is the whole guard rather than tidiness. A review
    /// stays on screen for as long as somebody leaves it there, and in that
    /// time they can turn spending off in Settings or change chains — a permit
    /// minted before either would still authorise a broadcast afterwards, and
    /// on the chain switch it would authorise one against a node on a chain the
    /// permit was never about. The signed bytes are kept; the authorisation to
    /// send them is asked for again. `confirm_send` and `confirm_conversion` do
    /// the same, for the same reason.
    fn confirm_launch(&mut self, ticket: u64) {
        let Some(prepared) = self.launches.remove(&ticket) else {
            return;
        };

        // Uncorroborated: the launch's fees come from the same transparent set
        // as a payment, checked by nobody but the active node.
        let permit = match self.nodes.spend_permit() {
            Ok(permit) => permit,
            Err(refused) => {
                // Put it back: the user may turn spending on and try again, and
                // rebuilding would produce different bytes — the ones already
                // signed are the only ones anybody agreed to.
                self.launches.insert(ticket, prepared);
                self.notice("spend_refused", refusal_note(&refused), &refused);
                return;
            }
        };

        let Some(chain) = self.chain() else {
            self.launches.insert(ticket, prepared);
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
                            [done.name.clone(), currency::thousands(done.start_block)],
                        ),
                        String::new(),
                        pecu_protocol::Severity::Info,
                    )));
                // The decision has been carried out. This is the only place the
                // file is removed by success; the other is somebody saying stop.
                self.intent.finish();
                self.emit_launch_pending();
                let _ =
                    self.events
                        .send(Event::LaunchDone(Box::new(pecu_protocol::LaunchDoneVm {
                            txid: done.txid,
                            address: done.address,
                            name: done.name,
                            start_block: currency::thousands(done.start_block),
                        })));
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
            self.notice_warning("currency_launch", NoteVm::plain("launch-busy"), "");
            return;
        }
        let tip = self.nodes.active().and_then(|node| node.tip).unwrap_or(0);
        if currency::problems(&draft, tip).iter().any(|p| p.blocking) {
            self.notice_warning("currency_launch", NoteVm::plain("launch-draft-invalid"), "");
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
                    NoteVm::with("launch-name-ready", [record.identity.clone()])
                } else {
                    NoteVm::with("launch-name-claiming", [record.identity.clone()])
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
                        NoteVm::plain("verusid-unknown")
                    }
                    other => NoteVm::with("verusid-unreadable", [other.to_string()]),
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
                    name: pecu_protocol::format::safe_name(&record.fully_qualified_name),
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

    /// Choose the notes a shielded payment will spend, or say why it cannot.
    ///
    /// Planned on the actor: choosing notes is arithmetic over state this
    /// already holds, and it fails fast for the two reasons somebody most often
    /// hits — nothing scanned, and not enough in one bundle. `Ok(None)` is a
    /// route that needs no plan rather than a plan that could not be made.
    ///
    /// It lives out here because [`Self::prepare_send`] sits on clippy's
    /// hundred-line ceiling exactly, so the function cannot take another line
    /// without something leaving it. This block is the most self-contained
    /// thing in it.
    fn shielded_plan(
        &mut self,
        draft: &pecu_protocol::SendDraft,
        route: pecu_protocol::Route,
    ) -> Result<Option<shielded::PlannedSpend>, Refused> {
        if !matches!(
            route,
            pecu_protocol::Route::Private | pecu_protocol::Route::Unshield
        ) {
            return Ok(None);
        }

        // Named separately from "this key has no shielded account". The two
        // look identical on screen — no shielded payment — and call for
        // opposite things: one is a key that can never have one, the other is a
        // setting nobody has filled in yet.
        if self.light_server.is_none() {
            self.refuse_send(
                NoteVm::plain("shielded-not-configured"),
                "no light server is configured",
            );
            return Err(Refused);
        }
        let Some(shielded) = self.shielded.as_ref() else {
            self.refuse_send(
                NoteVm::plain("shielded-none"),
                "this key has no shielded account",
            );
            return Err(Refused);
        };
        match shielded.plan_spend(&draft.to, &draft.amount) {
            Ok(plan) => Ok(Some(plan)),
            Err(error) => {
                self.refuse_send(Self::shielded_note(&error), &error.to_string());
                Err(Refused)
            }
        }
    }

    /// Turn away a "send everything" that has arrived on a route which cannot
    /// serve it, having said why.
    ///
    /// [`Refused`] rather than a `bool`, for the reason that type carries: the
    /// sentence has already been sent and the caller's only remaining move is
    /// to stop. It sits immediately below [`Self::shielded_plan`], which means
    /// the same thing by the same mechanism, and two spellings of "already
    /// reported" a dozen lines apart is one more than this file needs.
    ///
    /// The flag rides on the same draft as the pool selector, so it reaches
    /// every route whether or not the form offers it there. Ignored, a shielded
    /// send-all would send whatever string happened to be left in `amount`,
    /// which is the worst of the three things this could do.
    fn refuses_send_all(
        &mut self,
        draft: &pecu_protocol::SendDraft,
        route: pecu_protocol::Route,
    ) -> Result<(), Refused> {
        if !draft.send_all {
            return Ok(());
        }
        let Some((note, why)) = send_all_refusal(route) else {
            return Ok(());
        };
        self.refuse_send(note, why);
        Err(Refused)
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

        let to_shielded = verus_sdk::light::zaddr::decode(draft.to.trim()).is_ok();
        let route = pecu_protocol::Route::of(draft.from_pool, to_shielded);

        if self.refuses_send_all(&draft, route).is_err() {
            return;
        }

        // The parameters are settled *before* anything is dispatched. Loading
        // them takes seconds and proving takes tens of seconds, so a wallet that
        // discovered they were missing inside the worker would look like it had
        // hung and then blame the payment. This way the answer — "this needs a
        // file you do not have" — arrives immediately.
        let located = if route.needs_proving() {
            let Some(located) = params::find(self.paths.home()) else {
                self.refuse_send(
                    NoteVm::plain("shielded-params-missing"),
                    "the Sapling proving parameters are not on this machine",
                );
                return;
            };
            Some(located)
        } else {
            None
        };

        let Ok(plan) = self.shielded_plan(&draft, route) else {
            return;
        };

        let from_address = self.wallet.active_address().unwrap_or_default();
        // The chain the wallet was told to be on, not the one a node happens
        // to answer for. `Testnet` is the same default the rest of the core
        // uses when nothing has been chosen yet.
        let network = self
            .nodes
            .requested()
            .cloned()
            .unwrap_or(pecu_chain::Network::Testnet);

        // Which endpoint this send is held against, decided here on the actor
        // because it is a question about the node list and nothing else. The
        // client for it is built on the worker, where a network call belongs.
        let second_url = match corroborating_source(&self.nodes, route) {
            Ok(url) => url,
            Err(refused) => {
                self.refuse_send(refusal_note(&refused), &refused.to_string());
                return;
            }
        };

        self.tickets += 1;
        let ticket = self.tickets;
        self.busy(TaskKind::PreparingSend, true);

        let job = SendJob {
            chain,
            second_url,
            vault,
            label,
            draft,
            paid_name,
            from_address,
            network,
            route,
            located,
            plan,
        };
        self.blocking.dispatch(
            move || Work::Prepared {
                ticket,
                result: Box::new(build_on_worker(job)),
            },
            self.work.clone(),
        );
    }

    /// Turn a send away before anything is built, with a reason.
    ///
    /// Deliberately the same shape `finish_prepare` uses when the *builder*
    /// refuses, so the form behaves identically whether the objection was found
    /// here — before a worker was ever dispatched — or thirty seconds later by
    /// the prover. A refusal that looks different depending on where it came
    /// from teaches people that the wallet has moods.
    fn refuse_send(&mut self, note: NoteVm, why: &str) {
        // Wrapped rather than passed as a string: `notice` walks an error's
        // `source` chain, and giving it something that is not an error would
        // mean inventing a second logging path for refusals found early.
        let error = std::io::Error::other(why.to_string());
        self.notice("prepare_send", note.clone(), &error);
        let _ = self
            .events
            .send(Event::SendResult(pecu_protocol::SendOutcomeVm::Failed(
                pecu_protocol::UiError::simple(
                    "prepare_send",
                    note,
                    why.to_string(),
                    pecu_protocol::Severity::Danger,
                ),
            )));
    }

    /// What the send form says when the shielded side refuses.
    fn shielded_note(error: &shielded::ShieldedError) -> NoteVm {
        use shielded::ShieldedError;
        match error {
            ShieldedError::BadAddress => NoteVm::plain("address-unparsable"),
            ShieldedError::BadAmount => NoteVm::plain("amount-unparsable"),
            ShieldedError::NothingScanned => NoteVm::plain("shielded-not-scanned"),
            // Both figures, because they differ and the difference is the
            // answer: a balance spread across many small notes cannot all move
            // at once.
            ShieldedError::NotEnough {
                held, reachable, ..
            } => NoteVm::with("shielded-not-enough", [held.clone(), reachable.clone()]),
            ShieldedError::ServerBehind { .. } => NoteVm::plain("shielded-server-behind"),
            _ => NoteVm::plain("shielded-build-failed"),
        }
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
                // The balance the payment came out of, so "Left afterwards"
                // is about the key that just paid. Still the transparent one on
                // every route — see `docs/LATER.md` §12, which is the other
                // half of this line and is not fixed here.
                let spendable = self.active_key_funds().spendable;
                let review = send::review(ticket, &prepared, &from, spendable, known);
                self.prepared.insert(ticket, prepared);
                let _ = self.events.send(Event::SendPrepared(review));
            }
            Err(error) => {
                let refusal = send_note(&error);
                self.notice("prepare_send", refusal.clone(), &error);
                let _ = self
                    .events
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

    /// Where a transaction can be looked up, or empty when this chain has no
    /// explorer this build knows about.
    ///
    /// One place, because both broadcast paths and both outcomes need it and a
    /// second copy would be a second chance to send somebody to the wrong
    /// chain's explorer.
    fn explorer_for(&self, txid: &str) -> String {
        self.nodes
            .active()
            .and_then(|node| node.network.as_ref())
            .and_then(|network| network.explorer(txid))
            .unwrap_or_default()
    }

    /// Whether these bytes were built without a second node's word for the
    /// coins they spend, on a node list where that is required.
    ///
    /// The same decision `prepare_send` made before dispatching, read again at
    /// the moment of broadcast. It has to be the same decision or the two would
    /// disagree the first time a node's status changed: a `Prepared` that was
    /// correctly uncorroborated when it was built — the active node was a
    /// built-in — must not be refused here.
    ///
    /// The two ways to reach a refusal have different remedies, so they are
    /// different refusals. `Held` with nothing recorded can only mean the
    /// active node changed under an open review, and building the payment again
    /// fixes it. `Absent` means the shipped endpoint has since started
    /// answering about another chain, and building again would refuse for the
    /// same reason — so it gets the sentence that points at the node list.
    fn uncorroborated(&self, prepared: &send::Prepared) -> Result<(), pecu_chain::SpendRefused> {
        corroboration_missing(&self.nodes, prepared.route, &prepared.corroborated_by)
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
                // Put it back: the user may turn spending on and try again, and
                // rebuilding would pick different coins.
                self.prepared.insert(ticket, prepared);
                self.notice("spend_refused", refusal_note(&refused), &refused);
                return;
            }
        };

        // Belt and braces. `send::prepare` already filtered the coins to the
        // set a second node vouched for, so bytes that reach here are
        // corroborated by construction — but this is the last line before the
        // irreversible act, and the standard this codebase set for the permit
        // is that the check lives where it cannot be skipped. What it catches
        // is the gap between the two steps: a node coming online, or the user
        // moving to an endpoint they added, between pressing Review and
        // pressing Send. Rebuilding is the remedy, and refusing is what makes
        // somebody rebuild.
        if let Err(refused) = self.uncorroborated(&prepared) {
            // Not put back, unlike every other refusal here. The others are
            // states somebody can fix and press the same button again —
            // spending switched off, a node still catching up — and rebuilding
            // would pick different coins for no reason. This one is the
            // opposite: the node list moved under these bytes, nothing has held
            // them against it as it now stands, and the only remedy is to build
            // the payment again. Leaving them on the review would leave a Send
            // button that can only ever produce the same refusal.
            self.notice("spend_refused", refusal_note(&refused), &refused);
            return;
        }

        let Some(chain) = self.chain() else {
            self.prepared.insert(ticket, prepared);
            return;
        };

        // Committed to disk BEFORE the broadcast. A process that dies mid-send
        // must not lose the only copy of bytes that may already be propagating.
        let record = match self.pending.commit(
            prepared.signed.txid(),
            prepared.signed.hex(),
            &prepared.to,
            &portfolio::coins(prepared.amount),
        ) {
            Ok(id) => id,
            Err(error) => {
                // Refusing to send is the right failure. Sending bytes we could
                // not record is exactly the situation the ledger exists to
                // prevent, and it is not made better by proceeding.
                self.prepared.insert(ticket, prepared);
                self.notice("pending_commit", NoteVm::plain("pending-unsaved"), &error);
                return;
            }
        };

        self.busy(TaskKind::Broadcasting, true);
        self.broadcasting = true;
        // Read before the move: `prepared` goes to the worker, and the
        // nullifiers have to come back with the answer.
        let spends = prepared.spends.clone();
        self.blocking.dispatch(
            move || Work::Broadcast {
                record,
                spends,
                result: Box::new(send::broadcast(&chain, &permit, prepared)),
            },
            self.work.clone(),
        );
    }

    fn finish_broadcast(
        &mut self,
        record: u64,
        spends: &[[u8; 32]],
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
                // The notes are gone the moment the network takes the
                // transaction, and the chain will not say so for another block.
                // Until it does, this is the only thing between a second spend
                // and `bad-txns-sapling-nullifier-exists` — the shielded half of
                // what the pending ledger does for transparent outputs.
                if let Some(shielded) = self.shielded.as_mut() {
                    shielded.mark_spent(spends);
                    // Written down immediately rather than at the next scan.
                    // The window between the two is exactly the window this
                    // marker exists to cover, so leaving it in memory alone
                    // would mean a wallet closed in that minute reopens willing
                    // to spend a note it has already spent.
                    self.keep_shielded_scan();
                }
                self.remember_recipient(record);
                self.pending.set_state(record, pending::State::Confirmed);
                self.pending.forget_confirmed();
                let explorer_url = self.explorer_for(&sent.txid);
                let _ = self.events.send(Event::SendResult(SendOutcomeVm::Sent {
                    txid: sent.txid,
                    fee_display: portfolio::coins(sent.fee),
                    explorer_url,
                }));
                self.refresh();
            }

            // The one failure that is not a failure. The node may have taken
            // it. The bytes are on disk, and the only safe resolution is to ask
            // whether it confirmed — never to rebuild.
            Err(FlowError::BroadcastUncertain { txid, .. }) => {
                tracing::warn!(%txid, "the broadcast outcome is unknown");
                let explorer_url = self.explorer_for(&txid);
                let _ = self
                    .events
                    .send(Event::SendResult(SendOutcomeVm::Uncertain {
                        txid,
                        pending_id: record,
                        explorer_url,
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

    fn emit_wallet(&mut self) {
        // The shielded side follows the wallet's, at the one moment everything
        // else about the wallet is published. Doing it here rather than in each
        // of unlock, lock and switch-key means there is no lifecycle path that
        // can forget: whatever changed, the state is rebuilt from the wallet's
        // own answer before anyone is told about it.
        self.sync_shielded_state();

        let mut vm = self.wallet.view();
        // The balance belongs to the core, not the wallet: the wallet knows
        // which shielded account is active, and only this side has scanned for
        // what is in it. Left empty when there is no account, because zero and
        // "no account" are different statements.
        // Only once something has actually been looked at. Before that the
        // honest answer is nothing at all: a wallet that has not scanned does
        // not know its pool is empty, and printing "0" would be a claim about
        // somebody's money that nobody has earned the right to make.
        if let Some(share) = self.scan_share {
            vm.shielded_scan = Some(share);
        }

        // What the active key alone holds, for the send form. Formatted here
        // because money is formatted in one place in this workspace, and
        // published from `emit_wallet` because that is the moment the active
        // key is settled — a key switch and a refresh both pass through here,
        // and neither can forget.
        let funds = self.active_key_funds();
        vm.key_funds = pecu_protocol::KeyFundsVm {
            spendable_display: portfolio::coins(funds.spendable),
            immature_sats: funds.immature.to_sat().to_string(),
            immature_display: portfolio::coins(funds.immature),
        };

        vm.shielded_funds = match self.shielded.as_ref() {
            // Scanned: a figure, which may legitimately be zero.
            Some(held) if held.scanned_to().is_some() => pecu_protocol::ShieldedFunds::Scanned(
                portfolio::coins(verus_sdk::money::Amount::from_sat(held.balance())),
            ),
            // There is an account and nobody has looked in it. Saying "0" here
            // would be a claim no scan supports.
            Some(_) => pecu_protocol::ShieldedFunds::Unscanned,
            None => pecu_protocol::ShieldedFunds::Absent,
        };
        let _ = self.events.send(Event::Wallet(vm));
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
            chains: pecu_chain::Network::shipped()
                .into_iter()
                .map(|chain| pecu_protocol::ChainChoiceVm {
                    // `chain_name()` and never `label()`. The two differ on the
                    // two chains that have one: `label()` answers "Mainnet",
                    // which comes back through `SetRequestedNetwork` and parses
                    // as `Other("Mainnet")` — a chain with no endpoints that
                    // every real node then disagrees with. Held by
                    // `the_chain_buttons_offer_names_a_node_could_report`.
                    name: chain.chain_name().to_string(),
                    title: chain.title().to_string(),
                })
                .collect(),
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
            requested_name: self.confirm_word(),
            chain_title: self
                .nodes
                .requested()
                .map(|network| network.title().to_string())
                .unwrap_or_default(),
            spend_gate: self.spend_gate(),
            light_server: self.light_server.clone().unwrap_or_default(),
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

/// What the passphrase re-prompt says when a reveal is refused.
///
/// Every refusal used to be reported as `passphrase-wrong`, which was survivable
/// while the dashboard banner was the only way in: the label came from the
/// wallet, so it could not be wrong, and the passphrase was the only thing left
/// to blame. The keys screen chooses a label from a row somebody pointed at, and
/// a row can go stale between being drawn and being clicked — a key renamed in
/// another window, or removed. Telling that person their passphrase is wrong, on
/// the one screen where hearing it is most alarming, is worth two extra
/// sentences.
fn reveal_error_note(error: &pecu_keystore::VaultError) -> NoteVm {
    use pecu_keystore::VaultError;

    match error {
        VaultError::WrongPassphrase => NoteVm::plain("passphrase-wrong"),
        // Refused by the interface long before this, which does not offer the
        // action on a WIF row. This is what answers a caller that is not the
        // interface, and the day the interface gets it wrong.
        VaultError::NoPhrase(_) => NoteVm::plain("reveal-no-phrase"),
        VaultError::NoSuchKey(_) => NoteVm::plain("key-not-here"),
        VaultError::Locked => NoteVm::plain("wallet-locked"),
        _ => NoteVm::plain("reveal-failed"),
    }
}

/// Why "send everything" cannot be served on this route, or `None` if it can.
///
/// A free function so the mapping can be checked without an actor, a vault and
/// a chain behind it — see the tests at the bottom of this file. The wiring
/// that acts on it is [`Core::refuses_send_all`], which is three lines long
/// precisely because the decision is here.
///
/// **Two sentences, because there are two different situations.** A shielded
/// *source* is money this cannot sweep: the shielded fee is flat in the number
/// of input notes, so the fixpoint [`send::resolve_send_all`] exists for does
/// not arise there, but a ten-note ceiling on a single spend makes "everything"
/// unreachable in one transaction for a balance spread any wider — and whether
/// that should send what ten notes reach or decline is a product decision, not
/// one to settle inside a fee calculation. A shielded *destination* is money it
/// cannot deliver: the coins really are the transparent ones, and telling
/// somebody to pay from a different balance would be asking them to correct the
/// half that is already right.
fn send_all_refusal(route: pecu_protocol::Route) -> Option<(NoteVm, &'static str)> {
    match route {
        pecu_protocol::Route::Transparent => None,
        pecu_protocol::Route::Shield => Some((
            NoteVm::plain("send-all-shielded-recipient"),
            "sending everything does not shield a balance",
        )),
        pecu_protocol::Route::Private | pecu_protocol::Route::Unshield => Some((
            NoteVm::plain("send-all-transparent-only"),
            "sending everything is built for the transparent balance only",
        )),
    }
}

/// What the send form says when a build fails.
fn send_note(error: &send::SendError) -> NoteVm {
    use verus_sdk::network::FlowError;

    match error {
        send::SendError::BadAddress => NoteVm::plain("address-unparsable"),
        send::SendError::ParamsMissing => NoteVm::plain("shielded-params-missing"),
        send::SendError::Shielded(_) => NoteVm::plain("shielded-build-failed"),
        send::SendError::BadAmount => NoteVm::plain("amount-unparsable"),
        send::SendError::NothingToSend => NoteVm::plain("amount-zero"),
        // The same sentence the form already showed offline. It reaches here
        // when the coins moved between the keystroke and the build — the
        // refusal did not change, only when it was found.
        send::SendError::NotEnoughForFee => NoteVm::plain("amount-below-fee"),
        send::SendError::Vault(_) => NoteVm::plain("wallet-locked"),
        // The distinction the SDK draws and a wallet must not lose: what you
        // hold and what you can spend right now are different numbers, and a
        // bare "insufficient funds" against a screen showing a balance reads as
        // a bug in the wallet.
        send::SendError::Flow(FlowError::InsufficientFunds { .. }) => {
            NoteVm::plain("send-not-enough-spendable")
        }
        send::SendError::Flow(_) => NoteVm::plain("send-build-failed"),
        // One table for the spending guard's words, whether the refusal came
        // from the permit or from the build. Two would be two chances to say
        // different things about the same fact.
        send::SendError::Refused(refused) => refusal_note(refused),
    }
}

/// The halt as the interface receives it.
fn halt_vm(status: &upgrade::Status, stale: bool) -> pecu_protocol::ChainHaltVm {
    pecu_protocol::ChainHaltVm {
        severity: status.severity.label().to_string(),
        note: status.note.clone(),
        conversions_halted: status.conversions_halted,
        in_blocks: status.in_blocks,
        stale,
    }
}

/// What the convert screen says when a build fails.
///
/// Named reasons rather than the error's own text, for the reason `NoteVm`
/// records — except that the node's sentence still travels beside them, in the
/// `UiError` this is put into. That matters more here than anywhere else in
/// the wallet: while the network has DeFi paused, *every* conversion is
/// refused, and the only thing that distinguishes "the chain will not do this
/// today" from "this wallet built it wrong" is what the node said.
fn convert_note(error: &convert::ConvertError) -> NoteVm {
    use verus_sdk::network::FlowError;

    match error {
        convert::ConvertError::BadRecipient => NoteVm::plain("convert-bad-recipient"),
        convert::ConvertError::BadCurrency => NoteVm::plain("convert-bad-currency"),
        convert::ConvertError::BelowFloor { expected, floor } => {
            NoteVm::with("convert-below-floor", [expected.clone(), floor.clone()])
        }
        convert::ConvertError::UnusableTokens(_) => NoteVm::plain("convert-unusable-tokens"),
        convert::ConvertError::Vault(_) => NoteVm::plain("wallet-locked"),
        // Same distinction the send form draws, and it is worth as much here:
        // a conversion needs native coins for the fee even when the thing being
        // converted is a token, so "not enough" against a screen showing a
        // token balance reads as a bug unless it says which balance.
        convert::ConvertError::Flow(FlowError::InsufficientFunds { .. }) => {
            NoteVm::plain("send-not-enough-spendable")
        }
        convert::ConvertError::Flow(_) | convert::ConvertError::Read(_) => {
            NoteVm::plain("convert-build-failed")
        }
    }
}

/// Everything the send worker is handed, in one piece.
///
/// A struct rather than eleven parameters, and not only to keep clippy quiet:
/// this is exactly the set the dispatch closure was already capturing, and
/// naming it once is what lets the build live outside `prepare_send` — where it
/// can dial a second endpoint and route between four builders without that
/// function turning into the whole send path.
struct SendJob {
    chain: Arc<Chain>,
    /// The endpoint this send is held against, decided on the actor by
    /// [`corroborating_source`]. `None` when nothing needs holding.
    second_url: Option<String>,
    vault: Arc<pecu_keystore::Vault>,
    label: String,
    draft: pecu_protocol::SendDraft,
    paid_name: String,
    from_address: String,
    network: pecu_chain::Network,
    route: pecu_protocol::Route,
    located: Option<params::Located>,
    plan: Option<shielded::PlannedSpend>,
}

/// Build and sign, off the actor.
///
/// The second endpoint is dialled here rather than on the actor, because that
/// is a network call. Failing to dial one the node list already holds is a
/// refusal and not a quiet fall back to spending unchecked: the actor only
/// asked for a second source because it had decided one was required, and
/// substituting silence for corroboration is exactly what makes a guard
/// decorative.
fn build_on_worker(job: SendJob) -> Result<send::Prepared, send::SendError> {
    let dialled = second_source(job.second_url.as_deref())?;
    let second = dialled.as_ref().map(|chain| send::Corroborator {
        chain,
        url: job.second_url.as_deref().unwrap_or_default(),
    });

    match (job.route, job.located, job.plan) {
        (pecu_protocol::Route::Transparent, _, _) => send::prepare(
            &job.chain,
            second.as_ref(),
            &job.vault,
            &job.label,
            &job.draft,
            &job.paid_name,
        ),
        (pecu_protocol::Route::Shield, Some(located), _) => send::prepare_shield(
            job.chain.as_ref(),
            second.as_ref(),
            &job.vault,
            &job.label,
            &job.from_address,
            &job.draft,
            &located,
        ),
        (_, Some(located), Some(plan)) => {
            // The light server is reached here rather than on the actor: it is
            // a network call, and an actor inside one is an actor that will not
            // answer Lock.
            match pecu_chain::LightServer::shipped(&job.network) {
                Ok(server) => send::prepare_shielded(
                    server.client(),
                    job.chain.as_ref(),
                    &job.vault,
                    &job.label,
                    &plan,
                    &located,
                ),
                Err(refused) => Err(send::SendError::Shielded(refused.to_string())),
            }
        }
        // Unreachable: `located` is `Some` for every proving route and `plan`
        // for both shielded ones. Written as a refusal rather than a panic,
        // because a wallet that panics while holding signed bytes is worse than
        // one that declines.
        _ => Err(send::SendError::Shielded(
            "this payment could not be routed".into(),
        )),
    }
}

/// Dial the second source on the worker, if there is one.
///
/// A separate function because the client for the second endpoint has to be
/// built off the actor and has to live exactly as long as the build does — see
/// the note on `Core::chain`, which caches one client per active URL and
/// invalidates it on a node change. A second cache would need a second
/// invalidation; a client built per send needs none.
///
/// It gets `Chain::second_source` rather than `Chain::live`, which is the
/// shorter of the two timeouts. The reason is the key rather than the wait: a
/// send holds a decrypted key open, and how long somebody else's endpoint takes
/// to answer is not something they get to decide.
///
/// A URL that reached the node list already passed `validate_url`, so failing
/// to dial it means something odd — and the answer is still a refusal rather
/// than a quiet fall back to spending uncorroborated. It names the endpoint
/// that would not answer, because "add a second node" is the wrong instruction
/// for a second node that is already there.
fn second_source(url: Option<&str>) -> Result<Option<Chain>, send::SendError> {
    let Some(url) = url else {
        return Ok(None);
    };
    match Chain::second_source(url) {
        Ok(chain) => Ok(Some(chain)),
        Err(error) => {
            tracing::warn!(%url, %error, "the second source could not be dialled");
            Err(send::SendError::Refused(
                pecu_chain::SpendRefused::SecondSourceSilent {
                    secondary: url.to_string(),
                },
            ))
        }
    }
}

/// Which endpoint a send on `route` must be held against, or the refusal.
///
/// Free of `Core` on purpose: it is a decision about the node list and the
/// route and nothing else, and it is the one function in this change that can
/// stop a spend on a real-money chain. Keeping it out of the actor is what lets
/// it be tested against a node list rather than against a running wallet.
///
/// # Which routes ask
///
/// The ones that spend **transparent coins**: a transparent payment and a `t→z`
/// shield. Not the route's name — what the transaction spends. A shield has no
/// input notes and no anchor; `shield::plan` funds it from `getaddressutxos` at
/// the transparent address, which is the set this check exists to hold to
/// account, so gating on `Route::Transparent` alone would have left the attack
/// open behind one character in the recipient field. `z→z` and `z→t` really do
/// have nothing to ask — their inputs are notes, witnesses and an anchor from
/// one lightwalletd — and `pecu_chain::corroborate` says so out loud rather
/// than letting the transparent guard read as covering them.
fn corroborating_source(
    nodes: &NodeManager,
    route: pecu_protocol::Route,
) -> Result<Option<String>, pecu_chain::SpendRefused> {
    if !spends_transparent_coins(route) {
        return Ok(None);
    }

    match nodes.second_source() {
        pecu_chain::SecondSource::Held(node) => Ok(Some(node.url.clone())),
        // The active node is one the user added and the shipped endpoint for
        // this chain is itself answering about another chain, so nothing
        // configured could hold the primary to anything.
        //
        // Refused on **every** chain, testnet included, and that is a
        // deliberate departure from where `may_be_real_money` is drawn
        // elsewhere in this wallet. That line is justified by VRSCTEST coins
        // coming out of a faucet — which reasons from the very fact under
        // attack. The endpoint asking to be trusted here is one serving
        // *mainnet* outputs to a wallet that believes it is on testnet; the
        // coins at risk are worth what mainnet says they are worth, and the
        // chain the wallet was set to says nothing about them. Letting a
        // testnet setting authorise an unchecked spend would be taking the
        // attacker's own premise as the reason not to check.
        pecu_chain::SecondSource::Absent => Err(pecu_chain::SpendRefused::NoSecondSource {
            primary: nodes.active_url().unwrap_or_default().to_string(),
        }),
        // The active node is one this build shipped, so there is nothing
        // independent to hold it to and refusing would brick a default install.
        // `second_source` is where that is argued, along with what it costs: a
        // compromised built-in is corroborated by nothing.
        pecu_chain::SecondSource::Unheld => Ok(None),
    }
}

/// Whether bytes recorded as checked by `corroborated_by` still satisfy the
/// node list as it stands now.
///
/// The companion to [`corroborating_source`], and deliberately the same
/// decision: the two would disagree the first time a node's status changed if
/// either grew a rule the other did not have.
fn corroboration_missing(
    nodes: &NodeManager,
    route: pecu_protocol::Route,
    corroborated_by: &str,
) -> Result<(), pecu_chain::SpendRefused> {
    if !spends_transparent_coins(route) || !corroborated_by.is_empty() {
        return Ok(());
    }

    match nodes.second_source() {
        pecu_chain::SecondSource::Unheld => Ok(()),
        pecu_chain::SecondSource::Held(_) => Err(pecu_chain::SpendRefused::PreparedBeforeNodeChange),
        pecu_chain::SecondSource::Absent => Err(pecu_chain::SpendRefused::NoSecondSource {
            primary: nodes.active_url().unwrap_or_default().to_string(),
        }),
    }
}

/// Whether a route funds itself from this wallet's transparent coins.
///
/// The question corroboration turns on, and it is not the route's name. A `t→z`
/// shield has no input notes: it is funded from `getaddressutxos` at the
/// transparent address, exactly like a transparent payment, so it is checked
/// exactly like one. Only `z→z` and `z→t` spend notes, and those have no second
/// source of any kind — see `pecu_chain::corroborate`.
fn spends_transparent_coins(route: pecu_protocol::Route) -> bool {
    match route {
        pecu_protocol::Route::Transparent | pecu_protocol::Route::Shield => true,
        pecu_protocol::Route::Private | pecu_protocol::Route::Unshield => false,
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
        // Both claims, for the same reason the read refusal carries them: there
        // is no chain to switch to and nothing to wait for, so which two
        // statements disagreed is the whole of what a person can act on.
        SpendRefused::Unidentified { name, chain_id } => {
            NoteVm::with("spend-unidentified", [name.clone(), chain_id.clone()])
        }
        SpendRefused::NetworkMismatch {
            requested,
            effective,
        } => NoteVm::with(
            "spend-wrong-chain",
            [effective.to_string(), requested.to_string()],
        ),
        // Named, because one refusal covers five chains and "spending is turned
        // off" would leave a person on vARRR looking for a mainnet switch. The
        // title rather than the name: this is a sentence to read, and the
        // name's job is the word they have to type.
        SpendRefused::SpendingNotEnabled { network } => {
            NoteVm::with("spend-not-enabled", [network.title().to_string()])
        }
        SpendRefused::Syncing { blocks, longest } => NoteVm::with(
            "spend-node-syncing",
            [blocks.to_string(), longest.to_string()],
        ),
        SpendRefused::NodeNotReady { .. } => NoteVm::plain("spend-node-not-ready"),
        // The count and the endpoint, because both are what makes this
        // actionable: how much of what this node offered nobody else has heard
        // of, and who was asked. Without them it reads as the wallet being
        // difficult about a node that looks perfectly healthy on the node
        // screen — which it is, since corroboration is a property of a pair and
        // no single node's status can carry it.
        SpendRefused::Uncorroborated { count, secondary } => NoteVm::with(
            "spend-uncorroborated",
            [count.to_string(), secondary.clone()],
        ),
        // Deliberately not the same sentence as `Uncorroborated`. This pair is
        // not disagreeing about anything — the second node's own tip says it
        // has not reached the blocks these coins are in — and accusing an
        // honest pair is its own harm. The tip is in the sentence because "wait
        // for it to catch up" is only actionable if somebody can see how far
        // behind it is.
        SpendRefused::SecondSourceBehind {
            count,
            secondary,
            tip,
        } => NoteVm::with(
            "spend-second-source-behind",
            [count.to_string(), secondary.clone(), tip.to_string()],
        ),
        // The mirror, and it needs its own words rather than the tip being
        // dropped into the sentence above. There the second source has to catch
        // up; here it is already ahead and the node in use is the one behind,
        // so "wait for it to catch up" would be waiting on something that has
        // already happened.
        SpendRefused::SecondSourceAhead {
            count,
            secondary,
            tip,
        } => NoteVm::with(
            "spend-second-source-ahead",
            [count.to_string(), secondary.clone(), tip.to_string()],
        ),
        // Names the endpoint that went quiet, not the one being checked. The
        // remedy for the two is opposite: here a second node exists and is
        // configured, so "add a second node" would send somebody after a
        // problem they do not have.
        SpendRefused::SecondSourceSilent { secondary } => {
            NoteVm::with("spend-second-source-silent", [secondary.clone()])
        }
        SpendRefused::NoSecondSource { primary } => {
            NoteVm::with("spend-no-second-source", [primary.clone()])
        }
        SpendRefused::PreparedBeforeNodeChange => NoteVm::plain("spend-node-changed"),
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
///
/// # A built-in first, for the same reason `second_source` prefers one
///
/// This used to take the first online node it found, which is a reasonable
/// rule for "find something that answers" and a bad one now that the wallet
/// also uses the node list to decide what checks what. Somebody who was talked
/// into adding one endpoint can be talked into adding two, and failing over
/// from a built-in onto the first of those would move a wallet from the
/// arrangement that needs no corroboration to the one that does — silently,
/// and while a review may be open. Preferring the shipped endpoint means an
/// automatic move is either onto it or onto something already being held
/// against it.
fn failover_target(nodes: &NodeManager) -> Option<u32> {
    let active = nodes.active()?;
    if active.consecutive_failures < FAILURES_BEFORE_FAILOVER {
        return None;
    }

    let failed = active.id;
    let candidates = || {
        nodes
            .nodes()
            .iter()
            .filter(|node| node.id != failed && node.status == pecu_chain::NodeStatus::Online)
    };
    candidates()
        .find(|node| node.builtin)
        .or_else(|| candidates().next())
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
        note: node_note(&node.status),
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
/// Why a scan stopped before it reached the tip.
enum ScanStop {
    /// It failed, and whatever it managed is still worth keeping.
    Stopped(String),
    /// It failed in a way that poisons what was kept, so the caller must throw
    /// that away and scan again from the birthday. Only a reorg deeper than the
    /// scan can verify a rollback to reaches this.
    StartAgain(String),
}

/// Walk from where this scan left off to the tip, in strides.
///
/// Extracted from the worker closure rather than written inline, so that the
/// two decisions inside it — where to resume, and which failures are worth
/// retrying — are readable without the dispatch around them.
///
/// # Why it strides at all
///
/// A first scan with no birthday covers the whole chain. In one call that is
/// minutes of silence; in strides the interface can say how far it has got, and
/// a failure costs one stride rather than the lot.
fn walk_the_chain(
    watching: &mut shielded::Shielded,
    server: &pecu_chain::LightServer,
    from: Option<u64>,
    stride: u64,
    attempts: u32,
    progress: &mpsc::UnboundedSender<Work>,
) -> Result<(), ScanStop> {
    let to = server
        .synced_height()
        .map_err(|e| ScanStop::Stopped(e.to_string()))?;

    let start = scan_resumes_at(
        watching.scanned_to(),
        from,
        server.info().sapling_activation_height,
    );

    // The server has nothing this wallet has not already read.
    //
    // Ordinary, not a fault: it is every scan that runs while no new block has
    // been mined. It is also what a genuinely lagging server looks like — a
    // replica that rotated in with less of the same chain — and the SDK is
    // explicit that the answer to that is to wait rather than to roll anything
    // back. Either way there is nothing to do and nothing to say.
    if start > to {
        tracing::debug!(start, to, "nothing new to scan");
        return Ok(());
    }

    let mut at = start;
    loop {
        let until = at.saturating_add(stride).min(to);

        // Retried, because a transport failure on a scan this long is ordinary
        // rather than exceptional. One measured here was "Error while decoding
        // chunks" on a range that answered perfectly a minute later.
        let mut attempt = 1;
        loop {
            match watching.sync(server.client(), at, until) {
                Ok(_) => break,
                // Not retryable and not survivable: retrying fails the same way
                // for as long as the fork stands, and the state that cannot be
                // continued is the state about to be written down.
                Err(error @ shielded::ShieldedError::ReorgTooDeep) => {
                    return Err(ScanStop::StartAgain(error.to_string()))
                }
                Err(error) if attempt < attempts => {
                    tracing::warn!(
                        %error, attempt, at, until,
                        "a shielded scan stride failed; retrying",
                    );
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_secs(u64::from(attempt)));
                }
                Err(error) => return Err(ScanStop::Stopped(error.to_string())),
            }
        }

        let _ = progress.send(Work::ScanProgress {
            scanned_to: watching.scanned_to().unwrap_or(until),
            tip: to,
            start,
        });
        if until >= to {
            return Ok(());
        }
        at = until + 1;
    }
}

/// Where the next stride of a shielded scan should ask from.
///
/// # The bug this is named after
///
/// It used to be `from.unwrap_or(sapling_activation).max(2)`, and `from` is
/// `None` for every **continuation** — that is what tells the worker "carry on
/// where you left off". So a wallet that had scanned to 1 200 172 asked the
/// next stride for blocks 2..=50 002, and the SDK refused it, correctly, with
///
/// ```text
/// the light server is behind: it has 50002, and this wallet has scanned to 1200172
/// ```
///
/// The server was not behind. The wallet was asking about the wrong end of the
/// chain. `Shielded::sync` ignores `from` on a continuation and takes the range
/// from its own state, so only the *upper* bound of a stride ever mattered —
/// and that upper bound was being computed from block two.
///
/// It was reachable before scans were kept, on the second refresh tick of any
/// session that had finished one; keeping them across restarts made it fire on
/// every launch instead, which is how it was finally seen.
fn scan_resumes_at(scanned_to: Option<u64>, from: Option<u64>, sapling_activation: u64) -> u64 {
    match scanned_to {
        // A continuation asks for the block after the last one that finished.
        Some(scanned) => scanned.saturating_add(1),
        // A first scan asks from the birthday, or from Sapling activation when
        // there is no birthday to work from.
        None => from.unwrap_or(sapling_activation),
    }
    // Two, at the earliest, and not for a chain reason.
    //
    // Scanning block N needs the commitment tree as it stood at N-1, because
    // note positions are counted forward from that frontier. For N=1 that is
    // height 0 — and height 0 cannot be asked for at all: protobuf omits
    // zero-valued fields from the wire, so a `BlockID { height: 0 }` arrives as
    // an empty message and lightwalletd answers "request for unspecified
    // identifier". Sapling activated at height 1 on VRSCTEST, so starting there
    // hit exactly that. Block 1 is the chain's first and carries no Sapling
    // output, so nothing is lost — but it is a limit of the protocol rather
    // than a choice.
    .max(2)
}

/// What this wallet knows about when a shielded account could first have been
/// paid.
///
/// Three answers, and they call for three different things — which is why this
/// is not an `Option<u64>`. `Pending` in particular is not "no birthday": it is
/// "there will be one shortly, and scanning before it arrives would do the
/// expensive thing for no reason".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Birthday {
    /// Scan from here.
    Known(u64),
    /// A key generated here, waiting for a tip to measure against.
    Pending,
    /// Nobody knows — an imported phrase, or a wallet from before this was
    /// recorded. Scan from Sapling activation.
    Unknown,
}

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

    // ── The command palette's results ───────────────────────────────────────

    fn known(label: &str, address: &str) -> pecu_store::KnownAddress {
        pecu_store::KnownAddress {
            address: address.to_string(),
            label: label.to_string(),
            name: String::new(),
            paid_at: None,
            payments: 0,
        }
    }

    /// The same, for a saved address the chain has a name for.
    fn known_identity(name: &str, address: &str) -> pecu_store::KnownAddress {
        pecu_store::KnownAddress {
            name: name.to_string(),
            ..known("", address)
        }
    }

    fn currencies(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(address, name)| ((*address).to_string(), (*name).to_string()))
            .collect()
    }

    /// Addresses before currencies, whatever the query matched.
    ///
    /// The order is the answer to "who am I paying", which is what a wallet's
    /// search box is mostly asked.
    #[test]
    fn the_palette_puts_addresses_above_currencies() {
        let hits = palette_hits(
            "ve",
            &[known("Vera", "RQxJPwq")],
            &currencies(&[("iBoaN7s", "Bridge.vETH")]),
        );

        let kinds: Vec<&str> = hits.iter().map(|hit| hit.kind.as_str()).collect();
        assert_eq!(kinds, ["address", "currency"]);
    }

    /// An address nobody has named shows its address on both lines.
    ///
    /// Not an empty label: the row is two lines tall either way, and a blank
    /// top line reads as a broken row rather than an unnamed one.
    #[test]
    fn an_unnamed_address_falls_back_to_its_address_rather_than_showing_nothing() {
        let hits = palette_hits("rqx", &[known("", "RQxJPwq")], &currencies(&[]));

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].label, "RQxJPwq");
        assert_eq!(hits[0].sub, "RQxJPwq");
    }

    /// A VerusID paid by name is found by that name.
    ///
    /// The address book stores the i-address, because that is what the payment
    /// paid. The palette has to search what the person was *shown*.
    #[test]
    fn a_saved_identity_is_found_by_the_name_it_was_paid_under() {
        let saved = known_identity("dude.VRSCTEST@", "i4YzoP8ZHnh1gNywV9PAT6Yz3AkfXxJmtP");

        let hits = palette_hits("dude", std::slice::from_ref(&saved), &currencies(&[]));
        assert_eq!(hits.len(), 1, "searching by name found nothing");
        assert_eq!(hits[0].label, "dude.VRSCTEST@");
        assert_eq!(hits[0].target, "i4YzoP8ZHnh1gNywV9PAT6Yz3AkfXxJmtP");

        // And still by its address, which is what somebody arrives with pasted.
        let hits = palette_hits("i4YzoP8Z", std::slice::from_ref(&saved), &currencies(&[]));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].label, "dude.VRSCTEST@");
    }

    /// An empty query answers with nothing rather than with everything.
    ///
    /// A palette that opens showing the whole wallet has answered a question
    /// nobody asked — and `SearchState.search("")` is exactly what ⌘K sends on
    /// the way in.
    #[test]
    fn an_empty_query_clears_rather_than_listing_the_wallet() {
        let hits = palette_hits(
            "   ",
            &[known("Vera", "RQxJPwq")],
            &currencies(&[("iBoaN7s", "Bridge.vETH")]),
        );

        assert!(hits.is_empty());
    }

    /// The panel has no scroll and sizes itself to its content, so the list is
    /// capped — and the cap has to hold across *both* lists, not per list.
    #[test]
    fn the_palette_stops_before_it_grows_past_the_window() {
        let addresses: Vec<pecu_store::KnownAddress> = (0..20)
            .map(|i| known(&format!("saved {i}"), &format!("Raddr{i}")))
            .collect();
        let names: Vec<(String, String)> = (0..20)
            .map(|i| (format!("iCur{i}"), format!("saved{i}.vETH")))
            .collect();
        let catalog: std::collections::BTreeMap<String, String> = names.into_iter().collect();

        let hits = palette_hits("saved", &addresses, &catalog);

        assert_eq!(hits.len(), 8, "the panel would run off the bottom");
        assert!(
            hits.iter().all(|hit| hit.kind == "address"),
            "the cap is over the whole list, not one per kind"
        );
    }

    /// Nothing the palette hands back may point at a screen that is not in the
    /// rail. `wire_search` routes on `kind` alone, and the two it knows are the
    /// two screens this build shows.
    #[test]
    fn every_hit_names_a_screen_this_build_actually_offers() {
        let hits = palette_hits(
            "e",
            &[known("Vera", "RQxJPwq")],
            &currencies(&[("iBoaN7s", "Bridge.vETH")]),
        );

        assert!(!hits.is_empty(), "the fixture has to match something");
        for hit in &hits {
            assert!(
                hit.kind == "address" || hit.kind == "currency",
                "{} has nowhere to go",
                hit.kind
            );
        }
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

    /// Years later, the paper is gone — and the wallet still has the words.
    ///
    /// The property the keys screen's "Show recovery phrase" rests on, asserted
    /// where it is enforced. Reading a phrase again is a read: the same words
    /// come back, and nothing about the key's backup changes for having asked.
    /// If a reveal ever consumed the phrase, or if the flow behind it could
    /// clear the flag, somebody who came back because they had lost their paper
    /// would be told by their own wallet that they had never made a backup.
    #[tokio::test]
    async fn a_finished_backup_can_be_read_again_and_stays_finished() {
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
        let words = next_words(&mut events, &dispatcher).await;
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

        dispatcher.send(Command::RevealBackup {
            label: "main".to_string(),
            passphrase: pecu_protocol::Secret::from("a passphrase"),
        });
        loop {
            match events.recv().await {
                Some(Event::PhraseChallenge { word_count, .. }) => {
                    assert_eq!(word_count, 24);
                    break;
                }
                Some(Event::Notice(notice)) => {
                    panic!("the phrase could not be read again: {}", notice.message.code)
                }
                Some(_) => {}
                None => panic!("the core stopped before showing the phrase again"),
            }
        }
        assert_eq!(
            next_words(&mut events, &dispatcher).await,
            words,
            "the same key gave different words",
        );

        // Leaving the way the Done button does, and then asking the wallet what
        // it now thinks. Locking is the cheapest thing that makes it describe
        // itself again, and the backup flag is one of the facts that survives.
        dispatcher.send(Command::CancelBackup);
        dispatcher.send(Command::Lock);
        let after = loop {
            match events.recv().await {
                Some(Event::Wallet(vm)) => break vm,
                Some(_) => {}
                None => panic!("the core stopped before reporting the wallet"),
            }
        };
        assert!(
            after.needs_backup.is_none(),
            "reading the phrase again re-armed the backup warning",
        );
        assert!(
            after.keys.iter().all(|key| key.backed_up),
            "reading the phrase again un-recorded the backup",
        );
    }

    /// Hold to reveal, and what came back.
    async fn next_words(
        events: &mut tokio::sync::mpsc::UnboundedReceiver<Event>,
        dispatcher: &Dispatcher,
    ) -> Vec<pecu_protocol::SeedWordVm> {
        dispatcher.send(Command::ShowNewPhrase);
        loop {
            match events.recv().await {
                Some(Event::SeedWords(words)) if !words.is_empty() => return words,
                Some(_) => {}
                None => panic!("the core stopped before sending the words"),
            }
        }
    }

    /// The two refusals that are not about the passphrase.
    ///
    /// They were one refusal until the keys screen could name a key: every
    /// failure reached the person at the passphrase prompt as "that is not your
    /// passphrase", which is wrong for a key that never had words and wrong for
    /// a label that went stale between being drawn and being clicked.
    #[tokio::test]
    async fn a_reveal_says_which_of_the_three_things_went_wrong() {
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
        loop {
            match events.recv().await {
                Some(Event::Wallet(vm)) if vm.exists && !vm.locked => break,
                Some(_) => {}
                None => panic!("the core stopped before the wallet was created"),
            }
        }

        dispatcher.send(Command::ImportKey {
            label: "cold".to_string(),
            material: pecu_protocol::ImportMaterial::Wif(pecu_protocol::Secret::from(
                "UusoQWsobQKUkezgBJa22D9G4t9Avo6k8wD5UUxmmfAEoTN8bawc",
            )),
            passphrase: pecu_protocol::Secret::from("a passphrase"),
        });
        loop {
            match events.recv().await {
                Some(Event::Wallet(vm)) if vm.keys.len() == 2 => break,
                Some(_) => {}
                None => panic!("the core stopped before the second key arrived"),
            }
        }

        for (label, passphrase, expected) in [
            ("main", "not the passphrase", "passphrase-wrong"),
            ("cold", "a passphrase", "reveal-no-phrase"),
            ("renamed-since", "a passphrase", "key-not-here"),
        ] {
            dispatcher.send(Command::RevealBackup {
                label: label.to_string(),
                passphrase: pecu_protocol::Secret::from(passphrase),
            });
            let notice = loop {
                match events.recv().await {
                    Some(Event::Notice(notice)) => break notice,
                    Some(Event::PhraseChallenge { .. }) => {
                        panic!("`{label}` was shown when it should have been refused")
                    }
                    Some(_) => {}
                    None => panic!("the core stopped before refusing `{label}`"),
                }
            };
            assert_eq!(notice.code, "reveal_backup");
            assert_eq!(notice.message.code, expected, "refusing `{label}`");
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
        assert_eq!(
            notice.message.code, "phrase-checksum",
            "{:?}",
            notice.message
        );
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
                when_display: NoteVm::with("when-hours", ["2".to_string()]),
                group: NoteVm::plain("day-today"),
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
            rows[0].when_display.args.first().map(String::as_str),
            Some("2"),
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
            from_pool: pecu_protocol::Pool::Transparent,
            send_all: false,
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

    /// The route that can be swept, and the only one.
    #[test]
    fn only_a_wholly_transparent_payment_can_send_everything() {
        assert!(send_all_refusal(pecu_protocol::Route::Transparent).is_none());

        for route in [
            pecu_protocol::Route::Shield,
            pecu_protocol::Route::Private,
            pecu_protocol::Route::Unshield,
        ] {
            assert!(
                send_all_refusal(route).is_some(),
                "{route:?} was allowed to send everything",
            );
        }
    }

    /// A shielded **source** and a shielded **destination** are refused for
    /// different reasons and must not borrow each other's sentence.
    ///
    /// `R → z` reaches this at all because the toggle stays on screen whenever
    /// the transparent balance is paying: pasting a `zs1…` into a form already
    /// set to send everything is one keystroke away. It used to be told to
    /// "choose an amount to pay from the shielded one" — advice about a balance
    /// that is not the one paying.
    #[test]
    fn shielding_everything_and_sweeping_a_shielded_balance_are_told_apart() {
        let shielding = send_all_refusal(pecu_protocol::Route::Shield)
            .expect("shielding cannot send everything");
        let unshielding = send_all_refusal(pecu_protocol::Route::Unshield)
            .expect("a shielded balance cannot send everything");

        assert_eq!(shielding.0.code, "send-all-shielded-recipient");
        assert_eq!(unshielding.0.code, "send-all-transparent-only");
        assert_ne!(shielding.1, unshielding.1);

        // `z → z` is the same situation as `z → R` — the source is what cannot
        // be swept — so those two do share their words, deliberately.
        let private = send_all_refusal(pecu_protocol::Route::Private)
            .expect("a shielded balance cannot send everything");
        assert_eq!(private.0.code, unshielding.0.code);
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
    #[test]
    fn a_node_on_another_chain_is_not_read_from() {
        let mut nodes = nodes_for_a_testnet_wallet();

        // Nothing has answered yet: not knowing is not disagreeing, and a cold
        // start must not refuse to read.
        assert!(reading_refused(&nodes).is_none());

        answered(&mut nodes, "VRSCTEST", testnet_id());
        assert!(
            reading_refused(&nodes).is_none(),
            "a node on the requested chain was refused",
        );

        answered(&mut nodes, "CHIPS", "iWhateverChipsCallsItself");
        refuses(
            &nodes,
            "wrong_chain",
            "read-wrong-chain",
            &["CHIPS", "Testnet"],
        );

        // Mainnet against a testnet wallet is the same refusal, and the one
        // that would matter most.
        answered(&mut nodes, "VRSC", mainnet_id());
        refuses(
            &nodes,
            "wrong_chain",
            "read-wrong-chain",
            &["Mainnet", "Testnet"],
        );
    }

    /// A node that answered and still left the wallet unable to say which chain
    /// it is on is not read from either.
    ///
    /// This is what holds the ordering `reading_refused` argues for. The
    /// wrong-chain question reads `node.network`, which is `None` here, and a
    /// gate that asked it first would read that silence as a cold start and let
    /// balances, history, UTXOs and quotes through unwarned.
    #[test]
    fn a_node_that_cannot_say_which_chain_it_is_on_is_not_read_from() {
        let mut nodes = nodes_for_a_testnet_wallet();

        // The shape that matters: mainnet's own id under testnet's name.
        answered(&mut nodes, "VRSCTEST", mainnet_id());

        assert_eq!(
            nodes.active().and_then(|node| node.network.as_ref()),
            None,
            "the contradicted name must not have been believed",
        );
        refuses(
            &nodes,
            "unidentified_node",
            "read-unidentified",
            &["VRSCTEST", mainnet_id()],
        );
    }

    fn nodes_for_a_testnet_wallet() -> NodeManager {
        NodeManager::new(
            vec![Node::builtin(0, "one", "https://example.invalid")],
            Network::Testnet,
        )
    }

    fn mainnet_id() -> &'static str {
        Network::Mainnet.chain_id().expect("mainnet pins an id")
    }

    fn testnet_id() -> &'static str {
        Network::Testnet.chain_id().expect("testnet pins an id")
    }

    /// What the active node answered, fed through `record_success` rather than
    /// written into the node by hand.
    ///
    /// Assigning `network` directly would test a state the derivation cannot
    /// produce and miss the one it can: the answer that leaves `network` unset
    /// is exactly the case this rule is about.
    fn answered(nodes: &mut NodeManager, name: &str, chain_id: &str) {
        let requested = nodes.requested().cloned().expect("a requested chain");
        let info = verus_sdk::network::ChainInfo {
            name: name.to_string(),
            chain_id: chain_id.to_string(),
            blocks: 1_000,
            longest_chain: 1_000,
            version: "test".to_string(),
        };
        nodes.get_mut(0).expect("a node to answer").record_success(
            &info,
            std::time::Duration::from_millis(10),
            &requested,
        );
    }

    /// The refusal the screen would be handed: what it is filed under, the note
    /// it renders, and the words in it.
    #[track_caller]
    fn refuses(nodes: &NodeManager, filed_as: &str, code: &str, args: &[&str]) {
        let (filed, note) = reading_refused(nodes).expect("reading was allowed");
        assert_eq!(filed, filed_as);
        assert_eq!(note.code, code);
        assert_eq!(note.args, args);
    }

    /// A continuation asks about the end of the chain it is on, not the start.
    ///
    /// The regression test for the message a real wallet showed after a
    /// restart: *"the light server is behind: it has 50002, and this wallet has
    /// scanned to 1200172"*. Fifty thousand and two is one stride past block
    /// two — the wallet was asking about the wrong end of the chain and
    /// reporting the refusal as the server's fault.
    #[test]
    fn a_scan_resumes_after_what_it_has_already_read() {
        // The failure, in the shape it actually happened: a restored scan, and
        // therefore no birthday in play.
        assert_eq!(scan_resumes_at(Some(1_200_172), None, 1), 1_200_173);

        // A birthday does not drag a continuation backwards either. It is only
        // ever the answer for a scan that has read nothing.
        assert_eq!(
            scan_resumes_at(Some(1_200_172), Some(1_199_000), 1),
            1_200_173
        );
        assert_eq!(scan_resumes_at(None, Some(1_199_000), 1), 1_199_000);

        // Without one, the whole chain — slow, and never wrong.
        assert_eq!(scan_resumes_at(None, None, 227_520), 227_520);

        // Never block zero or one, whichever way it is reached. Sapling
        // activated at height 1 on VRSCTEST and height 0 cannot be asked for
        // at all.
        assert_eq!(scan_resumes_at(None, None, 1), 2);
        assert_eq!(scan_resumes_at(None, Some(0), 1), 2);
        assert_eq!(scan_resumes_at(Some(0), None, 1), 2);

        // A tip at the very top must not wrap round to the bottom.
        assert_eq!(scan_resumes_at(Some(u64::MAX), None, 1), u64::MAX);
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
            // The anchor is a wallet-wide figure, so the per-key breakdown has
            // nothing to say to it.
            by_address: std::collections::BTreeMap::new(),
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
        assert_eq!(
            notice.message.code, "key-name-taken",
            "{:?}",
            notice.message
        );

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
        assert_eq!(
            notice.message.code, "key-name-rules",
            "{:?}",
            notice.message
        );
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
        assert_eq!(
            notice.message.code, "node-url-insecure",
            "{:?}",
            notice.message
        );

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
        assert_eq!(
            notice.message.code, "node-duplicate",
            "{:?}",
            notice.message
        );
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
    async fn next_wallet(events: &mut mpsc::UnboundedReceiver<Event>) -> pecu_protocol::WalletVm {
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

    /// The chain buttons offer names a node could report, and nothing else.
    ///
    /// `ChainChoiceVm::name` is what comes straight back in
    /// `SetRequestedNetwork` and is parsed by `Network::from_chain_name`, so a
    /// display label in that field is not a cosmetic slip: pressing Verus would
    /// set the wallet to `Other("Mainnet")`, a chain with no endpoints that
    /// every real node then contradicts. The titles are checked too, because
    /// the pair being the wrong way round is exactly the mistake this catches.
    #[tokio::test]
    async fn the_chain_buttons_offer_names_a_node_could_report() {
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

        let vm = next_network(&mut events).await;
        let offered: Vec<(&str, &str)> = vm
            .chains
            .iter()
            .map(|chain| (chain.name.as_str(), chain.title.as_str()))
            .collect();
        assert_eq!(
            offered,
            vec![
                ("VRSCTEST", "Testnet"),
                ("VRSC", "Verus"),
                ("vARRR", "Pirate Chain"),
                ("CHIPS", "CHIPS"),
                ("vDEX", "vDEX"),
            ],
        );

        dispatcher.send(Command::Shutdown);
    }

    /// An arm made on one chain does not follow the wallet to another.
    ///
    /// One flag governs every chain, which is only safe while every route to a
    /// different chain clears it. Today that happens because `switch_network`
    /// rebuilds the whole `NodeManager` — a side effect of how the switch is
    /// written rather than a property anybody stated, so it is pinned here from
    /// the outside, where a refactor that kept node state across a switch would
    /// break it loudly.
    ///
    /// The chain switched to is read out of `vm.chains` rather than written as
    /// a literal, so this test and the emitted list cannot drift: a button that
    /// went back to offering display labels would fail here as well as in
    /// `the_chain_buttons_offer_names_a_node_could_report`.
    #[tokio::test]
    async fn arming_a_spend_does_not_survive_a_chain_switch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = tokio::runtime::Handle::current();
        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Mainnet,
                mock: false,
                home: dir.path().to_path_buf(),
            },
        );
        let vm = next_network(&mut events).await;

        // A chain that is not the one the wallet is on, taken from what the
        // buttons actually offer.
        let elsewhere = vm
            .chains
            .iter()
            .map(|chain| chain.name.clone())
            .find(|name| name != &vm.requested_name && name != "VRSCTEST")
            .expect("the build offers a second real-money chain");

        dispatcher.send(Command::SetAllowSpending {
            on: true,
            typed_confirmation: "VRSC".to_string(),
        });
        loop {
            if next_network(&mut events).await.spend_gate == pecu_protocol::SpendGate::Open {
                break;
            }
        }

        dispatcher.send(Command::SetRequestedNetwork(elsewhere.clone()));

        let vm = loop {
            let vm = next_network(&mut events).await;
            if vm.requested_name == elsewhere {
                break vm;
            }
        };
        assert_eq!(
            vm.spend_gate,
            pecu_protocol::SpendGate::Closed,
            "the arm followed the wallet onto another chain",
        );

        dispatcher.send(Command::Shutdown);
    }

    /// The word is the chain's own name, and the core is what decides that.
    ///
    /// The whole opt-in path had no test at this level: the command, the
    /// refusal it sends back and the word that refusal names were all only
    /// reachable through the interface. The refusal has to carry the expected
    /// word, because it differs per chain now — "that was not the word" without
    /// saying which word would leave somebody guessing.
    #[tokio::test]
    async fn arming_a_spend_needs_the_chains_own_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = tokio::runtime::Handle::current();
        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Mainnet,
                mock: false,
                home: dir.path().to_path_buf(),
            },
        );
        let vm = next_network(&mut events).await;
        assert_eq!(vm.spend_gate, pecu_protocol::SpendGate::Closed);
        assert_eq!(vm.requested_name, "VRSC");
        assert_eq!(vm.chain_title, "Verus");

        // The word this control used to ask for, everywhere, forever.
        dispatcher.send(Command::SetAllowSpending {
            on: true,
            typed_confirmation: "mainnet".to_string(),
        });

        let notice = loop {
            match events.recv().await {
                Some(Event::Notice(notice)) => break notice,
                Some(Event::Network(vm)) => {
                    panic!("the wrong word armed spending: {:?}", vm.spend_gate)
                }
                Some(_) => {}
                None => panic!("the core stopped before refusing"),
            }
        };
        assert_eq!(notice.code, "spend_confirmation");
        assert_eq!(
            notice.message.code, "spend-confirm-word",
            "{:?}",
            notice.message
        );
        assert_eq!(
            notice.message.args,
            vec!["VRSC".to_string()],
            "the refusal did not name the word it wanted",
        );

        dispatcher.send(Command::SetAllowSpending {
            on: true,
            typed_confirmation: "  vrsc  ".to_string(),
        });
        assert_eq!(
            next_network(&mut events).await.spend_gate,
            pecu_protocol::SpendGate::Open,
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

    // ── What holds a spend to a second node ─────────────────────────────────

    /// A node list with one shipped endpoint and one the user added, both
    /// having answered about VRSCTEST.
    ///
    /// The status matters less than it used to — `second_source` no longer
    /// filters on health — but they are recorded as answering so that a test
    /// which changes one is visibly changing it.
    fn two_nodes(requested: Network) -> NodeManager {
        let mut nodes = vec![
            Node::builtin(0, "shipped", "https://shipped.invalid"),
            Node::user_added(1000, "mine", "https://mine.invalid"),
        ];
        // Answering about the chain the wallet is set to, so that the only
        // reason a test sees a degraded node is a test having made one.
        let info = verus_sdk::network::ChainInfo {
            name: requested.chain_name().to_string(),
            chain_id: requested.chain_id().unwrap_or("i-something").to_string(),
            blocks: 1_000,
            longest_chain: 1_000,
            version: "test".to_string(),
        };
        for node in &mut nodes {
            node.record_success(&info, std::time::Duration::from_millis(5), &requested);
        }
        NodeManager::new(nodes, requested)
    }

    /// The default install: the active node is the shipped one, so there is
    /// nothing independent to hold it to and the send goes ahead unchecked.
    #[test]
    fn a_send_from_the_shipped_endpoint_asks_for_no_second_source() {
        let nodes = two_nodes(Network::Mainnet);
        assert_eq!(nodes.active().expect("a node is active").id, 0);

        assert_eq!(
            corroborating_source(&nodes, pecu_protocol::Route::Transparent),
            Ok(None)
        );
    }

    /// The case the whole change exists for, on both routes that spend
    /// transparent coins.
    #[test]
    fn a_send_from_an_endpoint_the_user_added_is_held_against_the_shipped_one() {
        let mut nodes = two_nodes(Network::Testnet);
        assert!(nodes.set_active(1000));

        for route in [pecu_protocol::Route::Transparent, pecu_protocol::Route::Shield] {
            assert_eq!(
                corroborating_source(&nodes, route),
                Ok(Some("https://shipped.invalid".to_string())),
                "{route:?} was not held against the shipped endpoint",
            );
        }
    }

    /// And the two routes whose inputs are notes are not, because there is no
    /// second lightwalletd to ask.
    #[test]
    fn a_spend_of_shielded_notes_has_no_second_source_to_ask() {
        let mut nodes = two_nodes(Network::Mainnet);
        assert!(nodes.set_active(1000));

        for route in [pecu_protocol::Route::Private, pecu_protocol::Route::Unshield] {
            assert_eq!(corroborating_source(&nodes, route), Ok(None), "{route:?}");
        }
    }

    /// Nothing left to ask means no spend, on **every** chain.
    ///
    /// Testnet included, and that is the point of the loop. Gating this on
    /// `may_be_real_money` would reason from the fact under attack: the wallet
    /// believes it is on testnet, and the coins the hostile endpoint is
    /// offering are mainnet coins. What "VRSCTEST is play money" describes is
    /// not what would be spent.
    #[test]
    fn an_unheld_endpoint_cannot_spend_on_any_chain_including_testnet() {
        for requested in [Network::Testnet, Network::Mainnet] {
            let mut nodes = two_nodes(requested.clone());
            assert!(nodes.set_active(1000));
            // The one state that leaves nothing to ask: the shipped endpoint is
            // answering about another chain, so its UTXO set is a fact about
            // somebody else's.
            nodes.get_mut(0).expect("the built-in").status = pecu_chain::NodeStatus::WrongNetwork {
                reported: Network::Mainnet,
            };

            assert_eq!(
                corroborating_source(&nodes, pecu_protocol::Route::Transparent),
                Err(pecu_chain::SpendRefused::NoSecondSource {
                    primary: "https://mine.invalid".to_string(),
                }),
                "an unchecked spend was allowed on {requested}",
            );
        }
    }

    /// A built-in nobody has probed this session is still the check.
    ///
    /// `Unknown` is the state of every inactive row at launch — only the active
    /// node is polled. Reading it as "nothing to check against" would make the
    /// refusal above the *default* on every start, which is a false refusal
    /// whose remedy ("open the node list and wait") appears nowhere.
    #[test]
    fn an_unprobed_shipped_endpoint_does_not_refuse_the_send() {
        let mut nodes = two_nodes(Network::Mainnet);
        assert!(nodes.set_active(1000));
        nodes.get_mut(0).expect("the built-in").status = pecu_chain::NodeStatus::Unknown;

        assert_eq!(
            corroborating_source(&nodes, pecu_protocol::Route::Transparent),
            Ok(Some("https://shipped.invalid".to_string())),
        );
    }

    /// The broadcast gate agrees with the prepare gate when nothing moved.
    #[test]
    fn bytes_built_against_the_shipped_endpoint_still_broadcast() {
        let nodes = two_nodes(Network::Mainnet);

        assert_eq!(
            corroboration_missing(&nodes, pecu_protocol::Route::Transparent, ""),
            Ok(())
        );
    }

    /// And refuses when it did, with the sentence that names the actual remedy.
    ///
    /// A failover, or somebody switching endpoints, between pressing Review and
    /// pressing Send. The bytes are not wrong; nothing has held them against the
    /// node list as it now stands, and only building again fixes that.
    #[test]
    fn bytes_prepared_before_the_active_node_changed_are_refused_at_the_broadcast_gate() {
        let mut nodes = two_nodes(Network::Mainnet);
        assert!(nodes.set_active(1000));

        assert_eq!(
            corroboration_missing(&nodes, pecu_protocol::Route::Transparent, ""),
            Err(pecu_chain::SpendRefused::PreparedBeforeNodeChange),
        );
        // A shield is the same spend of the same coins, so it is refused the
        // same way — this is the arm a route-name gate would have let through.
        assert_eq!(
            corroboration_missing(&nodes, pecu_protocol::Route::Shield, ""),
            Err(pecu_chain::SpendRefused::PreparedBeforeNodeChange),
        );
    }

    /// Bytes that were checked are not re-refused because the node list moved.
    #[test]
    fn bytes_a_second_node_already_vouched_for_are_not_refused_again() {
        let mut nodes = two_nodes(Network::Mainnet);
        assert!(nodes.set_active(1000));

        assert_eq!(
            corroboration_missing(
                &nodes,
                pecu_protocol::Route::Transparent,
                "https://shipped.invalid",
            ),
            Ok(())
        );
    }
}
