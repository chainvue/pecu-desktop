//! Reading the market book, and the chart under it, off a real node.
//!
//! `#[ignore]`d for the reason `live_portfolio.rs` gives: a suite that fails
//! when somebody's wifi drops is a suite people learn to ignore. Run it
//! deliberately:
//!
//! ```sh
//! cargo test -p pecu-core --test live_markets -- --ignored --nocapture
//! ```
//!
//! # What this is for
//!
//! The markets table and its chart are the only figures in this wallet derived
//! from *other people's* balances rather than from the wallet's own, and every
//! one of them comes out of `getcurrencyconverters` and `getcurrencystate`
//! arithmetic that a scripted chain cannot vouch for — the mock answers with
//! whatever it was told to. So the question "is the chart real data" has no
//! answer inside the unit tests, and this is where it gets one.
//!
//! **Read-only.** Every call here is a question. Reaching a `Broadcaster` needs
//! a `SpendPermit`, which cannot be constructed outside `pecu_chain::permit`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use pecu_chain::Chain;
use pecu_core::market::{self, Book, Pool};
use verus_sdk::network::ChainReader;

const TESTNET: &str = "https://api.verustest.net";
/// What prices are quoted in, the same one `Core::QUOTE` uses.
const QUOTE: &str = "DAI.vETH";

/// The book, assembled exactly the way `Core::refresh_markets` assembles it.
///
/// Duplicated rather than reached into, because that function is a method on a
/// private actor that owns channels and a keystore. The duplication is the
/// thing to watch: if the two ever disagree this test is checking a book the
/// screen does not show.
fn read_book(chain: &Chain) -> (Book, Vec<(String, String)>) {
    let native = chain.chain_info().expect("chain info").chain_id;
    let catalog = chain.list_currencies().expect("the currency list");

    let mut converters = chain
        .currency_converters(&[native.as_str()])
        .expect("converters for the chain currency");
    let seen: std::collections::BTreeSet<String> = converters
        .iter()
        .map(|entry| entry.converter_id.clone())
        .collect();
    if let Ok(more) = chain.currency_converters(&[QUOTE]) {
        converters.extend(
            more.into_iter()
                .filter(|entry| !seen.contains(&entry.converter_id)),
        );
    }

    let tip = chain.block_count().expect("a tip");
    let from = tip.saturating_sub(market::WINDOW_DAYS * market::BLOCKS_PER_DAY);

    let mut pools: Vec<Pool> = converters.iter().filter_map(Pool::from_converter).collect();
    for entry in &converters {
        if entry.last_notarization["prelaunch"]
            .as_bool()
            .unwrap_or(false)
        {
            continue;
        }
        let samples = chain
            .currency_state_range(&entry.converter_id, from, tip, market::BLOCKS_PER_DAY)
            .unwrap_or_default();
        if let Some(pool) = pools.iter_mut().find(|pool| pool.id == entry.converter_id) {
            pool.remember(&samples);
        }
    }

    let mut names: Vec<(String, String)> = catalog
        .iter()
        .map(|summary| {
            (
                summary.currency_id.clone(),
                summary.fully_qualified_name.clone(),
            )
        })
        .collect();

    // The bridged currencies `listcurrencies` never returns, asked for by id.
    // The same second pass `Core::refresh_markets` makes, and the reason this
    // test exists: without it the quote currency has no id and nothing on the
    // screen has a price.
    let known: std::collections::BTreeSet<&str> =
        catalog.iter().map(|s| s.currency_id.as_str()).collect();
    for id in market::currencies_in(&converters) {
        if known.contains(id.as_str()) {
            continue;
        }
        if let Ok(def) = chain.currency_definition(&id) {
            names.push((id, def.fully_qualified_name));
        }
    }

    let quote = names
        .iter()
        .find(|(_, name)| name == QUOTE)
        .map(|(id, _)| id.clone())
        .unwrap_or_default();

    (Book::new(pools, quote, native), names)
}

