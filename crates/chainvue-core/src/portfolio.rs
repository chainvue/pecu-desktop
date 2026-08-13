//! Reading what the wallet holds, and turning it into something a screen can
//! show.
//!
//! # What a refresh costs
//!
//! Counted, because a dashboard that quietly makes forty requests is how a
//! public node starts refusing to answer:
//!
//! | call | requests |
//! |---|---|
//! | `native_currency` | 1, **once ever** — cached and passed back in |
//! | `spendable`, per address | 3, plus 1 per output younger than 100 blocks |
//! | `address_mempool`, all addresses | 1 |
//! | `history`, all addresses | 2 |
//! | `currency_names` | 1 per currency **never seen before** |
//!
//! So a one-key wallet with no tokens and no young coinbase costs six. The two
//! caches are what keep it there: a currency's name is fixed when it is
//! registered and can never change, and the chain's own currency id is a
//! property of the chain.
//!
//! # Money never becomes a float
//!
//! Everything is summed as `Amount`/`SignedAmount` — `u64` and `i64` satoshis —
//! and only turns into text at the very end, through the SDK's own
//! `to_coins_string`. No `f64` appears anywhere in this file.

use std::collections::BTreeMap;

use chainvue_chain::Chain;
use chainvue_protocol::{AssetVm, BalanceVm, HistoryRowVm, PortfolioVm, TxDirection};
use verus_sdk::currency::CurrencyId;
use verus_sdk::money::Amount;
use verus_sdk::network::{self, FlowError, HistoryEntry, SignedAmount};
use verus_sdk::verus_keys::{Address, AddressKind};

/// How many transactions the dashboard's "recent" list shows. The Activity
/// screen gets everything fetched so far.
pub const RECENT: usize = 6;

/// How many transactions one page tries to gather before stopping.
pub const PAGE_ROWS: usize = 60;

/// How far back one `getaddressdeltas` reaches, in blocks.
///
/// Verus aims at one-minute blocks, so this is about a week. Small enough that
/// even a busy address answers in a bounded reply, and the window widens on its
/// own when it comes back empty — see [`history_page`].
const WINDOW_BLOCKS: u32 = 10_000;

/// The most a window may grow to before it stops doubling. About four years at
/// one-minute blocks, so an empty chain is crossed in a handful of requests
/// rather than hundreds.
const MAX_WINDOW_BLOCKS: u32 = 2_000_000;

/// What one refresh brings back.
///
/// Errors are per-section rather than one for the whole read: a node that
/// declines `getaddressdeltas` should cost the activity list, not the balance.
/// Folding them together would mean one refusal blanks a screen full of figures
/// the wallet already has.
pub struct Reading {
    pub tip: u32,
    pub spendable: Amount,
    pub immature: Amount,
    /// Confirmed coins an unconfirmed transaction already spends. Gone, not
    /// waiting — see `Funding::spent_unconfirmed`.
    pub pending_out: Amount,
    /// Arriving: unconfirmed outputs paying us.
    pub pending_in: Amount,
    /// Spendable tokens, and the ones that are not spendable yet, kept apart
    /// for the same reason the native figures are.
    pub tokens: BTreeMap<CurrencyId, Amount>,
    pub immature_tokens: BTreeMap<CurrencyId, Amount>,
    /// The chain's own currency, learned on the first read and kept.
    pub native: Option<CurrencyId>,
    /// Names for every currency seen so far, including the ones passed in.
    pub names: BTreeMap<CurrencyId, String>,
    /// `Err` here means the activity list is unknown, never empty.
    pub history: Result<Vec<HistoryEntry>, FlowError>,
    /// The lowest block height this scan actually looked at. Anything below is
    /// unexplored, not empty — the distinction the "Load older" button needs.
    pub scanned_to: u32,
    /// The scan reached the start of the chain. There is nothing older.
    pub reached_start: bool,
    /// A failure that cost us the balance itself.
    pub failure: Option<FlowError>,
}

/// What the caller already knows, so the read does not ask again.
#[derive(Clone, Default)]
pub struct Cached {
    pub native: Option<CurrencyId>,
    pub names: BTreeMap<CurrencyId, String>,
}

