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

pub mod pending;
pub mod portfolio;
pub mod runtime;
pub mod send;
pub mod wallet;

use std::sync::Arc;

use chainvue_chain::{Chain, Network, Node, NodeManager};
use chainvue_protocol::{Command, Event, LockReason, NetworkVm, NodeVm, Reachability, TaskKind};
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
    pub network: Network,
    pub mock: bool,
    /// Where the wallet file lives. Opened if present; not created until the
    /// user asks for a wallet.
    pub vault_path: std::path::PathBuf,
}

/// Start the core on `runtime`, returning the handle to talk to it and the
/// stream of events it produces.
pub fn start(
    runtime: &tokio::runtime::Handle,
    config: Config,
) -> (Dispatcher, mpsc::UnboundedReceiver<Event>) {
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();

    // Beside the wallet, so a backup of the wallet directory carries the
    // unresolved transactions with it.
    let pending_path = config.vault_path.with_file_name("pending-broadcast.json");

    // Beside the vault, in the per-network directory. Failing to open it is not
    // failing to start: everything in there is either a preference or something
    // a node can be asked for again.
    let store = config
        .vault_path
        .parent()
        .map(chainvue_store::Store::open)
        .transpose()
        .unwrap_or_else(|error| {
            tracing::error!(%error, "the wallet database could not be opened");
            None
        });

    let (work_tx, work_rx) = mpsc::unbounded_channel();

    let core = Core {
        wallet: Wallet::open_or_absent(config.vault_path.clone()),
        nodes: NodeManager::new(config.nodes, config.network),
        blocking: Blocking::new(runtime.clone(), 8),
        events: event_tx,
        mock: config.mock,
        chain: None,
        cached: portfolio::Cached::default(),
        refreshing: false,
        work: work_tx,
        spendable: verus_sdk::money::Amount::ZERO,
        prepared: std::collections::HashMap::new(),
        tickets: 0,
        pending: pending::Ledger::open(pending_path),
        last_checked: std::collections::HashMap::new(),
        checking: std::collections::HashSet::new(),
        tip_polled: None,
        polling_tip: false,
        history: HistoryScan::default(),
        store,
    };

    runtime.spawn(core.run(command_rx, work_rx));
    (Dispatcher(command_tx), event_rx)
}

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
    /// When the active node was last asked for the chain tip, and whether an
    /// ask is in flight.
    tip_polled: Option<std::time::Instant>,
    polling_tip: bool,
    history: HistoryScan,

    /// Settings, and a cache the wallet is free to throw away. `None` when the
    /// databases could not be opened — the wallet works without them, it just
    /// forgets between runs and starts every session with a blank dashboard.
    store: Option<chainvue_store::Store>,
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
enum Work {
    Portfolio(Box<portfolio::Reading>),
    /// A payment was built and signed, or the attempt failed.
    Prepared {
        ticket: u64,
        result: Box<Result<send::Prepared, send::SendError>>,
    },
    /// The active node reported where the chain is.
    Tip {
        node: u32,
        info: Box<Result<verus_sdk::network::ChainInfo, verus_sdk::network::RpcError>>,
        latency: std::time::Duration,
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
        // A payment left unresolved by a previous run is the first thing worth
        // saying — it is money whose fate nobody knows.
        self.emit_pending();

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
                    self.poll_pending();
                    continue;
                }
            };

            if !self.handle(command).await {
                break;
            }
        }

        tracing::info!("core stopped");
    }

    fn create_wallet(&mut self, name: &str, passphrase: &chainvue_protocol::Secret) {
        self.busy(TaskKind::CreatingWallet, true);
        match self.wallet.create(name, passphrase) {
            Ok(challenge) => {
                self.emit_wallet();
                // The phrase this just generated has never been seen by anyone.
                // Announcing the challenge is what puts the backup screen in
                // front of the dashboard rather than beside it.
                self.emit_challenge(&challenge);
                // A brand-new key has nothing on chain, and saying so from the
                // node beats showing a zero the wallet made up.
                self.refresh();
            }
            Err(error) => self.notice("wallet_create", "Could not create the wallet", &error),
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
        material: chainvue_protocol::ImportMaterial,
        passphrase: &chainvue_protocol::Secret,
    ) {
        use chainvue_protocol::ImportMaterial;

        let imported = match material {
            ImportMaterial::Phrase(p) => Imported::Phrase(p),
            ImportMaterial::Text(t) => Imported::Text(t),
            ImportMaterial::Wif(w) => Imported::Wif(w),
        };

        self.busy(TaskKind::CreatingWallet, true);
        match self.wallet.import(label, imported, passphrase) {
            Ok(()) => {
                self.emit_wallet();
                // A restored wallet usually has a history. Asking for it is the
                // whole reason someone restored.
                self.refresh();
            }
            Err(error) => {
                let title = import_title(&error);
                self.notice("import_key", &title, &error);
            }
        }
        self.busy(TaskKind::CreatingWallet, false);
    }

    fn reveal_backup(&mut self, label: &str, passphrase: &chainvue_protocol::Secret) {
        match self.wallet.begin_reveal(label, passphrase) {
            Ok(challenge) => self.emit_challenge(&challenge),
            Err(error) => self.notice(
                "reveal_backup",
                "That passphrase does not unlock this wallet",
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
                self.notice("finish_backup", "Could not record the backup", &error);
            }
            let _ = self.events.send(Event::SeedWords(Vec::new()));
        }

        // One bool. Never which word was wrong.
        let _ = self.events.send(Event::PhraseConfirmed(correct));

        if correct {
            self.emit_wallet();
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
                self.notice("node_connect", "Could not reach that node", &error);
                None
            }
        }
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

        let addresses: Vec<String> = self
            .wallet
            .view()
            .keys
            .into_iter()
            .map(|key| key.address)
            .collect();

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
    }

    fn finish_work(&mut self, work: Work) {
        match work {
            Work::Portfolio(reading) => self.finish_refresh(&reading),
            Work::Prepared { ticket, result } => self.finish_prepare(ticket, *result),
            Work::Tip {
                node,
                info,
                latency,
            } => self.finish_tip(node, &info, latency),
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

        let ticker = self
            .nodes
            .active()
            .and_then(|node| node.network.as_ref())
            .map_or("VRSC", chainvue_chain::Network::ticker);

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
            }
            Err(error) => {
                // An unknown history is not an empty one, and the difference
                // matters: "no transactions yet" is a claim about the chain.
                self.notice("history", "Could not read this wallet's activity", error);
            }
        }

        self.remember(&portfolio, reading);
    }

    /// Keep what this refresh learned, for the next cold start.
    ///
    /// Only a read that actually worked. Caching a failed one would mean the
    /// next start restores a wrong balance and presents it as the last known
    /// good figure.
    fn remember(&self, portfolio: &chainvue_protocol::PortfolioVm, reading: &portfolio::Reading) {
        let Some(store) = &self.store else {
            return;
        };
        if reading.failure.is_some() || reading.history.is_err() {
            return;
        }

        store.save_snapshot(
            portfolio,
            &portfolio::rows_from(&self.history.entries, &self.cached.names, now()),
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
    async fn handle(&mut self, command: Command) -> bool {
        match command {
            Command::ProbeNodes => self.probe_all().await,
            Command::SelectNode(id) => {
                if self.nodes.set_active(id) {
                    // The client belongs to the node it was built for.
                    self.chain = None;
                    self.emit_network();
                    self.refresh();
                }
            }
            Command::Refresh(_) => self.refresh(),
            Command::LoadHistory { .. } => self.load_older_history(),
            Command::LoadTxDetail(txid) => self.load_tx_detail(&txid),

            // ── Send ─────────────────────────────────────────────────
            Command::ValidateDraft(draft) => {
                let _ = self.events.send(Event::SendValidation(send::validate(
                    &draft,
                    self.spendable,
                )));
            }
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
                        // Unlocking is the moment the figures become worth
                        // fetching, and the moment they are stalest.
                        self.refresh();
                    }
                    Err(error) => self.notice(
                        "unlock",
                        "That passphrase does not unlock this wallet",
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
            }
            Command::ChangePassphrase { old, new } => {
                self.change_passphrase(&old, &new);
            }
            Command::SetAutoLockMinutes(minutes) => self.set_auto_lock(minutes),
            Command::SetAllowMainnetSpend {
                on,
                typed_confirmation,
            } => self.set_mainnet_spend(on, &typed_confirmation),
            Command::ResolvePending { id, action } => self.resolve_pending(id, action),
            Command::ScreenEntered(_) | Command::UserActivity => self.wallet.touch(),
            Command::Shutdown => return false,
            other => {
                tracing::debug!(?other, "command not handled yet");
            }
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

        let addresses: Vec<String> = self
            .wallet
            .view()
            .keys
            .into_iter()
            .map(|key| key.address)
            .collect();
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
                self.notice("load_history", "Could not read older transactions", &error);
                return;
            }
        };

        // Older entries go at the FRONT: the list is oldest-first, and the
        // renderer reverses it.
        self.history.entries.splice(0..0, page.entries);
        self.history.scanned_to = page.scanned_to;
        self.history.complete = page.reached_start;

        self.emit_history();
    }

    /// Send the whole list, re-grouped.
    ///
    /// A whole replacement rather than an append, because the day headings have
    /// to be recomputed across the join — appending a page whose first row is
    /// another "Yesterday" would print the heading twice. The list is a few
    /// hundred rows and carries no in-flight transitions, so there is nothing
    /// for a delta to protect.
    fn emit_history(&self) {
        let rows = portfolio::rows_from(&self.history.entries, &self.cached.names, now());
        let _ = self.events.send(Event::History {
            key: String::new(),
            delta: chainvue_protocol::ListDelta::Replace(rows),
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
            delta: chainvue_protocol::ListDelta::Replace(history),
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

        if self.polling_tip || self.tip_polled.is_some_and(|last| last.elapsed() < EVERY) {
            return;
        }
        let Some(node) = self.nodes.active() else {
            return;
        };
        let (id, url) = (node.id, node.url.clone());

        self.polling_tip = true;
        self.tip_polled = Some(std::time::Instant::now());

        self.blocking.dispatch(
            move || {
                let (info, latency) = chainvue_chain::probe(&url);
                Work::Tip {
                    node: id,
                    info: Box::new(info),
                    latency,
                }
            },
            self.work.clone(),
        );
    }

    fn finish_tip(
        &mut self,
        node: u32,
        info: &Result<verus_sdk::network::ChainInfo, verus_sdk::network::RpcError>,
        latency: std::time::Duration,
    ) {
        self.polling_tip = false;

        let before = self.nodes.active().and_then(|n| n.tip);
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

        // A new block is the only reason to re-read anything. Polling the tip
        // and refreshing regardless would turn a one-request poll into seven.
        if info.is_ok() && after != before {
            tracing::debug!(?before, ?after, "a new block");
            self.refresh();
        }
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

    fn resolve_pending(&mut self, id: u64, action: chainvue_protocol::PendingAction) {
        use chainvue_protocol::PendingAction;

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
                self.notice("spend_refused", &refusal_title(&refused), &refused);
                return;
            }
        };
        let Some(chain) = self.chain() else {
            return;
        };

        self.busy(TaskKind::Broadcasting, true);
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

        match result {
            Ok(txid) => {
                tracing::info!(record, %txid, "the same bytes were accepted on a resend");
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
                    "That payment still could not be confirmed",
                    &error,
                );
                self.emit_pending();
            }
        }
    }

    fn emit_pending(&self) {
        let rows: Vec<chainvue_protocol::PendingVm> = self
            .pending
            .unresolved()
            .map(|record| chainvue_protocol::PendingVm {
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
            .send(Event::Pending(chainvue_protocol::ListDelta::Replace(rows)));
    }

    // ── Settings ────────────────────────────────────────────────────────────

    fn change_passphrase(
        &mut self,
        old: &chainvue_protocol::Secret,
        new: &chainvue_protocol::Secret,
    ) {
        match self.wallet.change_passphrase(old, new) {
            Ok(()) => {
                self.notice_info("passphrase_changed", "Passphrase changed");
                self.emit_wallet();
            }
            Err(error) => self.notice(
                "change_passphrase",
                "That is not the passphrase this wallet is using",
                &error,
            ),
        }
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
            .send(Event::Notice(chainvue_protocol::UiError::simple(
                "mainnet_confirmation",
                "Type the word mainnet to turn this on".to_string(),
                "Spending real coins is off by default, and turning it on is deliberately a \
                 little awkward."
                    .to_string(),
                chainvue_protocol::Severity::Warning,
            )));
    }

    // ── Send ────────────────────────────────────────────────────────────────

    /// Build and sign, off the actor.
    ///
    /// `prepare_send` reads the funding set first, so this is several requests
    /// plus an ECDSA signature — not something to hold the actor for. The key
    /// is decrypted inside `with_key` on the worker thread and dropped before
    /// that closure returns.
    fn prepare_send(&mut self, draft: chainvue_protocol::SendDraft) {
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
                result: Box::new(send::prepare(&chain, &vault, &label, &draft)),
            },
            self.work.clone(),
        );
    }

    fn finish_prepare(&mut self, ticket: u64, result: Result<send::Prepared, send::SendError>) {
        self.busy(TaskKind::PreparingSend, false);

        match result {
            Ok(prepared) => {
                let from = self.wallet.active_address().unwrap_or_default();
                // Built from the SIGNED bytes, not from the draft — see
                // `send::review`.
                let review = send::review(ticket, &prepared, &from, self.spendable, false);
                self.prepared.insert(ticket, prepared);
                let _ = self.events.send(Event::SendPrepared(review));
            }
            Err(error) => {
                let title = send_title(&error);
                self.notice("prepare_send", &title, &error);
                let _ =
                    self.events
                        .send(Event::SendResult(chainvue_protocol::SendOutcomeVm::Failed(
                            chainvue_protocol::UiError::simple(
                                "prepare_send",
                                title,
                                error.to_string(),
                                chainvue_protocol::Severity::Danger,
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
                self.notice("spend_refused", &refusal_title(&refused), &refused);
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
                    "Could not record the payment before sending it, so it was not sent",
                    &error,
                );
                return;
            }
        };

        self.busy(TaskKind::Broadcasting, true);
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
        use chainvue_protocol::SendOutcomeVm;
        use verus_sdk::network::FlowError;

        self.busy(TaskKind::Broadcasting, false);

        match result {
            Ok(sent) => {
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
                let title = "The network rejected this transaction".to_string();
                self.notice("broadcast_rejected", &title, &error);
                let _ = self.events.send(Event::SendResult(SendOutcomeVm::Failed(
                    chainvue_protocol::UiError::simple(
                        "broadcast_rejected",
                        title,
                        format!("Nothing was spent. The node said: {error}"),
                        chainvue_protocol::Severity::Danger,
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
            let outcome = self
                .blocking
                .run(move || {
                    let (result, latency) = chainvue_chain::probe(&url);
                    (result, latency)
                })
                .await;

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
    fn notice(&self, code: &'static str, title: &str, error: &dyn std::error::Error) {
        let mut technical = error.to_string();
        let mut source = error.source();
        while let Some(cause) = source {
            technical.push_str("\n  caused by: ");
            technical.push_str(&cause.to_string());
            source = cause.source();
        }
        tracing::warn!(code, %technical, "notice");

        let _ = self.events.send(Event::Notice(
            chainvue_protocol::UiError::simple(
                code,
                title.to_string(),
                error.to_string(),
                chainvue_protocol::Severity::Warning,
            )
            .with_technical(technical),
        ));
    }

    /// A notice that is not a failure. Same channel, so the UI has one place to
    /// render everything it is told.
    fn notice_info(&self, code: &'static str, title: &str) {
        let _ = self
            .events
            .send(Event::Notice(chainvue_protocol::UiError::simple(
                code,
                title.to_string(),
                String::new(),
                chainvue_protocol::Severity::Info,
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
                Some(chainvue_chain::NodeStatus::Syncing { .. })
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
fn import_title(error: &wallet::ImportError) -> String {
    let problem = match error {
        wallet::ImportError::Mnemonic(problem) => problem,
        // `from_wif` enforces the Verus version byte, so a Bitcoin WIF lands
        // here — and "invalid key" would leave someone staring at a key that
        // is perfectly valid, just not for this chain.
        wallet::ImportError::Key(_) => {
            return "That is not a private key ChainVue can read. Verus keys start with a U; \
                    a key from another chain is refused rather than silently reinterpreted."
                .to_string()
        }
        wallet::ImportError::Vault(_) => return "Could not import that key".to_string(),
    };

    match problem {
        MnemonicError::Checksum => {
            "There is a typo in that phrase — one word is wrong or out of order.".to_string()
        }
        MnemonicError::UnknownWord { position } => {
            format!("Word {position} is not a recovery word. Check its spelling.")
        }
        MnemonicError::WordCount(count) => format!(
            "That is {count} words, and a recovery phrase has 12, 15, 18, 21 or 24. \
             If it is not a recovery phrase, choose Free text."
        ),
        // `MnemonicError` is `#[non_exhaustive]` — the SDK gains a variant
        // whenever it learns to refuse something new.
        other => format!("That phrase cannot be used: {other}"),
    }
}

/// What the send form says when a build fails.
fn send_title(error: &send::SendError) -> String {
    use verus_sdk::network::FlowError;

    match error {
        send::SendError::BadAddress => "That is not an address this wallet can pay".to_string(),
        send::SendError::BadAmount => "That is not an amount".to_string(),
        send::SendError::NothingToSend => "Enter an amount above zero".to_string(),
        send::SendError::Vault(_) => "The wallet is locked".to_string(),
        // The distinction the SDK draws and a wallet must not lose: what you
        // hold and what you can spend right now are different numbers, and a
        // bare "insufficient funds" against a screen showing a balance reads as
        // a bug in the wallet.
        send::SendError::Flow(FlowError::InsufficientFunds { .. }) => {
            "Not enough spendable coins. Mined coins need 100 confirmations, and coins held by \
             a VerusID cannot be moved by this key."
                .to_string()
        }
        send::SendError::Flow(_) => "Could not build this payment".to_string(),
    }
}

/// What the send screen says when the spending guard refuses.
fn refusal_title(refused: &chainvue_chain::SpendRefused) -> String {
    // Deliberately specific. "Refused" tells someone nothing about what to do,
    // and each of these has a different answer.
    refused.to_string()
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
                vault_path: std::path::PathBuf::from("/nonexistent/vault.json"),
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
    /// cargo test -p chainvue-core --lib -- --ignored an_idle_wallet
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
                vault_path: dir.path().join("vault.json"),
            },
        );

        dispatcher.send(Command::CreateWallet {
            name: "test".to_string(),
            passphrase: chainvue_protocol::Secret::from("a passphrase"),
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
                vault_path: dir.path().join("vault.json"),
            },
        );

        dispatcher.send(Command::CreateWallet {
            name: "test".to_string(),
            passphrase: chainvue_protocol::Secret::from("a passphrase"),
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
        let path = dir.path().join("vault.json");
        let handle = tokio::runtime::Handle::current();

        let (dispatcher, mut events) = start(
            &handle,
            Config {
                nodes: testnet_nodes(),
                network: Network::Testnet,
                mock: false,
                vault_path: path.clone(),
            },
        );

        // A phrase with a broken checksum.
        dispatcher.send(Command::ImportKey {
            label: "main".to_string(),
            material: chainvue_protocol::ImportMaterial::Phrase(chainvue_protocol::Secret::from(
                "abandon abandon abandon abandon abandon abandon \
                 abandon abandon abandon abandon abandon abandon",
            )),
            passphrase: chainvue_protocol::Secret::from("pass"),
        });

        let notice = loop {
            match events.recv().await {
                Some(Event::Notice(notice)) => break notice,
                Some(_) => {}
                None => panic!("the core stopped before refusing"),
            }
        };
        assert_eq!(notice.code, "import_key");
        assert!(notice.title.contains("typo"), "{}", notice.title);
        assert!(!path.exists(), "a refused restore created a wallet file");

        // The same words with a valid checksum.
        dispatcher.send(Command::ImportKey {
            label: "main".to_string(),
            material: chainvue_protocol::ImportMaterial::Phrase(chainvue_protocol::Secret::from(
                "abandon abandon abandon abandon abandon abandon \
                 abandon abandon abandon abandon abandon about",
            )),
            passphrase: chainvue_protocol::Secret::from("pass"),
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

        // What a previous run would have left behind.
        {
            let store = chainvue_store::Store::open(dir.path()).expect("store");
            let mut portfolio = chainvue_protocol::PortfolioVm::default();
            portfolio.balance.total_display = "48.8999 0000".to_string();
            // Written as current, because it WAS current when it was written.
            portfolio.stale = false;

            let history = vec![chainvue_protocol::HistoryRowVm {
                txid: "abc".to_string(),
                height: 1_187_000,
                block_time: 1_000_000_000,
                when_display: "2 hours ago".to_string(),
                group: "Today".to_string(),
                ..chainvue_protocol::HistoryRowVm::default()
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
                vault_path: dir.path().join("vault.json"),
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

        let chainvue_protocol::ListDelta::Replace(rows) = history.expect("cached history") else {
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
                vault_path: std::path::PathBuf::from("/nonexistent/vault.json"),
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
