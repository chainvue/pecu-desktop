//! Reading a real address off a real node.
//!
//! `#[ignore]`d, because a test suite that fails when someone's wifi drops is a
//! test suite people learn to ignore. Run it deliberately:
//!
//! ```sh
//! cargo test -p pecu-core --test live_portfolio -- --ignored --nocapture
//! ```
//!
//! **Read-only.** Nothing here can spend: `portfolio::read` takes a `&Chain`
//! and every call it makes is a question. Reaching a `Broadcaster` needs a
//! `SpendPermit`, which cannot be constructed outside `pecu_chain::permit`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use pecu_chain::Chain;
use pecu_core::portfolio;

const TESTNET: &str = "https://api.verustest.net";

/// A transparent address from the SDK's own fixtures. Whether it holds anything
/// is not the point — what is being checked is that six real requests come back
/// and add up.
///
/// Override it with `PECU_LIVE_ADDRESS` to point this at an address that
/// actually holds tokens, which is the only way to see the currency-naming path
/// run against real data. Deliberately **not** a constant in the file: nobody's
/// address belongs in a committed test.
fn address() -> String {
    std::env::var("PECU_LIVE_ADDRESS")
        .unwrap_or_else(|_| "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX".to_string())
}

#[ignore = "talks to api.verustest.net"]
#[test]
fn a_real_address_reads_coherently() {
    let chain = Chain::live(TESTNET).expect("a client for the testnet endpoint");

    let reading = portfolio::read(&chain, &[address()], portfolio::Cached::default());

    assert!(
        reading.failure.is_none(),
        "the balance read failed: {:?}",
        reading.failure,
    );

    // The tip has to be a real height. Zero would mean nothing answered, and
    // every maturity decision is made against this number.
    assert!(reading.tip > 1_000_000, "tip looks wrong: {}", reading.tip);

    // The chain's own currency id is what tells a token apart from the native
    // leg of a reserve output. Without it the asset list is guesswork.
    assert!(
        reading.native.is_some(),
        "the chain's own currency id was not learned",
    );

    let history = reading
        .history
        .as_ref()
        .expect("the node should answer getaddressdeltas");

    let portfolio = reading.portfolio("VRSCTEST");
    assert!(!portfolio.stale);
    assert!(
        portfolio.assets.first().is_some_and(|asset| asset.native),
        "the native currency must sort first",
    );

    // Every figure crosses the boundary twice: once as satoshis, once as text.
    // They have to agree, or the screen and the arithmetic have diverged.
    let balance = &portfolio.balance;
    let sats: u64 = balance
        .total_sats
        .parse()
        .expect("total is a satoshi count");
    let digits: String = balance
        .total_display
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    assert_eq!(
        digits.parse::<u128>().expect("digits"),
        u128::from(sats),
        "{} and {} disagree",
        balance.total_sats,
        balance.total_display,
    );

    println!("tip          {}", reading.tip);
    println!("native       {:?}", reading.native);
    println!("total        {}", balance.total_display);
    println!("spendable    {}", balance.spendable_display);
    println!("maturing     {}", balance.immature_display);
    println!("assets       {}", portfolio.assets.len());
    for asset in &portfolio.assets {
        // The line that was wrong on a real screen: a currency has to be named
        // by its `i…` address, never by the raw twenty bytes behind it.
        assert!(
            asset.native || asset.currency_id.starts_with('i'),
            "a currency reached the screen as {}",
            asset.currency_id,
        );
        println!(
            "  {:<24} {:>22}  {}",
            asset.name, asset.amount_display, asset.currency_id
        );
    }
    println!("transactions {}", history.len());
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock after 1970")
            .as_secs(),
    )
    .expect("a year before 292 billion AD");

    for row in reading.rows(now).into_iter().take(portfolio::RECENT) {
        if !row.group.code.is_empty() {
            println!("  — {:?}", row.group);
        }
        println!("  {:?} {} {}", row.when_display, row.net_display, row.txid);
    }
}

