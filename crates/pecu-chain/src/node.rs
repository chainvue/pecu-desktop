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
    /// Answering with a chain name and a chain id that do not go together.
    ///
    /// Kept apart from [`NodeStatus::WrongNetwork`] because the two are not the
    /// same problem and do not have the same remedy. `WrongNetwork` is a node
    /// honestly on a chain the wallet is not set to, and switching the wallet
    /// to that chain fixes it. This is a node whose two statements about its
    /// own identity disagree, so there is no chain to switch to: a relabelling
    /// proxy, a hand-rolled RPC shim, or something dishonest. Reporting it as
    /// `WrongNetwork` would tell a user on VRSCTEST that "this node is on
    /// VRSCTEST", which is the least useful sentence available for the one case
    /// it would be describing.
    Unidentified {
        name: String,
        chain_id: String,
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
            Self::Syncing { .. }
            | Self::WrongNetwork { .. }
            | Self::Unidentified { .. }
            | Self::MethodRefused { .. } => "degraded",
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
            Self::Unidentified { name, chain_id } => Some(format!(
                "calls itself {name} but reports chain id {chain_id}"
            )),
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
    ///
    /// # Identity is judged before anything else in the reply
    ///
    /// For a chain [`Network::chain_id`] pins an id for, the reported name has
    /// to be borne out by the reported chain id, and that check comes before
    /// the sync check rather than after it. A node whose two statements about
    /// its own identity contradict each other has not made a believable
    /// statement about anything else in the same reply either, its own height
    /// included, so "catching up" would be the wrong thing to say about it.
    ///
    /// `network` is then left `None` rather than set to what the name read as,
    /// because that is simply the truth — the wallet does not know which chain
    /// this is — and because it keeps the spend gate refusing even if that
    /// gate's own identity branch were ever lost: an unset network is a refusal
    /// there on its own.
    ///
    /// So `None` no longer only means "nothing has answered yet", and every
    /// gate reading `network` has to decide which of the two it is looking at.
    /// `pecu_core`'s read gate makes that decision, and argues it, where it
    /// lives.
    ///
    /// The node is left selected all the same. Nothing here rotates away from
    /// it — `consecutive_failures` is cleared below, because the endpoint did
    /// answer — and failing over would be worse than staying: the wallet would
    /// silently move to some other node while the one the user chose sits there
    /// contradicting itself unread. Refusing loudly on the node the user picked
    /// is the state a person can act on.
    ///
    /// A chain this build pins no id for has nothing to cross-check against and
    /// is believed on its name exactly as before, so every PBaaS node keeps
    /// working. Read [`Network`]'s own docs — "The name is cross-checked
    /// against the chain id, and what that buys" — for what the pair is and is
    /// not worth: it is not a defence against a hostile endpoint.
    pub fn record_success(&mut self, info: &ChainInfo, latency: Duration, requested: &Network) {
        let reported = Network::from_chain_name(&info.name);
        let identified = reported
            .chain_id()
            .is_none_or(|pinned| pinned == info.chain_id);

        self.status = if !identified {
            NodeStatus::Unidentified {
                name: info.name.clone(),
                chain_id: info.chain_id.clone(),
            }
        } else if info.blocks < info.longest_chain {
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

        self.network = identified.then_some(reported);
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
/// One `chain_info()` call yields the chain name, the chain's own currency id,
/// the tip, the sync state and the version — so a probe is also the tip poller
/// for the active node, and costs one request rather than five.
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
    /// One flag for every chain, and not a map from chain to flag.
    ///
    /// It can only ever authorise the chain this manager was built for: the
    /// typed word is that chain's own name, and `permit::evaluate` refuses a
    /// spend whose effective chain is not the requested one.
    ///
    /// That makes one bool safe for five chains, but only while every route to
    /// a different chain clears it — otherwise a word typed for VRSCTEST would
    /// still be standing when the wallet arrives on vARRR. Two routes exist and
    /// both clear it: `pecu_core` switches chains by throwing this manager away
    /// and building another with this `false`, and `set_requested` clears it
    /// for anyone who changes the chain in place. This is the argument for
    /// both; they carry a pointer here rather than repeating it.
    allow_spending: bool,
}

impl NodeManager {
    pub fn new(nodes: Vec<Node>, requested: Network) -> Self {
        let active = nodes.first().map(|n| n.id);
        Self {
            nodes,
            active,
            requested: Some(requested),
            allow_spending: false,
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

    /// Point the manager at another chain, and disarm spending.
    ///
    /// The disarm is the point, and the reason is on the `allow_spending`
    /// field: an arm belongs to the chain it was made on. `pecu_core` happens
    /// to switch chains by building a fresh manager, so this is the second of
    /// the two routes rather than the live one — which is exactly why it is
    /// written down here instead of being left to whichever way the switch is
    /// implemented next.
    pub fn set_requested(&mut self, network: Network) {
        if self.requested.as_ref() != Some(&network) {
            self.allow_spending = false;
        }
        self.requested = Some(network);
    }

    /// Turn spending on or off for the chain this manager is set to.
    ///
    /// The typed confirmation is checked **here**, not in the UI. A
    /// confirmation the interface could skip is decoration; this is the only
    /// place that decides.
    ///
    /// The word is the chain's own name — `VRSC`, `vARRR`, or whatever an
    /// unknown chain calls itself — rather than a fixed `mainnet`. Asking
    /// somebody to type "mainnet" before spending their ARRR is a sentence that
    /// means nothing, and a confirmation that means nothing is one people learn
    /// to type through. Spelling the chain also makes the word the single piece
    /// of evidence that the person knew *which* chain they were arming: it does
    /// not carry over by muscle memory from the last one.
    ///
    /// Compared against `requested` rather than against what a node reports,
    /// because arming happens on a settings screen where no node need have
    /// answered yet. The two are the same by the time it matters:
    /// `permit::evaluate` refuses any spend where they differ.
    ///
    /// Refused outright when there is no chain to name, and when the chain
    /// names itself with an empty string. `Network::Other(String::new())` is
    /// constructible — `from_chain_name("")` produces it — and an expected word
    /// of `""` would be matched by an untouched box, so the guard would arm
    /// itself on exactly the chain nobody can vouch for.
    pub fn set_allow_spending(&mut self, on: bool, typed: &str) -> bool {
        if on {
            let expected = self.requested.as_ref().map_or("", Network::chain_name);
            if expected.is_empty() || !typed.trim().eq_ignore_ascii_case(expected) {
                return false;
            }
        }
        self.allow_spending = on;
        true
    }

    pub fn allow_spending(&self) -> bool {
        self.allow_spending
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
        permit::evaluate(self.active(), requested, self.allow_spending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a node says about itself, with the chain id that goes with the name.
    ///
    /// The pair has to be consistent or the helper builds a node no daemon
    /// could be, and every test that is about something else then fails for a
    /// reason it is not about — `record_success` holds the two together. A name
    /// this build pins no id for keeps the placeholder, because for those there
    /// is nothing for the id to agree with.
    fn info(blocks: u32, longest: u32, name: &str) -> ChainInfo {
        ChainInfo {
            name: name.to_string(),
            chain_id: Network::from_chain_name(name)
                .chain_id()
                .unwrap_or("i-something")
                .to_string(),
            blocks,
            longest_chain: longest,
            version: "test".to_string(),
        }
    }

    /// The same, with both halves under the test's control, so a test can build
    /// the self-contradicting answer the check exists to catch.
    fn claiming(name: &str, chain_id: &str) -> ChainInfo {
        ChainInfo {
            chain_id: chain_id.to_string(),
            ..info(1_000, 1_000, name)
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

    /// The case the cross-check exists for. A node calling itself VRSCTEST
    /// while reporting mainnet's own currency id has contradicted itself, and
    /// resolving that in favour of the name would leave a wallet set to testnet
    /// holding a mainnet node's answers — which is fund loss, because the
    /// addresses are the same on both chains and a signature made "for testnet"
    /// is valid on mainnet.
    #[test]
    fn a_node_whose_name_and_chain_id_disagree_is_not_believed() {
        let mut node = Node::builtin(0, "n", "https://example.invalid");
        node.record_success(
            &claiming("VRSCTEST", "i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV"),
            Duration::from_millis(10),
            &Network::Testnet,
        );

        assert_eq!(
            node.status,
            NodeStatus::Unidentified {
                name: "VRSCTEST".to_string(),
                chain_id: "i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV".to_string(),
            }
        );
        assert_eq!(node.status.label(), "degraded");
        // Deliberately not `Some(Network::Testnet)`. The name is the half that
        // was contradicted, so believing it here would be believing the thing
        // the check just rejected.
        assert_eq!(node.network, None);
    }

    /// A node that sends no id at all has still failed to bear out its name.
    /// Reading the empty string as "there was nothing to check" would leave any
    /// endpoint a one-character way past the check, which is worse than not
    /// having it: it would look like a guard in the source and not be one.
    #[test]
    fn an_empty_chain_id_is_a_mismatch_and_not_an_absence() {
        let mut node = Node::builtin(0, "n", "https://example.invalid");
        node.record_success(
            &claiming("VRSCTEST", ""),
            Duration::from_millis(10),
            &Network::Testnet,
        );

        assert_eq!(
            node.status,
            NodeStatus::Unidentified {
                name: "VRSCTEST".to_string(),
                chain_id: String::new(),
            }
        );
        assert_eq!(node.network, None);
    }

    /// Which of three true things the status gets to say, when all three are
    /// true at once.
    ///
    /// A reply can be behind the chain, from a chain the wallet did not ask
    /// for, and self-contradicting, all together — and the status is one value,
    /// so the order of the branches is what decides which sentence a person
    /// reads. Every other test here puts only one branch in contention, so
    /// without this one the ordering `record_success` argues for above would be
    /// held by prose and nothing else: the three could be rewritten in any
    /// order and the suite would stay green.
    ///
    /// Identity has to win because the other two answers are built out of the
    /// same reply that just contradicted itself. "Catching up — 900 of 1000
    /// blocks" quotes heights from a node with no established identity, and
    /// "this node is on Mainnet" states as fact the very half of the pair that
    /// was contradicted.
    #[test]
    fn a_node_that_is_behind_and_on_another_chain_is_still_reported_as_unidentified() {
        let mut node = Node::builtin(0, "n", "https://example.invalid");
        node.record_success(
            &ChainInfo {
                blocks: 900,
                longest_chain: 1_000,
                ..claiming("VRSC", "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq")
            },
            Duration::from_millis(10),
            &Network::Testnet,
        );

        assert_eq!(
            node.status,
            NodeStatus::Unidentified {
                name: "VRSC".to_string(),
                chain_id: "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq".to_string(),
            }
        );
        assert_eq!(node.network, None);
    }

    /// Every PBaaS chain has to keep working, and this is what keeps it honest.
    ///
    /// A PBaaS chain is not a root chain — vARRR is registered under VRSC, so
    /// its id is not derivable from its own name — and this build pins no id
    /// for it. The id below is arbitrary on purpose: nothing here checks it,
    /// and pinning a real one from memory rather than from that chain's own
    /// node is how a wallet ships a guard that refuses honest endpoints.
    ///
    /// The mechanism is asserted alongside the outcome, because the outcome
    /// alone cannot fail: with the cross-check deleted this node would be
    /// `Online` too. What actually holds every PBaaS chain up is
    /// [`Network::chain_id`] answering `None` for [`Network::Other`], so that
    /// is the line a future pin would have to break here rather than in the
    /// field.
    #[test]
    fn a_pbaas_node_is_still_believed_because_no_id_is_pinned_for_it() {
        let varrr = Network::from_chain_name("vARRR");
        assert_eq!(varrr.chain_id(), None);

        let mut node = Node::builtin(0, "n", "https://example.invalid");
        node.record_success(
            &claiming("vARRR", "iSomethingOnlyThatChainKnows"),
            Duration::from_millis(10),
            &varrr,
        );

        assert_eq!(node.status, NodeStatus::Online);
        assert_eq!(node.network, Some(varrr));
    }

    /// The spelling a node uses is not the spelling a button sends, and the
    /// wallet has to survive the difference.
    ///
    /// `vapi.piratechain.com` answers `"name":"vARRR"`. A wallet set to that
    /// chain by any other capitalisation must still see the node come `Online`:
    /// [`Network::Other`] compares by exact string, so without the
    /// canonicalisation in [`Network::from_chain_name`] this node would be
    /// `WrongNetwork` forever, and vARRR would read no balance and refuse every
    /// spend while looking like a misconfigured endpoint.
    #[test]
    fn a_node_that_shouts_its_own_name_is_still_the_chain_that_was_asked_for() {
        let mut node = Node::builtin(0, "n", "https://example.invalid");
        node.record_success(
            &claiming("vARRR", "iSomethingOnlyThatChainKnows"),
            Duration::from_millis(10),
            &Network::from_chain_name("VARRR"),
        );

        assert_eq!(node.status, NodeStatus::Online);
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

    /// Enabling spending requires the chain's own name, and the check lives
    /// here rather than in the interface.
    ///
    /// Driven on vARRR rather than on VRSC, because a PBaaS chain is where the
    /// word does the most work: it carries real coins, it is not the chain
    /// anybody assumes a money guard is about, and the confirmation is the only
    /// evidence the person knew which of the five they were arming.
    #[test]
    fn the_spending_opt_in_needs_the_chains_own_name() {
        let mut manager = NodeManager::new(
            vec![Node::builtin(0, "n", "https://example.invalid")],
            Network::from_chain_name("vARRR"),
        );

        assert!(!manager.set_allow_spending(true, ""));
        assert!(!manager.set_allow_spending(true, "yes"));
        // The old fixed word, which now means nothing here.
        assert!(!manager.set_allow_spending(true, "mainnet"));
        // And no other chain's name arms this one. This is the property the
        // chain-specific word exists for: the confirmation is evidence about
        // WHICH chain, and nothing else would pin that.
        assert!(!manager.set_allow_spending(true, "VRSC"));
        assert!(!manager.allow_spending());

        assert!(manager.set_allow_spending(true, "vARRR"));
        assert!(manager.allow_spending());

        // Turning it back off needs no confirmation — refusing to spend is
        // never the dangerous direction.
        assert!(manager.set_allow_spending(false, ""));
        assert!(!manager.allow_spending());

        // Case and surrounding space are forgiven. Somebody typing their own
        // chain's name in lowercase has demonstrated everything the word is
        // there to demonstrate.
        assert!(manager.set_allow_spending(true, "  varrr  "));
        assert!(manager.allow_spending());
    }

    /// An empty box must not arm a chain that calls itself nothing.
    ///
    /// `Network::Other(String::new())` is constructible — a node answering with
    /// an empty name produces it — and its `chain_name` is `""`. Comparing the
    /// typed text against that would make the untouched field the correct
    /// answer, on the one chain the wallet knows least about.
    #[test]
    fn a_chain_with_no_name_cannot_be_armed_at_all() {
        let mut manager = NodeManager::new(
            vec![Node::builtin(0, "n", "https://example.invalid")],
            Network::from_chain_name(""),
        );

        assert!(!manager.set_allow_spending(true, ""));
        assert!(!manager.set_allow_spending(true, "   "));
        assert!(!manager.allow_spending());
    }

    /// An arm belongs to the chain it was made on — see the `allow_spending`
    /// field for why one flag can serve five chains only while that holds.
    /// Pinned here because in the running application it is true by
    /// construction, and construction is a thing that gets changed.
    #[test]
    fn changing_chains_disarms_spending() {
        let mut manager = NodeManager::new(
            vec![Node::builtin(0, "n", "https://example.invalid")],
            Network::Mainnet,
        );
        assert!(manager.set_allow_spending(true, "VRSC"));

        manager.set_requested(Network::from_chain_name("vARRR"));
        assert!(!manager.allow_spending(), "the arm followed the wallet");
    }

    /// A permit already issued outlives the switch that authorised it.
    ///
    /// Not a defect and not fixable here: the seal is what makes the guard
    /// unskippable, and a sealed value is still a value — once minted, nothing
    /// in this crate can reach back and revoke it. It is pinned because the
    /// consequence belongs to the callers. `pecu_core` holds signed bytes for a
    /// review that stays on screen as long as somebody leaves it there, and a
    /// permit stored beside them would still authorise a broadcast after the
    /// person turned spending off, or changed chains. So the rule the callers
    /// follow is that a permit is taken at the moment of broadcast and never
    /// carried alongside the bytes; this is the fact that rule exists for.
    #[test]
    fn a_permit_already_issued_is_not_reached_by_turning_spending_off() {
        let mut manager = NodeManager::new(
            vec![Node::builtin(0, "n", "https://example.invalid")],
            Network::Mainnet,
        );
        manager
            .get_mut(0)
            .expect("the node just added")
            .record_success(
                &info(1_000, 1_000, "VRSC"),
                Duration::from_millis(10),
                &Network::Mainnet,
            );
        assert!(manager.set_allow_spending(true, "VRSC"));

        let held = manager.spend_permit().expect("the guard is satisfied");

        assert!(manager.set_allow_spending(false, ""));
        assert!(
            manager.spend_permit().is_err(),
            "the off switch did not shut the gate",
        );
        // And the one taken beforehand is untouched, which is the point.
        assert_eq!(held.network(), &Network::Mainnet);
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
