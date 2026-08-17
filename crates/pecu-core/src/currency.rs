//! Reading currencies: what kind one is, and which identities could still
//! define one.
//!
//! # A currency is an identity wearing a second hat
//!
//! In Verus a currency is not a separate object with its own identifier. Its
//! `i` address **is** the address of the identity of the same name, and
//! defining one flips a flag on that identity. `verus-flows` states the
//! consequence as a refusal: an identity that already carries an active
//! currency cannot define another, ever.
//!
//! So this module reads currencies *through* identities. That is not a
//! shortcut — it is the only relationship there is, and a wallet that kept two
//! independent lists would be able to show a name as both free and taken.
//!
//! # Why eligibility is asked of the chain rather than read off a flag
//!
//! The obvious implementation is `flags & FLAG_ACTIVE_CURRENCY`. That constant
//! exists in `verus-tx-identity` and is **not re-exported by the SDK facade**
//! this wallet depends on, and copying a consensus bit's value across a crate
//! boundary to save a request is exactly the kind of guess this project does
//! not make.
//!
//! Asking `getcurrency` for the name is the authoritative answer and costs one
//! request per identity — on a screen whose being open is what justifies the
//! requests it makes. It also yields the definition itself, which the list has
//! to show anyway, so the "extra" request is not extra.

use verus_sdk::currency::option;
use verus_sdk::network::{CurrencySummary, RpcError};

/// What `proof_protocol` means when it is 2.
///
/// Named here rather than imported because the SDK facade does not export it;
/// its definition lives in `verus-tx-identity/src/register.rs` as
/// `CENTRALIZED_PROOF_PROTOCOL`. The value is consensus and the field arrives
/// from the node already typed, so this is a comparison against a documented
/// integer rather than a re-implementation of a rule.
const CENTRALIZED: u32 = 2;

/// What kind of currency this is, read off the options bitfield.
///
/// Read rather than inferred from which fields happen to be set: a basket with
/// its reserves not yet listed is still a basket, and a token that happens to
/// carry one preallocation is still a token. The bitfield is what consensus
/// looks at.
///
/// NFT is checked first because an NFT is also a token — `NFT_TOKEN` never
/// appears without `TOKEN` — so testing for the token bit first would call
/// every NFT a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Token,
    Basket,
    Nft,
}

impl Kind {
    pub fn of(options: u32) -> Self {
        if options & option::NFT_TOKEN != 0 {
            Self::Nft
        } else if options & option::FRACTIONAL != 0 {
            Self::Basket
        } else {
            Self::Token
        }
    }

    /// The kind a draft names, defaulting to the simplest one.
    ///
    /// A string this wallet does not recognise becomes a token rather than an
    /// error: the interface only ever sends one of three, and the safe reading
    /// of an unknown fourth is the kind with the fewest permanent decisions in
    /// it.
    pub fn named(text: &str) -> Self {
        match text {
            "basket" => Self::Basket,
            "nft" => Self::Nft,
            _ => Self::Token,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Token => "Token",
            Self::Basket => "Basket",
            Self::Nft => "NFT",
        }
    }

    /// One sentence about what this kind *is*, for somebody who did not define
    /// it. Empty for a plain token, where the label already says everything and
    /// a row that explains "Token" is a row nobody reads.
    pub fn note(self) -> &'static str {
        match self {
            Self::Token => "",
            Self::Basket => "Holds reserves and converts between them.",
            Self::Nft => "A single indivisible unit.",
        }
    }

    /// The pill colour, in the vocabulary the node list already uses.
    ///
    /// Nothing here is a warning: a currency's kind is not a problem, and
    /// colouring a basket amber because it is more complicated than a token
    /// would be the interface having an opinion about somebody's design.
    pub fn tone(self) -> &'static str {
        "online"
    }
}

/// One row in the currency list.
///
/// `tip` decides one thing: whether this currency has begun. A launch is not
/// instant — the definition is mined and the currency does nothing until its
/// start block — and for those twenty-odd minutes a row that says only the name
/// and the kind is indistinguishable from one that has been trading for a
/// month. The wallet says so at the moment of launch; the list has to keep
/// saying it until it stops being true.
pub fn row(summary: &CurrencySummary, tip: u32) -> pecu_protocol::CurrencyVm {
    let kind = Kind::of(summary.options);
    pecu_protocol::CurrencyVm {
        // No trailing `@`. That suffix is the identity convention, and none of
        // the currency names on VRSCTEST carries one — copying it here would
        // print a name that does not exist.
        name: summary.fully_qualified_name.clone(),
        address: summary.currency_id.clone(),
        kind: kind.label().to_string(),
        tone: kind.tone().to_string(),
        note: kind.note().to_string(),
        mintable: summary.proof_protocol == CENTRALIZED,
        start_block: thousands(u64::from(summary.start_block)),
        // A tip of zero means nothing has been read yet. Treated as started, so
        // a wallet that has not reached a node does not label every currency it
        // knows about as pending — an unanswered question must not read as an
        // answer, and the answer it would give is the alarming one.
        started: tip == 0 || summary.start_block <= tip,
    }
}

/// What a read of one identity's currency turned into.
///
/// Three answers, not two, for the reason `check_name` gives about names: a
/// question going unanswered must not read as an answer. An identity whose
/// currency could not be read is neither listed as having one nor offered as
/// free to use.
#[derive(Clone, Debug)]
pub enum Lookup {
    /// It defines this.
    Defines(Box<CurrencySummary>),
    /// It defines none, and the node said so.
    None,
    /// The node would not say. Carries what it said, for the log.
    Unknown(String),
}

/// Classify what `currency_definition` came back with.
///
/// # The code is `-8`, and assuming otherwise made the whole screen useless
///
/// This first accepted only `-5`, reasoned by analogy with `getidentity` and
/// never checked. It is wrong. Measured against `api.verustest.net`:
///
/// ```text
/// getidentity "notregistered@"  → -5  "Identity not found"
/// getcurrency "maker"           → -8  "Invalid currency or currency not found"
/// ```
///
/// Two methods, two codes. With only `-5` accepted, every identity that had no
/// currency — which is every identity somebody would want to use — came back
/// `Unknown`, was refused with "the node would not say", and **none of them
/// could be selected**. The screen looked like it worked and offered nothing.
///
/// Both are accepted now: a node that answers either is saying the same thing,
/// and there is no reading of `-5` here that would be dangerous. Anything else
/// — a method the node will not serve, a transport that failed — is still not
/// evidence that the currency is free.
pub fn classify(result: Result<CurrencySummary, RpcError>) -> Lookup {
    match result {
        Ok(summary) => Lookup::Defines(Box::new(summary)),
        Err(RpcError::Node { code: -5 | -8, .. }) => Lookup::None,
        Err(error) => Lookup::Unknown(error.to_string()),
    }
}

/// Why this identity may not be used to define a currency, or empty if it may.
///
/// # Why a refused identity is listed rather than hidden
///
/// A name missing from a picker with no explanation reads as a bug, and the
/// most common reason here is permanent and worth knowing: an identity that
/// already defines a currency can never define another. Somebody who registered
/// a name for this purpose and is now looking for it deserves the sentence
/// rather than an empty list.
///
/// # Why the identity's status is asked about here at all
///
/// Because the flow refuses it later and says less about it. `prepare_launch`
/// turns a revoked identity into `TxError::AlreadyRevoked` — after the picker
/// offered it, after a draft was configured, and phrased as a transaction
/// error rather than as a fact about the name that was chosen. A locked one is
/// worse: nothing in the flow refuses it, and its output cannot be spent until
/// the timelock passes, so it fails at the node with nothing pointing back at
/// the reason. Both are knowable before anything is built, from the status
/// this wallet already reads.
pub fn refusal(lookup: &Lookup, can_sign: bool, status: &str) -> String {
    match lookup {
        Lookup::Defines(summary) => format!(
            "Already defines {}. An identity can define one currency, and only once.",
            summary.fully_qualified_name,
        ),
        Lookup::Unknown(_) => {
            "The node would not say whether this already defines a currency.".to_string()
        }
        Lookup::None if !can_sign => {
            "This wallet does not hold the keys that sign for it.".to_string()
        }
        // Permanent, and the only one of these that stays true forever.
        Lookup::None if status == crate::identity::Status::Revoked.label() => {
            "Revoked. A revoked identity cannot define a currency.".to_string()
        }
        // Not permanent, and the wording says which: a timelocked identity
        // cannot spend the output the launch has to spend, but it will be able
        // to.
        Lookup::None if timelocked(status) => {
            "Timelocked. Its output cannot be spent until the lock passes.".to_string()
        }
        Lookup::None => String::new(),
    }
}