/// Read everything the dashboard shows. **Blocking** — call it through
/// [`crate::runtime::Blocking::dispatch`].
///
/// Never returns `Err`: a refresh that fails halfway still knows things worth
/// putting on screen, and the failures it did hit are in [`Reading::failure`]
/// and [`Reading::history`].
pub fn read(chain: &Chain, addresses: &[String], cached: Cached) -> Reading {
    let mut reading = Reading {
        tip: 0,
        spendable: Amount::ZERO,
        immature: Amount::ZERO,
        pending_out: Amount::ZERO,
        pending_in: Amount::ZERO,
        tokens: BTreeMap::new(),
        immature_tokens: BTreeMap::new(),
        native: cached.native,
        names: cached.names,
        history: Ok(Vec::new()),
        scanned_to: 0,
        reached_start: false,
        failure: None,
    };

    // The chain's own currency id, once ever. Needed to tell the native leg of
    // a reserve output apart from a token; without it those are refused by name
    // rather than double-counted, so a failure here is survivable.
    if reading.native.is_none() {
        match network::native_currency(chain) {
            Ok(native) => reading.native = Some(native),
            Err(error) => {
                tracing::warn!(%error, "the chain's own currency id is unknown for now");
            }
        }
    }

    for address in addresses {
        let funding = match network::spendable(chain, address) {
            Ok(funding) => funding,
            Err(error) => {
                // One unreachable address must not be reported as an empty one.
                // Whatever the other addresses hold is still true.
                tracing::warn!(%error, %address, "could not read this address");
                reading.failure.get_or_insert(error);
                continue;
            }
        };

        reading.tip = reading.tip.max(funding.tip);
        reading.spendable = add(reading.spendable, funding.total);
        reading.immature = add(reading.immature, funding.immature_total());
        reading.pending_out = add(reading.pending_out, funding.spent_unconfirmed_total());

        // Token counting can fail where the native figure did not — a
        // CryptoCondition output this build cannot read. That means the token
        // total is UNKNOWN, which is not the same as zero, so it is recorded
        // rather than defaulted.
        match funding.token_balances(reading.native) {
            Ok(found) => merge(&mut reading.tokens, found),
            Err(error) => tracing::warn!(%error, "spendable tokens could not be counted"),
        }
        match funding.immature_token_balances(reading.native) {
            Ok(found) => merge(&mut reading.immature_tokens, found),
            Err(error) => tracing::warn!(%error, "maturing tokens could not be counted"),
        }
    }

    let refs: Vec<&str> = addresses.iter().map(String::as_str).collect();

    // Arriving money. `spendable` reads the mempool too, but only to withhold
    // coins that are already spent — it does not report what is coming in, and
    // a wallet that cannot say "a payment is on its way" looks broken for the
    // ten minutes that matters most.
    match chain_mempool(chain, &refs) {
        Ok(incoming) => reading.pending_in = incoming,
        Err(error) => tracing::warn!(%error, "the mempool could not be read"),
    }

    // A bounded window rather than the whole chain. `history(.., None)` asks
    // for every transaction an address has ever been in, and the SDK says
    // plainly what that costs: "on a busy address is a large reply. Page with
    // explicit ranges rather than finding the transport's size ceiling." That
    // ceiling is 8 MB, and hitting it does not truncate the list — it fails the
    // whole read, which on screen looks like a wallet that has lost your
    // transactions rather than one that found too many.
    match history_page(chain, &refs, reading.tip, PAGE_ROWS) {
        Ok(page) => {
            reading.history = Ok(page.entries);
            reading.scanned_to = page.scanned_to;
            reading.reached_start = page.reached_start;
        }
        Err(error) => reading.history = Err(error),
    }

    // Only for currencies whose name is not already known. This is the request
    // that grows with what a wallet holds, and the cache is what keeps it from
    // growing on every refresh.
    let unknown: Vec<CurrencyId> = reading
        .tokens
        .keys()
        .chain(reading.immature_tokens.keys())
        .filter(|id| !reading.names.contains_key(id))
        .copied()
        .collect();

    if !unknown.is_empty() {
        match network::currency_names(chain, unknown) {
            Ok(found) => reading.names.extend(found),
            Err(error) => tracing::warn!(%error, "currency names could not be read"),
        }
    }

    reading
}

