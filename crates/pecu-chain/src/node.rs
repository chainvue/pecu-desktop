//! Nodes: what we know about them, and how often to ask again.

use std::time::{Duration, Instant, SystemTime};

use verus_sdk::network::{ChainInfo, ChainReader, HttpTransport, RpcClient, RpcError};

use crate::network::Network;
use crate::permit::{self, SpendPermit, SpendRefused};

/// A read-and-broadcast client.
///
/// The SDK keeps reading and broadcasting in separate traits, so a function
/// that takes `&impl ChainReader` is *incapable* of spending — a property of
/// the signature rather than a rule anyone has to remember.
pub type Client = RpcClient<HttpTransport>;

/// How long a normal request may take.
///
/// Long enough for a public node under load, short enough that a wrong URL
/// fails while someone is still looking at the screen.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// A probe gets a tighter budget than a real request: its whole job is to
/// answer "is this usable", and a node that needs 20 s to say hello is not.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// What we currently believe about a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeStatus {
    /// Never asked.
    Unknown,
    /// A probe is in flight.
    Probing,
    Online,
    /// Answering, but behind. Spending against a stale tip builds a
    /// transaction the network rejects, so this is not "online with a note".
    Syncing {
        blocks: u32,
        longest: u32,
    },
    /// Answering about a different chain than the wallet is set to.
    WrongNetwork {
        reported: Network,
    },
    /// Refused a method we need. **Not counted as a failure** — see
    /// [`Node::record_failure`].
    MethodRefused {
        method: String,
    },
    Offline {
        reason: String,
    },
}

impl NodeStatus {
    /// The short word the UI shows.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Probing => "probing",
            Self::Online => "online",
            Self::Syncing { .. } | Self::WrongNetwork { .. } | Self::MethodRefused { .. } => {
                "degraded"
            }
            Self::Offline { .. } => "offline",
        }
    }

    /// A sentence explaining a status that is not simply "online".
    pub fn note(&self) -> Option<String> {
        match self {
            Self::Syncing { blocks, longest } => {
                Some(format!("catching up — {blocks} of {longest} blocks"))
            }
            Self::WrongNetwork { reported } => Some(format!("this node is on {reported}")),
            Self::MethodRefused { method } => Some(format!("refused `{method}`")),
            Self::Offline { reason } => Some(reason.clone()),
            Self::Unknown | Self::Probing | Self::Online => None,
        }
    }
}

/// One configured endpoint.
#[derive(Clone, Debug)]
pub struct Node {
    pub id: u32,
    pub label: String,
    pub url: String,
    /// Built-ins cannot be deleted, only added to.
    pub builtin: bool,
    /// What the node said it is. `None` until it answers — and never inferred
    /// from the URL.
    pub network: Option<Network>,
    pub status: NodeStatus,
    pub tip: Option<u32>,
    pub latency: Option<Duration>,
    pub last_success: Option<SystemTime>,
    pub consecutive_failures: u32,
    /// Set when a probe fails, so the poller can back off.
    pub retry_after: Option<Duration>,
}

impl Node {
    pub fn builtin(id: u32, label: &str, url: &str) -> Self {
        Self {
            id,
            label: label.to_string(),
            url: url.to_string(),
            builtin: true,
            network: None,
            status: NodeStatus::Unknown,
            tip: None,
            latency: None,
            last_success: None,
            consecutive_failures: 0,
            retry_after: None,
        }
    }

    pub fn user_added(id: u32, label: &str, url: &str) -> Self {
        Self {
            builtin: false,
            ..Self::builtin(id, label, url)
        }
    }

    /// Fold in a successful probe.
    ///
    /// `requested` is the chain the wallet is set to: a node that answers
    /// perfectly about the wrong chain is degraded, not online.
    pub fn record_success(&mut self, info: &ChainInfo, latency: Duration, requested: &Network) {
        let reported = Network::from_chain_name(&info.name);

        self.status = if info.blocks < info.longest_chain {
            NodeStatus::Syncing {
                blocks: info.blocks,
                longest: info.longest_chain,
            }
        } else if reported != *requested {
            NodeStatus::WrongNetwork {
                reported: reported.clone(),
            }
        } else {
            NodeStatus::Online
        };

        self.network = Some(reported);
        self.tip = Some(info.blocks);
        self.latency = Some(latency);
        self.last_success = Some(SystemTime::now());
        self.consecutive_failures = 0;
        self.retry_after = None;
    }