/// Whether a status word from the identity list means "cannot spend its own
/// output today".
///
/// Both spellings of a timelock count: `Locked` is a delay nobody has started,
/// `Unlocking` a wait that is running. They read as opposites and neither can
/// spend now.
///
/// Compared against [`crate::identity::Status`]'s own labels rather than
/// against written-out words, so this cannot drift from the vocabulary the
/// pills are drawn from. `is_the_same_words_the_pills_use` pins that.
fn timelocked(status: &str) -> bool {
    status == crate::identity::Status::Locked { delay: 0 }.label()
        || status == crate::identity::Status::Unlocking { at: 0 }.label()
}

// ── Configuring one ─────────────────────────────────────────────────────────

/// Satoshis in one coin. Weights are expressed against this: consensus wants
/// the reserve weights of a basket to add to exactly one coin.
const ONE: i64 = 100_000_000;

/// How far ahead of the tip a launch may be scheduled before the wallet stops
/// calling it a launch and starts calling it a plan.
///
/// Not a consensus limit — consensus only asks for `start_block > tip`. This is
/// the wallet declining to let somebody schedule a currency for a fortnight's
/// time by typing an extra digit, when the field means blocks and reads like
/// minutes.
const FAR_AHEAD: u32 = 20_000;

/// Something wrong with a draft, or something permanent about to happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    /// Whether this stops the launch. A warning is the chain saying yes to
    /// something with no second attempt, and rendering the two the same either
    /// blocks a legal currency or waves through the one mistake that cannot be
    /// undone.
    pub blocking: bool,
    pub text: String,
}

impl Problem {
    fn stop(text: impl Into<String>) -> Self {
        Self {
            blocking: true,
            text: text.into(),
        }
    }

    fn warn(text: impl Into<String>) -> Self {
        Self {
            blocking: false,
            text: text.into(),
        }
    }
}

/// Parse a weight written as a percentage into satoshi-scaled units.
///
/// `"40"` and `"40.5"` both work; the result is what consensus counts, so the
/// sum is compared against exactly one coin rather than against 100.
fn weight_units(typed: &str) -> Option<i64> {
    let typed = typed.trim();
    if typed.is_empty() {
        return None;
    }
    // A percentage of one coin: 100% is `ONE`, so one per cent is ONE/100.
    let coins: f64 = typed.parse().ok()?;
    if !coins.is_finite() || coins < 0.0 {
        return None;
    }
    // Rounded rather than truncated. Truncating loses a satoshi per reserve and
    // a three-reserve basket then never adds up, which would refuse a draft
    // that is correct.
    #[allow(clippy::cast_possible_truncation)]
    Some((coins * f64::from(u32::try_from(ONE).unwrap_or(u32::MAX)) / 100.0).round() as i64)
}

/// Every check the interface must make, in the order somebody meets them.
///
/// # Why these live here and not in the SDK
///
/// Some of them do live in the SDK — but in the **wasm** layer, not in the Rust
/// core this wallet calls. `verus-tx-currency` will happily serialise a basket
/// whose weights do not add up, and a per-reserve vector shorter than the
/// reserve list silently attributes an amount to the wrong currency. Both are
/// documented upstream as things the caller has to get right.
///
/// So this is not duplicated validation. It is the validation, for this caller.
pub fn problems(draft: &pecu_protocol::CurrencyDraft, tip: u32) -> Vec<Problem> {
    let mut out = Vec::new();
    let kind = Kind::named(&draft.kind);

    identity_problems(draft, &mut out);

    match kind {
        Kind::Basket => basket_problems(draft, &mut out),
        Kind::Nft => nft_problems(draft, &mut out),
        Kind::Token => token_problems(draft, &mut out),
    }

    start_problems(draft, tip, &mut out);
    out
}

/// Where the identity is coming from, and whether that answer is usable.
///
/// # Why a name being claimed is checked here rather than only by the registrar
///
/// Because the two decisions are made on one screen and paid for in one press.
/// A name that the chain will refuse — a capital letter, a dot, a space — is
/// found by `identity::name_problem` without asking anybody, and finding it
/// after the launch fee has been agreed to means the form said "ready" about
/// something that cannot happen.
///
/// The rule is the registrar's, called rather than restated: one spelling of
/// what a name may contain, in one place.
fn identity_problems(draft: &pecu_protocol::CurrencyDraft, out: &mut Vec<Problem>) {
    let identity = draft.identity.trim();
    let name = draft.new_name.trim();

    match (identity.is_empty(), name.is_empty()) {
        (true, true) => out.push(Problem::stop(
            "Choose the identity this currency will be defined under, or claim a new name for it. \
             A currency cannot exist without one.",
        )),
        // Two answers to one question. Reachable only from a second interface
        // or a stale draft, and refused rather than resolved: guessing which
        // one was meant would define a currency under an identity nobody
        // chose.
        (false, false) => out.push(Problem::stop(format!(
            "This draft names both {identity} and a new name, {name}. It can only be defined under one.",
        ))),
        (true, false) => {
            if let Some(problem) = crate::identity::name_problem(name) {
                out.push(Problem::stop(format!("{name} cannot be claimed. {problem}")));
            }
        }
        (false, true) => {}
    }
}

/// What to call a reserve in a sentence somebody reads.
///
/// The name when the picker supplied one, and the i-address when it did not —
/// which is a draft from somewhere other than this wallet's form. Never an
/// empty string where a name should be: a refusal that names nothing cannot be
/// acted on.
pub(crate) fn reserve_label(reserve: &pecu_protocol::ReserveDraft) -> &str {
    let name = reserve.name.trim();
    if name.is_empty() {
        reserve.currency.trim()
    } else {
        name
    }
}

fn basket_problems(draft: &pecu_protocol::CurrencyDraft, out: &mut Vec<Problem>) {
    if draft.reserves.is_empty() {
        out.push(Problem::stop(
            "A basket needs at least one reserve. Without one it holds nothing and converts nothing.",
        ));
        return;
    }

    let mut total: i64 = 0;
    let mut seen: Vec<&str> = Vec::new();
    for reserve in &draft.reserves {
        if reserve.currency.trim().is_empty() {
            out.push(Problem::stop("A reserve has no currency."));
            continue;
        }
        // Keyed on the i-address, exactly, and not on a lowercased name. Two
        // currencies can be spelled alike and one currency can be reached by
        // two spellings; the address is the identity of the thing, and it is
        // what the definition will carry.
        let key = reserve.currency.trim();
        if seen.contains(&key) {
            out.push(Problem::stop(format!(
                "{} is listed twice. Each reserve appears once, with one weight.",
                reserve_label(reserve),
            )));
        }
        seen.push(key);

        match weight_units(&reserve.weight) {
            Some(0) | None => out.push(Problem::stop(format!(
                "{} has no weight. Every reserve needs a share of the basket.",
                reserve_label(reserve),
            ))),
            Some(units) => total += units,
        }
    }

    // The check the Rust core does not make. Consensus reads these as parts of
    // one whole, and a set that sums to anything else builds a market with the
    // wrong prices in it — silently, and permanently.
    if total != ONE && !out.iter().any(|p| p.blocking) {
        out.push(Problem::stop(format!(
            "The weights add up to {}, not 100%. Consensus reads them as shares of one whole.",
            percent(total),
        )));
    }
}

fn nft_problems(draft: &pecu_protocol::CurrencyDraft, out: &mut Vec<Problem>) {
    if !draft.reserves.is_empty() {
        out.push(Problem::stop(
            "An NFT holds no reserves. It is a single indivisible unit, and that is the whole of it.",
        ));
    }
    if draft.preallocations.len() > 1 {
        out.push(Problem::stop(
            "An NFT is one unit and can go to one holder.",
        ));
    }
    // Never broadcast successfully from this SDK. Said here rather than in a
    // footnote, because it is the one thing about this option somebody cannot
    // find out by reading the form.
    out.push(Problem::warn(
        "No NFT has ever been accepted by a node from this wallet's SDK. The transaction is built correctly against two live examples, and has never been sent.",
    ));
}

fn token_problems(draft: &pecu_protocol::CurrencyDraft, out: &mut Vec<Problem>) {
    if !draft.reserves.is_empty() {
        out.push(Problem::stop(
            "A token holds no reserves. Add them and it becomes a basket, which is a different thing.",
        ));
    }

    // The easiest catastrophic mistake in this form, and the SDK's own
    // constructor walks straight into it: a token with no preallocation and no
    // reserves launches a currency that can never hold anything, and it cannot
    // be fixed afterwards.
    let supply: i64 = draft
        .preallocations
        .iter()
        .filter_map(|p| coins_to_sats(&p.amount))
        .sum();

    if supply == 0 && !draft.mintable {
        out.push(Problem::stop(
            "This would launch a currency with no supply that can never be minted — it could never hold anything, and that cannot be undone.",
        ));
    } else if supply == 0 {
        out.push(Problem::warn(
            "No starting supply. Nothing exists until it is minted, which only this identity can do.",
        ));
    }

    for allocation in &draft.preallocations {
        if allocation.recipient.trim().is_empty() {
            out.push(Problem::stop("A preallocation has no recipient."));
        }
        if coins_to_sats(&allocation.amount).is_none_or(|sats| sats <= 0) {
            out.push(Problem::stop(format!(
                "{} is allocated nothing.",
                allocation.recipient.trim(),
            )));
        }
    }
}

