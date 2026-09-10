//! Holding one node's coins against another's.
//!
//! # What this is for
//!
//! [`crate::network::Network`] says why a node's own account of which chain it
//! is on cannot be checked from that same node: both halves of the name and the
//! chain id arrive in one `getinfo` reply, and a source cannot corroborate
//! itself. The attack that leaves open is a node that calls itself VRSCTEST and
//! answers `getaddressutxos` with **mainnet** outputs. Every check this wallet
//! makes about identity passes; the wallet then builds, signs and broadcasts a
//! transaction that is consensus-valid on mainnet, because the two chains share
//! address version bytes and there is no branch-id separation between them.
//!
//! The only thing that reaches a node lying selectively about the coins is
//! asking a *second* node about the same address. That is what this module
//! does, once per send, for the one address the payment is funded from.
//!
//! # What it covers, and what it does not
//!
//! The **transparent funding set of a spend**, on the two routes the send
//! screen offers that read one: a transparent payment and a `t→z` shield. A
//! shield is in scope precisely because it has no input notes — `shield::plan`
//! funds it from `getaddressutxos` at the transparent address, byte for byte
//! the set a transparent payment reads, so leaving it out would have left the
//! attack open behind one character in the recipient field.
//!
//! `z→z` and `z→t` are out of scope and for a different reason: their inputs
//! are notes, and the notes, the witnesses and the anchor all come from one
//! lightwalletd with no second one to ask.
//!
//! Every other spend this wallet can sign — a conversion, an identity update, a
//! name registration, a currency launch, a resend — still funds from the
//! primary's word alone. Those sites carry a one-line pointer back here saying
//! so. Making the compiler enumerate them needs a second unforgeable token
//! beside [`crate::SpendPermit`] on `Chain::broadcaster`, which is the change
//! this one is deliberately smaller than.
//!
//! # The rule, and why it is not a set comparison
//!
//! **One-directional outpoint membership.** Only the outpoints the primary
//! offers are looked for in the secondary's answer. An outpoint only the
//! *secondary* has is a coin the primary has not indexed yet, or one it has
//! already seen spent — refusing on that would be refusing because the other
//! node is honest and a block ahead. The issue this closes phrases the rule
//! symmetrically ("refusing an outpoint only one of them has"); implemented as
//! written it would fail on the commonest honest state two nodes can be in.
//!
//! **Outpoints only** — `(txid, vout)`. Not `AddressUtxo` equality: that
//! derives over `is_spendable`, which the SDK calls *"the node's own opinion of
//! spendability. Recorded, not relied on"*, and over vector order, and two
//! honest nodes are obliged to agree on neither. Value and script are not
//! compared because they need no comparison — the sighash commits to both, so a
//! node misreporting either produces a transaction the network rejects. That is
//! the half of the SDK's argument for declining to corroborate a UTXO set which
//! does hold. The half that does not is the other one: it reasons about a node
//! *omitting* or *misvaluing* a coin, and this attack *adds* one, whose value
//! and script are perfectly true — about the wrong chain.
//!
//! **Not the tips as a comparison, but both tips as evidence.** Two healthy
//! nodes disagree about the tip most of the time and neither tip is what a
//! signature commits to, so the tips are never compared for equality. They are
//! read to sort a *withheld* coin into one of two piles: one the secondary has
//! not looked for yet, and one it has looked for and does not have. Only the
//! second is a disagreement, and the difference is what keeps an honest lagging
//! node from being accused of the attack — which on a single-coin address, the
//! commonest wallet shape there is, would otherwise happen every time a payment
//! confirmed into a block the second node had not reached.
//!
//! Three questions decide it, in this order, and it takes **both** tips to ask
//! them:
//!
//! 1. Is the coin's block above the *primary's own* tip? Then the primary is
//!    contradicting itself. `getaddressutxos` is confirmed-only, so no node
//!    reports an output from a block it has not got. Divergence, and it is the
//!    one rule the answering node cannot satisfy by choosing a number, because
//!    both numbers are its own.
//! 2. Is it above the *secondary's* tip? Then the secondary has not indexed
//!    that block yet — the commonest honest state two nodes are ever in — but
//!    only within [`CREDIBLE_LAG`]. Further above than that and the "lag" would
//!    have to be weeks; the shape it actually has is a primary on a different
//!    and much longer chain. Divergence.
//! 3. Otherwise the secondary has indexed the block and does not have the coin.
//!    A disagreement — *unless* the secondary is the one that is ahead, in
//!    which case the likelier reading is that the coin has been spent in a
//!    block the primary has not seen yet, by this wallet's own key, and the two
//!    are simply out of step in the other direction.
//!
//! Those three sort the disagreement. They do not settle it, because every
//! height in them is a number the answering node chose — the next section is
//! about that. So a verdict that ends up *excusing* every missing coin is held
//! to one more question, and it is not about heights at all:
//!
//! 4. Do the two nodes name the **same block** at a height both say they have
//!    reached? The lower of the two tips is such a height, and a hash is not a
//!    field either node gets to fill in: it is the block. Two nodes on one
//!    chain agree there; two nodes on different chains cannot, however they
//!    number things. If they disagree, the excuse rules 1 to 3 just granted was
//!    an excuse about somebody else's chain, and the verdict is
//!    [`Corroboration::Diverged`] whatever the heights said.
//!
//! Rule 2 is why the scenario in the issue is caught at all. A node serving
//! VRSC outputs while calling itself VRSCTEST offers coins from around block
//! 4,207,412 against a VRSCTEST second source whose tip is around 1,203,115:
//! three million blocks above it, so the withheld coins are not credibly
//! unindexed and the verdict is [`Corroboration::Diverged`]. Written as "above
//! the secondary's tip is lag" with no bound — which is how this module shipped
//! first — the same reply reads as a node that is merely behind, and the wallet
//! tells somebody under attack to try again later.
//!
//! # The heights are the answering node's own field
//!
//! Every height in rules 1 to 3 arrives in the primary's `getaddressutxos`
//! reply, and so does the primary's tip. A node willing to invent an outpoint
//! will give it whatever height suits it: report a tip just above the
//! secondary's and put the invented coins in the blocks between the two, and
//! rule 2 says lag; report a tip *below* the secondary's and rule 3 says out of
//! step. Both shapes were reachable, and the second one is the defect issue #45
//! was opened about — a primary forwarding a **shorter** chain's answers, every
//! invented coin of which sits below the second source's tip, rendering as
//! "nothing is wrong, try again once the node you are using catches up".
//!
//! What a chosen height buys is the softer **sentence**, and the sentence is
//! not cosmetic: `Diverged` refuses a payment outright and `OutOfStep`
//! completes it from whatever survived, so wherever a genuine coin survives the
//! verdict decides whether money moves. What a chosen height cannot buy, and
//! never could, is a wider **allowed set**. Every verdict withholds exactly the
//! outpoints the secondary did not name, and [`Corroborated`] is built from what
//! the secondary recognised and from nothing else, so an invented outpoint is
//! unspendable under all of them.
//!
//! Rule 4 is what takes the sentence away too, and it is why rules 1 to 3 are
//! no longer the last thing standing between the attack and a reassuring
//! caption. `getblockhash` at the lower of the two tips is a question about a
//! height both nodes claim, whose answer neither of them chooses, so the whole
//! chosen-height class closes at once and in both directions. Two residual
//! limits, stated rather than left to be found:
//!
//! * **A reorganisation deeper than the gap between the two tips** would make
//!   two honest nodes name different blocks at the shared height. At Verus's one
//!   block a minute that is not a reorg, and it is the same judgement already
//!   written into [`CREDIBLE_LAG`].
//! * **A node that will not answer `getblockhash`** is neither agreed with nor
//!   accused. The check did not happen, and that is
//!   [`Corroboration::Unavailable`] when it is the second source and
//!   [`Corroboration::PrimarySilent`] when it is the funding node — two states
//!   rather than one, because the remedy is a different machine. Silence is not
//!   a pass anywhere else in this module and it is not one here.
//!
//! **Not the mempool.** Legitimately different between two healthy nodes, and
//! `getaddressutxos` does not report it in the first place.
//!
//! # This is not a type-level guard, and cannot be
//!
//! [`crate::SpendPermit`] is unforgeable because `NodeManager::spend_permit` is
//! its only constructor and that constructor does no I/O — it reads what a
//! probe already recorded. Corroboration is a network call. So it cannot happen
//! inside the permit, and nothing here widens what a permit means: a permit
//! still says only that the node is ready, identified, on the requested chain
//! and armed. Corroboration is a **runtime check on the prepare/confirm path**,
//! and a reader who assumes the permit covers it will be wrong.

