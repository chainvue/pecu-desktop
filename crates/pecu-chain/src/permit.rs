//! The gate every spend has to pass.

use crate::network::Network;
use crate::node::{Node, NodeStatus};

/// Permission to broadcast, which cannot be forged.
///
/// # Why this is a type and not an `if`
///
/// The private `_seal` field means no other module — and no other crate — can
/// construct one. [`crate::Chain::broadcaster`] is the only way to reach a
/// `Broadcaster`, and it demands one of these. So "did we check whether
/// spending is allowed?" is not a question anyone can forget to ask: without a
/// permit there is no broadcaster, and the only way to get a permit is
/// [`NodeManager::spend_permit`], which runs every check.
///
/// A boolean flag and a runtime `if` would do the same job right up until
/// someone adds a second broadcast path and does not repeat the check.
///
/// [`NodeManager::spend_permit`]: crate::node::NodeManager::spend_permit
#[derive(Clone, Debug)]
pub struct SpendPermit {
    network: Network,
    node_id: u32,
    tip: u32,
    /// Unforgeable outside this module. This is the whole mechanism.
    _seal: (),
}

impl SpendPermit {
    /// Only [`crate::node::NodeManager`] mints these.
    pub(crate) fn issue(network: Network, node_id: u32, tip: u32) -> Self {
        Self {
            network,
            node_id,
            tip,
            _seal: (),
        }
    }

    pub fn network(&self) -> &Network {
        &self.network
    }

    pub fn node_id(&self) -> u32 {
        self.node_id
    }

    /// The height this permit was judged against. Worth recording on anything
    /// that gets signed: coin maturity was decided against this number.
    pub fn tip(&self) -> u32 {
        self.tip
    }
}