/// One page of history, scanning backwards from `before`.
///
/// # Why the window grows
///
/// A height range is the only bound `getaddressdeltas` offers — there is no
/// "give me the last fifty". So a busy address gets a small window and a
/// bounded reply, and a quiet one would otherwise need hundreds of empty
/// requests to walk back two years of chain. Doubling on an empty window
/// crosses that in a handful.
///
/// `before` is exclusive: pass the tip for the newest page, and the previous
/// page's `scanned_to` for the next one.
pub fn history_page(
    chain: &Chain,
    addresses: &[&str],
    before: u32,
    want: usize,
) -> Result<HistoryPage, FlowError> {
    let mut entries: Vec<HistoryEntry> = Vec::new();
    let mut end = before;
    let mut window = WINDOW_BLOCKS;
    let mut requests = 0u32;

    loop {
        if end == 0 {
            return Ok(HistoryPage {
                entries,
                scanned_to: 0,
                reached_start: true,
            });
        }

        let start = window_start(end, window);

        let found = network::history(chain, addresses, Some((start, end)))?;
        requests += 1;

        let empty = found.is_empty();
        entries.splice(0..0, found);

        if start <= 1 {
            return Ok(HistoryPage {
                entries,
                scanned_to: 0,
                reached_start: true,
            });
        }
        if entries.len() >= want {
            return Ok(HistoryPage {
                entries,
                scanned_to: start,
                reached_start: false,
            });
        }

        // A window that found nothing was too narrow for how quiet this address
        // is. One that found something and still came up short is simply a
        // sparse stretch, and widening it risks the reply this is here to bound.
        if empty {
            window = window.saturating_mul(2).min(MAX_WINDOW_BLOCKS);
        }
        end = start - 1;

        // A backstop, not a policy. Nothing here should loop this long, and if
        // it ever does, returning what we have beats hammering a public node.
        if requests >= 24 {
            tracing::warn!(requests, scanned_to = end, "the history scan gave up early");
            return Ok(HistoryPage {
                entries,
                scanned_to: end,
                reached_start: false,
            });
        }
    }
}

/// Everything the detail sheet can say without asking a node.
///
/// All of it comes from the history entry already on screen, which is why
/// opening a transaction is instant and the one request it does make — the raw
/// JSON — arrives afterwards.
pub fn detail(
    entry: &HistoryEntry,
    names: &BTreeMap<CurrencyId, String>,
    tip: u32,
    now: i64,
    explorer: Option<String>,
) -> chainvue_protocol::TxDetailVm {
    let by_address: BTreeMap<String, String> = names
        .iter()
        .map(|(id, name)| (i_address(*id), name.clone()))
        .collect();

    let listed = row(entry, now, &by_address);
    // `row` substitutes the token line for the amount when no native value
    // moved — so the amount is the native figure exactly when it did.
    let amount_is_native = entry.net_native != SignedAmount::ZERO;

    chainvue_protocol::TxDetailVm {
        txid: entry.txid.to_string(),
        height: entry.height,
        // Derived from the tip, not asked for. A confirmation count is
        // arithmetic on two numbers the wallet already has, and asking a node
        // for it would be a request that can also be wrong by a block.
        confirmations: (entry.height > 0 && tip >= entry.height).then(|| tip - entry.height + 1),
        block_time: entry.block_time,
        when_display: listed.when_display,
        net_display: listed.net_display,
        amount_is_native,
        // Unknown until the raw transaction says so. See the field's docs for
        // why it is not simply computed.
        fee_display: None,
        direction: listed.direction,
        // Only alongside a native figure. When the amount IS the token line,
        // listing it again below would print the same movement twice.
        currency_lines: if amount_is_native {
            listed.currency_lines
        } else {
            Vec::new()
        },
        explorer_url: explorer,
        raw_json: None,
    }
}