/// The prices on screen are the chain's, and the chart under them is a real
/// series of real notarizations.
///
/// Printed rather than only asserted: the numbers are the evidence, and a green
/// tick that says "some currencies were priced" would pass on a book of
/// nonsense.
#[ignore = "talks to api.verustest.net"]
#[test]
fn the_market_book_and_its_chart_come_off_the_chain() {
    let chain = Chain::live(TESTNET).expect("a client for the testnet endpoint");
    let (book, names) = read_book(&chain);

    let name_of = |id: &str| {
        names
            .iter()
            .find(|(known, _)| known == id)
            .map_or_else(|| id.to_string(), |(_, name)| name.clone())
    };

    let currencies = book.currencies();
    assert!(
        !currencies.is_empty(),
        "no currency on VRSCTEST could be priced at all"
    );

    println!("priced currencies: {}", currencies.len());

    let mut with_a_series = 0usize;
    let mut points_total = 0usize;
    for id in &currencies {
        let price = book.quote_for(id);
        let series = book.series(id);
        points_total += series.len();
        if series.len() > 1 {
            with_a_series += 1;
        }

        let moved = if series.len() > 1 {
            let first = series.first().expect("checked").sats;
            let last = series.last().expect("checked").sats;
            format!("{first} → {last}")
        } else {
            "—".to_string()
        };

        println!(
            "  {:<24} price {:>14}  readings {:>3}  {}",
            name_of(id),
            price.map_or_else(|| "—".to_string(), |q| format!("{:.8}", q.price)),
            series.len(),
            moved,
        );
    }

    // A chart needs at least two readings to be a line rather than a dot. On a
    // quiet testnet most pools publish rarely, so this asserts that *something*
    // has a real series rather than that everything does.
    assert!(
        points_total > 0,
        "no pool on VRSCTEST returned a single reading — `currency_state_range` \
         answered nothing, and the chart would be empty everywhere"
    );
    println!(
        "{with_a_series} of {} have a drawable line",
        currencies.len()
    );
}

/// Every reading in a series is a distinct, increasing moment in real time.
///
/// The trap this exists for: the two-hop path joins a pool's readings to a
/// reference pool's by block time, and joining by *position* instead would plot
/// one currency's price against another currency's clock. That renders as a
/// perfectly ordinary chart.
#[ignore = "talks to api.verustest.net"]
#[test]
fn a_series_is_ordered_in_real_time_and_lands_in_the_window() {
    let chain = Chain::live(TESTNET).expect("a client for the testnet endpoint");
    let (book, _) = read_book(&chain);

    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock after 1970")
            .as_secs(),
    )
    .expect("a clock before the year 292 billion");
    let window = i64::from(market::WINDOW_DAYS) * 86_400;

    let mut checked = 0usize;
    for id in book.currencies() {
        let series = book.series(&id);
        for pair in series.windows(2) {
            assert!(
                pair[1].t > pair[0].t,
                "two readings share a moment, or go backwards: {} then {}",
                pair[0].t,
                pair[1].t,
            );
        }
        for point in &series {
            assert!(
                point.t > now - window - 86_400 && point.t <= now + 86_400,
                "a reading at {} is outside the {}-day window this asked for",
                point.t,
                market::WINDOW_DAYS,
            );
            checked += 1;
        }
    }

    println!("{checked} readings, all inside the window and in order");
}

