//! Reading a real address off a real node.
//!
//! `#[ignore]`d, because a test suite that fails when someone's wifi drops is a
//! test suite people learn to ignore. Run it deliberately:
//!
//! ```sh
//! cargo test -p chainvue-core --test live_portfolio -- --ignored --nocapture
//! ```
//!
//! **Read-only.** Nothing here can spend: `portfolio::read` takes a `&Chain`
//! and every call it makes is a question. Reaching a `Broadcaster` needs a
//! `SpendPermit`, which cannot be constructed outside `chainvue_chain::permit`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use chainvue_chain::Chain;
use chainvue_core::portfolio;

const TESTNET: &str = "https://api.verustest.net";

/// A transparent address from the SDK's own fixtures. Whether it holds anything
/// is not the point — what is being checked is that six real requests come back
/// and add up.
///
/// Override it with `CHAINVUE_LIVE_ADDRESS` to point this at an address that
/// actually holds tokens, which is the only way to see the currency-naming path
/// run against real data. Deliberately **not** a constant in the file: nobody's
/// address belongs in a committed test.
fn address() -> String {
    std::env::var("CHAINVUE_LIVE_ADDRESS")
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
    for row in reading.recent(
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("a clock after 1970")
                .as_secs(),
        )
        .expect("a year before 292 billion AD"),
    ) {
        println!("  {} {} {}", row.when_display, row.net_display, row.txid);
    }
}