/// The lower bound of a window ending at `end`.
///
/// Never zero. `getaddressdeltas` refuses a zero bound outright — measured
/// against api.verustest.net: *"Start and end is expected to be greater than
/// zero"* — so a scan that walked down to 0 would fail on its last window and
/// lose the entire page rather than finishing. There are no transactions in the
/// genesis block anyway.
fn window_start(end: u32, window: u32) -> u32 {
    end.saturating_sub(window).max(1)
}

/// What one scan found, and how far down it looked.
pub struct HistoryPage {
    pub entries: Vec<HistoryEntry>,
    pub scanned_to: u32,
    pub reached_start: bool,
}

/// Sum the unconfirmed native value arriving at these addresses.
///
/// Outputs only. A spend of ours is already reported as `pending_out` by
/// `Funding`, and counting it here as well would show the same money twice.
fn chain_mempool(chain: &Chain, addresses: &[&str]) -> Result<Amount, FlowError> {
    use verus_sdk::network::ChainReader;

    let deltas = chain.address_mempool(addresses)?;
    let total = deltas
        .iter()
        .filter(|delta| !delta.spending)
        .filter_map(|delta| {
            // `satoshis` is signed and, for an output, positive. A token-only
            // output is zero here by design — its value is in the payload.
            u64::try_from(delta.satoshis.to_sat()).ok()
        })
        .try_fold(0u64, u64::checked_add);

    Ok(Amount::from_sat(total.unwrap_or(0)))
}

fn add(a: Amount, b: Amount) -> Amount {
    // Saturating rather than wrapping: a total that silently wrapped to a small
    // number is the one arithmetic failure a wallet must never show. The chain's
    // whole supply does not come close to `u64::MAX` satoshis, so this cannot
    // trigger on real data — it is here so that it cannot trigger on unreal
    // data either.
    a.checked_add(b).unwrap_or(Amount::from_sat(u64::MAX))
}

fn merge(into: &mut BTreeMap<CurrencyId, Amount>, found: BTreeMap<CurrencyId, Amount>) {
    for (currency, amount) in found {
        let slot = into.entry(currency).or_insert(Amount::ZERO);
        *slot = add(*slot, amount);
    }
}

// ── Building the view ───────────────────────────────────────────────────────

impl Reading {
    /// The dashboard's figures, formatted once, here.
    pub fn portfolio(&self, native_ticker: &str) -> PortfolioVm {
        let total = add(self.spendable, self.immature);

        let balance = BalanceVm {
            total_sats: sats(total),
            spendable_sats: sats(self.spendable),
            immature_sats: sats(self.immature),
            pending_out_sats: sats(self.pending_out),
            pending_in_sats: sats(self.pending_in),
            // Which block the nearest immature output matures at needs the
            // per-output heights, which `Funding::immature` has and this
            // summary does not keep. It lands with the Activity screen.
            matures_in_blocks: None,
            total_display: coins(total),
            spendable_display: coins(self.spendable),
            immature_display: coins(self.immature),
            pending_display: coins(self.pending_out),
            incoming_display: coins(self.pending_in),
        };

        let mut assets = vec![AssetVm {
            currency_id: self
                .native
                .map_or_else(|| native_ticker.to_string(), i_address),
            name: native_ticker.to_string(),
            amount_sats: sats(total),
            amount_display: coins(total),
            native: true,
        }];

        // Tokens after the native currency, and only ones actually held. A row
        // reading zero is a row that invites someone to wonder what happened to
        // it.
        for (currency, amount) in &self.tokens {
            if amount.is_zero() {
                continue;
            }
            assets.push(AssetVm {
                currency_id: i_address(*currency),
                // A name from the node is untrusted display text. The id is the
                // part that cannot lie, so it is what shows when there is no
                // name — never a blank row.
                name: self
                    .names
                    .get(currency)
                    .cloned()
                    .unwrap_or_else(|| i_address(*currency)),
                amount_sats: sats(*amount),
                amount_display: coins(*amount),
                native: false,
            });
        }

        PortfolioVm {
            balance,
            assets,
            stale: self.failure.is_some(),
            // An error counting tokens means unknown, never zero.
            tokens_unknown: self.failure.is_some(),
        }
    }