use std::collections::HashSet;

use verus_sdk::money::{Amount, Txid};
use verus_sdk::network::{
    AddressBalance, AddressDelta, AddressUtxo, ChainInfo, ChainReader, ConversionEstimate,
    CurrencyConverter, CurrencyPolicy, CurrencyStateAt, CurrencySummary, IdentityAtAddress,
    IdentityContent, IdentityRecord, MempoolDelta, OfferListing, RpcError,
};

/// One unspent output, identified the only two ways that cannot be restated.
pub type Outpoint = (Txid, u32);

/// How far above a second source's tip a withheld coin may sit and still be
/// read as that source not having got there yet.
///
/// A week of blocks, at Verus's one a minute. The number is a judgement and the
/// judgement is asymmetric on purpose, so it is worth saying which way and why.
///
/// Too tight and an honest second source that has genuinely fallen behind —
/// a shipped endpoint resyncing, a machine that was off — is accused of serving
/// another chain the first time the wallet holds a coin newer than the gap.
/// That is a false accusation with a remedy that does not apply, aimed at the
/// one endpoint this build asks people to trust.
///
/// Too loose and a primary on a longer chain is excused. But note what a
/// tighter bound would actually buy against a deliberate liar: nothing, because
/// the same reply carries the coins' heights *and* the primary's own tip, so a
/// node that wanted the softer verdict would report a tip a few blocks above
/// the secondary's and put its invented coins in between. That is the
/// chosen-height dodge, and what closes it is rule 4 in the module docs — the
/// block hash at the lower of the two tips, which neither node fills in — and
/// not a tighter number here. The bound is still worth having because it is
/// free and it catches the version of this attack that exists, a proxy
/// forwarding a real mainnet node's answers unaltered, where the two chains'
/// heights are three million blocks apart. Against that, a week and a day are
/// the same check, and a week is the one that never accuses an honest node.
pub const CREDIBLE_LAG: u32 = 10_080;

/// What a second source said about the coins the primary offered.
#[derive(Debug)]
pub enum Corroboration {
    /// Every outpoint the primary offered is one the secondary also has.
    Agreed { checked: usize },
    /// The secondary is missing coins, and the two nodes being at different
    /// heights explains every one of them.
    ///
    /// Both directions land here, which is why this is not called `Lagging`.
    /// Usually the secondary is behind: `getaddressutxos` is confirmed-only, so
    /// an output in a block one node has not indexed yet is missing from its
    /// answer, and that node's own tip says so. Occasionally the secondary is
    /// *ahead* and the coin is one this wallet has already spent, whose
    /// spending transaction the secondary has seen confirm and the primary has
    /// not. The two want different sentences — one says wait for the second
    /// node, the other says wait for the first — and the caller tells them
    /// apart by comparing the two tips carried here.
    ///
    /// The safe response either way is to spend the subset both nodes have and
    /// say how much was left out: refusing outright would punish the commonest
    /// honest state, and filtering silently would let a send-all quietly move
    /// less than the balance on screen.
    ///
    /// `kept` can be zero. A wallet whose only coin confirmed a minute ago
    /// against a second node a block behind lands exactly there, and it is
    /// still a difference of height rather than of chain — which is the whole
    /// reason this verdict is decided by the heights and not by whether
    /// anything survived.
    OutOfStep {
        withheld: Vec<Outpoint>,
        kept: usize,
        /// The secondary's tip, which is half the evidence for this verdict and
        /// the figure that makes a sentence about it actionable.
        secondary_tip: u32,
        /// The primary's tip, as the caller read it. The other half: which of
        /// the two is ahead is what decides which sentence this verdict gets.
        primary_tip: u32,
    },
    /// The missing coins are not explained by the two nodes being at different
    /// heights.
    ///
    /// This is the shape of the attack. Two chains' UTXO sets for one address
    /// are disjoint, and a node serving mainnet outputs while calling itself
    /// testnet offers coins from blocks around 4.2 million against a testnet
    /// second source that has reached about 1.2 million — three million blocks
    /// of "lag" that no honest pair has. A node offering a coin from a block
    /// above *its own* tip lands here too, and that one it cannot talk its way
    /// out of, because both numbers came from it.
    ///
    /// Kept apart from [`Corroboration::OutOfStep`] because the two have
    /// different remedies, and apart from a filter because filtering to the
    /// empty set would surface as "not enough funds", which is the least useful
    /// sentence available for the one case it would be describing.
    ///
    /// Reached whatever `kept` is. A hostile endpoint that mixes one real coin
    /// in with fifty invented ones is not a node one block behind, and letting
    /// a non-empty `kept` downgrade it to a caption about lag would render the
    /// one signal that the active endpoint is lying as routine.
    Diverged {
        /// The withheld outpoints the heights do not account for. Withheld
        /// coins that *are* accounted for are not counted here, because the
        /// number is going into a sentence accusing somebody's node of serving
        /// another chain.
        ///
        /// When rule 4 is what reached this verdict — the two nodes named
        /// different blocks at a height both claim — that is every withheld
        /// coin, and the count is not a softening of the accusation but the
        /// whole of it: the heights that excused those coins were heights on
        /// another chain, so they accounted for nothing.
        unexplained: Vec<Outpoint>,
        kept: usize,
    },
    /// The secondary could not answer.
    ///
    /// **Not a pass.** The SDK's own second source puts it plainly: *"a source
    /// that cannot answer is a failure, not a pass. The point is a corroborated
    /// answer; an uncorroborated one silently substituted would make the whole
    /// thing decorative the first time a node went down."* The caller decides
    /// what to do with that, and the message it shows has to distinguish "this
    /// endpoint cannot answer the question" from "these two nodes disagree", or
    /// somebody will go and chase the wrong problem.
    Unavailable { reason: RpcError },
    /// The **primary** could not answer the one question this module puts to
    /// it.
    ///
    /// Rule 4 asks both nodes to name the block at the lower of the two tips,
    /// and that is the only thing read from the funding node here — everything
    /// else about it arrived in the reply the caller already holds. The height
    /// asked about is at or below the tip that node itself just reported, so a
    /// node that cannot name it has stopped answering rather than formed an
    /// opinion about a chain, and this is not an accusation.
    ///
    /// Kept apart from [`Corroboration::Unavailable`] because the two have
    /// different remedies and name different machines. "The second source could
    /// not be asked" would send somebody to check an endpoint that is answering
    /// perfectly, and a refusal naming the wrong server is worse than one naming
    /// none. A caller already has a way to report a funding node that will not
    /// answer — it is the same one every other read on the send path uses — and
    /// this variant exists so it can.
    PrimarySilent { reason: RpcError },
}

