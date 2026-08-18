//! What things are worth, derived from reserve state.
//!
//! # Where a price comes from on this chain
//!
//! There is no feed. A currency on Verus is worth what a fractional currency
//! holding it will give you, and a fractional currency publishes its reserves,
//! its supply and its weights in every notarization. So a price here is not
//! reported — it is *derived*, and the derivation is short enough to state:
//!
//! For a basket `B` holding reserve `R`, the notarization's `priceinreserve`
//! says what **one unit of B** is worth in `R`. Two reserves of the same basket
//! therefore price against each other by division:
//!
//! ```text
//! price(X in Q)  =  priceinreserve(Q) / priceinreserve(X)
//! ```
//!
//! and the basket itself is priced by `priceinreserve(Q)` alone, since one unit
//! of it is exactly what that figure describes.
//!
//! Measured against the node's own estimator on VRSCTEST: this yields
//! `1 VRSCTEST = 0.53722594 DAI.vETH`, where `estimateconversion` answers
//! `0.53694578`. The gap is 0.052%, which is the 0.05% conversion fee the
//! estimate has already taken off — so what is derived here is the **mid
//! price**, before fees, and the two agree to the extent they should.
//!
//! # Why f64, in a workspace that keeps money in satoshis
//!
//! Because these are not amounts. Nothing here is signed, spent, or added to a
//! balance; a price is a ratio of two figures the daemon itself publishes as
//! JSON floats, so there is no exact integer being given up. The rule the rest
//! of the workspace enforces — never let a float near an amount somebody can
//! lose — is untouched: [`crate::send`] still builds from `Amount`, and no
//! number computed in this module reaches a transaction.
//!
//! # What is honestly missing
//!
//! **The 24-hour change.** `getcurrencystate` will answer for a height range
//! and that is exactly the series both the change column and a price chart
//! need, but the SDK's `ChainReader::currency_state` takes a currency and no
//! range, and its client offers no escape hatch to pass one. So every row's
//! change is `"—"` and its tone is `"unknown"` — which the markets screen is
//! built to show, and which is the truth. See `docs/LATER.md`.

use std::collections::BTreeMap;

use pecu_protocol::{MarketDetailVm, MarketRowVm, StatVm, VenueVm};
use verus_sdk::network::CurrencyConverter;

/// What the interface shows where the wallet does not know.
pub const UNKNOWN: &str = "—";

/// How far a price may move before [`depth`] stops counting.
const TOLERANCE: f64 = 0.02;

/// One reserve currency inside a pool, as the notarization reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Reserve {
    pub id: String,
    /// What one unit of the pool's own currency is worth in this reserve.
    pub price_in_reserve: f64,
    /// How much of it the pool is holding.
    pub held: f64,
    /// Its share of the basket, in `0..1`.
    pub weight: f64,
}

/// A fractional currency, and the state it last published.
#[derive(Clone, Debug, PartialEq)]
pub struct Pool {
    pub id: String,
    /// Fully qualified, as the chain spells it.
    pub name: String,
    /// Whether it has finished launching. A pre-launch basket quotes the price
    /// its initial contributions imply, which is a real figure about a market
    /// that does not exist yet — so it is listed and never priced from.
    pub started: bool,
    /// The block its state was taken at. Shown, because a price is only as
    /// current as the notarization it came out of.
    pub height: u32,
    pub supply: f64,
    pub reserves: Vec<Reserve>,
}