/// Why a spend was refused.
///
/// Each of these is a distinct thing to tell a user, which is why they are not
/// one `bool`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SpendRefused {
    #[error("no node is selected")]
    NoNode,

    #[error("the node has not said which chain it is on yet")]
    NetworkUnknown,

    #[error("this node calls itself {name} but reports chain id {chain_id}")]
    Unidentified { name: String, chain_id: String },

    #[error("this node is on {effective}, but the wallet is set to {requested}")]
    NetworkMismatch {
        requested: Network,
        effective: Network,
    },

    /// Carries the chain, because this is the half that reaches the logs and a
    /// refusal reading "spending on mainnet is turned off" while a vARRR spend
    /// is being refused would be a lie. It is also the only thing
    /// `pecu_core::refusal_note` has to name the chain with.
    #[error("spending on {network} is turned off")]
    SpendingNotEnabled { network: Network },

    #[error("the node is still catching up ({blocks} of {longest} blocks)")]
    Syncing { blocks: u32, longest: u32 },

    #[error("the node is not ready to be spent through")]
    NodeNotReady { status: NodeStatus },

    /// The primary offered coins whose absence from the second source is not
    /// explained by the two nodes being at different heights.
    ///
    /// None of the five variants below is raised by [`evaluate`], and none can
    /// be: answering these questions costs a network call, and the whole reason
    /// a [`SpendPermit`] is worth anything is that its only constructor does no
    /// I/O. So corroboration is a runtime check on the prepare/confirm path and
    /// these are carried here for their words, not because the permit covers
    /// them — see [`crate::corroborate`], which says the same at more length.
    ///
    /// They are also **chain-independent**. Once corroboration has been
    /// required for a send, every way it can fail refuses on testnet exactly as
    /// it does on VRSC. [`crate::network::Network::may_be_real_money`] does not
    /// gate any of them.
    ///
    /// The wording is careful not to promise more than the check does. It says
    /// the second source does not have these outputs, which is a fact it
    /// reported; it does not say the primary invented them, which is an
    /// inference from heights the primary itself supplied. See
    /// [`crate::corroborate`] on what a chosen height can and cannot buy.
    #[error("this node offers {count} output(s) that {secondary} does not have")]
    Uncorroborated { count: usize, secondary: String },

    /// The second source has not reached the blocks these coins are in, and
    /// once they were set aside there was nothing left to pay with.
    ///
    /// Not a disagreement — the secondary's own tip says it has not indexed
    /// them yet, and it is close enough behind for that to be credible. Kept
    /// apart from [`SpendRefused::Uncorroborated`] because accusing an honest
    /// pair of the attack is its own harm, and because the remedy is to wait
    /// rather than to change anything.
    #[error("{secondary} has only reached block {tip}, which is behind {count} of these coins")]
    SecondSourceBehind {
        count: usize,
        secondary: String,
        tip: u32,
    },

    /// The mirror of [`SpendRefused::SecondSourceBehind`]: the *primary* is the
    /// node that is behind, and once the coins the second source has already
    /// seen spent were set aside there was nothing left to pay with.
    ///
    /// The likeliest cause is this wallet's own earlier payment. It confirmed,
    /// the second source indexed the block, and the node being spent through
    /// has not caught up and is still offering the coin that payment consumed.
    ///
    /// A separate variant rather than reusing the one above, because the two
    /// tips point in opposite directions and so do the remedies: there the
    /// second source has to catch up, here the node in use does. Telling
    /// somebody to wait for a node that is already ahead is advice that never
    /// comes true.
    #[error(
        "{secondary} has reached block {tip} and has already seen {count} of these coins spent"
    )]
    SecondSourceAhead {
        count: usize,
        secondary: String,
        tip: u32,
    },

    /// Nothing exists that could hold the primary to anything.
    ///
    /// The narrow case: the active node is one the user added and the shipped
    /// endpoint for this chain is itself answering about another chain, so
    /// there is no configured endpoint whose answer would mean anything. The
    /// remedy is the node list, and it is not "add a second node" — one is
    /// already there.
    #[error("nothing configured can corroborate what {primary} reports")]
    NoSecondSource { primary: String },

    /// A second source exists and could not answer.
    ///
    /// A timeout, a dial failure, or a filtering proxy answering `-32601` to
    /// `getaddressutxos`. Distinct from [`SpendRefused::NoSecondSource`]
    /// because the sentences have opposite remedies: telling somebody to look
    /// at their node list when the node list is fine sends them after the wrong
    /// problem, which [`crate::corroborate`] argues at the site. It names the
    /// **secondary**, not the primary, for the same reason.
    #[error("{secondary} could not be asked about these coins")]
    SecondSourceSilent { secondary: String },

    /// The bytes on the review were built before the active node changed.
    ///
    /// Reached only at the broadcast gate: a failover, or somebody switching
    /// endpoints, between pressing Review and pressing Send. The signed bytes
    /// are not wrong, but nothing has held them against the node list as it is
    /// now, and the only remedy is to build the payment again — which is why
    /// this is the one refusal that discards the prepared transaction instead
    /// of leaving it for a second press of the same button.
    #[error("the active node changed after this payment was prepared")]
    PreparedBeforeNodeChange,
}