/// The chart's anchor, checked against a real chain.
///
/// # Why this needs real data
///
/// The balance chart is built by walking **backwards** from the balance the
/// wallet holds now, subtracting each transaction on the way down. That is only
/// correct if the figure it starts from is exactly what the confirmed history
/// sums to — and there are four plausible candidates for that figure, differing
/// by whatever is maturing and whatever an unconfirmed transaction has already
/// spent.
///
/// Pick the wrong one and nothing looks broken. The chart is still a plausible
/// staircase; it just sits a constant distance below the number printed above
/// it, and a constant offset is invisible. No unit test catches this, because a
/// unit test asserts the arithmetic against the same assumption that produced
/// it. Only a real address with real history can disagree.
#[ignore = "talks to api.verustest.net"]
#[test]
fn the_chart_starts_where_the_history_adds_up_to() {
    let chain = Chain::live(TESTNET).expect("a client for the testnet endpoint");
    let reading = portfolio::read(&chain, &[address()], portfolio::Cached::default());

    assert!(reading.failure.is_none(), "{:?}", reading.failure);
    let history = reading.history.as_ref().expect("the activity list");

    // What the chart hangs from: confirmed coins, spendable or not, including
    // any an unconfirmed transaction has claimed — the transaction spending
    // them is not in this history either.
    let anchor = i64::try_from(
        reading.spendable.to_sat() + reading.immature.to_sat() + reading.pending_out.to_sat(),
    )
    .expect("a balance inside i64");

    // Only confirmed movements. An unconfirmed one has no block time and is not
    // in the anchor, so it has no place on a time axis.
    let deltas: Vec<(i64, i64)> = history
        .iter()
        .filter(|entry| entry.height > 0)
        .map(|entry| (entry.block_time, entry.net_native.to_sat()))
        .collect();

    let moved: i64 = deltas.iter().map(|(_, net)| net).sum();
    let points = pecu_chart::from_deltas(1_800_000_000, anchor, &deltas);

    let first = points.first().expect("a series").value;
    let last = points.last().expect("a series").value;

    // The last point is the balance now, by construction.
    assert_eq!(last, anchor, "the chart does not end where the wallet is");

    // And the first is what was held before the scanned window — which must be
    // the current balance minus everything that moved inside it. If the anchor
    // included something the history does not account for, this comes out
    // non-zero on a wallet whose whole history is inside the window, and
    // negative on one where the extra is larger than what moved.
    assert_eq!(
        first,
        anchor - moved,
        "the chart's starting balance does not follow from what moved",
    );

    println!("anchor       {anchor} sats");
    println!("moved        {moved} sats over {} confirmed", deltas.len());
    println!("chart starts {first} sats");
    println!("chart ends   {last} sats");

    // A wallet whose entire history fits in the scanned window started at
    // nothing. Reported rather than asserted: the window is ten thousand
    // blocks, so an older address legitimately starts above zero and this test
    // must not fail for having been pointed at one.
    if reading.reached_start {
        assert_eq!(
            first, 0,
            "the whole chain was scanned, so the wallet must start empty",
        );
        println!("scan reached the start of the chain, and the series begins at zero");
    } else {
        println!("scan has not reached the chain start; a non-zero start is correct");
    }
}

/// A real read must actually reach the cache, and come back out of it.
///
/// # Why this test exists
///
/// It was written after finding that it did not. The restore path was built
/// and tested, the write path was not wired at all, and everything looked
/// right: the cold-start test seeded the database itself, so it passed against
/// a cache nothing ever filled. A round trip through real data is the only
/// shape of test that could have caught that.
#[ignore = "talks to api.verustest.net"]
#[test]
fn a_real_read_round_trips_through_the_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    let chain = Chain::live(TESTNET).expect("a client for the testnet endpoint");

    let reading = portfolio::read(&chain, &[address()], portfolio::Cached::default());
    assert!(reading.failure.is_none(), "{:?}", reading.failure);

    let now = 1_800_000_000;
    let portfolio_vm = reading.portfolio("VRSCTEST");
    let rows = reading.rows(now);

    {
        let store = pecu_store::Store::open(dir.path()).expect("store");
        store.save_snapshot(&portfolio_vm, &rows, now);
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

    // A different process would see exactly this.
    let store = pecu_store::Store::open(dir.path()).expect("reopen");

    let restored = store.snapshot().expect("the snapshot was written");
    assert_eq!(
        restored.portfolio.balance.total_display,
        portfolio_vm.balance.total_display,
    );
    assert_eq!(restored.history.len(), rows.len());

    // The caches that make the next refresh cheap.
    let native = store
        .native_currency()
        .expect("the native currency was cached");
    assert!(native.starts_with('i'), "{native}");
    assert_eq!(
        portfolio::currency_from_i_address(&native),
        reading.native,
        "the currency id did not survive the round trip",
    );

    for (id, name) in &reading.names {
        assert_eq!(
            store.currency_names().get(&portfolio::i_address(*id)),
            Some(name),
            "a currency name was not cached",
        );
    }

    println!("cached {} rows, {} names", rows.len(), reading.names.len());
}