/// Ask `secondary` which of `address`'s outputs it also has.
///
/// `offered` is what the **primary** answered for the same address, and
/// `primary_tip` is the height it answered from. Both are passed in rather than
/// read here — the caller already has them, and asking again would be comparing
/// two different moments.
///
/// The primary reader itself is needed for exactly one question: rule 4, the
/// block hash at a height both nodes claim to have reached. That one cannot be
/// hoisted into the caller, because the height it asks about is the lower of the
/// two tips and the secondary's tip is not known until this function has read
/// it. Nothing else is asked of the primary, and a funding node that will not
/// answer it lands in [`Corroboration::PrimarySilent`] rather than in a refusal
/// naming the other endpoint.
///
/// # Why the caller supplies the primary's tip, and what it costs
///
/// Because the verdict needs it and there is nowhere free to get it. Two of the
/// three rules in the module docs are about it: a coin above the primary's own
/// tip is a node contradicting itself, and which of the two nodes is ahead is
/// what separates "the second one has not got there" from "the second one has
/// seen this coin spent".
///
/// `verus_flows::funding::spendable` reads a tip and hands it back on
/// `Funding::tip`, so the obvious move is to reuse that one. It does not fit:
/// `spendable` runs *inside* the build, after this answer has already been
/// turned into the filtered reader the build is handed, and the whole point of
/// the ordering is that nothing selects a coin before it has been corroborated.
/// Hoisting `spendable` above this would mean running it twice — it costs three
/// requests plus one per young coinbase — or moving corroboration below
/// selection, which is the window this design exists to close.
///
/// So it is one `getblockcount` to the primary, on the send path, and only when
/// a second source was required in the first place. The stale alternative was
/// available and rejected: [`crate::SpendPermit`] carries the tip the last probe
/// saw, and a tip a probe interval old, read low, would put honest coins above
/// "the primary's own tip" and manufacture the accusation this rule exists to
/// make possible.
///
/// Two requests to the secondary whenever there is anything to check: its tip,
/// then its coins, in that order. The tip used to be read last and only when the
/// two answers differed, which bought a round trip back on the path where the
/// two nodes agree — and had a block-crossing race in it. A coin that confirms
/// between the two reads is absent from an answer composed before the block and
/// sits below a tip read after it, so the rules see "indexed, and does not have
/// it" and accuse a node that did nothing wrong. One round trip out of a
/// sixty-second block, and it fails closed and self-clears on retry, which is
/// still not worth a false accusation. Read first, the tip is a lower bound on
/// what the answer covers, and the same coin falls into rule 2.
///
/// One more request to each node on top of that, and only where the heights have
/// excused every missing coin: rule 4 asks both to name the block at the lower
/// of the two tips. On the ordinary send, where the second source recognises
/// everything offered, neither of those is made.
///
/// An empty `offered` costs no request at all. There is nothing to hold the
/// primary to, the send is about to fail for want of coins whatever this said,
/// and a second operator learns nothing about an address this wallet is not
/// going to spend from.
pub fn against(
    primary: &impl ChainReader,
    secondary: &impl ChainReader,
    address: &str,
    offered: &[AddressUtxo],
    primary_tip: u32,
) -> Corroboration {
    if offered.is_empty() {
        return Corroboration::Agreed { checked: 0 };
    }

    // The tip first and the coins after, so that the tip is a lower bound on
    // what the coin answer covers rather than a figure from after it. See the
    // note on the ordering above.
    let secondary_tip = match secondary.block_count() {
        Ok(tip) => tip,
        Err(reason) => return Corroboration::Unavailable { reason },
    };

    let held: HashSet<Outpoint> = match secondary.address_utxos(&[address]) {
        Ok(found) => found
            .into_iter()
            .map(|utxo| (utxo.utxo.txid, utxo.utxo.vout))
            .collect(),
        Err(reason) => return Corroboration::Unavailable { reason },
    };

    let missing: Vec<&AddressUtxo> = offered
        .iter()
        .filter(|utxo| !held.contains(&(utxo.utxo.txid, utxo.utxo.vout)))
        .collect();
    let kept = offered.len() - missing.len();
    if missing.is_empty() {
        return Corroboration::Agreed { checked: kept };
    }

    let unexplained: Vec<Outpoint> = missing
        .iter()
        .filter(|utxo| !out_of_step(utxo.height, primary_tip, secondary_tip))
        .map(|utxo| (utxo.utxo.txid, utxo.utxo.vout))
        .collect();
    let withheld: Vec<Outpoint> = missing
        .iter()
        .map(|utxo| (utxo.utxo.txid, utxo.utxo.vout))
        .collect();

    if !unexplained.is_empty() {
        return Corroboration::Diverged { unexplained, kept };
    }

    // The heights excused every missing coin, and every height in them is the
    // primary's own. Rule 4, and only here: before the wallet says nothing is
    // wrong, make both nodes name the same block.
    match same_chain(primary, secondary, primary_tip.min(secondary_tip)) {
        SharedBlock::Agreed => Corroboration::OutOfStep {
            withheld,
            kept,
            secondary_tip,
            primary_tip,
        },
        // Whatever the heights said about these coins, they said it about
        // another chain — so they accounted for none of them, and every withheld
        // coin is unexplained.
        SharedBlock::Differ => Corroboration::Diverged {
            unexplained: withheld,
            kept,
        },
        SharedBlock::PrimarySilent(reason) => Corroboration::PrimarySilent { reason },
        SharedBlock::SecondarySilent(reason) => Corroboration::Unavailable { reason },
    }
}

/// What two nodes said when asked to name the same block.
enum SharedBlock {
    /// Both named it, and named the same one. One chain.
    Agreed,
    /// Both named it and named different ones. Two chains, and no arrangement
    /// of tips or coin heights makes that anything else.
    Differ,
    /// The funding node would not say.
    PrimarySilent(RpcError),
    /// The second source would not say.
    SecondarySilent(RpcError),
}

/// Whether two nodes name the same block at `height`.
///
/// Rule 4 from the module docs, and the only question this module puts to the
/// primary. It is what bounds rule 3's deliberately unbounded branch, and it is
/// the reason a node reporting a tip below the second source's no longer buys
/// itself a caption about being behind.
///
/// `height` has to be one both nodes claim to have reached, which is why
/// [`against`] passes the lower of the two tips and nothing else: `getblockhash`
/// above a node's own tip is an error on a real daemon, so a height either of
/// them has not got would measure which of two requests failed rather than which
/// chain anybody is on. The lower tip is also the deepest shared height
/// available, which is what makes it unreachable by a primary choosing numbers —
/// inflating its own tip moves the height down onto the secondary's side, not up
/// out of reach.
///
/// Compared case-insensitively because the hash is hex and the casing is a
/// rendering. `verusd` answers lowercase; accusing a proxy that upper-cased it
/// of serving another chain would be a false accusation over presentation.
fn same_chain(
    primary: &impl ChainReader,
    secondary: &impl ChainReader,
    height: u32,
) -> SharedBlock {
    let ours = match primary.block_hash(height) {
        Ok(hash) => hash,
        Err(reason) => return SharedBlock::PrimarySilent(reason),
    };
    let theirs = match secondary.block_hash(height) {
        Ok(hash) => hash,
        Err(reason) => return SharedBlock::SecondarySilent(reason),
    };
    if ours.eq_ignore_ascii_case(&theirs) {
        SharedBlock::Agreed
    } else {
        SharedBlock::Differ
    }
}

/// Whether the two nodes being at different heights accounts for one missing
/// coin.
///
/// The three rules from the module docs, in the order they are argued there.
/// `false` means the heights do not explain it, which is the accusation.
fn out_of_step(height: u32, primary_tip: u32, secondary_tip: u32) -> bool {
    if height > primary_tip {
        // The primary is contradicting itself: `getaddressutxos` is
        // confirmed-only, so this is an output from a block it says it has not
        // got. Both numbers are its own, which is what makes this the one rule
        // a chosen height cannot get around.
        return false;
    }
    if height > secondary_tip {
        // The secondary has not indexed that block — credible, up to a point,
        // and [`CREDIBLE_LAG`] argues where the point is.
        return height - secondary_tip <= CREDIBLE_LAG;
    }
    // The secondary has indexed the block and does not have the coin. That is a
    // disagreement, unless the secondary is the node that is ahead: then the
    // likelier reading is a coin this wallet already spent, whose spending
    // transaction confirmed into a block the primary has not reached.
    //
    // Deliberately unbounded, where the branch above is bounded — and the
    // comment that used to be here said the unboundedness guarded nothing,
    // "because only the sentence changes". That is false, and it is issue #45.
    // No invented coin becomes spendable either way, true: a coin the secondary
    // does not have is withheld from the build under both answers. But
    // `Diverged` refuses the payment outright and `OutOfStep` completes it from
    // whatever survived, so wherever a genuine coin survived this branch decides
    // whether money moves — and it decided it in favour of any primary that had
    // reported a tip below the secondary's, which is the shape of a node
    // forwarding a shorter chain.
    //
    // Clamping the gap here is the wrong repair, and issue #45 says why: it
    // turns an honestly resyncing node more than a week behind, still offering a
    // coin the wallet has since spent elsewhere, into "this node is serving
    // another chain" — the exact false accusation CREDIBLE_LAG exists to
    // prevent, aimed at the most careful user there is. What bounds this branch
    // instead is rule 4, one level up in `against`: an excuse made of heights
    // stands only while the two nodes name the same block at a height both have,
    // and a primary on a shorter chain does not.
    secondary_tip > primary_tip
}