impl Pool {
    /// Read a pool out of what `getcurrencyconverters` answered.
    ///
    /// `None` when the entry carries no reserve state — a converter that has
    /// never notarized has nothing to price with, and inventing a zero for it
    /// would put a currency on screen at a price of nothing.
    pub fn from_converter(entry: &CurrencyConverter) -> Option<Self> {
        let notarization = &entry.last_notarization;
        let state = notarization.get("currencystate")?;
        let reserves: Vec<Reserve> = state
            .get("reservecurrencies")?
            .as_array()?
            .iter()
            .filter_map(|reserve| {
                Some(Reserve {
                    id: reserve.get("currencyid")?.as_str()?.to_string(),
                    price_in_reserve: reserve.get("priceinreserve")?.as_f64()?,
                    held: reserve.get("reserves")?.as_f64()?,
                    weight: reserve.get("weight")?.as_f64()?,
                })
            })
            .collect();

        if reserves.is_empty() {
            return None;
        }

        Some(Pool {
            id: state
                .get("currencyid")
                .and_then(|id| id.as_str())
                .unwrap_or(&entry.converter_id)
                .to_string(),
            name: entry.name.clone(),
            // Absent means launched: the flag is only written while a basket is
            // still in its pre-launch window.
            started: !notarization
                .get("prelaunch")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            height: entry.height,
            supply: state
                .get("supply")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or_default(),
            reserves,
        })
    }

    pub fn reserve(&self, id: &str) -> Option<&Reserve> {
        self.reserves.iter().find(|reserve| reserve.id == id)
    }

    /// Whether this pool can price `id` at all.
    ///
    /// **A basket trades its own currency as well as its reserves** — the same
    /// trap the SDK documents on `CurrencyConverter::reserves`. Testing the
    /// reserve list alone would drop every basket from its own market.
    pub fn trades(&self, id: &str) -> bool {
        self.id == id || self.reserve(id).is_some()
    }

    /// What one `target` is worth in `quote`, within this pool.
    ///
    /// `None` when the pool does not hold both, or when the divisor is zero —
    /// a reserve priced at nothing gives no ratio, and a very large number
    /// there would be an artefact rather than a price.
    pub fn price(&self, target: &str, quote: &str) -> Option<f64> {
        let quoted = self.reserve(quote)?.price_in_reserve;
        if !quoted.is_finite() || quoted <= 0.0 {
            return None;
        }
        if target == self.id {
            return Some(quoted);
        }
        let priced = self.reserve(target)?.price_in_reserve;
        if !priced.is_finite() || priced <= 0.0 {
            return None;
        }
        Some(quoted / priced)
    }

    /// How much `target` can move through this pool before its price shifts by
    /// [`TOLERANCE`], in units of `target`.
    ///
    /// # The derivation
    ///
    /// A fractional currency prices a reserve `R` of weight `w` against a
    /// supply `S` as `R / (S · w)`. Adding `Δ` to the reserve mints
    /// `S·((1 + Δ/R)^w − 1)`, so with `d = Δ/R` the price afterwards is
    ///
    /// ```text
    /// R(1 + d) / (S(1 + d)^w · w)  =  price · (1 + d)^(1 − w)
    /// ```
    ///
    /// which reaches `1 + TOLERANCE` at `d = (1 + TOLERANCE)^(1/(1 − w)) − 1`.
    /// At the quarter weight every basket on VRSCTEST uses, that is 2.68% of
    /// the reserve — so the figure is close to, but not the same as, "2% of
    /// what is in the pool", and the difference grows with the weight.
    ///
    /// For the pool's **own** currency there is no reserve entry to measure, so
    /// the quote side's room is converted into units of the basket. That
    /// answers the same question in the same denomination as the price beside
    /// it: how much of this can be moved against the quote currency.
    pub fn depth(&self, target: &str, quote: &str) -> Option<f64> {
        if target == self.id {
            let room = self.depth(quote, quote)?;
            let price = self.price(target, quote)?;
            return Some(room / price);
        }
        let reserve = self.reserve(target)?;
        if !(0.0..1.0).contains(&reserve.weight) || reserve.weight <= 0.0 {
            return None;
        }
        if !reserve.held.is_finite() || reserve.held <= 0.0 {
            return None;
        }
        let share = (1.0 + TOLERANCE).powf(1.0 / (1.0 - reserve.weight)) - 1.0;
        Some(reserve.held * share)
    }
}