fn start_problems(draft: &pecu_protocol::CurrencyDraft, tip: u32, out: &mut Vec<Problem>) {
    let delay: u32 = draft.start_delay.trim().parse().unwrap_or(0);
    if delay == 0 {
        // Consensus wants `start_block > tip`, and the tip moves while somebody
        // is reading the screen. A launch aimed at the current block is a
        // launch aimed at the past by the time it is signed.
        out.push(Problem::stop(
            "Choose how many blocks ahead it starts. Consensus refuses a currency that begins at or before the current block.",
        ));
    } else if delay > FAR_AHEAD {
        out.push(Problem::warn(format!(
            "That is {} blocks away — roughly {} days. Nothing happens until then.",
            thousands(u64::from(delay)),
            delay / 1440,
        )));
    }

    // Overflow rather than an argument: a delay that would push the start past
    // the end of the height space is not a schedule.
    if tip.checked_add(delay).is_none() {
        out.push(Problem::stop("That start is beyond the end of the chain."));
    }
}

/// Everything the configure screen renders, from the draft and two facts the
/// wallet already holds.
///
/// One function rather than several, because the bars, the numbers printed
/// beside them and the preview are three renderings of one arithmetic — and
/// three places dividing the same totals will one day disagree by a satoshi in
/// front of somebody deciding whether to spend two hundred coins.
pub fn check(
    draft: &pecu_protocol::CurrencyDraft,
    tip: u32,
    fee: Option<verus_sdk::money::Amount>,
    ticker: &str,
    identity_name: &str,
) -> pecu_protocol::CurrencyDraftVm {
    let kind = Kind::named(&draft.kind);
    let found = problems(draft, tip);
    let ready = !found.iter().any(|p| p.blocking);

    let weights: Vec<i64> = draft
        .reserves
        .iter()
        .map(|r| weight_units(&r.weight).unwrap_or(0))
        .collect();
    let weight_total: i64 = weights.iter().sum();

    // Against one whole rather than against the sum, deliberately. A bar
    // normalised to its own total always looks complete, which would hide the
    // one basket mistake that cannot be fixed after the launch.
    let slices = bar(
        draft
            .reserves
            .iter()
            .zip(&weights)
            .map(|(reserve, units)| (reserve_label(reserve), *units)),
        ONE,
    );

    let allocations: Vec<i64> = draft
        .preallocations
        .iter()
        .map(|p| coins_to_sats(&p.amount).unwrap_or(0))
        .collect();
    let supply: i64 = allocations.iter().sum();

    let supply_slices = bar(
        draft
            .preallocations
            .iter()
            .zip(&allocations)
            .map(|(allocation, sats)| (allocation.recipient.as_str(), *sats)),
        supply.max(1),
    );

    let delay: u32 = draft.start_delay.trim().parse().unwrap_or(0);
    let start = tip.saturating_add(delay);

    pecu_protocol::CurrencyDraftVm {
        problems: found
            .into_iter()
            .map(|p| pecu_protocol::CurrencyProblemVm {
                blocking: p.blocking,
                text: p.text,
            })
            .collect(),
        ready,
        slices,
        weights_total: percent(weight_total),
        supply_total: sats_display(supply),
        supply_slices,
        start_block: thousands(u64::from(start)),
        fee_display: fee.map_or_else(String::new, crate::portfolio::coins),
        preview: preview(draft, kind, start, supply, ticker, identity_name),
        steps: plan(draft),
    }
}

/// How many matches cross to the picker at once.
///
/// Not a page — there is no "next" — but a ceiling on one answer. VRSCTEST
/// lists 290 currencies and mainnet lists more; a list that long is scrolled
/// past rather than read, and the way to the one you want is the search field
/// above it. What is left over is counted and said, because a list that
/// truncated silently would read as "that is all there is".
const SHOWN: usize = 40;

/// Narrow the chain's currency list to what somebody is looking for.
///
/// # Why this is here and not in the interface
///
/// Two reasons, and the second is the one that decides it. Matching is a rule —
/// case, what counts as a hit, what ranks above what — and a rule with no test
/// is a rule that drifts. And Slint cannot filter a model in a binding, so the
/// interface could not do it even if it should.
///
/// # The order
///
/// A name that *starts* with the query first, then anything else that contains
/// it, each alphabetically. Typing `vrsc` should put `VRSCTEST` above
/// `Bridge.vETH.vrsc-something`, and without the split it does not: alphabetical
/// order alone buries an exact prefix under every currency that mentions it.
pub fn choices(
    all: &[CurrencySummary],
    query: &str,
    exclude: &[String],
    tip: u32,
) -> pecu_protocol::CurrencyChoicesVm {
    let needle = query.trim().to_lowercase();

    let mut hits: Vec<(bool, String, &CurrencySummary)> = all
        .iter()
        .filter(|summary| {
            !exclude
                .iter()
                .any(|taken| taken.trim() == summary.currency_id)
        })
        .filter_map(|summary| {
            let name = summary.fully_qualified_name.to_lowercase();
            if needle.is_empty() {
                return Some((false, name, summary));
            }
            // The address matches too, so somebody who pasted one is not told
            // the chain has never heard of it.
            if name.contains(&needle) {
                Some((name.starts_with(&needle), name, summary))
            } else if summary.currency_id.to_lowercase().contains(&needle) {
                Some((false, name, summary))
            } else {
                None
            }
        })
        .collect();

    // `sort_by` rather than `sort_by_key`: the key borrows, and the ordering is
    // "prefix first, then alphabetical" rather than a single field.
    hits.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

    let more = hits.len().saturating_sub(SHOWN);
    let rows = hits
        .into_iter()
        .take(SHOWN)
        .map(|(_, _, summary)| pecu_protocol::CurrencyPickVm {
            name: summary.fully_qualified_name.clone(),
            address: summary.currency_id.clone(),
            kind: Kind::of(summary.options).label().to_string(),
            // The one fact about a currency that changes what using it as a
            // reserve means, and it is invisible in a name. A basket whose
            // reserve has not begun holds something that does not exist yet.
            note: if tip > 0 && summary.start_block > tip {
                format!(
                    "Starts at block {}",
                    thousands(u64::from(summary.start_block))
                )
            } else {
                String::new()
            },
        })
        .collect();

    pecu_protocol::CurrencyChoicesVm {
        rows,
        more,
        loading: false,
        problem: String::new(),
    }
}

/// A whole proportional bar, laid out left to right.
///
/// `whole` is what the bar is measured against — one coin for weights, the total
/// for a supply split. Passing it in rather than summing here is what keeps a
/// weights bar able to draw short, which is the one basket mistake that cannot
/// be fixed after the launch.
///
/// The running offset is accumulated here, beside the share it belongs to,
/// because it is the same arithmetic. The interface used to place each slice
/// after its neighbour with a layout and derive nothing; it cannot any more —
/// see `ProportionBar` — and an offset computed there would be a second opinion
/// about where a slice starts.
fn bar<'a>(
    parts: impl Iterator<Item = (&'a str, i64)>,
    whole: i64,
) -> Vec<pecu_protocol::CurrencySliceVm> {
    let mut out = Vec::new();
    let mut offset = 0.0_f32;
    for (index, (label, part)) in parts.enumerate() {
        // Both casts are deliberate and both are safe here: a percentage between
        // 0 and 100 is exactly representable, and nobody reads this number — the
        // layout multiplies a width by it, and the figure a person reads is the
        // string built beside it.
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let share = if whole > 0 {
            (part as f64 * 100.0 / whole as f64) as f32
        } else {
            0.0
        };
        out.push(pecu_protocol::CurrencySliceVm {
            label: label.trim().to_string(),
            percent: share,
            offset_percent: offset,
            percent_display: format!("{share:.2}")
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_string()
                + "%",
            // Cycled so two adjacent slices never share a colour. Four tones is
            // enough: past four reserves the labels carry the reading anyway.
            tone: ["accent", "positive", "warning", "neutral"][index % 4].to_string(),
        });
        offset += share;
    }
    out
}

/// The four labels a launch from a new name passes through, in order.
///
/// Written once because two views draw them — the review, as what is about to
/// happen, and the unfinished panel, as progress — and a diagram whose two
/// renderings disagree about how many steps there are is worse than no diagram.
///
/// The first entry is the only one that is not a transaction. `progress` needs
/// it, because a name that has been chosen is a step that has been taken;
/// `plan` drops it, because by the time that list is read the name is on the
/// screen above it.
const CLAIMING: [(&str, bool); 4] = [
    ("Choose a name", false),
    ("Register the identity", true),
    ("Wait for it to confirm", false),
    ("Define the currency", true),
];