/// The outpoints of `offered` that `secondary` also has.
///
/// The set a build may fund from. Separate from [`against`] so the caller
/// reports and filters from one answer rather than asking twice.
pub fn agreed_outpoints(offered: &[AddressUtxo], withheld: &[Outpoint]) -> HashSet<Outpoint> {
    // A set rather than a scan of the slice per coin: the same line, and it
    // stops being quadratic on the wallet shape that reaches it — a swept
    // address with hundreds of small outputs against a second node a block
    // behind.
    let withheld: HashSet<Outpoint> = withheld.iter().copied().collect();
    offered
        .iter()
        .map(|utxo| (utxo.utxo.txid, utxo.utxo.vout))
        .filter(|outpoint| !withheld.contains(outpoint))
        .collect()
}

/// A reader that can only offer coins a second node has confirmed exist.
///
/// # Why a filtering reader rather than a check after the build
///
/// Because a check after the build has a window in it. Coin selection would
/// have already chosen its inputs, the transaction would already be signed, and
/// the check could only refuse the whole thing — including in the ordinary case
/// where one coin of six is simply newer than the second node. Filtering the
/// source instead means the signed transaction is corroborated **by
/// construction**: the builder is handed a reader that is incapable of offering
/// an uncorroborated outpoint, so there is no order of operations in which one
/// gets selected.
///
/// It wraps the primary rather than replacing it. Every other question — the
/// mempool, coinbase provenance, the tip the build itself reads — is still the
/// primary's to answer, which is what keeps corroboration a handful of extra
/// requests rather than a doubling of the send path.
///
/// # It answers about one address
///
/// The allowed set was built for the funding address of one send, so it is
/// applied to that address and to nothing else: an output at any other address
/// is passed through exactly as the primary reported it. Filtering those too
/// would look fail-closed and is not — it would silently answer "no coins" to a
/// caller asking a question this type never checked, which is a wrong answer
/// dressed as a safe one. A future caller that needs a second address checked
/// has to say so by building a second one of these.
pub struct Corroborated<'a, R> {
    primary: &'a R,
    /// The address `allowed` was built for. Nothing else is filtered.
    address: String,
    allowed: HashSet<Outpoint>,
}

impl<'a, R: ChainReader> Corroborated<'a, R> {
    pub fn new(primary: &'a R, address: &str, allowed: HashSet<Outpoint>) -> Self {
        Self {
            primary,
            address: address.to_string(),
            allowed,
        }
    }
}

/// Forward every `ChainReader` question to the primary, unchanged.
///
/// A macro for the same reason [`crate::Chain`] uses one: the trait has 29
/// methods, and the one that matters here is the one written out by hand below.
/// Hand-writing the other 28 is how one of them quietly stops matching.
macro_rules! passthrough {
    ($($method:ident ( $($arg:ident : $ty:ty),* ) -> $ret:ty;)*) => {
        $(
            fn $method(&self $(, $arg: $ty)*) -> Result<$ret, RpcError> {
                self.primary.$method($($arg),*)
            }
        )*
    };
}