/// A price, and the pools it was reached through.
#[derive(Clone, Debug, PartialEq)]
pub struct Quote {
    pub price: f64,
    /// In units of the currency being quoted. `None` when the pool it came
    /// through has no measurable room.
    pub depth: Option<f64>,
    /// The pools crossed, in order. One for a direct quote, two for a hop
    /// through the chain's own currency.
    pub via: Vec<String>,
    /// The block the least recent of those pools last notarized at — the age of
    /// the whole answer, not of its freshest part.
    pub height: u32,
}

/// Every pool the wallet knows about, and what it can price against what.
#[derive(Clone, Debug, Default)]
pub struct Book {
    pub pools: Vec<Pool>,
    /// The currency prices are expressed in. DAI.vETH on this chain — a dollar
    /// stablecoin is the only thing here that a person reads as a price.
    pub quote: String,
    /// The chain's own currency, which is the reserve nearly every basket
    /// holds and therefore the one useful intermediate hop.
    pub chain: String,
}

impl Book {
    pub fn new(pools: Vec<Pool>, quote: String, chain: String) -> Self {
        Book {
            pools,
            quote,
            chain,
        }
    }

    /// Every currency any pool can price, quote included.
    pub fn currencies(&self) -> Vec<String> {
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for pool in &self.pools {
            seen.insert(pool.id.clone());
            for reserve in &pool.reserves {
                seen.insert(reserve.id.clone());
            }
        }
        seen.into_iter().collect()
    }