fn flow_step(label: &str, state: &str, costs: bool) -> pecu_protocol::FlowStepVm {
    pecu_protocol::FlowStepVm {
        label: label.to_string(),
        state: state.to_string(),
        costs,
    }
}

/// What the chain will be asked to do once the review is agreed to.
///
/// # Why this is not the form's own steps
///
/// The form's breadcrumb — which question somebody is on — is built in the
/// interface, because where you are in a set of questions is not a fact about
/// the chain and routing it through here would put a round trip between
/// pressing Next and the step changing. This is the other list, and it is
/// entirely chain fact: how many transactions the press makes, in what order,
/// and which of them cost money.
///
/// # Why the two paths have different lists rather than one greyed out
///
/// They are not the same journey with steps skipped. Defining under an identity
/// that already exists is one transaction and one fee. Starting from a name is
/// three transactions, two of which are paid for, with a wait in the middle
/// that can outlive the application. Drawing four steps and dimming two would
/// say the short path is a subset of the long one, and it is not — it is the
/// whole thing.
///
/// Every state here is `later`: this is read before anything has happened.
pub fn plan(draft: &pecu_protocol::CurrencyDraft) -> Vec<pecu_protocol::FlowStepVm> {
    if draft.new_name.trim().is_empty() {
        return vec![flow_step("Define the currency", "later", true)];
    }

    // Everything after choosing the name, which is `CLAIMING`'s first entry and
    // is not a transaction. Listing it here would count a form field as
    // something the chain does.
    CLAIMING[1..]
        .iter()
        .map(|(label, costs)| flow_step(label, "later", *costs))
        .collect()
}

/// How far a launch that has already started has got.
///
/// Progress rather than a plan, and the difference is paid for: by the time
/// this is drawn a name has been claimed with real money.
pub fn progress(step: crate::launch::Step) -> Vec<pecu_protocol::FlowStepVm> {
    // `AwaitingIdentity` means the registration is out and the chain has not
    // confirmed it — so waiting is where we are, and the two before it are
    // done. `ReadyToDefine` means the name is on the chain and only the
    // definition is left.
    let reached = match step {
        crate::launch::Step::AwaitingIdentity => 2,
        crate::launch::Step::ReadyToDefine => 3,
    };

    CLAIMING
        .iter()
        .enumerate()
        .map(|(index, (label, costs))| {
            let state = match index.cmp(&reached) {
                std::cmp::Ordering::Less => "done",
                std::cmp::Ordering::Equal => "now",
                std::cmp::Ordering::Greater => "later",
            };
            flow_step(label, state, *costs)
        })
        .collect()
}

/// What the preview says the currency will be defined under: the name, and the
/// address it resolves to.
///
/// # Why both, on two lines
///
/// Because they answer different halves of one check, and this panel is what
/// somebody reads before agreeing to something permanent. The address is what
/// goes on the chain. The name is what was chosen — and **an address nobody
/// typed cannot be verified by the person who typed a name**. The send review
/// learned this and shows both for the same reason: somebody shown only an
/// i-address is being asked to trust a lookup they were not told happened.
///
/// Two rows rather than one wrapped value because every other value here is a
/// consensus value that fits on one line, and the rows are laid out for that —
/// a wrapped one sat on top of the label beneath it.
///
/// A name being claimed has no address yet and gets no address row at all,
/// which is itself the difference worth seeing.
fn under(
    draft: &pecu_protocol::CurrencyDraft,
    identity_name: &str,
) -> Vec<(&'static str, String)> {
    let identity = draft.identity.trim();
    if !identity.is_empty() {
        let named = identity_name.trim();
        return if named.is_empty() {
            vec![("Defined under", identity.to_string())]
        } else {
            vec![
                ("Defined under", named.to_string()),
                ("Its address", identity.to_string()),
            ]
        };
    }

    let name = draft.new_name.trim();
    if name.is_empty() {
        return vec![("Defined under", "— not chosen —".to_string())];
    }
    vec![("Defined under", format!("{name}@ (to be claimed)"))]
}