/// **The bug this whole file was written to find.**
///
/// `listcurrencies` with no query does not mean "every currency". It means
/// `systemtype: "local"` — every currency launched *from this chain* — and a
/// bridged currency was launched from the system it came across, so it is
/// simply absent. On VRSCTEST that is 316 entries that do not include
/// `DAI.vETH`, which is the currency this wallet quotes every price in.
///
/// The effect was total and silent: no id for the quote currency, so no price
/// on any row, no change column, and no chart — on a screen that looked empty
/// rather than broken. The scripted chain never showed it, because it answers
/// with a catalogue containing whatever it was told to contain.
///
/// So this pins the cause rather than the symptom. If somebody later decides
/// the second pass in `Core::refresh_markets` looks redundant and takes it out,
/// this fails and says why.
#[ignore = "talks to api.verustest.net"]
#[test]
fn the_quote_currency_is_not_in_listcurrencies_and_has_to_be_asked_for() {
    let chain = Chain::live(TESTNET).expect("a client for the testnet endpoint");

    let catalog = chain.list_currencies().expect("the currency list");
    assert!(
        !catalog
            .iter()
            .any(|summary| summary.fully_qualified_name == QUOTE),
        "{QUOTE} is in listcurrencies now. If the daemon changed, the second \
         pass in `refresh_markets` may be unnecessary — check before removing it, \
         because the other seven bridged currencies may still be missing."
    );

    // And it exists perfectly well when asked for by name.
    let definition = chain
        .currency_definition(QUOTE)
        .expect("the quote currency exists, it is simply not listed");
    assert_eq!(definition.fully_qualified_name, QUOTE);

    // Which is what the book has to end up with, or nothing is priced.
    let (book, _) = read_book(&chain);
    assert!(
        !book.quote.is_empty(),
        "the book has no quote currency, so every price on the markets screen \
         would be blank"
    );
    assert!(
        book.quote_for(&book.quote).is_some(),
        "the quote currency cannot price itself, which means the book is unusable"
    );
}

/// What the filter takes off the table, on a real chain.
///
/// Printed rather than asserted by count: which currencies have a started
/// basket is a fact about VRSCTEST today and will not hold still. What is
/// asserted is the property — everything the table shows can be converted, and
/// everything it drops cannot.
#[ignore = "talks to api.verustest.net"]
#[test]
fn the_table_drops_only_what_no_basket_can_convert() {
    let chain = Chain::live(TESTNET).expect("a client");
    let (book, names) = read_book(&chain);
    let name_of = |id: &str| {
        names
            .iter()
            .find(|(known, _)| known == id)
            .map_or_else(|| id.to_string(), |(_, name)| name.clone())
    };

    let all = book.currencies();
    let shown = book.convertible();
    let dropped: Vec<&String> = all.iter().filter(|id| !shown.contains(id)).collect();

    println!("in the book {}   on the table {}", all.len(), shown.len());
    println!("dropped:");
    for id in &dropped {
        println!(
            "  {:<22} price={}",
            name_of(id),
            book.quote_for(id)
                .map_or_else(|| "—".to_string(), |q| format!("{:.8}", q.price)),
        );
    }

    for id in &dropped {
        assert!(
            book.quote_for(id).is_none(),
            "{} was dropped and yet has a price, which means a route exists to it",
            name_of(id),
        );
    }
    assert!(
        !shown.is_empty(),
        "the filter took everything, which cannot be right on a chain with markets"
    );
}

#[ignore = "talks to api.verustest.net"]
#[test]
fn what_the_detail_panel_would_get() {
    let chain = Chain::live(TESTNET).expect("a client");
    let (book, names) = read_book(&chain);
    let map: std::collections::BTreeMap<String, String> = names.iter().cloned().collect();
    let now = 1_787_200_000_i64;

    for want in [
        "AMERICA",
        "Bridge.vETH",
        "VRSCTEST",
        "CHIPS",
        "Bridge.Betelgeuse",
    ] {
        let Some((id, _)) = names.iter().find(|(_, n)| n == want) else {
            println!("{want:<20} not in the catalog");
            continue;
        };
        let d = pecu_core::market::detail(&book, id, &map, "DAI.vETH", now);
        println!(
            "{want:<20} price={:<14} change={:<9} series={:<4} venues={} route={}",
            d.price,
            d.change,
            d.series.len(),
            d.venues.len(),
            d.route,
        );
    }
}