    /// Fold in a failed probe.
    ///
    /// # `MethodUnavailable` is deliberately not a failure
    ///
    /// The SDK documents `-32601` as meaning "refused at every arity this crate
    /// knows how to ask", which on a filtering public proxy can mean the method
    /// is simply not exposed — not that the node is down. Counting it would
    /// back off, and eventually abandon, a node that is answering everything
    /// else perfectly well.
    pub fn record_failure(&mut self, error: &RpcError) {
        if let RpcError::MethodUnavailable { method } = error {
            self.status = NodeStatus::MethodRefused {
                method: (*method).to_string(),
            };
            self.retry_after = None;
            return;
        }

        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.status = NodeStatus::Offline {
            reason: error.to_string(),
        };
        self.retry_after = Some(backoff(self.consecutive_failures));
    }

    /// Milliseconds, for display.
    pub fn latency_ms(&self) -> Option<u32> {
        self.latency
            .map(|d| u32::try_from(d.as_millis()).unwrap_or(u32::MAX))
    }
}

/// How long to wait before probing a node that just failed.
///
/// Doubles from 5 s to a 5-minute ceiling. A fixed short interval hammers a
/// node that is already struggling; no backoff at all is how a wallet keeps a
/// dead endpoint warm forever.
///
/// Deliberately a pure function of the failure count so it can be table-tested
/// — jitter is applied by the caller, which owns the randomness.
pub fn backoff(consecutive_failures: u32) -> Duration {
    const BASE_MS: u64 = 5_000;
    const CAP_MS: u64 = 300_000;

    if consecutive_failures == 0 {
        return Duration::from_millis(BASE_MS);
    }
    let shift = (consecutive_failures - 1).min(6);
    Duration::from_millis(BASE_MS.saturating_mul(1u64 << shift).min(CAP_MS))
}

/// Build a client for `url`.
///
/// Opens no connection — the first request does that. So this succeeding means
/// the URL is well formed, not that anything is listening.
///
/// Plaintext `http://` to anything but loopback is refused by the SDK's
/// transport, because every address the wallet asks about would otherwise be
/// readable in transit.
pub fn connect(url: &str, timeout: Duration) -> Result<Client, RpcError> {
    Ok(RpcClient::new(
        HttpTransport::new(url)?.with_timeout(timeout),
    ))
}

/// Check that a URL is one this wallet may talk to, without contacting it.
///
/// The gate a user-added endpoint has to pass before it is written down. It is
/// deliberately the SDK's own check rather than one of ours: `HttpTransport`
/// refuses a scheme it does not know and refuses plaintext `http://` to
/// anything but loopback, because every address the wallet asks about would
/// otherwise be readable by anyone on the path.
///
/// What this does **not** say is that anything is listening. That answer costs
/// a request, and it is the probe's job.
pub fn validate_url(url: &str) -> Result<(), RpcError> {
    HttpTransport::new(url).map(|_| ())
}

/// Two URLs that mean the same endpoint.
///
/// Only the differences that are certainly cosmetic: surrounding whitespace and
/// a trailing slash. Deliberately not case-folding the whole URL — a host is
/// case-insensitive but a path is not, and treating `/API` and `/api` as one
/// endpoint would be this function inventing a fact about somebody's server.
fn same_endpoint(left: &str, right: &str) -> bool {
    fn tidy(url: &str) -> &str {
        url.trim().trim_end_matches('/')
    }
    tidy(left) == tidy(right)
}

/// Ask a node what it is, and how long it took to answer.
///
/// One `chain_info()` call yields the chain name, the tip, the sync state and
/// the version — so a probe is also the tip poller for the active node, and
/// costs one request rather than four.
pub fn probe(url: &str) -> (Result<ChainInfo, RpcError>, Duration) {
    let started = Instant::now();
    let result = connect(url, PROBE_TIMEOUT).and_then(|client| client.chain_info());
    (result, started.elapsed())
}

/// The set of configured nodes, and which one is in use.
#[derive(Debug, Default)]
pub struct NodeManager {
    nodes: Vec<Node>,
    active: Option<u32>,
    requested: Option<Network>,
    allow_mainnet_spend: bool,
}