/// What would go on the chain, in the order a definition is read.
///
/// The fields marked permanent are the ones with no second attempt. That marking
/// is the entire point of the panel — everything else here is also visible in
/// the form above it.
fn preview(
    draft: &pecu_protocol::CurrencyDraft,
    kind: Kind,
    start: u32,
    supply: i64,
    ticker: &str,
    identity_name: &str,
) -> Vec<pecu_protocol::CurrencyFieldVm> {
    let field = |label: &str, value: String, permanent: bool| pecu_protocol::CurrencyFieldVm {
        label: label.to_string(),
        value,
        permanent,
    };

    let mut out = vec![field("Kind", kind.label().to_string(), true)];
    for (label, value) in under(draft, identity_name) {
        out.push(field(label, value, true));
    }
    out.extend([
        field("Starts at block", thousands(u64::from(start)), true),
        field(
            "Supply can grow",
            if draft.mintable {
                "Yes — this identity may mint more".to_string()
            } else {
                "No — fixed at launch, forever".to_string()
            },
            true,
        ),
    ]);

    if kind == Kind::Basket {
        out.push(field(
            "Reserves",
            if draft.reserves.is_empty() {
                "— none —".to_string()
            } else {
                draft
                    .reserves
                    .iter()
                    .map(|r| {
                        format!(
                            "{} {}",
                            reserve_label(r),
                            percent(weight_units(&r.weight).unwrap_or(0))
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            true,
        ));
    }

    if kind != Kind::Nft {
        out.push(field(
            "Starting supply",
            format!("{} {ticker}", sats_display(supply)),
            true,
        ));
    }

    out
}

/// Turn a checked draft into the definition that goes on the chain.
///
/// # Why the recipients arrive already resolved
///
/// A preallocation pays an **identity**, and consensus wants its 20-byte hash.
/// An i-address carries that hash and decodes offline; a `name@` does not and
/// costs a lookup. Resolving inside here would make this function do I/O, and
/// then the one place in this module that turns a form into consensus values
/// could not be tested without a chain.
///
/// So the caller resolves first and passes a map. What is left is arithmetic
/// and bit-setting, which is exactly what wants a test.
///
/// # Why there is no basket constructor to call
///
/// The SDK has `token()` and `nft()` and nothing else. A fractional basket is
/// `token()` with the `FRACTIONAL` bit set and `currencies`/`weights` filled in
/// by hand — the upstream tests build one the same way. This is that, in one
/// place, rather than at every call site.
pub fn definition(
    draft: &pecu_protocol::CurrencyDraft,
    name: &str,
    parent: verus_sdk::currency::CurrencyId,
    start_block: u64,
    resolved: &std::collections::BTreeMap<String, [u8; 20]>,
) -> Result<verus_sdk::currency::CurrencyDefinition, String> {
    use verus_sdk::currency::{CurrencyDefinition, Preallocation};
    use verus_sdk::money::Amount;

    let kind = Kind::named(&draft.kind);

    // An NFT is built by its own constructor, not by hand.
    //
    // Hand-building one was wrong and `serialize_definition` said so by name:
    // the single satoshi has to be accompanied by `conversions`,
    // `max_preconversion` and `initial_contributions` vectors that this code
    // knew nothing about, and the reserve is the *system's* currency rather
    // than the parent — a distinction that only stops being the same value
    // under a sub-identity parent.
    //
    // Caught by `tests/currency_build.rs` feeding each kind through the SDK's
    // own serialiser, which cost nothing. On chain it would have cost the fee
    // and an identity that can never define another currency.
    if kind == Kind::Nft {
        let holder = match draft.preallocations.as_slice() {
            [only] => {
                let key = only.recipient.trim();
                *resolved
                    .get(key)
                    .ok_or_else(|| format!("{key} could not be resolved to an identity"))?
            }
            [] => return Err("An NFT needs a holder.".to_string()),
            _ => return Err("An NFT needs exactly one holder.".to_string()),
        };
        return Ok(CurrencyDefinition::nft(parent, name, start_block, holder));
    }

    let mut definition = CurrencyDefinition::token(parent, name, start_block);

    // The one decision that decides whether a supply can ever grow. `1` is
    // fixed forever; `2` is what `prepare_mint` requires and refuses anything
    // else for.
    if draft.mintable {
        definition.proof_protocol = i32::try_from(CENTRALIZED).unwrap_or(2);
    }

    for allocation in &draft.preallocations {
        let key = allocation.recipient.trim();
        let sats = coins_to_sats(&allocation.amount)
            .ok_or_else(|| format!("{key} is allocated an amount that is not a number"))?;
        let recipient = *resolved
            .get(key)
            .ok_or_else(|| format!("{key} could not be resolved to an identity"))?;
        definition.preallocations.push(Preallocation {
            recipient,
            amount: Amount::from_sat(u64::try_from(sats).unwrap_or(0)),
        });
    }

    match kind {
        Kind::Token => {}
        Kind::Basket => {
            definition.options |= option::FRACTIONAL;
            for entry in &draft.reserves {
                let currency = entry
                    .currency
                    .trim()
                    .parse::<verus_sdk::verus_keys::Address>()
                    .map(|address| verus_sdk::currency::CurrencyId::from_bytes(address.hash()))
                    .map_err(|_| {
                        format!(
                            "{} was not chosen from the currency list, so the wallet has no \
                             address for it.",
                            reserve_label(entry),
                        )
                    })?;
                let units = weight_units(&entry.weight)
                    .ok_or_else(|| format!("{} has no weight", reserve_label(entry)))?;
                definition.currencies.push(currency);
                // Satoshi-scaled, and refused rather than clamped if it does not
                // fit. A clamped weight is a market with different prices than
                // the one somebody agreed to.
                definition.weights.push(i32::try_from(units).map_err(|_| {
                    format!("{} has a weight too large to record", reserve_label(entry))
                })?);
            }
            // The whole supply of a basket comes from its reserves, so a
            // fractional currency declares what it starts with.
            definition.initial_supply = Amount::from_sat(
                u64::try_from(
                    draft
                        .preallocations
                        .iter()
                        .filter_map(|p| coins_to_sats(&p.amount))
                        .sum::<i64>(),
                )
                .unwrap_or(0),
            );
        }
        // Handled above, by the SDK's own constructor.
        Kind::Nft => unreachable!("an NFT returns before this match"),
    }

    Ok(definition)
}

// ── Launching one ───────────────────────────────────────────────────────────

/// A launch, built and signed, not sent.
///
/// **Never leaves the core.** The interface holds a ticket number and a decoded
/// summary; the bytes stay here, which is what makes an interface that could be
/// made to send them impossible rather than merely unlikely.
pub struct Prepared {
    unsent: verus_sdk::network::Unsent<verus_sdk::network::Launched>,
    /// What it is called, for the review. Read back off the definition rather
    /// than carried from the form, so a review cannot describe something the
    /// transaction does not do.
    pub name: String,
}

impl Prepared {
    /// What the chain charges for this launch, as the flow computed it.
    ///
    /// Not the miner fee, and not the whole of what leaves the wallet — see
    /// [`cost`], which is what the review shows.
    pub fn launch_fee(&self) -> verus_sdk::money::Amount {
        self.unsent.outcome.launch_fee
    }

    /// The height the currency begins at.
    ///
    /// Read back off the signed outcome rather than recomputed from the form.
    /// The form holds a delay and the tip it was checked against, and both move
    /// while somebody reads a review; this is the height that is actually in
    /// the bytes.
    pub fn start_block(&self) -> u64 {
        self.unsent.outcome.start_block
    }

    /// Send it. Takes a broadcaster, which needs a permit — the same gate every
    /// other write in this application goes through.
    pub fn broadcast(
        self,
        broadcaster: &pecu_chain::Permitted<'_>,
    ) -> Result<Launch, verus_sdk::network::FlowError> {
        let name = self.name;
        self.unsent.broadcast(broadcaster).map(|done| Launch {
            name,
            txid: done.txid,
            address: verus_sdk::verus_keys::Address::new(
                verus_sdk::verus_keys::AddressKind::Identity,
                done.currency_id,
            )
            .to_string(),
            start_block: done.start_block,
        })
    }
}

/// What a finished launch produced.
pub struct Launch {
    /// The currency's name, carried over from the definition that was signed.
    pub name: String,
    pub txid: String,
    /// The new currency's i-address — which is the defining identity's.
    pub address: String,
    pub start_block: u64,
}

/// Build and sign a launch without sending it.
///
/// Blocking, and the key exists only inside the closure. The `ChainReader` it
/// is handed has no `Broadcaster`, so this cannot send by construction — the
/// same property the send path relies on.
pub fn prepare(
    chain: &pecu_chain::Chain,
    vault: &pecu_keystore::Vault,
    label: &str,
    identity: &str,
    definition: &verus_sdk::currency::CurrencyDefinition,
) -> Result<Prepared, String> {
    let name = definition.name.clone();
    vault
        .with_key(label, |key| {
            verus_sdk::network::prepare_launch(chain, &[key], identity, definition, None)
        })
        .map_err(|error| error.to_string())?
        .map(|unsent| Prepared { unsent, name })
        .map_err(|error| error.to_string())
}

/// What leaves the wallet, as three figures rather than one.
///
/// # Why the launch fee alone understates it
///
/// The fee splits in half: one half becomes the new currency's reserve deposit
/// output and the other is **burned with no output at all** — verified against
/// the daemon at 205 in, 105 out, 100 unaccounted for. Both halves have to be
/// funded. On top of that sits the miner fee, which the SDK's own funding
/// precheck does not include.
///
/// So a wallet that printed `currency_registration_fee` and stopped would be
/// telling somebody a smaller number than the one about to leave their wallet.
pub struct Cost {
    /// What chain policy charges. Half deposit, half burned.
    pub launch_fee: verus_sdk::money::Amount,
    /// The half that becomes the currency's own reserve deposit.
    pub deposit: verus_sdk::money::Amount,
    /// The half that is burned.
    pub burned: verus_sdk::money::Amount,
}

pub fn cost(launch_fee: verus_sdk::money::Amount) -> Cost {
    let half = launch_fee.to_sat() / 2;
    Cost {
        launch_fee,
        deposit: verus_sdk::money::Amount::from_sat(half),
        // The remainder rather than the same half twice: an odd satoshi has to
        // go somewhere, and the two halves must add back to the fee or the
        // review does not reconcile.
        burned: verus_sdk::money::Amount::from_sat(launch_fee.to_sat() - half),
    }
}

/// Satoshis written the way every amount in this wallet is written.
///
/// `pecu_protocol::coins_u64` is the one rule; this only gets the count
/// there. A negative total cannot happen — every part is checked non-negative
/// before it is summed — and clamping rather than panicking keeps a form that
/// is being typed into from taking the wallet down.
fn sats_display(sats: i64) -> String {
    pecu_protocol::coins_u64(u64::try_from(sats).unwrap_or(0))
}

/// Coins as typed into satoshis. `None` when it is not a number.
fn coins_to_sats(typed: &str) -> Option<i64> {
    let typed = typed.trim().replace(' ', "");
    if typed.is_empty() {
        return None;
    }
    let coins: f64 = typed.parse().ok()?;
    if !coins.is_finite() || coins < 0.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    Some((coins * 100_000_000.0).round() as i64)
}

/// Satoshi-scaled weight units written as a percentage.
fn percent(units: i64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let share = units as f64 * 100.0 / ONE as f64;
    // Two decimals, because a weight of 33.33% is a thing somebody types and a
    // rounded "33%" would make a correct basket look wrong.
    let text = format!("{share:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.').to_string();
    format!("{text}%")
}

/// A block height with its thousands spaced, the way every other height in this
/// wallet is written.
pub(crate) fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// The ordinary status, so the tests that are about something else do not
    /// have to keep saying so.
    const ACTIVE: &str = "Active";

    /// A tip well past every start block written down here, so a row is only
    /// reported as pending when a test is about that.
    const TIP: u32 = 2_000_000;

    fn summary(options: u32, proof_protocol: u32) -> CurrencySummary {
        CurrencySummary {
            currency_id: "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".to_string(),
            name: "demo".to_string(),
            fully_qualified_name: "demo".to_string(),
            parent: Some("VRSCTEST".to_string()),
            system_id: "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq".to_string(),
            start_block: 1_188_900,
            end_block: 0,
            options,
            proof_protocol,
            definition: serde_json::Value::Null,
        }
    }

    /// An NFT carries the token bit as well, so the order of the tests decides
    /// the answer. Getting it wrong calls every NFT a token, which is the sort
    /// of thing that looks right in the one case somebody checks.
    #[test]
    fn an_nft_is_not_reported_as_a_token() {
        assert_eq!(Kind::of(option::TOKEN), Kind::Token);
        assert_eq!(Kind::of(option::TOKEN | option::FRACTIONAL), Kind::Basket);
        assert_eq!(Kind::of(option::TOKEN | option::NFT_TOKEN), Kind::Nft);
        // A basket that is also flagged NFT is nonsense the chain would refuse,
        // but if one ever arrives it is the more restrictive answer that is
        // safe to show.
        assert_eq!(
            Kind::of(option::TOKEN | option::FRACTIONAL | option::NFT_TOKEN),
            Kind::Nft,
        );
    }

    #[test]
    fn only_a_centralized_currency_is_reported_as_mintable() {
        assert!(!row(&summary(option::TOKEN, 1), TIP).mintable);
        assert!(row(&summary(option::TOKEN, CENTRALIZED), TIP).mintable);
    }

    /// The distinction the whole picker rests on: "no currency" and "no answer"
    /// must not become the same thing, or a wallet offers a name that is
    /// already taken and the launch fails after the fee.
    #[test]
    fn an_unanswered_read_is_not_read_as_free() {
        // `-8` is what `getcurrency` actually returns, measured against
        // api.verustest.net. Accepting only `-5` — `getidentity`'s code —
        // refused every identity in the wallet and left nothing selectable.
        let missing = classify(Err(RpcError::Node {
            code: -8,
            message: "Invalid currency or currency not found".to_string(),
        }));
        assert!(matches!(missing, Lookup::None));
        assert!(refusal(&missing, true, ACTIVE).is_empty());

        // And `-5` stays accepted: a node that answers with it means the same
        // thing, and there is no reading of it here that would be dangerous.
        let also = classify(Err(RpcError::Node {
            code: -5,
            message: "identity, currency, or ID not found".to_string(),
        }));
        assert!(matches!(also, Lookup::None));

        let quiet = classify(Err(RpcError::MethodUnavailable {
            method: "getcurrency",
        }));
        assert!(matches!(quiet, Lookup::Unknown(_)));
        assert!(
            !refusal(&quiet, true, ACTIVE).is_empty(),
            "a node that would not answer left the identity offered as free",
        );
    }

    #[test]
    fn an_identity_that_already_defines_one_says_which() {
        let taken = classify(Ok(summary(option::TOKEN, 1)));
        let said = refusal(&taken, true, ACTIVE);
        assert!(said.contains("demo"), "{said}");
        assert!(said.contains("only once"), "{said}");
    }

    /// A key this wallet does not hold is a different refusal from a currency
    /// that already exists, and both have to survive being read out loud.
    #[test]
    fn an_identity_somebody_else_controls_is_refused_for_that_reason() {
        let free = classify(Err(RpcError::Node {
            code: -5,
            message: String::new(),
        }));
        let said = refusal(&free, false, ACTIVE);
        assert!(said.contains("keys"), "{said}");
    }

    /// An identity nothing can be launched from is refused in the picker, not
    /// after a draft has been configured and a build attempted.
    ///
    /// The flow does refuse a revoked one — as `AlreadyRevoked`, phrased as a
    /// transaction error, several screens later. It refuses a timelocked one
    /// not at all: that fails at the node, with nothing naming the timelock.
    #[test]
    fn an_identity_that_cannot_spend_its_own_output_is_refused_before_anything_is_built() {
        let free = classify(Err(RpcError::Node {
            code: -8,
            message: String::new(),
        }));

        let revoked = refusal(&free, true, crate::identity::Status::Revoked.label());
        assert!(revoked.contains("Revoked"), "{revoked}");

        for status in [
            crate::identity::Status::Locked { delay: 20 },
            crate::identity::Status::Unlocking { at: 1_200_000 },
        ] {
            let said = refusal(&free, true, status.label());
            assert!(
                said.contains("Timelocked"),
                "{} was offered as able to define a currency: {said:?}",
                status.label(),
            );
        }

        assert!(
            refusal(&free, true, crate::identity::Status::Active.label()).is_empty(),
            "an ordinary identity was refused",
        );
    }

    /// The words this reacts to are the words the status pills are drawn with.
    ///
    /// They are produced two modules away, and a rename there that left this
    /// matching the old spelling would quietly start offering revoked and
    /// timelocked identities again — with nothing failing.
    #[test]
    fn is_the_same_words_the_pills_use() {
        assert!(timelocked(
            crate::identity::Status::Locked { delay: 1 }.label()
        ));
        assert!(timelocked(
            crate::identity::Status::Unlocking { at: 1 }.label()
        ));
        assert!(!timelocked(crate::identity::Status::Active.label()));
        assert!(!timelocked(crate::identity::Status::Revoked.label()));
        assert_eq!(ACTIVE, crate::identity::Status::Active.label());
    }

    fn draft(kind: &str) -> pecu_protocol::CurrencyDraft {
        pecu_protocol::CurrencyDraft {
            kind: kind.to_string(),
            identity: "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".to_string(),
            new_name: String::new(),
            mintable: false,
            start_delay: "20".to_string(),
            reserves: Vec::new(),
            preallocations: Vec::new(),
        }
    }

    /// A reserve as the picker produces one: an address, and the name it was
    /// chosen by. The tests below read the name back out of refusals, which is
    /// the whole reason the draft carries it.
    ///
    /// The address is derived from the name so two different names are two
    /// different reserves, which is what the rules under test are about.
    fn reserve(name: &str, weight: &str) -> pecu_protocol::ReserveDraft {
        reserve_at(name, &format!("i-{name}"), weight)
    }

    fn reserve_at(name: &str, address: &str, weight: &str) -> pecu_protocol::ReserveDraft {
        pecu_protocol::ReserveDraft {
            currency: address.to_string(),
            name: name.to_string(),
            weight: weight.to_string(),
        }
    }

    fn allocation(to: &str, amount: &str) -> pecu_protocol::PreallocationDraft {
        pecu_protocol::PreallocationDraft {
            recipient: to.to_string(),
            amount: amount.to_string(),
        }
    }

    fn blocking(problems: &[Problem]) -> Vec<String> {
        problems
            .iter()
            .filter(|p| p.blocking)
            .map(|p| p.text.clone())
            .collect()
    }

    /// The check the SDK's Rust core does not make.
    ///
    /// `verus-tx-currency` serialises whatever weights it is handed; only the
    /// wasm layer enforces the sum. A basket launched with weights that do not
    /// add to one whole is a market with the wrong prices in it, permanently,
    /// and nothing on the chain refuses it.
    #[test]
    fn a_basket_whose_weights_do_not_add_up_is_refused_with_the_total() {
        let mut wrong = draft("basket");
        wrong.reserves = vec![reserve("VRSCTEST", "40"), reserve("Bridge.vETH", "40")];

        let said = blocking(&problems(&wrong, 1_000));
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("80%"), "{}", said[0]);
        assert!(said[0].contains("not 100%"), "{}", said[0]);

        let mut right = wrong.clone();
        right.reserves = vec![reserve("VRSCTEST", "40"), reserve("Bridge.vETH", "60")];
        assert!(blocking(&problems(&right, 1_000)).is_empty());
    }

    /// Thirds are the case a naive implementation gets wrong: 33.33 three times
    /// is not one whole, and truncating instead of rounding loses a satoshi per
    /// reserve so a correct basket gets refused.
    #[test]
    fn thirds_are_handled_the_way_somebody_would_type_them() {
        let mut third = draft("basket");
        third.reserves = vec![
            reserve("A", "33.34"),
            reserve("B", "33.33"),
            reserve("C", "33.33"),
        ];
        assert!(
            blocking(&problems(&third, 1_000)).is_empty(),
            "{:?}",
            blocking(&problems(&third, 1_000)),
        );
    }

    /// The same currency twice, which consensus reads as two entries and which
    /// a basket cannot mean.
    ///
    /// Keyed on the **address**, not on a case-folded name. Two currencies are
    /// the same currency when they are the same twenty bytes; a name is a label
    /// the picker put beside them, and two labels that differ in case are not
    /// evidence either way.
    #[test]
    fn a_reserve_listed_twice_is_refused() {
        let mut twice = draft("basket");
        twice.reserves = vec![
            reserve_at("VRSCTEST", "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq", "50"),
            // The same currency, chosen again under a different spelling of its
            // name — which is what a stale draft or a second interface could
            // produce.
            reserve_at("vrsctest", "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq", "50"),
        ];
        let said = blocking(&problems(&twice, 1_000));
        assert!(said.iter().any(|s| s.contains("twice")), "{said:?}");
    }

    /// The easiest catastrophic mistake in the form, and the SDK's own
    /// `token()` constructor walks into it: no supply and no way to mint means a
    /// currency that can never hold anything, and it cannot be fixed.
    #[test]
    fn a_token_that_could_never_hold_anything_is_refused() {
        let empty = draft("token");
        let said = blocking(&problems(&empty, 1_000));
        assert!(
            said.iter().any(|s| s.contains("never hold anything")),
            "{said:?}",
        );

        // Mintable is a way out — the supply arrives later.
        let mut mintable = empty.clone();
        mintable.mintable = true;
        assert!(blocking(&problems(&mintable, 1_000)).is_empty());

        // So is a preallocation.
        let mut allocated = empty;
        allocated.preallocations = vec![allocation("demo@", "1000")];
        assert!(blocking(&problems(&allocated, 1_000)).is_empty());
    }

    /// A token is not a basket and a basket is not a token, and the difference
    /// is a bit in the options field that cannot be changed afterwards.
    #[test]
    fn the_kinds_refuse_each_others_fields() {
        let mut token = draft("token");
        token.mintable = true;
        token.reserves = vec![reserve("VRSCTEST", "100")];
        assert!(blocking(&problems(&token, 1_000))
            .iter()
            .any(|s| s.contains("becomes a basket")),);

        let bare = draft("basket");
        assert!(blocking(&problems(&bare, 1_000))
            .iter()
            .any(|s| s.contains("at least one reserve")),);
    }

    /// Consensus refuses a start at or before the tip, and the tip moves while
    /// somebody reads the screen.
    #[test]
    fn a_launch_must_start_ahead_of_the_tip() {
        let mut now = draft("token");
        now.mintable = true;
        now.start_delay = "0".to_string();
        assert!(blocking(&problems(&now, 1_000))
            .iter()
            .any(|s| s.contains("blocks ahead")),);
    }

    /// A currency has to be defined under something, and under exactly one
    /// thing.
    ///
    /// The interesting case is the third: a draft carrying an identity *and* a
    /// name is two answers to one question, and picking one would define a
    /// currency under something nobody chose — permanently, since an identity
    /// defines one currency ever.
    #[test]
    fn a_draft_names_one_identity_or_one_name_and_never_both() {
        let mut nothing = draft("token");
        nothing.mintable = true;
        nothing.identity = String::new();
        assert!(
            blocking(&problems(&nothing, 1_000))
                .iter()
                .any(|s| s.contains("claim a new name")),
            "a draft with nothing to define under was allowed",
        );

        let mut claiming = draft("token");
        claiming.mintable = true;
        claiming.identity = String::new();
        claiming.new_name = "livecoin".to_string();
        assert!(
            blocking(&problems(&claiming, 1_000)).is_empty(),
            "{:?}",
            blocking(&problems(&claiming, 1_000)),
        );

        let mut both = claiming.clone();
        both.identity = "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".to_string();
        assert!(
            blocking(&problems(&both, 1_000))
                .iter()
                .any(|s| s.contains("only be defined under one")),
            "a draft naming two things to define under was allowed",
        );
    }

    /// A name the chain will refuse is refused here, before the launch fee has
    /// been agreed to — and in the registrar's own words.
    #[test]
    fn a_name_that_cannot_be_claimed_stops_the_launch() {
        let mut shouting = draft("token");
        shouting.mintable = true;
        shouting.identity = String::new();
        shouting.new_name = "LiveCoin".to_string();

        let said = blocking(&problems(&shouting, 1_000));
        assert!(
            said.iter().any(|s| s.contains("lowercase")),
            "a name with capitals was accepted: {said:?}",
        );
    }

    /// One entry of the chain's currency list, with only the fields the picker
    /// reads filled in.
    fn listed(name: &str, address: &str, options: u32, start_block: u32) -> CurrencySummary {
        CurrencySummary {
            currency_id: address.to_string(),
            name: name.rsplit('.').next().unwrap_or(name).to_string(),
            fully_qualified_name: name.to_string(),
            parent: None,
            system_id: String::new(),
            start_block,
            end_block: 0,
            options,
            proof_protocol: 1,
            definition: serde_json::Value::Null,
        }
    }

    fn catalog() -> Vec<CurrencySummary> {
        vec![
            listed("VRSCTEST", "iAAA", 0x20, 0),
            listed("Bridge.vETH", "iBBB", 0x21, 0),
            listed("usdt-vrsc-basket", "iCCC", 0x21, 0),
            // Defined and not begun, which is the one fact about a reserve that
            // is invisible in its name.
            listed("later", "iDDD", 0x20, 9_000),
        ]
    }

    /// An exact prefix outranks a currency that merely mentions the word.
    ///
    /// Alphabetical order alone buries `VRSCTEST` under everything with `vrsc`
    /// in the middle of its name — which is the search somebody types when they
    /// want the chain's own currency, and it is the reserve most baskets have.
    #[test]
    fn a_search_puts_what_it_starts_with_first() {
        let found = choices(&catalog(), "vrsc", &[], 1_000);
        let names: Vec<&str> = found.rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, vec!["VRSCTEST", "usdt-vrsc-basket"], "{names:?}");
        assert_eq!(found.more, 0);
    }

    /// A pasted address finds its currency. Somebody who has one in the
    /// clipboard must not be told the chain has never heard of it.
    #[test]
    fn a_search_matches_an_address_too() {
        let found = choices(&catalog(), "ibbb", &[], 1_000);
        assert_eq!(found.rows.len(), 1, "{:?}", found.rows);
        assert_eq!(found.rows[0].name, "Bridge.vETH");
    }

    /// What is already in the basket is not offered again. Consensus reads a
    /// repeated reserve as two entries and the wallet refuses the draft either
    /// way, so offering one is offering a mistake.
    #[test]
    fn a_reserve_already_chosen_is_not_offered_again() {
        let taken = vec!["iAAA".to_string()];
        let found = choices(&catalog(), "", &taken, 1_000);
        assert!(
            !found.rows.iter().any(|row| row.address == "iAAA"),
            "{:?}",
            found.rows,
        );
    }

    /// A currency that has not begun says so. A basket whose reserve starts
    /// later holds something that does not exist yet, and nothing in a name
    /// says that.
    #[test]
    fn a_currency_that_has_not_started_is_marked() {
        let found = choices(&catalog(), "later", &[], 1_000);
        assert_eq!(found.rows.len(), 1);
        assert!(found.rows[0].note.contains("9 000"), "{:?}", found.rows[0]);

        // And with no tip yet, nothing is claimed either way — a wallet that
        // has not read a block height cannot know what has started.
        let unknown = choices(&catalog(), "later", &[], 0);
        assert!(unknown.rows[0].note.is_empty(), "{:?}", unknown.rows[0]);
    }

    /// The kind comes off the options bitfield, which is what decides it.
    #[test]
    fn a_search_says_what_kind_each_one_is() {
        let found = choices(&catalog(), "bridge", &[], 1_000);
        assert_eq!(found.rows[0].kind, "Basket");
    }

    /// A truncated list says how much it is holding back. One that stopped
    /// silently would read as "that is all there is" while hiding the currency
    /// somebody came for.
    #[test]
    fn a_long_answer_counts_what_it_left_out() {
        let many: Vec<CurrencySummary> = (0..SHOWN + 7)
            .map(|index| listed(&format!("coin{index:03}"), &format!("i{index:03}"), 0x20, 0))
            .collect();
        let found = choices(&many, "coin", &[], 1_000);
        assert_eq!(found.rows.len(), SHOWN);
        assert_eq!(found.more, 7);
    }

    /// Two paths, two diagrams — and the long one is not the short one with
    /// steps skipped.
    #[test]
    fn the_diagram_counts_the_transactions_the_path_actually_makes() {
        let mut existing = draft("token");
        existing.mintable = true;
        let short = plan(&existing);
        assert_eq!(short.len(), 1, "{short:?}");
        assert_eq!(
            short.iter().filter(|step| step.costs).count(),
            1,
            "a launch under an existing identity charged for more than one step",
        );

        existing.identity = String::new();
        existing.new_name = "livecoin".to_string();
        let long = plan(&existing);
        assert_eq!(long.len(), 3, "{long:?}");
        assert_eq!(
            long.iter().filter(|step| step.costs).count(),
            2,
            "claiming a name did not say that two of its steps cost money",
        );
        // Nothing has happened yet — this is read before the button is pressed.
        assert!(
            long.iter().all(|step| step.state == "later"),
            "a plan claimed something was already under way: {long:?}",
        );
        assert!(
            !long.iter().any(|step| step.label.contains("Choose")),
            "choosing the name is a form field, not a transaction: {long:?}",
        );
    }

    /// Progress moves, and the step it is on is the one that has not happened.
    #[test]
    fn the_diagram_advances_when_the_name_lands() {
        let waiting = progress(crate::launch::Step::AwaitingIdentity);
        assert_eq!(waiting.iter().filter(|s| s.state == "done").count(), 2);
        assert_eq!(waiting[2].state, "now");

        let ready = progress(crate::launch::Step::ReadyToDefine);
        assert_eq!(ready.iter().filter(|s| s.state == "done").count(), 3);
        assert_eq!(
            ready[3].state, "now",
            "an identity that exists was not shown as waiting for its currency",
        );

        // Same steps, same order, in both renderings — the property that lets
        // one component draw a plan and a progress without them disagreeing.
        let labels: Vec<&String> = waiting.iter().map(|s| &s.label).collect();
        let after: Vec<&String> = ready.iter().map(|s| &s.label).collect();
        assert_eq!(labels, after);
    }

    /// Never broadcast successfully from this SDK. The form has to say so, and
    /// it must not be a refusal — the option is offered, honestly labelled.
    #[test]
    fn the_nft_caveat_is_stated_and_does_not_block() {
        let nft = draft("nft");
        let found = problems(&nft, 1_000);
        assert!(blocking(&found).is_empty(), "{:?}", blocking(&found));
        assert!(
            found
                .iter()
                .any(|p| !p.blocking && p.text.contains("has never been sent")),
            "{found:?}",
        );
    }

    /// The bar is measured against one whole, not against its own total — so a
    /// basket that does not add up draws short instead of looking complete.
    #[test]
    fn the_reserve_bar_draws_short_when_the_weights_do_not_add_up() {
        let mut short = draft("basket");
        short.reserves = vec![reserve("A", "30"), reserve("B", "30")];
        let view = check(&short, 1_000, None, "VRSCTEST", "");

        let drawn: f32 = view.slices.iter().map(|s| s.percent).sum();
        assert!((drawn - 60.0).abs() < 0.01, "{drawn}");
        assert_eq!(view.weights_total, "60%");
        assert!(!view.ready);
    }

    /// The preview marks what cannot be changed later, which is the whole
    /// reason it is on screen.
    #[test]
    fn the_preview_marks_the_permanent_decisions() {
        let mut token = draft("token");
        token.preallocations = vec![allocation("demo@", "1000")];
        let view = check(&token, 1_188_000, None, "VRSCTEST", "");

        assert!(view.ready);
        assert!(view.preview.iter().all(|f| f.permanent));
        let mintable = view
            .preview
            .iter()
            .find(|f| f.label == "Supply can grow")
            .expect("the preview says whether the supply can grow");
        assert!(mintable.value.contains("forever"), "{}", mintable.value);
        assert_eq!(view.start_block, "1 188 020");
    }

    /// The preview shows the name that was chosen **and** the address that goes
    /// on the chain.
    ///
    /// The send review learned this first and says why: somebody shown only an
    /// i-address is being asked to trust a lookup they were not told happened,
    /// and somebody who picked a name cannot check an address they have never
    /// seen. This panel is the last place that comparison can be made.
    #[test]
    fn the_preview_names_the_identity_and_gives_its_address() {
        let mut token = draft("token");
        token.mintable = true;

        let view = check(&token, 1_000, None, "VRSCTEST", "spare.VRSCTEST@");
        let value = |label: &str| {
            view.preview
                .iter()
                .find(|field| field.label == label)
                .map(|field| field.value.clone())
        };
        assert_eq!(value("Defined under").as_deref(), Some("spare.VRSCTEST@"));
        assert_eq!(
            value("Its address").as_deref(),
            Some(token.identity.as_str())
        );

        // With no name to hand — a draft restored from disk, or an address
        // typed straight in — the address stands alone rather than the panel
        // inventing a name for it.
        let bare = check(&token, 1_000, None, "VRSCTEST", "");
        assert_eq!(
            bare.preview
                .iter()
                .find(|field| field.label == "Defined under")
                .map(|field| field.value.as_str()),
            Some(token.identity.as_str()),
        );
        assert!(
            !bare
                .preview
                .iter()
                .any(|field| field.label == "Its address"),
            "an address was repeated under two labels",
        );

        // A name being claimed has no address, and says so rather than leaving
        // a row that looks like a lookup nobody made.
        let mut claiming = draft("token");
        claiming.mintable = true;
        claiming.identity = String::new();
        claiming.new_name = "livecoin".to_string();
        let ahead = check(&claiming, 1_000, None, "VRSCTEST", "");
        assert_eq!(
            ahead
                .preview
                .iter()
                .find(|field| field.label == "Defined under")
                .map(|field| field.value.as_str()),
            Some("livecoin@ (to be claimed)"),
        );
        assert!(!ahead
            .preview
            .iter()
            .any(|field| field.label == "Its address"));
    }

    /// A currency that has not reached its start block is not shown as running.
    #[test]
    fn a_currency_that_has_not_begun_says_so() {
        let mut ahead = summary(option::TOKEN, 1);
        ahead.start_block = 1_200_000;

        assert!(!row(&ahead, 1_199_999).started);
        assert!(
            row(&ahead, 1_200_000).started,
            "the start block itself has begun"
        );
        assert!(row(&ahead, 1_200_001).started);

        // A tip of zero is "nobody has asked yet", and that must not read as
        // "none of your currencies has started".
        assert!(row(&ahead, 0).started);
    }

    /// A basket has no constructor in the SDK — it is `token()` plus a bit plus
    /// two parallel vectors. This is the one place that is done, so this is
    /// where it is checked.
    #[test]
    fn a_basket_sets_the_fractional_bit_and_its_reserves() {
        let parent = verus_sdk::currency::CurrencyId::from_bytes([7; 20]);
        let mut basket = draft("basket");
        basket.reserves = vec![
            reserve_at("demo", "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv", "25"),
            reserve_at("market", "i5Qcj82gvrHdHCCvTwy2yCFeMz3s3dgB6m", "75"),
        ];

        let built = definition(&basket, "market", parent, 1_188_000, &BTreeMap::new())
            .expect("a basket with two reserves builds");

        assert_ne!(built.options & option::FRACTIONAL, 0);
        assert_ne!(
            built.options & option::TOKEN,
            0,
            "a basket is still a token"
        );
        assert_eq!(built.currencies.len(), 2);
        // Satoshi-scaled shares of one coin, not percentages.
        assert_eq!(built.weights, vec![25_000_000, 75_000_000]);
        assert_eq!(
            built.weights.iter().map(|w| i64::from(*w)).sum::<i64>(),
            ONE
        );
        assert_eq!(built.start_block, 1_188_000);
    }

    /// Fixed or mintable is the one switch that cannot be flipped afterwards,
    /// and it is a `proof_protocol` value rather than a flag — easy to get
    /// backwards, and invisible until somebody tries to mint.
    #[test]
    fn mintable_is_the_centralized_proof_protocol_and_nothing_else() {
        let parent = verus_sdk::currency::CurrencyId::from_bytes([7; 20]);
        let mut fixed = draft("token");
        fixed.preallocations = vec![allocation("iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv", "1000")];
        let mut resolved = BTreeMap::new();
        resolved.insert("iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".to_string(), [3; 20]);

        let built = definition(&fixed, "demo", parent, 1_000, &resolved).expect("builds");
        assert_eq!(built.proof_protocol, 1);
        assert_eq!(built.preallocations.len(), 1);
        assert_eq!(built.preallocations[0].recipient, [3; 20]);
        assert_eq!(built.preallocations[0].amount.to_sat(), 1000 * 100_000_000);

        let mut mintable = fixed;
        mintable.mintable = true;
        let built = definition(&mintable, "demo", parent, 1_000, &resolved).expect("builds");
        assert_eq!(built.proof_protocol, 2);
    }

    /// A recipient the caller did not resolve is refused rather than dropped.
    /// Silently skipping one would launch a currency whose supply went
    /// somewhere else — permanently.
    #[test]
    fn an_unresolved_recipient_stops_the_build() {
        let parent = verus_sdk::currency::CurrencyId::from_bytes([7; 20]);
        let mut token = draft("token");
        token.preallocations = vec![allocation("nobody@", "5")];

        let refused = definition(&token, "demo", parent, 1_000, &BTreeMap::new())
            .expect_err("an unresolved recipient must not be dropped");
        assert!(refused.contains("nobody@"), "{refused}");
    }

    /// The SDK refuses any other shape by name, and an NFT with two units is
    /// not an NFT.
    #[test]
    fn an_nft_is_exactly_one_satoshi_to_exactly_one_holder() {
        let parent = verus_sdk::currency::CurrencyId::from_bytes([7; 20]);
        let mut nft = draft("nft");
        nft.preallocations = vec![allocation("holder@", "999")];
        let mut resolved = BTreeMap::new();
        resolved.insert("holder@".to_string(), [9; 20]);

        let built = definition(&nft, "art", parent, 1_000, &resolved).expect("builds");
        assert_ne!(built.options & option::NFT_TOKEN, 0);
        assert_eq!(built.preallocations.len(), 1);
        // Whatever was typed, an NFT is one unit.
        assert_eq!(built.preallocations[0].amount.to_sat(), 1);
        assert_eq!(built.initial_supply.to_sat(), 0);
        assert_eq!(built.currencies, vec![parent]);

        let mut two = nft;
        two.preallocations.push(allocation("other@", "1"));
        assert!(definition(&two, "art", parent, 1_000, &resolved).is_err());
    }

    #[test]
    fn a_height_is_spaced_the_way_every_other_height_is() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(900), "900");
        assert_eq!(thousands(1_188_900), "1 188 900");
    }
}