/// Run every check and, if they all pass, mint a permit.
///
/// Four independent gates, all of which must pass:
///
/// 1. **A believable identity.** A node whose reported name and reported chain
///    id do not go together has said nothing this gate can use — see
///    [`crate::network::Network`] for what that pair is and is not worth.
/// 2. **Node readiness.** Not syncing, not degraded. This one is easy to
///    dismiss as pedantry and is not: `verus_flows::spendable` decides coin
///    maturity against the tip, so a stale tip builds a transaction the network
///    rejects — after the work is done.
/// 3. **Network agreement.** The chain the user chose must be the chain the
///    node reports. A testnet-configured wallet pointed at a mainnet node is
///    refused rather than allowed to sign something real.
/// 4. **The spending opt-in.** Off by default on every chain except testnet —
///    see [`Network::may_be_real_money`] for why the exception is that narrow
///    and not "everything but VRSC". Enabling it requires typing the chain's
///    own name, and that check happens in the core rather than in the UI, since
///    a confirmation the UI could skip would be decoration. It is **not**
///    persisted: it is a field on `NodeManager`, so it is off again at the next
///    start and off again after a chain switch, which rebuilds the manager.
///
/// Listed in the order `evaluate` asks them, which is also the order of most
/// specific first: a person on the wrong chain is better told that than told to
/// arm spending on a chain they did not mean to be on.
pub(crate) fn evaluate(
    active: Option<&Node>,
    requested: &Network,
    allow_spending: bool,
) -> Result<SpendPermit, SpendRefused> {
    let node = active.ok_or(SpendRefused::NoNode)?;

    // Asked before the network, because a node whose two claims disagree has no
    // network for the next line to read: `record_success` will not pick one of
    // two statements that cannot both be true. Reached in that order the
    // refusal would be `NetworkUnknown`, whose words are "the node has not said
    // which chain it is on yet" — true of a cold start, and misleading here,
    // where the node has answered and will go on contradicting itself for as
    // long as anybody waits.
    if let NodeStatus::Unidentified { name, chain_id } = &node.status {
        return Err(SpendRefused::Unidentified {
            name: name.clone(),
            chain_id: chain_id.clone(),
        });
    }

    let effective = node.network.clone().ok_or(SpendRefused::NetworkUnknown)?;

    match &node.status {
        NodeStatus::Online => {}
        NodeStatus::Syncing { blocks, longest } => {
            return Err(SpendRefused::Syncing {
                blocks: *blocks,
                longest: *longest,
            })
        }
        other => {
            return Err(SpendRefused::NodeNotReady {
                status: other.clone(),
            })
        }
    }

    if *requested != effective {
        return Err(SpendRefused::NetworkMismatch {
            requested: requested.clone(),
            effective,
        });
    }

    // Keyed on what the NODE reports, never on what the user asked for:
    // `requested` is the weaker of the two claims. The mismatch check above
    // makes them equal in the success path, so this reads as a distinction
    // without a difference until somebody removes that check.
    if effective.may_be_real_money() && !allow_spending {
        return Err(SpendRefused::SpendingNotEnabled { network: effective });
    }

    Ok(SpendPermit::issue(
        effective,
        node.id,
        node.tip.unwrap_or_default(),
    ))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use verus_sdk::network::ChainInfo;

    use super::*;

    fn node(network: Network, status: NodeStatus) -> Node {
        Node {
            network: Some(network),
            status,
            tip: Some(1_000),
            ..Node::builtin(0, "test", "https://example.invalid")
        }
    }

    /// The highest-value test in the repository: a wallet that has not been
    /// told it may spend real money must not spend real money.
    ///
    /// It needs the two siblings below to mean what it says: on its own this
    /// pins the claim for VRSC, and the sentence above is about every chain
    /// where a signature moves value.
    #[test]
    fn mainnet_is_refused_without_the_opt_in() {
        let mainnet = node(Network::Mainnet, NodeStatus::Online);
        assert_eq!(
            evaluate(Some(&mainnet), &Network::Mainnet, false).unwrap_err(),
            SpendRefused::SpendingNotEnabled {
                network: Network::Mainnet
            }
        );
    }

    /// The refusal this whole change exists for.
    ///
    /// vARRR carries real coins and this build ships an endpoint for it, so it
    /// is the easiest real-money spend in the wallet to reach: it is
    /// [`Network::Other`], the variant a guard written around "is this VRSC"
    /// answers `false` for.
    #[test]
    fn varrr_is_refused_without_the_opt_in() {
        let varrr = Network::from_chain_name("vARRR");
        let node = node(varrr.clone(), NodeStatus::Online);
        assert_eq!(
            evaluate(Some(&node), &varrr, false).unwrap_err(),
            SpendRefused::SpendingNotEnabled {
                network: varrr.clone()
            }
        );
    }

    /// A chain nobody here can vouch for is the case where the confirmation is
    /// worth the most, not the least.
    #[test]
    fn an_unknown_chain_is_refused_without_the_opt_in() {
        let unknown = Network::Other("SOMEPBAAS".to_string());
        let node = node(unknown.clone(), NodeStatus::Online);
        assert_eq!(
            evaluate(Some(&node), &unknown, false).unwrap_err(),
            SpendRefused::SpendingNotEnabled {
                network: unknown.clone()
            }
        );
    }

    #[test]
    fn mainnet_is_allowed_once_the_opt_in_is_set() {
        let mainnet = node(Network::Mainnet, NodeStatus::Online);
        let permit = evaluate(Some(&mainnet), &Network::Mainnet, true).expect("permit");
        assert_eq!(permit.network(), &Network::Mainnet);
        assert_eq!(permit.tip(), 1_000);
    }

    /// Testnet never needs the opt-in — the guard exists for real money, and
    /// testnet is now the only chain that is free of it. That used to be an
    /// aside; it is the definition of the rule. See
    /// [`Network::may_be_real_money`].
    #[test]
    fn testnet_needs_no_opt_in() {
        let testnet = node(Network::Testnet, NodeStatus::Online);
        assert!(evaluate(Some(&testnet), &Network::Testnet, false).is_ok());
    }

    /// A testnet-configured wallet pointed at a mainnet node. Refused before
    /// the spending opt-in is even consulted, because the mismatch is the more
    /// specific problem and the more useful message.
    #[test]
    fn a_network_mismatch_is_refused_even_with_the_opt_in_set() {
        let mainnet = node(Network::Mainnet, NodeStatus::Online);
        assert_eq!(
            evaluate(Some(&mainnet), &Network::Testnet, true).unwrap_err(),
            SpendRefused::NetworkMismatch {
                requested: Network::Testnet,
                effective: Network::Mainnet,
            }
        );
    }

    /// `spendable` judges coin maturity against the tip. A syncing node has the
    /// wrong tip, so the transaction would be built correctly against a stale
    /// world and rejected by the network.
    #[test]
    fn a_syncing_node_cannot_be_spent_through() {
        let syncing = node(
            Network::Testnet,
            NodeStatus::Syncing {
                blocks: 900,
                longest: 1_000,
            },
        );
        assert_eq!(
            evaluate(Some(&syncing), &Network::Testnet, false).unwrap_err(),
            SpendRefused::Syncing {
                blocks: 900,
                longest: 1_000,
            }
        );
    }

    #[test]
    fn a_node_that_has_not_answered_yet_cannot_be_spent_through() {
        let silent = Node::builtin(0, "test", "https://example.invalid");
        assert_eq!(
            evaluate(Some(&silent), &Network::Testnet, false).unwrap_err(),
            SpendRefused::NetworkUnknown
        );
        assert_eq!(
            evaluate(None, &Network::Testnet, false).unwrap_err(),
            SpendRefused::NoNode
        );
    }

    /// The failure this repository calls its highest-value test, driven through
    /// the real derivation instead of a `Node` literal.
    ///
    /// A node serving mainnet while calling itself VRSCTEST never reaches
    /// [`SpendRefused::SpendingNotEnabled`], because that guard keys on the
    /// chain the node said it was on. Whatever the wallet then signed would be
    /// consensus-valid on mainnet, so the money would be real and gone. The
    /// name/chain-id cross-check in `record_success` stops such a node before
    /// the network questions are asked at all.
    ///
    /// It is worth being exact about what this pins, because it is narrower
    /// than that headline. What it pins is that a node whose own two claims
    /// disagree cannot be spent through — the relabelling-proxy and broken-shim
    /// case. A hostile endpoint is not covered and cannot be from here; see
    /// [`crate::network::Network`] for why.
    ///
    /// The refusal names both claims, and is deliberately not
    /// [`SpendRefused::NetworkUnknown`]: there is no chain to switch to and
    /// nothing to wait for, so which two statements failed to agree is the only
    /// thing a person can act on — and "has not said which chain it is on
    /// **yet**" would promise that waiting helps, which here it never does.
    #[test]
    fn a_node_whose_two_claims_disagree_cannot_be_spent_through() {
        let mut lying = Node::builtin(0, "test", "https://example.invalid");
        lying.record_success(
            &ChainInfo {
                name: "VRSCTEST".to_string(),
                chain_id: "i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV".to_string(),
                blocks: 1_000,
                longest_chain: 1_000,
                version: "test".to_string(),
            },
            Duration::from_millis(10),
            &Network::Testnet,
        );

        assert_eq!(
            lying.status,
            NodeStatus::Unidentified {
                name: "VRSCTEST".to_string(),
                chain_id: "i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV".to_string(),
            }
        );
        assert_eq!(
            evaluate(Some(&lying), &Network::Testnet, false).unwrap_err(),
            SpendRefused::Unidentified {
                name: "VRSCTEST".to_string(),
                chain_id: "i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV".to_string(),
            }
        );
    }

    /// A permit cannot be built by anyone outside this module, which is what
    /// makes every check above unskippable.
    ///
    /// ```compile_fail
    /// use pecu_chain::{Network, SpendPermit};
    /// let _ = SpendPermit { network: Network::Testnet, node_id: 0, tip: 0, _seal: () };
    /// ```
    #[test]
    fn a_permit_cannot_be_forged() {
        // The assertion is the compile_fail doc-test above.
    }
}