    /// Every started pool that trades `target`, deepest first.
    ///
    /// Depth rather than name: which pool a price is taken from decides the
    /// price, and the deepest one is the one a transaction of any size would
    /// actually route through.
    fn pools_for(&self, target: &str) -> Vec<&Pool> {
        let mut pools: Vec<&Pool> = self
            .pools
            .iter()
            .filter(|pool| pool.started && pool.trades(target))
            .collect();
        pools.sort_by(|a, b| {
            let depth = |pool: &Pool| pool.depth(target, &self.quote).unwrap_or_default();
            depth(b)
                .partial_cmp(&depth(a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        });
        pools
    }

    /// The reference hop: the chain's own currency, priced in the quote.
    ///
    /// Everything that does not trade against the quote directly is priced
    /// through this. Taken once, from the deepest pool holding both.
    fn reference(&self) -> Option<(&Pool, f64)> {
        self.pools
            .iter()
            .filter(|pool| pool.started && pool.trades(&self.chain) && pool.trades(&self.quote))
            .filter_map(|pool| Some((pool, pool.price(&self.chain, &self.quote)?)))
            .max_by(|(a, _), (b, _)| {
                let depth = |pool: &Pool| pool.depth(&self.chain, &self.quote).unwrap_or_default();
                depth(a)
                    .partial_cmp(&depth(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    /// What `target` is worth in the quote currency, and how that was reached.
    ///
    /// Direct first — a pool holding both sides needs no intermediate and
    /// cannot compound two stale notarizations. Otherwise one hop through the
    /// chain currency, which is the reserve nearly everything here holds.
    /// Nothing beyond two hops: a third would multiply a third notarization's
    /// age into the answer for a currency almost nobody holds.
    pub fn quote_for(&self, target: &str) -> Option<Quote> {
        if target == self.quote {
            // The quote currency is worth one of itself, definitionally. Its
            // depth still comes from a pool, because "how much can move" is a
            // question about a market rather than about a definition.
            let pool = self.pools_for(target).into_iter().next();
            return Some(Quote {
                price: 1.0,
                depth: pool.and_then(|pool| pool.depth(target, &self.quote)),
                via: pool.map(|pool| vec![pool.name.clone()]).unwrap_or_default(),
                height: pool.map(|pool| pool.height).unwrap_or_default(),
            });
        }

        let candidates = self.pools_for(target);

        if let Some(pool) = candidates
            .iter()
            .find(|pool| pool.trades(&self.quote))
            .copied()
        {
            if let Some(price) = pool.price(target, &self.quote) {
                return Some(Quote {
                    price,
                    depth: pool.depth(target, &self.quote),
                    via: vec![pool.name.clone()],
                    height: pool.height,
                });
            }
        }

        let (reference, reference_price) = self.reference()?;
        let pool = candidates
            .iter()
            .find(|pool| pool.trades(&self.chain) && pool.id != reference.id)
            .copied()?;
        let priced = pool.price(target, &self.chain)?;

        Some(Quote {
            price: priced * reference_price,
            depth: pool.depth(target, &self.chain).map(|room| room / priced),
            via: vec![pool.name.clone(), reference.name.clone()],
            height: pool.height.min(reference.height),
        })
    }

    /// Every pool that trades `target`, started or not, with its own price.
    ///
    /// The unstarted ones are the point of showing this at all: two pools
    /// quoting the same currency at prices five times apart is a fact about
    /// where a conversion should go, and it is invisible from one row.
    pub fn venues(&self, target: &str) -> Vec<(&Pool, Option<f64>)> {
        let mut venues: Vec<(&Pool, Option<f64>)> = self
            .pools
            .iter()
            .filter(|pool| pool.trades(target))
            .map(|pool| {
                let direct = pool.price(target, &self.quote);
                let hopped = || {
                    let (reference, reference_price) = self.reference()?;
                    if pool.id == reference.id {
                        return None;
                    }
                    Some(pool.price(target, &self.chain)? * reference_price)
                };
                (pool, direct.or_else(hopped))
            })
            .collect();
        venues
            .sort_by(|(a, _), (b, _)| b.started.cmp(&a.started).then_with(|| a.name.cmp(&b.name)));
        venues
    }
}

/// The markets table.
///
/// `names` maps i-address to the name to show. A currency the catalog has never
/// heard of keeps its i-address, which is ugly and true — the alternative is a
/// row labelled with a guess.
pub fn rows(book: &Book, names: &BTreeMap<String, String>) -> Vec<MarketRowVm> {
    let mut rows: Vec<MarketRowVm> = book
        .currencies()
        .into_iter()
        .map(|address| {
            let quote = book.quote_for(&address);
            MarketRowVm {
                name: names
                    .get(&address)
                    .cloned()
                    .unwrap_or_else(|| address.clone()),
                price: quote.as_ref().map_or_else(
                    || UNKNOWN.to_string(),
                    |quote| pecu_protocol::format::price(quote.price),
                ),
                depth: quote
                    .as_ref()
                    .and_then(|quote| quote.depth)
                    .map_or_else(|| UNKNOWN.to_string(), pecu_protocol::format::approx),
                // No series, so no change, so no colour. See the module docs.
                change: UNKNOWN.to_string(),
                tone: "unknown".to_string(),
                address,
            }
        })
        .collect();

    // Priced first, then alphabetically. A table whose first screenful is
    // currencies nothing will quote is a table nobody scrolls.
    rows.sort_by(|a, b| {
        let unpriced = |row: &MarketRowVm| row.price == UNKNOWN;
        unpriced(a)
            .cmp(&unpriced(b))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    rows
}

/// One currency in full.
pub fn detail(
    book: &Book,
    address: &str,
    names: &BTreeMap<String, String>,
    quote_name: &str,
) -> MarketDetailVm {
    let name = names
        .get(address)
        .cloned()
        .unwrap_or_else(|| address.to_string());
    let quote = book.quote_for(address);
    let venues = book.venues(address);
    let started = venues.iter().filter(|(pool, _)| pool.started).count();

    let mut stats = vec![StatVm {
        label: format!("Exit @2% · {name}"),
        value: quote
            .as_ref()
            .and_then(|quote| quote.depth)
            .map_or_else(|| UNKNOWN.to_string(), pecu_protocol::format::approx),
    }];
    if let Some(pool) = book.pools.iter().find(|pool| pool.id == address) {
        stats.push(StatVm {
            label: "Supply".to_string(),
            value: pecu_protocol::format::approx(pool.supply),
        });
        stats.push(StatVm {
            label: "Reserves".to_string(),
            value: pool.reserves.len().to_string(),
        });
    } else {
        stats.push(StatVm {
            label: "Venues".to_string(),
            value: format!("{started} of {}", venues.len()),
        });
        stats.push(StatVm {
            label: "Quoted in".to_string(),
            value: quote_name.to_string(),
        });
    }

    let route = match quote.as_ref() {
        Some(quote) if !quote.via.is_empty() => {
            let mut hops = vec![name.clone()];
            hops.extend(quote.via.iter().cloned());
            hops.push(quote_name.to_string());
            hops.join("  →  ")
        }
        _ => String::new(),
    };

    let route_note = match quote.as_ref() {
        Some(quote) => format!(
            "Mid price from reserve state notarized at block {}, before the conversion fee.",
            pecu_protocol::format::group(&quote.height.to_string())
        ),
        None => format!("No started pool holds both {name} and {quote_name}."),
    };

    MarketDetailVm {
        name,
        subtitle: format!(
            "{} · {} of {} venue{} trading",
            address,
            started,
            venues.len(),
            if venues.len() == 1 { "" } else { "s" }
        ),
        price: quote.as_ref().map_or_else(
            || UNKNOWN.to_string(),
            |quote| pecu_protocol::format::price(quote.price),
        ),
        change: UNKNOWN.to_string(),
        tone: "unknown".to_string(),
        stats,
        venues: venues
            .into_iter()
            .map(|(pool, price)| VenueVm {
                name: pool.name.clone(),
                state: if pool.started { "active" } else { "unstarted" }.to_string(),
                price: price.map_or_else(|| UNKNOWN.to_string(), pecu_protocol::format::price),
                change: UNKNOWN.to_string(),
                tone: "unknown".to_string(),
                depth: pool
                    .depth(address, &book.quote)
                    .or_else(|| {
                        let priced = pool.price(address, &book.chain)?;
                        Some(pool.depth(address, &book.chain)? / priced)
                    })
                    .map_or_else(|| UNKNOWN.to_string(), pecu_protocol::format::approx),
            })
            .collect(),
        route,
        route_note,
    }
}

#[cfg(test)]
// Every float here is transcribed from a `getcurrencyconverters` reply. Digit
// separators would make them harder to check against the daemon, which is the
// only thing that makes them worth having.
#[allow(clippy::unreadable_literal)]
mod tests {
    use super::*;

    // The four i-addresses this chain's markets are actually built on.
    const VRSCTEST: &str = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq";
    const DAI: &str = "iN9vbHXexEh6GTZ45fRoJGKTQThfbgUwMh";
    const VETH: &str = "iCtawpxUiCc2sEupt7Z4u8SDAncGZpgSKm";
    const MKR: &str = "i3WBJ7xEjTna5345D7gPnK4nKfbEBujZqL";
    const BRIDGE: &str = "iSojYsotVzXz4wh2eJriASGo6UidJDDhL2";

    /// Bridge.vETH as VRSCTEST reported it, figure for figure.
    ///
    /// Not invented. This is `getcurrencyconverters ["VRSCTEST"]` for the entry
    /// whose reserves are the four above, and the prices in it are what the
    /// assertions below are measured against — including the one that was
    /// checked against the node's own `estimateconversion`.
    fn bridge() -> CurrencyConverter {
        converter(
            BRIDGE,
            "Bridge.vETH",
            1_156_331,
            false,
            14_147.66083307,
            &[
                (VRSCTEST, 13.19790409, 46_679.8676944, 0.25),
                (DAI, 7.09025643, 25_077.63581784, 0.25),
                (MKR, 0.00387142, 13.6929058, 0.25),
                (VETH, 0.00346814, 12.26653719, 0.25),
            ],
        )
    }

    /// Bridge.Betelgeuse: a pool that never launched, quoting VRSCTEST five
    /// times higher than the market does. It exists in this test because it is
    /// the case a naive "first pool that holds both" would price from.
    fn betelgeuse() -> CurrencyConverter {
        converter(
            "iPxbKpFzNbaSF2jshZEkk14vFG3tWzvsFB",
            "Bridge.Betelgeuse",
            243_201,
            true,
            100_000.0,
            &[(VRSCTEST, 0.02, 500.0, 0.25), (DAI, 0.1, 2_500.0, 0.25)],
        )
    }

    fn converter(
        id: &str,
        name: &str,
        height: u32,
        prelaunch: bool,
        supply: f64,
        reserves: &[(&str, f64, f64, f64)],
    ) -> CurrencyConverter {
        let reserves: Vec<serde_json::Value> = reserves
            .iter()
            .map(|(id, price, held, weight)| {
                serde_json::json!({
                    "currencyid": id,
                    "priceinreserve": price,
                    "reserves": held,
                    "weight": weight,
                })
            })
            .collect();
        CurrencyConverter {
            converter_id: id.to_string(),
            name: name.to_string(),
            height,
            reserves: reserves
                .iter()
                .map(|r| r["currencyid"].as_str().unwrap_or_default().to_string())
                .collect(),
            definition: serde_json::Value::Null,
            last_notarization: serde_json::json!({
                "prelaunch": prelaunch,
                "currencystate": {
                    "currencyid": id,
                    "supply": supply,
                    "reservecurrencies": reserves,
                },
            }),
        }
    }

    fn book() -> Book {
        let pools = [bridge(), betelgeuse()]
            .iter()
            .filter_map(Pool::from_converter)
            .collect();
        Book::new(pools, DAI.to_string(), VRSCTEST.to_string())
    }

    fn names() -> BTreeMap<String, String> {
        [
            (VRSCTEST, "VRSCTEST"),
            (DAI, "DAI.vETH"),
            (VETH, "vETH"),
            (MKR, "MKR.vETH"),
            (BRIDGE, "Bridge.vETH"),
        ]
        .into_iter()
        .map(|(id, name)| (id.to_string(), name.to_string()))
        .collect()
    }

    /// The number this whole module exists to produce, checked against the
    /// daemon rather than against itself.
    ///
    /// `estimateconversion` on VRSCTEST answered `0.53694578` for one VRSCTEST
    /// into DAI.vETH via Bridge.vETH at the same state. The 0.052% between them
    /// is the 0.05% conversion fee that estimate has already deducted — so this
    /// is the mid price, and agreeing to within the fee is agreeing.
    #[test]
    fn one_vrsctest_is_priced_in_dai_the_way_the_node_prices_it() {
        let derived = book().quote_for(VRSCTEST).expect("a price").price;

        assert!(
            (derived - 0.53722594).abs() < 1e-8,
            "derived {derived}, expected 0.53722594"
        );

        let estimated = 0.53694578;
        let fee = (derived - estimated) / derived;
        assert!(
            (0.0004..0.0006).contains(&fee),
            "the gap to the node's estimate should be the conversion fee, was {fee}"
        );
    }

    /// A pool that has not started quotes a real figure about a market that
    /// does not exist. Betelgeuse says 5.00 DAI to the VRSCTEST — twenty-five
    /// times over — and pricing from it would be pricing from nothing.
    #[test]
    fn an_unstarted_pool_never_sets_a_price() {
        let quote = book().quote_for(VRSCTEST).expect("a price");
        assert_eq!(quote.via, vec!["Bridge.vETH".to_string()]);
        assert!(quote.price < 1.0, "{}", quote.price);
    }

    /// …and is still listed, because two pools disagreeing by a factor of
    /// twenty-five is the fact the venue list is there to show.
    #[test]
    fn an_unstarted_pool_is_still_a_venue() {
        let book = book();
        let venues = book.venues(VRSCTEST);
        let states: Vec<&str> = venues
            .iter()
            .map(|(pool, _)| if pool.started { "active" } else { "unstarted" })
            .collect();
        assert_eq!(states, vec!["active", "unstarted"]);

        let (_, betelgeuse_price) = venues
            .iter()
            .find(|(pool, _)| pool.name == "Bridge.Betelgeuse")
            .expect("listed");
        assert_eq!(*betelgeuse_price, Some(5.0));
    }

    /// The quote currency is worth one of itself. A book that priced DAI at
    /// 0.9998 through its own reserves would be showing rounding as a market.
    #[test]
    fn the_quote_currency_is_exactly_one() {
        // Exactly, not approximately. The whole reason this case is special is
        // that dividing DAI's own reserve figure by itself would land near one
        // and show rounding as a market.
        let price = book().quote_for(DAI).expect("a price").price;
        assert!((price - 1.0).abs() < f64::EPSILON, "{price}");
    }

    /// A basket is priced as one unit of itself, which is what
    /// `priceinreserve` already says — no division.
    #[test]
    fn a_basket_is_priced_by_its_own_reserve_figure() {
        let quote = book().quote_for(BRIDGE).expect("a price");
        assert!((quote.price - 7.09025643).abs() < 1e-8, "{}", quote.price);
    }

    /// The 2% figure is the Bancor curve's, not two percent of the pool. At the
    /// quarter weight every basket here uses it comes out at 2.6756%.
    #[test]
    fn depth_follows_the_curve_rather_than_the_reserve() {
        let pool = Pool::from_converter(&bridge()).expect("state");

        let held = 46_679.8676944_f64;
        let depth = pool.depth(VRSCTEST, DAI).expect("a depth");
        assert!((depth - held * 0.026756).abs() < 0.5, "{depth}");
        // Distinctly more than a flat 2% of the reserve — that is the point.
        assert!(depth > held * 0.02 * 1.3, "{depth}");
    }

    /// A currency the book cannot reach is `—`, not zero, and says so where the
    /// route would have been.
    #[test]
    fn an_unreachable_currency_is_unknown_rather_than_free() {
        let book = book();
        let stranger = "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv";
        assert!(book.quote_for(stranger).is_none());

        let detail = detail(&book, stranger, &names(), "DAI.vETH");
        assert_eq!(detail.price, UNKNOWN);
        assert_eq!(detail.route, "");
        assert!(
            detail.route_note.contains("No started pool"),
            "{}",
            detail.route_note
        );
    }

    /// Priced rows come first. A table whose first screenful is dashes is a
    /// table nobody scrolls.
    ///
    /// The one row that lands at the bottom here is Betelgeuse's own basket
    /// currency: no *started* pool trades it, so nothing can price it — which
    /// is the same rule that kept it from setting VRSCTEST's price, seen from
    /// the other side.
    #[test]
    fn the_table_puts_what_it_knows_first() {
        let rows = rows(&book(), &names());
        let priced = rows.iter().take_while(|row| row.price != UNKNOWN).count();

        assert_eq!(rows.len(), 6);
        assert_eq!(priced, 5);
        assert_eq!(rows[5].address, "iPxbKpFzNbaSF2jshZEkk14vFG3tWzvsFB");

        // Unknown to the catalog, so it keeps its i-address rather than being
        // given a name nobody checked.
        assert_eq!(rows[5].name, rows[5].address);

        assert!(rows.iter().any(|row| row.name == "MKR.vETH"));
        assert!(rows.iter().all(|row| row.change == UNKNOWN));
        assert!(rows.iter().all(|row| row.tone == "unknown"));
    }

    /// The route is the promise this screen makes: a price you can check.
    #[test]
    fn the_route_names_every_hop_and_the_block_it_was_read_at() {
        let detail = detail(&book(), VRSCTEST, &names(), "DAI.vETH");
        assert_eq!(detail.route, "VRSCTEST  →  Bridge.vETH  →  DAI.vETH");
        assert!(
            detail.route_note.contains("1 156 331"),
            "{}",
            detail.route_note
        );
        assert!(detail.route_note.contains("before the conversion fee"));
    }

    /// A converter that has never notarized has nothing to price with, and is
    /// not a pool holding zeroes.
    #[test]
    fn a_converter_without_state_is_not_a_pool() {
        let mut empty = bridge();
        empty.last_notarization = serde_json::Value::Null;
        assert!(Pool::from_converter(&empty).is_none());
    }
}