impl NodeManager {
    pub fn new(nodes: Vec<Node>, requested: Network) -> Self {
        let active = nodes.first().map(|n| n.id);
        Self {
            nodes,
            active,
            requested: Some(requested),
            allow_mainnet_spend: false,
        }
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn active(&self) -> Option<&Node> {
        self.active.and_then(|id| self.get(id))
    }

    pub fn get(&self, id: u32) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    pub fn set_active(&mut self, id: u32) -> bool {
        if self.get(id).is_some() {
            self.active = Some(id);
            true
        } else {
            false
        }
    }

    /// The node currently in use, by URL.
    ///
    /// The URL rather than the id, because the id of a built-in is its position
    /// in a compiled-in list and that position is not a promise. This is what
    /// gets written down so the choice survives a restart that reordered them.
    pub fn active_url(&self) -> Option<&str> {
        self.active().map(|node| node.url.as_str())
    }

    /// Make active whichever node serves `url`, if one does.
    pub fn set_active_by_url(&mut self, url: &str) -> bool {
        let Some(id) = self
            .nodes
            .iter()
            .find(|node| same_endpoint(&node.url, url))
            .map(|node| node.id)
        else {
            return false;
        };
        self.active = Some(id);
        true
    }

    /// Whether some node already serves this endpoint.
    pub fn has_url(&self, url: &str) -> bool {
        self.nodes.iter().any(|node| same_endpoint(&node.url, url))
    }

    /// Add an endpoint the user configured.
    ///
    /// The caller supplies the id, because the id has to be the one the durable
    /// store assigned — a list that numbered its own entries would disagree
    /// with the file the moment anything was removed.
    ///
    /// Does **not** validate the URL. That is [`validate_url`], and it belongs
    /// before the row is written rather than after.
    pub fn add(&mut self, id: u32, label: &str, url: &str) {
        self.nodes.push(Node::user_added(id, label, url));
    }

    /// Remove a user-added endpoint.
    ///
    /// Refuses a built-in: those come from the build, so "removing" one would
    /// last until the next start and then quietly undo itself.
    ///
    /// Removing the active node hands the active slot to the first one left,
    /// so the wallet is never pointed at something that is no longer there.
    /// `true` when it was removed.
    pub fn remove(&mut self, id: u32) -> bool {
        let Some(index) = self
            .nodes
            .iter()
            .position(|node| node.id == id && !node.builtin)
        else {
            return false;
        };

        self.nodes.remove(index);
        if self.active == Some(id) {
            self.active = self.nodes.first().map(|node| node.id);
        }
        true
    }

    pub fn requested(&self) -> Option<&Network> {
        self.requested.as_ref()
    }

    pub fn set_requested(&mut self, network: Network) {
        self.requested = Some(network);
    }

    /// Turn mainnet spending on or off.
    ///
    /// The typed confirmation is checked **here**, not in the UI. A
    /// confirmation the interface could skip is decoration; this is the only
    /// place that decides.
    pub fn set_allow_mainnet_spend(&mut self, on: bool, typed: &str) -> bool {
        if on && !typed.trim().eq_ignore_ascii_case("mainnet") {
            return false;
        }
        self.allow_mainnet_spend = on;
        true
    }

    pub fn allow_mainnet_spend(&self) -> bool {
        self.allow_mainnet_spend
    }

    /// Mint a permit, or explain why not.
    ///
    /// The only way to obtain a [`SpendPermit`], and therefore the only way to
    /// reach a broadcaster anywhere in the application.
    pub fn spend_permit(&self) -> Result<SpendPermit, SpendRefused> {
        let requested = self
            .requested
            .as_ref()
            .ok_or(SpendRefused::NetworkUnknown)?;
        permit::evaluate(self.active(), requested, self.allow_mainnet_spend)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(blocks: u32, longest: u32, name: &str) -> ChainInfo {
        ChainInfo {
            name: name.to_string(),
            chain_id: "i-something".to_string(),
            blocks,
            longest_chain: longest,
            version: "test".to_string(),
        }
    }

    #[test]
    fn backoff_doubles_and_then_stops() {
        assert_eq!(backoff(0), Duration::from_secs(5));
        assert_eq!(backoff(1), Duration::from_secs(5));
        assert_eq!(backoff(2), Duration::from_secs(10));
        assert_eq!(backoff(3), Duration::from_secs(20));
        // Capped, and it stays capped however long the outage lasts.
        assert_eq!(backoff(20), Duration::from_mins(5));
        assert_eq!(backoff(u32::MAX), Duration::from_mins(5));
    }

    #[test]
    fn a_healthy_node_reports_online_and_clears_its_failures() {
        let mut node = Node::builtin(0, "n", "https://example.invalid");
        node.consecutive_failures = 4;
        node.record_success(
            &info(1000, 1000, "VRSCTEST"),
            Duration::from_millis(42),
            &Network::Testnet,
        );

        assert_eq!(node.status, NodeStatus::Online);
        assert_eq!(node.network, Some(Network::Testnet));
        assert_eq!(node.tip, Some(1000));
        assert_eq!(node.latency_ms(), Some(42));
        assert_eq!(node.consecutive_failures, 0);
    }

    #[test]
    fn a_node_behind_the_chain_is_syncing_not_online() {
        let mut node = Node::builtin(0, "n", "https://example.invalid");
        node.record_success(
            &info(900, 1000, "VRSCTEST"),
            Duration::from_millis(10),
            &Network::Testnet,
        );
        assert_eq!(
            node.status,
            NodeStatus::Syncing {
                blocks: 900,
                longest: 1000
            }
        );
        assert_eq!(node.status.label(), "degraded");
    }

    /// Answering perfectly about the wrong chain is not "online".
    #[test]
    fn a_node_on_another_chain_is_degraded() {
        let mut node = Node::builtin(0, "n", "https://example.invalid");
        node.record_success(
            &info(1000, 1000, "VRSC"),
            Duration::from_millis(10),
            &Network::Testnet,
        );
        assert_eq!(
            node.status,
            NodeStatus::WrongNetwork {
                reported: Network::Mainnet
            }
        );
    }

    /// The SDK says `-32601` can mean "not at this arity" rather than "down".
    /// Backing off a node that is answering everything else would be wrong.
    #[test]
    fn a_refused_method_does_not_count_as_a_failure() {
        let mut node = Node::builtin(0, "n", "https://example.invalid");
        node.record_failure(&RpcError::MethodUnavailable {
            method: "getaddressutxos",
        });

        assert_eq!(node.consecutive_failures, 0);
        assert!(node.retry_after.is_none());
        assert_eq!(node.status.label(), "degraded");
    }

    #[test]
    fn a_transport_failure_backs_off() {
        let mut node = Node::builtin(0, "n", "https://example.invalid");
        node.record_failure(&RpcError::Transport("connection reset".into()));
        assert_eq!(node.consecutive_failures, 1);
        assert_eq!(node.retry_after, Some(Duration::from_secs(5)));

        node.record_failure(&RpcError::Transport("connection reset".into()));
        assert_eq!(node.retry_after, Some(Duration::from_secs(10)));
    }

    /// Enabling mainnet spending requires the word, and the check lives here
    /// rather than in the interface.
    #[test]
    fn the_mainnet_opt_in_needs_the_typed_word() {
        let mut manager = NodeManager::new(
            vec![Node::builtin(0, "n", "https://example.invalid")],
            Network::Mainnet,
        );

        assert!(!manager.set_allow_mainnet_spend(true, ""));
        assert!(!manager.set_allow_mainnet_spend(true, "yes"));
        assert!(!manager.allow_mainnet_spend());

        assert!(manager.set_allow_mainnet_spend(true, "mainnet"));
        assert!(manager.allow_mainnet_spend());

        // Turning it back off needs no confirmation — refusing to spend is
        // never the dangerous direction.
        assert!(manager.set_allow_mainnet_spend(false, ""));
        assert!(!manager.allow_mainnet_spend());
    }

    /// The check that stands between a user-added endpoint and every address
    /// this wallet is about to ask about.
    #[test]
    fn a_url_that_would_leak_in_transit_is_refused() {
        assert!(validate_url("https://api.verustest.net").is_ok());
        // Loopback is allowed: there is no network to read it off.
        assert!(validate_url("http://127.0.0.1:27486").is_ok());

        // Plaintext to anything else would put every address the wallet asks
        // about in front of whoever is on the path.
        assert!(validate_url("http://api.verustest.net").is_err());
        // And a scheme that is not HTTP at all.
        assert!(validate_url("ftp://example.invalid").is_err());
        assert!(validate_url("not a url").is_err());
        assert!(validate_url("").is_err());
    }

    #[test]
    fn a_user_added_node_can_be_removed_and_a_builtin_cannot() {
        let mut manager = NodeManager::new(
            vec![Node::builtin(0, "shipped", "https://builtin.invalid")],
            Network::Testnet,
        );
        manager.add(1000, "mine", "https://mine.invalid");
        assert_eq!(manager.nodes().len(), 2);

        // A built-in comes from the build. "Removing" one would last until the
        // next start and then undo itself.
        assert!(!manager.remove(0));
        assert_eq!(manager.nodes().len(), 2);

        assert!(manager.remove(1000));
        assert_eq!(manager.nodes().len(), 1);
        // And removing something that was never there is not a removal.
        assert!(!manager.remove(1000));
    }

    /// Removing whichever node is in use must not leave the wallet pointed at
    /// an endpoint that is no longer configured.
    #[test]
    fn removing_the_active_node_moves_the_active_slot() {
        let mut manager = NodeManager::new(
            vec![Node::builtin(0, "shipped", "https://builtin.invalid")],
            Network::Testnet,
        );
        manager.add(1000, "mine", "https://mine.invalid");
        assert!(manager.set_active(1000));

        assert!(manager.remove(1000));
        assert_eq!(manager.active().map(|node| node.id), Some(0));
    }

    /// The active node is remembered by URL, because the id of a built-in is
    /// its position in a compiled-in list and that position is not a promise.
    #[test]
    fn the_active_node_is_identified_by_its_url() {
        let mut manager = NodeManager::new(
            vec![
                Node::builtin(0, "one", "https://one.invalid"),
                Node::builtin(1, "two", "https://two.invalid"),
            ],
            Network::Testnet,
        );

        assert_eq!(manager.active_url(), Some("https://one.invalid"));
        assert!(manager.set_active_by_url("https://two.invalid"));
        assert_eq!(manager.active().map(|node| node.id), Some(1));

        // A trailing slash is the same endpoint; an unknown one changes nothing.
        assert!(manager.set_active_by_url("https://one.invalid/"));
        assert_eq!(manager.active().map(|node| node.id), Some(0));
        assert!(!manager.set_active_by_url("https://elsewhere.invalid"));
        assert_eq!(manager.active().map(|node| node.id), Some(0));
    }

    #[test]
    fn an_endpoint_that_is_already_configured_is_recognised() {
        let mut manager = NodeManager::new(
            vec![Node::builtin(0, "one", "https://one.invalid")],
            Network::Testnet,
        );
        manager.add(1000, "mine", "https://mine.invalid/");

        assert!(manager.has_url("https://one.invalid"));
        assert!(manager.has_url("  https://one.invalid/  "));
        assert!(manager.has_url("https://mine.invalid"));
        assert!(!manager.has_url("https://other.invalid"));
        // A path is not case-insensitive, and pretending otherwise would be
        // this wallet inventing a fact about somebody's server.
        assert!(!manager.has_url("https://one.invalid/API"));
    }

    #[test]
    fn a_permit_is_refused_until_a_node_has_answered() {
        let manager = NodeManager::new(
            vec![Node::builtin(0, "n", "https://example.invalid")],
            Network::Testnet,
        );
        assert_eq!(
            manager.spend_permit().unwrap_err(),
            SpendRefused::NetworkUnknown
        );
    }

    #[test]
    fn a_permit_is_issued_once_the_node_is_online_and_agrees() {
        let mut manager = NodeManager::new(
            vec![Node::builtin(0, "n", "https://example.invalid")],
            Network::Testnet,
        );
        manager.get_mut(0).expect("node 0").record_success(
            &info(1000, 1000, "VRSCTEST"),
            Duration::from_millis(5),
            &Network::Testnet,
        );

        let permit = manager.spend_permit().expect("permit");
        assert_eq!(permit.tip(), 1000);
        assert_eq!(permit.node_id(), 0);
    }
}