    /// Every transaction this read found, newest first, with day headings.
    pub fn rows(&self, now: i64) -> Vec<HistoryRowVm> {
        let Ok(entries) = &self.history else {
            return Vec::new();
        };
        rows_from(entries, &self.names, now)
    }
}

/// Turn history entries into rows, newest first, with day headings.
///
/// The SDK returns them oldest first, because that is the order the chain puts
/// them in. A person reads their own history the other way round.
///
/// A free function because a later page has to be re-grouped **together with**
/// everything already on screen: a day heading is a statement about the row
/// above, and appending a page without redoing them would print "Yesterday"
/// twice.
pub fn rows_from(
    entries: &[HistoryEntry],
    names: &BTreeMap<CurrencyId, String>,
    now: i64,
) -> Vec<HistoryRowVm> {
    // `net_currencies` is keyed by i-address and `names` by `CurrencyId`, so
    // the two need bridging before a row can say "mambo" instead of a fragment
    // of an id.
    let by_address: BTreeMap<String, String> = names
        .iter()
        .map(|(id, name)| (i_address(*id), name.clone()))
        .collect();

    let mut rows: Vec<HistoryRowVm> = entries
        .iter()
        .rev()
        .map(|entry| row(entry, now, &by_address))
        .collect();

    // The heading goes on the first row of each day. Done after the rows
    // exist because it depends on comparing neighbours, which is exactly
    // what a `for` loop over a Slint model cannot do.
    let mut previous: Option<String> = None;
    for row in &mut rows {
        let day = calendar_day(row.block_time, now);
        if previous.as_ref() != Some(&day) {
            row.group.clone_from(&day);
            previous = Some(day);
        }
    }

    rows
}