impl<R: ChainReader> ChainReader for Corroborated<'_, R> {
    /// The one filtered answer.
    ///
    /// A coin the primary offers *at the corroborated address* that the second
    /// source has never heard of is dropped here, before anything selects it.
    /// An output at any other address is the primary's answer unchanged — see
    /// the note on the type about why that is the honest direction rather than
    /// the lax one.
    fn address_utxos(&self, addresses: &[&str]) -> Result<Vec<AddressUtxo>, RpcError> {
        Ok(self
            .primary
            .address_utxos(addresses)?
            .into_iter()
            .filter(|utxo| {
                utxo.address != self.address
                    || self.allowed.contains(&(utxo.utxo.txid, utxo.utxo.vout))
            })
            .collect())
    }

    passthrough! {
        chain_info() -> ChainInfo;
        block_count() -> u32;
        best_block_hash() -> String;
        block_hash(height: u32) -> String;
        mempool() -> Vec<String>;
        block(height_or_hash: &str) -> serde_json::Value;
        address_deltas(addresses: &[&str], range: Option<(u32, u32)>) -> Vec<AddressDelta>;
        address_mempool(addresses: &[&str]) -> Vec<MempoolDelta>;
        address_balance(addresses: &[&str]) -> AddressBalance;
        currency(name_or_id: &str) -> CurrencyPolicy;
        currency_definition(name_or_id: &str) -> CurrencySummary;
        estimate_conversion(from: &str, to: &str, amount: &str, via: Option<&str>) -> ConversionEstimate;
        currency_state(name_or_id: &str) -> serde_json::Value;
        currency_state_range(name_or_id: &str, from: u32, to: u32, step: u32) -> Vec<CurrencyStateAt>;
        list_currencies() -> Vec<CurrencySummary>;
        currency_converters(currencies: &[&str]) -> Vec<CurrencyConverter>;
        estimate_fee(blocks: u32) -> Option<Amount>;
        identity(name_or_id: &str) -> IdentityRecord;
        identities_with_address(address: &str) -> Vec<IdentityAtAddress>;
        identity_at(name_or_id: &str, height: u32) -> IdentityRecord;
        identity_content(name_or_id: &str) -> IdentityContent;
        identity_registration(name_or_id: &str) -> String;
        vdxf_id(name: &str) -> [u8; 20];
        offers(currency_or_id: &str, is_currency: bool, with_tx: bool) -> Vec<OfferListing>;
        verify_message(identity: &str, signature: &str, message: &str) -> bool;
        raw_transaction(txid: &str) -> serde_json::Value;
        decode_raw_transaction(hex: &str) -> serde_json::Value;
        confirmations(txid: &str) -> Option<u32>;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use verus_flows::testing::ScriptedReader;

    use super::*;

    /// The address both scripted nodes answer about. Any well-formed R-address
    /// does — nothing here parses it, and `ScriptedReader` keys its answers on
    /// the string.
    const ADDRESS: &str = "RQVsJRf98iq8YmRQdehzRcbLGHEx6YfjdH";

    /// A block hash from some other chain entirely.
    ///
    /// `ScriptedReader` derives every hash from the height, so two scripted
    /// nodes are on the same chain at every height *by construction* — which is
    /// precisely the thing rule 4 asks about, and precisely why a fixture that
    /// could not say "these are not those blocks" could not reach it.
    /// `with_best_hash` is the one builder that says it. It answers the same
    /// hash at every height, which is more than a real fork does and enough for
    /// a comparison at one.
    const ANOTHER_CHAIN: &str =
        "00000000000000000000000000000000000000000000000000000000deadbeef";

    /// What the primary offers, as the corroborator receives it.
    fn offered(reader: &impl ChainReader) -> Vec<AddressUtxo> {
        reader
            .address_utxos(&[ADDRESS])
            .expect("a scripted node answers")
    }

    /// The whole comparison, with the primary's tip read off the primary.
    ///
    /// `send::corroborated_funding` reads it the same way and from the same
    /// node, so a test that made one up could pin a verdict no caller can
    /// produce.
    fn hold(primary: &impl ChainReader, secondary: &impl ChainReader) -> Corroboration {
        hold_offering(primary, secondary, &offered(primary))
    }

    /// The same, for the few tests that alter what the primary offered before
    /// handing it over.
    fn hold_offering(
        primary: &impl ChainReader,
        secondary: &impl ChainReader,
        offered: &[AddressUtxo],
    ) -> Corroboration {
        let tip = primary.block_count().expect("a scripted node answers");
        against(primary, secondary, ADDRESS, offered, tip)
    }

    /// A node that answers every other question and refuses one of these two.
    ///
    /// `ScriptedReader` has no builder for a failing read, and the cases matter
    /// more than the others: a filtering public proxy that does not expose
    /// `getaddressutxos` is the realistic way a second source goes permanently
    /// silent, and treating silence as agreement is precisely the failure this
    /// module exists to avoid. The tip is refusable separately because the
    /// verdict now turns on it — a node that will not say how far it has got
    /// cannot tell lag from divergence either.
    struct Refuses {
        /// The node every unrefused question is forwarded to.
        ///
        /// Named `primary` because that is the field `passthrough!` writes
        /// against, and not because of which side of the comparison this double
        /// stands on — it stands on both. `Refuses::hash` takes the inner reader
        /// precisely so that it can be the *funding* node in a test, which the
        /// two older constructors, hard-coding an empty chain at 1,000, cannot.
        primary: ScriptedReader,
        /// Built fresh per call, because `RpcError` is not `Clone`.
        utxos: Option<fn() -> RpcError>,
        tip: Option<fn() -> RpcError>,
        hash: Option<fn() -> RpcError>,
    }

    impl Refuses {
        fn utxos(refuse: fn() -> RpcError) -> Self {
            Self {
                primary: ScriptedReader::new(1_000),
                utxos: Some(refuse),
                tip: None,
                hash: None,
            }
        }

        fn tip(refuse: fn() -> RpcError) -> Self {
            Self {
                // Holds nothing, so every coin the primary offers is missing
                // and the tip is what the verdict would turn on.
                primary: ScriptedReader::new(1_000),
                utxos: None,
                tip: Some(refuse),
                hash: None,
            }
        }

        /// Answers everything except `getblockhash`, holding whatever `inner`
        /// holds.
        ///
        /// Rule 4 is the first question this module asks of *both* nodes, so it
        /// is the first one where a refusal has two different meanings depending
        /// on which side refused — and the double has to be able to stand on
        /// either side to say so.
        fn hash(inner: ScriptedReader, refuse: fn() -> RpcError) -> Self {
            Self {
                primary: inner,
                utxos: None,
                tip: None,
                hash: Some(refuse),
            }
        }
    }

    impl ChainReader for Refuses {
        fn address_utxos(&self, addresses: &[&str]) -> Result<Vec<AddressUtxo>, RpcError> {
            match self.utxos {
                Some(refuse) => Err(refuse()),
                None => self.primary.address_utxos(addresses),
            }
        }

        fn block_count(&self) -> Result<u32, RpcError> {
            match self.tip {
                Some(refuse) => Err(refuse()),
                None => self.primary.block_count(),
            }
        }

        fn block_hash(&self, height: u32) -> Result<String, RpcError> {
            match self.hash {
                Some(refuse) => Err(refuse()),
                None => self.primary.block_hash(height),
            }
        }

        passthrough! {
            chain_info() -> ChainInfo;
            best_block_hash() -> String;
            mempool() -> Vec<String>;
            block(height_or_hash: &str) -> serde_json::Value;
            address_deltas(addresses: &[&str], range: Option<(u32, u32)>) -> Vec<AddressDelta>;
            address_mempool(addresses: &[&str]) -> Vec<MempoolDelta>;
            address_balance(addresses: &[&str]) -> AddressBalance;
            currency(name_or_id: &str) -> CurrencyPolicy;
            currency_definition(name_or_id: &str) -> CurrencySummary;
            estimate_conversion(from: &str, to: &str, amount: &str, via: Option<&str>) -> ConversionEstimate;
            currency_state(name_or_id: &str) -> serde_json::Value;
            currency_state_range(name_or_id: &str, from: u32, to: u32, step: u32) -> Vec<CurrencyStateAt>;
            list_currencies() -> Vec<CurrencySummary>;
            currency_converters(currencies: &[&str]) -> Vec<CurrencyConverter>;
            estimate_fee(blocks: u32) -> Option<Amount>;
            identity(name_or_id: &str) -> IdentityRecord;
            identities_with_address(address: &str) -> Vec<IdentityAtAddress>;
            identity_at(name_or_id: &str, height: u32) -> IdentityRecord;
            identity_content(name_or_id: &str) -> IdentityContent;
            identity_registration(name_or_id: &str) -> String;
            vdxf_id(name: &str) -> [u8; 20];
            offers(currency_or_id: &str, is_currency: bool, with_tx: bool) -> Vec<OfferListing>;
            verify_message(identity: &str, signature: &str, message: &str) -> bool;
            raw_transaction(txid: &str) -> serde_json::Value;
            decode_raw_transaction(hex: &str) -> serde_json::Value;
            confirmations(txid: &str) -> Option<u32>;
        }
    }

    /// A second source that crosses a block between the two reads it gets.
    ///
    /// Models one specific moment and nothing else: the UTXO answer was composed
    /// before a block was mined and the tip read after it. `ScriptedReader`
    /// cannot express that — its coin answer does not move when its tip does —
    /// and the race is invisible without it, because the verdict then depends on
    /// *which of the two reads happens first* rather than on the fixture.
    ///
    /// The field is `primary` for the reason [`Refuses`] gives: it is the name
    /// `passthrough!` writes against.
    struct Crossing {
        primary: ScriptedReader,
        /// The tip before the block, and after.
        before: u32,
        after: u32,
        /// Set once the coin answer has been handed over, which is the instant
        /// the block is taken to arrive.
        answered: Cell<bool>,
    }

    impl Crossing {
        fn across(before: u32, after: u32) -> Self {
            Self {
                // Holding nothing, so the coin the primary offers is missing and
                // the tip is what sorts it.
                primary: ScriptedReader::new(before),
                before,
                after,
                answered: Cell::new(false),
            }
        }
    }

    impl ChainReader for Crossing {
        fn block_count(&self) -> Result<u32, RpcError> {
            Ok(if self.answered.get() {
                self.after
            } else {
                self.before
            })
        }

        fn address_utxos(&self, addresses: &[&str]) -> Result<Vec<AddressUtxo>, RpcError> {
            self.answered.set(true);
            self.primary.address_utxos(addresses)
        }

        passthrough! {
            chain_info() -> ChainInfo;
            best_block_hash() -> String;
            block_hash(height: u32) -> String;
            mempool() -> Vec<String>;
            block(height_or_hash: &str) -> serde_json::Value;
            address_deltas(addresses: &[&str], range: Option<(u32, u32)>) -> Vec<AddressDelta>;
            address_mempool(addresses: &[&str]) -> Vec<MempoolDelta>;
            address_balance(addresses: &[&str]) -> AddressBalance;
            currency(name_or_id: &str) -> CurrencyPolicy;
            currency_definition(name_or_id: &str) -> CurrencySummary;
            estimate_conversion(from: &str, to: &str, amount: &str, via: Option<&str>) -> ConversionEstimate;
            currency_state(name_or_id: &str) -> serde_json::Value;
            currency_state_range(name_or_id: &str, from: u32, to: u32, step: u32) -> Vec<CurrencyStateAt>;
            list_currencies() -> Vec<CurrencySummary>;
            currency_converters(currencies: &[&str]) -> Vec<CurrencyConverter>;
            estimate_fee(blocks: u32) -> Option<Amount>;
            identity(name_or_id: &str) -> IdentityRecord;
            identities_with_address(address: &str) -> Vec<IdentityAtAddress>;
            identity_at(name_or_id: &str, height: u32) -> IdentityRecord;
            identity_content(name_or_id: &str) -> IdentityContent;
            identity_registration(name_or_id: &str) -> String;
            vdxf_id(name: &str) -> [u8; 20];
            offers(currency_or_id: &str, is_currency: bool, with_tx: bool) -> Vec<OfferListing>;
            verify_message(identity: &str, signature: &str, message: &str) -> bool;
            raw_transaction(txid: &str) -> serde_json::Value;
            decode_raw_transaction(hex: &str) -> serde_json::Value;
            confirmations(txid: &str) -> Option<u32>;
        }
    }

    /// The rule is one-directional, and this is the half the issue got wrong.
    ///
    /// A coin only the *second* node has is the second node being a block
    /// ahead, or the primary having already seen it spent. Refusing on it would
    /// be refusing because the other node is honest, which would make the guard
    /// fire on the commonest state two nodes are ever in.
    #[test]
    fn an_outpoint_only_the_second_source_has_is_not_a_disagreement() {
        let primary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);
        let secondary = ScriptedReader::new(1_001)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(ADDRESS, 901, 200_000);

        match hold(&primary, &secondary) {
            Corroboration::Agreed { checked } => assert_eq!(checked, 1),
            other => panic!("an honest node a block ahead was treated as a disagreement: {other:?}"),
        }
    }

    /// The same statement from the other side: what the older coins are worth
    /// is unaffected by a block one node has and the other has not.
    #[test]
    fn two_nodes_a_block_apart_still_agree_about_the_older_coins() {
        let older = |reader: ScriptedReader| {
            reader
                .with_utxo(ADDRESS, 800, 100_000)
                .with_utxo(ADDRESS, 801, 200_000)
        };
        let primary = older(ScriptedReader::new(1_000));
        let secondary = older(ScriptedReader::new(1_001)).with_utxo(ADDRESS, 1_001, 50_000);

        match hold(&primary, &secondary) {
            Corroboration::Agreed { checked } => assert_eq!(checked, 2),
            other => panic!("the wrong verdict: {other:?}"),
        }
    }

    /// A coin in a block the second node has not reached is lag, and the second
    /// node's own tip is the evidence.
    ///
    /// `getaddressutxos` is confirmed-only, so a payment that confirmed one
    /// block ago is missing from the answer of anything a block behind. Spend
    /// the rest, say what was left out.
    #[test]
    fn a_coin_above_the_second_sources_tip_is_lag_rather_than_a_disagreement() {
        let primary = ScriptedReader::new(1_001)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(ADDRESS, 1_001, 200_000);
        let secondary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);

        match hold(&primary, &secondary) {
            Corroboration::OutOfStep {
                withheld,
                kept,
                secondary_tip,
                primary_tip,
            } => {
                assert_eq!(withheld.len(), 1);
                assert_eq!(kept, 1);
                assert_eq!(secondary_tip, 1_000);
                assert_eq!(primary_tip, 1_001);
            }
            other => panic!("a node one block behind was accused of something: {other:?}"),
        }
    }

    /// And it is still lag when nothing survives it, which is the commonest
    /// wallet shape there is.
    ///
    /// One coin at the address — a fresh key, a swept key, a merchant
    /// forwarding a payment that just confirmed — and a second node that has
    /// not reached its block. Deciding the verdict on "did anything survive"
    /// would make that pair unreachable by any answer except the one that
    /// accuses them, on a guard with no off switch.
    #[test]
    fn a_lagging_second_source_is_still_lagging_when_it_leaves_nothing_to_spend() {
        let primary = ScriptedReader::new(1_001).with_utxo(ADDRESS, 1_001, 100_000);
        let secondary = ScriptedReader::new(1_000);

        match hold(&primary, &secondary) {
            Corroboration::OutOfStep {
                kept, secondary_tip, ..
            } => {
                assert_eq!(kept, 0);
                assert_eq!(secondary_tip, 1_000);
            }
            other => panic!("a single-coin address against a lagging node was refused: {other:?}"),
        }
    }

    /// Total disagreement at heights the second node has indexed is its own
    /// answer, not a filter down to nothing.
    ///
    /// Two chains' unspent outputs for one address are disjoint in both
    /// directions, so this is the shape the wrong-chain endpoint produces.
    /// Filtering to the empty set instead would reach the send form as "not
    /// enough funds", which is the least useful sentence available for the one
    /// case it would be describing.
    #[test]
    fn a_second_source_that_recognises_nothing_the_primary_offered_is_a_refusal_and_not_a_filter() {
        let primary = ScriptedReader::new(1_000)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(ADDRESS, 901, 200_000);
        let secondary = ScriptedReader::new(1_000)
            .with_utxo(ADDRESS, 700, 100_000)
            .with_utxo(ADDRESS, 701, 200_000);

        match hold(&primary, &secondary) {
            Corroboration::Diverged { unexplained, kept } => {
                assert_eq!(unexplained.len(), 2);
                assert_eq!(kept, 0);
            }
            other => panic!("a node serving another chain's coins was not refused: {other:?}"),
        }
    }

    /// One real coin does not buy a hostile endpoint a caption about lag.
    ///
    /// A node that offers the wallet's own coin alongside a pile of invented
    /// ones is not a node a block behind, and a verdict decided by "did
    /// anything survive" would say it was — rendering the only signal that the
    /// active endpoint is lying as the same tertiary sentence an honest node
    /// gets.
    #[test]
    fn a_node_that_mixes_one_real_coin_in_with_invented_ones_is_still_a_disagreement() {
        let primary = ScriptedReader::new(1_000)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(ADDRESS, 700, 200_000)
            .with_utxo(ADDRESS, 701, 300_000);
        // Holds the first coin and nothing else, and has long since indexed the
        // blocks the other two claim to be in.
        let secondary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);

        match hold(&primary, &secondary) {
            Corroboration::Diverged { unexplained, kept } => {
                assert_eq!(unexplained.len(), 2);
                assert_eq!(kept, 1);
            }
            other => panic!("a mixed set was downgraded to lag: {other:?}"),
        }
    }

    /// Issue #29's own numbers, which is the case the first version of this got
    /// backwards.
    ///
    /// `api.verus.services` was at block 4,207,412 and `api.verustest.net` at
    /// 1,203,115 when this was written. A proxy forwarding the first one's
    /// answers while calling itself VRSCTEST therefore offers coins three
    /// million blocks *above* anything the second source has reached — so a
    /// rule that read "above the secondary's tip is lag" put the whole attack
    /// in the lag bucket and told the user to try again later. The bound is
    /// what makes the real scenario reach the verdict its own doc comment
    /// claims.
    #[test]
    fn coins_three_million_blocks_above_the_second_sources_tip_are_a_disagreement_and_not_lag() {
        let primary = ScriptedReader::new(4_207_412)
            .with_utxo(ADDRESS, 4_206_912, 500_000_000)
            .with_utxo(ADDRESS, 4_206_913, 300_000_000);
        let secondary = ScriptedReader::new(1_203_115).with_utxo(ADDRESS, 1_203_000, 100_000);

        match hold(&primary, &secondary) {
            Corroboration::Diverged { unexplained, kept } => {
                assert_eq!(unexplained.len(), 2);
                assert_eq!(kept, 0);
            }
            other => panic!("the scenario in the issue was read as lag: {other:?}"),
        }
    }

    /// The far edge of the window is still lag, so the bound is a bound and not
    /// a second, tighter rule by accident.
    #[test]
    fn a_coin_exactly_a_credible_lag_above_the_second_sources_tip_is_still_lag() {
        let reached = 1_200_000;
        let coin = reached + CREDIBLE_LAG;
        let primary = ScriptedReader::new(coin).with_utxo(ADDRESS, coin, 100_000);
        let secondary = ScriptedReader::new(reached);

        match hold(&primary, &secondary) {
            Corroboration::OutOfStep { secondary_tip, .. } => assert_eq!(secondary_tip, reached),
            other => panic!("a node inside the credible window was accused: {other:?}"),
        }
    }

    /// And one block past it is not.
    ///
    /// The pair with the test above is the whole content of [`CREDIBLE_LAG`]:
    /// a second source that would have to be more than a week behind is not
    /// behind, it is on another chain.
    #[test]
    fn a_coin_one_block_past_the_credible_lag_window_is_a_disagreement() {
        let reached = 1_200_000;
        let coin = reached + CREDIBLE_LAG + 1;
        let primary = ScriptedReader::new(coin).with_utxo(ADDRESS, coin, 100_000);
        let secondary = ScriptedReader::new(reached);

        match hold(&primary, &secondary) {
            Corroboration::Diverged { unexplained, .. } => assert_eq!(unexplained.len(), 1),
            other => panic!("a week and a day of lag was believed: {other:?}"),
        }
    }

    /// A node cannot offer a coin from a block it says it has not got.
    ///
    /// This is the one rule the answering node cannot choose its way around,
    /// because both numbers in it — the coin's height and the tip — come from
    /// that node. It is deliberately checked before the out-of-step rule below:
    /// the second source here is far *ahead*, so without this the same coin
    /// would be excused as one the primary has not caught up with.
    #[test]
    fn a_coin_the_primary_places_above_its_own_tip_is_a_disagreement() {
        let primary = ScriptedReader::new(1_000)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(ADDRESS, 1_500, 200_000);
        let secondary = ScriptedReader::new(2_000).with_utxo(ADDRESS, 900, 100_000);

        match hold(&primary, &secondary) {
            Corroboration::Diverged { unexplained, kept } => {
                assert_eq!(unexplained.len(), 1);
                assert_eq!(kept, 1);
            }
            other => panic!("a node contradicting its own tip was excused: {other:?}"),
        }
    }

    /// The mirror of lag, and it must not be an accusation.
    ///
    /// The primary is the node that is behind. A coin it still offers, at a
    /// height the secondary indexed long ago, is one this wallet has already
    /// spent — the spending transaction confirmed, the second node saw it, the
    /// first has not caught up. Read as a disagreement it produces the sentence
    /// written for an endpoint serving another chain, over the wallet's own
    /// earlier payment.
    #[test]
    fn a_coin_the_second_source_has_already_seen_spent_is_out_of_step_and_not_an_accusation() {
        let primary = ScriptedReader::new(1_000)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(ADDRESS, 901, 200_000);
        // Ten blocks ahead, and holding only the older coin: the newer one was
        // spent in a block between the two tips.
        let secondary = ScriptedReader::new(1_010).with_utxo(ADDRESS, 900, 100_000);

        match hold(&primary, &secondary) {
            Corroboration::OutOfStep {
                withheld,
                kept,
                secondary_tip,
                primary_tip,
            } => {
                assert_eq!(withheld.len(), 1);
                assert_eq!(kept, 1);
                assert!(
                    secondary_tip > primary_tip,
                    "the pair this is about is the second node being ahead",
                );
            }
            other => panic!("the wallet's own earlier spend was read as the attack: {other:?}"),
        }
    }

    /// A node that cannot answer has not agreed.
    #[test]
    fn a_second_source_that_cannot_answer_is_reported_as_unchecked_and_never_as_agreement() {
        let primary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);
        let secondary = Refuses::utxos(|| RpcError::Transport("connection reset".to_string()));

        match hold(&primary, &secondary) {
            Corroboration::Unavailable { .. } => {}
            other => panic!("silence was read as an answer: {other:?}"),
        }
    }

    /// And the specific silence that is easiest to mistake for an answer.
    ///
    /// `-32601` from a filtering proxy comes back as an empty list to anything
    /// that swallows it — and an empty list from the second node, compared
    /// naively, would withhold every coin the primary offered and read as the
    /// attack. `Node::record_failure` is deliberately lenient about this error;
    /// the corroborator must not inherit that leniency, and the two states have
    /// to be told apart or somebody will chase the wrong problem.
    #[test]
    fn a_second_source_that_refuses_getaddressutxos_is_unavailable_rather_than_empty() {
        let primary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);
        let secondary = Refuses::utxos(|| RpcError::MethodUnavailable {
            method: "getaddressutxos",
        });

        match hold(&primary, &secondary) {
            Corroboration::Unavailable { reason } => assert!(
                matches!(reason, RpcError::MethodUnavailable { .. }),
                "the reason was lost: {reason}",
            ),
            other => panic!("a refused method was read as an empty UTXO set: {other:?}"),
        }
    }

    /// A node that will not say how far it has got cannot be told apart from a
    /// node on another chain, so it is neither.
    ///
    /// The tip is the whole evidence for the lag verdict. Guessing it — in
    /// either direction — would either accuse an honest node or excuse a lying
    /// one, and both are worse than saying the check did not happen.
    #[test]
    fn a_second_source_that_will_not_say_how_far_it_has_got_is_unavailable_rather_than_lagging() {
        let primary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);
        let secondary = Refuses::tip(|| RpcError::Transport("timed out".to_string()));

        match hold(&primary, &secondary) {
            Corroboration::Unavailable { .. } => {}
            other => panic!("a missing tip was guessed at: {other:?}"),
        }
    }

    /// `AddressUtxo` derives `PartialEq` over `is_spendable`, which the SDK
    /// calls the node's own opinion and says is recorded rather than relied on.
    /// Two honest nodes are free to disagree about it — and the wallet does not
    /// take either node's word for it anyway, since `spendable_at` decides
    /// maturity from the tip. A comparison that included it would report a
    /// disagreement about a field neither side is being trusted on.
    #[test]
    fn the_comparison_ignores_the_nodes_own_opinion_of_spendability() {
        let primary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);
        let secondary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);

        // The same coin, with the two nodes' opinions of it set opposite. The
        // double has no builder for the flag, so it is flipped on the answer.
        let mut mine = offered(&primary);
        for utxo in &mut mine {
            utxo.is_spendable = false;
        }
        assert_ne!(
            mine,
            offered(&secondary),
            "the two answers were made identical, so this test proves nothing",
        );

        match hold_offering(&primary, &secondary, &mine) {
            Corroboration::Agreed { checked } => assert_eq!(checked, 1),
            other => panic!("one node's opinion of spendability decided a spend: {other:?}"),
        }
    }

    /// And the other thing `AddressUtxo` equality would drag in: vector order,
    /// which no node promises and two nodes need not share.
    ///
    /// Only the primary's order can be varied here — `ScriptedReader` derives a
    /// coin's txid from its insertion index, so two readers cannot describe the
    /// same coins in different orders. The secondary's side is order-free by
    /// construction: [`against`] collects its answer into a set before anything
    /// is compared, which is the property this pins from the other direction.
    #[test]
    fn the_comparison_ignores_the_order_the_outputs_arrive_in() {
        let primary = ScriptedReader::new(1_000)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(ADDRESS, 901, 200_000);
        let secondary = ScriptedReader::new(1_000)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(ADDRESS, 901, 200_000);

        let mut mine = offered(&primary);
        mine.reverse();
        assert_ne!(mine, offered(&secondary));

        match hold_offering(&primary, &secondary, &mine) {
            Corroboration::Agreed { checked } => assert_eq!(checked, 2),
            other => panic!("the order the outputs arrived in decided a spend: {other:?}"),
        }
    }

    /// The filter, which is the half that reaches a signed transaction.
    ///
    /// A verdict nobody applies is a comment. This asserts the reader handed to
    /// the builder cannot offer the coin the second node never heard of, which
    /// is what makes the transaction corroborated by construction rather than
    /// checked afterwards.
    #[test]
    fn the_filtered_reader_cannot_offer_a_coin_the_second_source_withheld() {
        let primary = ScriptedReader::new(1_001)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(ADDRESS, 1_001, 200_000);
        let secondary = ScriptedReader::new(1_000).with_utxo(ADDRESS, 900, 100_000);

        let mine = offered(&primary);
        let Corroboration::OutOfStep { withheld, .. } =
            hold_offering(&primary, &secondary, &mine)
        else {
            panic!("the scripted pair does not disagree the way this test needs");
        };

        let reader = Corroborated::new(&primary, ADDRESS, agreed_outpoints(&mine, &withheld));
        let filtered = reader
            .address_utxos(&[ADDRESS])
            .expect("the primary answers");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].utxo.satoshis.to_sat(), 100_000);
        // And everything else is still the primary's to answer.
        assert_eq!(
            reader.block_count().expect("the primary answers"),
            primary.block_count().expect("the primary answers"),
        );
    }

    /// An address this was not built for gets the primary's answer, unchanged.
    ///
    /// The fail-closed-looking alternative — filter everything against one
    /// address's allowed set — answers "no coins" to a question nobody checked,
    /// which is a wrong answer wearing a safe one's clothes. Today no caller
    /// asks; the type should not depend on that staying true.
    #[test]
    fn the_filtered_reader_passes_through_an_address_it_was_not_built_for() {
        const OTHER: &str = "RJmMPWfLZDoxrCRDbTLMSy6QCn9jsx7Mzy";
        let primary = ScriptedReader::new(1_000)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(OTHER, 900, 300_000);

        // An allowed set that vouches for nothing at all.
        let reader = Corroborated::new(&primary, ADDRESS, HashSet::new());

        assert!(
            reader
                .address_utxos(&[ADDRESS])
                .expect("the primary answers")
                .is_empty(),
            "the corroborated address was not filtered",
        );
        let other = reader
            .address_utxos(&[OTHER])
            .expect("the primary answers");
        assert_eq!(
            other.len(),
            1,
            "an address this reader never checked was silently answered as empty",
        );
    }

    /// Issue #45, which is #29 with the two tips swapped.
    ///
    /// The wallet is set to the longer chain and the node in use forwards a
    /// shorter one's answers — its tip and every coin height included. Every
    /// invented coin then sits *below* the second source's tip, which is rule 3,
    /// which excused it with no bound at all: the verdict was `OutOfStep` and the
    /// sentence was "nothing is wrong, try again once the node you are using
    /// catches up", said to somebody whose node is lying to them.
    ///
    /// One coin in common, so this is not only about a sentence. With `kept > 0`
    /// the two verdicts differ about whether the payment happens: `Diverged`
    /// refuses it, `OutOfStep` completes it from whatever survived. Rule 4 is
    /// what tells them apart — the two nodes do not name the same block at the
    /// shared height, so the heights that excused the coin were heights on
    /// another chain.
    #[test]
    fn a_primary_forwarding_a_shorter_chain_is_a_disagreement_and_not_an_excuse() {
        let primary = ScriptedReader::new(1_203_115)
            .with_utxo(ADDRESS, 1_203_000, 100_000)
            .with_utxo(ADDRESS, 1_203_001, 200_000)
            .with_best_hash(ANOTHER_CHAIN);
        let secondary = ScriptedReader::new(4_207_412).with_utxo(ADDRESS, 1_203_000, 100_000);

        match hold(&primary, &secondary) {
            Corroboration::Diverged { unexplained, kept } => {
                assert_eq!(unexplained.len(), 1);
                assert_eq!(
                    kept, 1,
                    "the coin in common is what makes this verdict decide a payment",
                );
            }
            other => panic!("a node forwarding a shorter chain was excused: {other:?}"),
        }
    }

    /// And the figures the issue was filed with, where nothing survives.
    ///
    /// `OutOfStep { kept: 0, secondary_tip: 4207412, primary_tip: 1203115 }` is
    /// what it reported, and `kept == 0` refuses the payment either way — so no
    /// money was ever reachable in this state. What was wrong is that the refusal
    /// said the wrong thing: wait for the node you are using to catch up, about a
    /// node on another chain that is never going to.
    #[test]
    fn a_primary_forwarding_a_shorter_chain_is_still_a_disagreement_when_nothing_survives() {
        let primary = ScriptedReader::new(1_203_115)
            .with_utxo(ADDRESS, 1_203_000, 100_000)
            .with_utxo(ADDRESS, 1_203_001, 200_000)
            .with_best_hash(ANOTHER_CHAIN);
        let secondary = ScriptedReader::new(4_207_412);

        match hold(&primary, &secondary) {
            Corroboration::Diverged { unexplained, kept } => {
                assert_eq!(unexplained.len(), 2);
                assert_eq!(kept, 0);
            }
            other => panic!("the figures in the issue were read as lag: {other:?}"),
        }
    }

    /// The pair the fix must not touch, at the same heights as the one above.
    ///
    /// Identical fixture, one thing changed: the two nodes are on the same chain.
    /// A second source three million blocks ahead is an odd thing to meet, but
    /// the ordinary version of it — a primary behind, still offering a coin this
    /// wallet has already spent — is the commonest honest pair in this direction,
    /// and a fix that refused it would have turned every such payment into an
    /// accusation. Rule 4 is what makes this a check rather than a blanket
    /// refusal of a primary that is behind, so it is worth pinning against the
    /// test above and not only on its own.
    #[test]
    fn two_nodes_that_name_the_same_block_are_still_merely_out_of_step() {
        let primary = ScriptedReader::new(1_203_115)
            .with_utxo(ADDRESS, 1_203_000, 100_000)
            .with_utxo(ADDRESS, 1_203_001, 200_000);
        let secondary = ScriptedReader::new(4_207_412).with_utxo(ADDRESS, 1_203_000, 100_000);

        match hold(&primary, &secondary) {
            Corroboration::OutOfStep {
                withheld,
                kept,
                secondary_tip,
                primary_tip,
            } => {
                assert_eq!(withheld.len(), 1);
                assert_eq!(kept, 1);
                assert!(
                    secondary_tip > primary_tip,
                    "the pair this is about is the second source being ahead",
                );
            }
            other => panic!("two nodes on one chain were accused of being two: {other:?}"),
        }
    }

    /// The other half of the chosen-height class, which rule 4 closes with the
    /// same question.
    ///
    /// This is the dodge the module docs used to concede: report a tip a little
    /// way above the second source's and put the invented coins in between, and
    /// rule 2 calls them merely unindexed. It is also why the shared height is
    /// the *lower* of the two tips and not a fixed distance below the primary's —
    /// inflating its own tip moves the height asked about down onto the second
    /// source's side, where it certainly has the block, rather than up out of
    /// reach where "it cannot say" would have been an answer worth buying.
    #[test]
    fn a_primary_claiming_a_tip_far_above_the_second_sources_is_checked_where_that_node_has_blocks() {
        let reached = 1_200_000;
        let primary = ScriptedReader::new(reached + CREDIBLE_LAG * 2)
            .with_utxo(ADDRESS, reached, 100_000)
            .with_utxo(ADDRESS, reached + 1, 200_000)
            .with_best_hash(ANOTHER_CHAIN);
        let secondary = ScriptedReader::new(reached).with_utxo(ADDRESS, reached, 100_000);

        match hold(&primary, &secondary) {
            Corroboration::Diverged { unexplained, kept } => {
                assert_eq!(unexplained.len(), 1);
                assert_eq!(kept, 1);
            }
            other => panic!("a coin placed one block above the second source's tip bought a caption: {other:?}"),
        }
    }

    /// A block crossing between the two reads must not manufacture an
    /// accusation.
    ///
    /// The second source's coin answer is composed before a block is mined and
    /// its tip read after it. With the tip read last — which is how this shipped,
    /// to save a round trip on the path where the two nodes agree — the coin that
    /// confirmed in that block is missing from the answer *and* below the tip, so
    /// rules 2 and 3 both say the node has indexed the block and does not have
    /// the coin. That is `Diverged`: an accusation of serving another chain,
    /// against an honest node, produced by the wallet's own request ordering.
    ///
    /// Reading the tip first makes it a lower bound on what the coin answer
    /// covers, and the same coin lands in rule 2 where it belongs.
    #[test]
    fn a_block_crossing_between_the_two_reads_does_not_manufacture_an_accusation() {
        let primary = ScriptedReader::new(1_001).with_utxo(ADDRESS, 1_001, 100_000);
        let secondary = Crossing::across(1_000, 1_001);

        match hold(&primary, &secondary) {
            Corroboration::OutOfStep { secondary_tip, .. } => assert_eq!(
                secondary_tip, 1_000,
                "the tip was read after the coins rather than before them",
            ),
            other => panic!("a block crossing mid-check accused an honest node: {other:?}"),
        }
    }

    /// A funding node that will not name the shared block is its own state.
    ///
    /// Rule 4 is the first thing this module asks of the primary, so it is the
    /// first place the primary can go silent — and "the second source could not
    /// be asked" would be a false sentence pointing at a machine that answered
    /// every question put to it. Not an accusation either: the height asked about
    /// is at or below the tip that node just reported, and a node that cannot
    /// name a block it says it has is broken rather than lying.
    #[test]
    fn a_primary_that_will_not_name_the_shared_block_is_not_the_second_sources_fault() {
        let primary = Refuses::hash(
            ScriptedReader::new(1_000)
                .with_utxo(ADDRESS, 900, 100_000)
                .with_utxo(ADDRESS, 901, 200_000),
            || RpcError::Transport("connection reset".to_string()),
        );
        let secondary = ScriptedReader::new(1_010).with_utxo(ADDRESS, 900, 100_000);

        match hold(&primary, &secondary) {
            Corroboration::PrimarySilent { .. } => {}
            other => panic!("a silent funding node was reported as something else: {other:?}"),
        }
    }

    /// And a second source that will not name it has not agreed either.
    ///
    /// The same rule the rest of this module runs on: a source that cannot answer
    /// is a failure and not a pass. A filtering proxy that serves
    /// `getaddressutxos` and `getblockcount` and refuses `getblockhash` would
    /// otherwise have every height excuse restored to it unexamined.
    #[test]
    fn a_second_source_that_will_not_name_the_shared_block_is_unavailable_rather_than_agreement() {
        let primary = ScriptedReader::new(1_000)
            .with_utxo(ADDRESS, 900, 100_000)
            .with_utxo(ADDRESS, 901, 200_000);
        let secondary = Refuses::hash(
            ScriptedReader::new(1_010).with_utxo(ADDRESS, 900, 100_000),
            || RpcError::MethodUnavailable {
                method: "getblockhash",
            },
        );

        match hold(&primary, &secondary) {
            Corroboration::Unavailable { reason } => assert!(
                matches!(reason, RpcError::MethodUnavailable { .. }),
                "the reason was lost: {reason}",
            ),
            other => panic!("a refused block hash was read as agreement: {other:?}"),
        }
    }
}