/// "Today" / "Yesterday" / "12 March 2026", in the machine's own timezone.
///
/// A block timestamp is UTC seconds; which calendar day that falls on is a
/// question about where the reader is sitting. Getting it wrong puts a
/// transaction under the wrong heading for anyone more than a few hours from
/// Greenwich.
///
/// An unconfirmed transaction has no timestamp and gets its own heading — it is
/// not on any day yet.
fn calendar_day(block_time: i64, now: i64) -> String {
    use chrono::{Datelike, Local, TimeZone};

    if block_time == 0 {
        return "Pending".to_string();
    }

    let Some(when) = Local.timestamp_opt(block_time, 0).single() else {
        return "Unknown date".to_string();
    };
    let Some(today) = Local.timestamp_opt(now, 0).single() else {
        return "Unknown date".to_string();
    };

    let days = today
        .date_naive()
        .signed_duration_since(when.date_naive())
        .num_days();
    match days {
        0 => "Today".to_string(),
        1 => "Yesterday".to_string(),
        _ => {
            let month = MONTHS.get(when.month0() as usize).copied().unwrap_or("");
            // The year only once it is not this one. Repeating it on every
            // heading is noise until the moment it is not.
            if when.year() == today.year() {
                format!("{} {month}", when.day())
            } else {
                format!("{} {month} {}", when.day(), when.year())
            }
        }
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Redo the wording on rows that were cached earlier.
///
/// "2 hours ago" and "Today" are true relative to when they were written, and a
/// snapshot restored the next morning would file yesterday's payment under
/// today. Every row keeps its `block_time`, so the wording is recomputed rather
/// than trusted — the figures are stale, the dates are not allowed to be wrong.
pub fn restamp(rows: &mut [HistoryRowVm], now: i64) {
    let mut previous: Option<String> = None;
    for row in rows.iter_mut() {
        row.when_display = when(row.block_time, now);

        let day = calendar_day(row.block_time, now);
        if previous.as_ref() == Some(&day) {
            row.group.clear();
        } else {
            row.group.clone_from(&day);
            previous = Some(day);
        }
    }
}

/// Read an i-address back into a currency id.
///
/// The inverse of [`i_address`], for the cache: what is stored is the form a
/// person can read, and what the SDK wants is the twenty bytes behind it.
pub fn currency_from_i_address(text: &str) -> Option<CurrencyId> {
    let address: Address = text.parse().ok()?;
    (address.kind() == AddressKind::Identity).then(|| CurrencyId::from_bytes(address.hash()))
}

/// A currency's `i…` address — the form people actually see.
///
/// `CurrencyId` renders through `Display` as **raw hex**, which is the wire
/// form: correct for a script, and not something anyone can paste into an
/// explorer or recognise. Every other Verus tool shows the i-address, and it is
/// the same twenty bytes read the way a person reads them.
pub fn i_address(id: CurrencyId) -> String {
    Address::new(AddressKind::Identity, id.to_bytes()).to_string()
}

fn row(entry: &HistoryEntry, now: i64, named: &BTreeMap<String, String>) -> HistoryRowVm {
    let native_still = entry.net_native == SignedAmount::ZERO;

    // Token legs, formatted. A transaction can move a token and no native
    // value at all, which is not a rare shape — measured against a real
    // testnet address, two of the six most recent transactions were exactly
    // that.
    let currency_lines: Vec<String> = entry
        .net_currencies
        .iter()
        // The currency's NAME where the node has given us one. Showing the tail
        // of an i-address instead reads as an address someone was paid at,
        // which is a different fact entirely.
        .map(|(id, amount)| {
            let label = named.get(id).cloned().unwrap_or_else(|| short(id));
            // The same formatter as the native figures: two formatters side by
            // side on one row is how "9999.99999998" ends up next to
            // "1 383.4051 2553" and neither reads as money.
            format!("{} {}", signed_coins(*amount), label)
        })
        .collect();

    // Direction is not just the sign of the native leg.
    //
    // `is_outgoing` is the SDK's own answer and covers "negative in ANY
    // currency", which the native sign alone misses for a token-only send. On
    // top of that, a transfer between two of our own addresses spends an
    // output and takes the value back, netting to the fee — which reads as
    // "sent" from the sign and is not.
    let direction = if entry.is_outgoing() {
        if entry.spent_something && currency_lines.is_empty() && small(entry.net_native) {
            TxDirection::Self_
        } else {
            TxDirection::Outgoing
        }
    } else if entry.spent_something && native_still && currency_lines.is_empty() {
        TxDirection::Self_
    } else {
        TxDirection::Incoming
    };

    // A token-only transfer showed as "Received +0.0000 0000" — true about the
    // native currency and useless about what happened. When no native value
    // moved, the token line IS the amount.
    let net_display = if native_still && !currency_lines.is_empty() {
        currency_lines.join(" · ")
    } else {
        signed_coins(entry.net_native)
    };

    HistoryRowVm {
        txid: entry.txid.to_string(),
        height: entry.height,
        block_time: entry.block_time,
        direction,
        net_sats: entry.net_native.to_sat().to_string(),
        net_display,
        currency_lines,
        when_display: when(entry.block_time, now),
        // Filled in by `rows`, which can see the row before this one.
        group: String::new(),
        pending: entry.height == 0,
    }
}

/// Whether a negative net is small enough to be just a fee.
///
/// A tenth of a coin. Verus fees are four orders of magnitude below that, so
/// this only ever catches a self-transfer — but it is a heuristic, and it is
/// named as one rather than dressed up as a fact. The transaction detail screen
/// decodes the real inputs and outputs and does not need to guess.
fn small(net: SignedAmount) -> bool {
    net.magnitude().to_sat() < chainvue_protocol::SATS_PER_COIN as u64 / 10
}

/// An i-address squeezed onto one line, for a currency the node would not name.
///
/// Head and tail, not just the tail: the leading `i` is what says this is an
/// identifier at all. A bare `…2xhwfL` reads as the end of an address someone
/// was paid at — which is what it looked like on a real screen.
fn short(id: &str) -> String {
    let characters: Vec<char> = id.chars().collect();
    if characters.len() <= 12 {
        return id.to_string();
    }
    let head: String = characters.iter().take(5).collect();
    let tail: String = characters[characters.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

fn sats(amount: Amount) -> String {
    amount.to_sat().to_string()
}

/// `12482.42` → `12 482.4200 0000`.
///
/// Grouped thousands and the satoshi digits in two blocks of four, because a
/// bare `12482.42000000` is a number nobody can read at a glance and eight
/// decimal places are not optional in a currency where the last one is a
/// meaningful unit.
pub fn coins(amount: Amount) -> String {
    let text = amount.to_coins_string();
    let (whole, fraction) = text.split_once('.').unwrap_or((text.as_str(), ""));
    let mut padded = fraction.to_string();
    while padded.len() < 8 {
        padded.push('0');
    }
    format!("{}.{} {}", group(whole), &padded[..4], &padded[4..])
}

fn signed_coins(amount: SignedAmount) -> String {
    let sign = if amount.is_negative() { "−" } else { "+" };
    format!("{sign}{}", coins(amount.magnitude()))
}

/// `12482` → `12 482`. A thin space would be better typography; a normal space
/// is what every font here actually has.
fn group(digits: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// "2 hours ago" / "yesterday" / "12 March".
///
/// No year for anything inside the last year, because the year is noise until
/// it is not. `block_time` is miner-chosen and only loosely monotonic, so this
/// is display only — never ordering.
fn when(block_time: i64, now: i64) -> String {
    if block_time == 0 {
        return "pending".to_string();
    }

    // A block claiming to be from the future is a miner's clock, not something
    // worth showing anyone — hence the negative arm folding into "just now".
    let age = now.saturating_sub(block_time);
    match age {
        ..60 => "just now".to_string(),
        60..3_600 => plural(age / 60, "minute"),
        3_600..86_400 => plural(age / 3_600, "hour"),
        86_400..172_800 => "yesterday".to_string(),
        172_800..2_592_000 => plural(age / 86_400, "day"),
        _ => plural(age / 2_592_000, "month"),
    }
}

fn plural(count: i64, unit: &str) -> String {
    if count == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{count} {unit}s ago")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coins_are_grouped_and_padded_to_eight_places() {
        assert_eq!(coins(Amount::ZERO), "0.0000 0000");
        assert_eq!(coins(Amount::from_sat(1)), "0.0000 0001");
        assert_eq!(coins(Amount::from_sat(100_000_000)), "1.0000 0000");
        assert_eq!(
            coins(Amount::from_sat(1_248_242_000_000)),
            "12 482.4200 0000"
        );
        assert_eq!(
            coins(Amount::from_sat(u64::MAX)),
            "184 467 440 737.0955 1615"
        );
    }

    /// The formatter must not lose a satoshi, at any magnitude.
    #[test]
    fn every_satoshi_survives_formatting() {
        for sats in [
            0u64,
            1,
            7,
            99_999_999,
            100_000_000,
            100_000_001,
            2_100_000_000_000_000,
            u64::MAX,
        ] {
            let shown = coins(Amount::from_sat(sats));
            let digits: String = shown.chars().filter(char::is_ascii_digit).collect();
            let parsed: u128 = digits.parse().expect("digits");
            assert_eq!(parsed, u128::from(sats), "{sats} rendered as {shown}");
        }
    }

    #[test]
    fn a_signed_amount_carries_its_sign() {
        assert_eq!(signed_coins(SignedAmount::from_sat(100)), "+0.0000 0100");
        assert_eq!(signed_coins(SignedAmount::from_sat(-100)), "−0.0000 0100");
        assert_eq!(signed_coins(SignedAmount::ZERO), "+0.0000 0000");
    }

    #[test]
    fn ages_read_the_way_people_say_them() {
        let now = 1_000_000_000;
        assert_eq!(when(0, now), "pending");
        assert_eq!(when(now, now), "just now");
        assert_eq!(when(now - 60, now), "1 minute ago");
        assert_eq!(when(now - 7_200, now), "2 hours ago");
        assert_eq!(when(now - 90_000, now), "yesterday");
        assert_eq!(when(now - 86_400 * 5, now), "5 days ago");
        assert_eq!(when(now - 86_400 * 90, now), "3 months ago");
        // A block claiming to be from the future is a miner's clock, not a bug
        // worth showing anyone.
        assert_eq!(when(now + 500, now), "just now");
    }

    /// A currency id must reach the screen as an `i…` address.
    ///
    /// It used to reach it as raw hex — `c08de1cb7df3…` under a token's name —
    /// which is the wire form, cannot be pasted into an explorer, and does not
    /// look like anything a Verus user recognises.
    #[test]
    fn a_currency_is_named_by_its_i_address_not_its_bytes() {
        let id = CurrencyId::from_bytes([0xab; 20]);

        // What `Display` gives, and what must NOT be shown.
        assert_eq!(id.to_string(), "ab".repeat(20));

        let shown = i_address(id);
        assert!(shown.starts_with('i'), "{shown}");
        assert!(!shown.contains("abab"), "raw bytes leaked into {shown}");
        assert!(shown.len() < 40, "an i-address is shorter than its hex");
    }

    /// A shortened id has to keep its head, or it reads as the end of somebody's
    /// address rather than as an identifier.
    #[test]
    fn a_shortened_id_keeps_the_leading_i() {
        let full = i_address(CurrencyId::from_bytes([0x11; 20]));
        let brief = short(&full);

        assert!(brief.starts_with('i'), "{brief}");
        assert!(brief.contains('…'));
        assert!(brief.len() < full.len());

        // Short enough already: nothing to elide.
        assert_eq!(short("iShort"), "iShort");
    }

    /// The activity list says what the token IS, when the node has told us.
    #[test]
    fn a_token_movement_is_labelled_with_its_name() {
        use std::collections::BTreeMap;

        let id = CurrencyId::from_bytes([0x42; 20]);
        let address = i_address(id);

        let mut entry = HistoryEntry {
            txid: "0".repeat(64).parse().expect("a txid"),
            height: 100,
            block_index: 0,
            block_time: 1_000,
            // A token transfer moves no native value at all.
            net_native: SignedAmount::ZERO,
            net_currencies: BTreeMap::new(),
            spent_something: false,
        };
        entry
            .net_currencies
            .insert(address.clone(), SignedAmount::from_sat(1_234_500_000_000));

        let mut named = BTreeMap::new();
        named.insert(address.clone(), "mambo".to_string());

        let labelled = row(&entry, 2_000, &named);
        assert_eq!(labelled.net_display, "+12 345.0000 0000 mambo");

        // And without a name, an identifier that still looks like one.
        let anonymous = row(&entry, 2_000, &BTreeMap::new());
        assert!(
            anonymous.net_display.ends_with(&short(&address)),
            "{}",
            anonymous.net_display,
        );
        assert!(
            anonymous.net_display.contains(" i"),
            "{}",
            anonymous.net_display
        );
    }

    /// The bound that a real node rejected.
    ///
    /// Found by the live test, not by reasoning: the arithmetic looked right
    /// and the daemon refused it. A scan that reaches the start of the chain
    /// must ask for height 1, never 0, or its last window fails and takes the
    /// whole page with it.
    #[test]
    fn a_window_never_reaches_down_to_height_zero() {
        assert_eq!(window_start(100, 10), 90);
        assert_eq!(window_start(100, 100), 1, "a window reaching 0 must clamp");
        assert_eq!(
            window_start(100, 1_000_000),
            1,
            "and so must an oversized one"
        );
        assert_eq!(window_start(1, 10), 1);
        assert_eq!(window_start(0, 10), 1);

        // Whatever the inputs, the bound the node sees is a legal one.
        for end in [0u32, 1, 2, 9_999, 1_187_611, u32::MAX] {
            for window in [1u32, WINDOW_BLOCKS, MAX_WINDOW_BLOCKS, u32::MAX] {
                assert!(window_start(end, window) >= 1, "{end}/{window}");
            }
        }
    }

    #[test]
    fn an_addition_that_cannot_be_represented_saturates_rather_than_wrapping() {
        let huge = Amount::from_sat(u64::MAX);
        assert_eq!(add(huge, Amount::from_sat(1)), huge);
    }
}
