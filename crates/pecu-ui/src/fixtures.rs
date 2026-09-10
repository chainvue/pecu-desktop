//! The view state snapshots and previews are rendered from.
//!
//! Shared by `examples/render_shots.rs` and `tests/visual.rs` so the images you
//! review and the images the test compares are produced from the same data. If
//! they diverged, a green test would mean nothing.
//!
//! Most of these are deliberately the *empty* state: no balances, no
//! transactions, and nodes that have not been asked anything.
//!
//! [`funded`] is the exception, because a reference image of an empty dashboard
//! verifies nothing about the layout of a full one — and a dashboard is mostly
//! layout for numbers. It carries invented figures, so it turns **mock mode
//! on**: the rendered image then has the permanent "MOCK DATA" banner across
//! its title bar and cannot be mistaken for someone's real balance, in a README
//! or anywhere else. A fixture that needs fake numbers has to say so in the
//! picture, not in a comment next to it.

use std::rc::Rc;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::{
    ActivityRow, AppInfo, AppWindow, AssetRow, ChainChoice, ContentEntry, CurrencyField, CurrencyPick,
    CurrencyProblem, CurrencyRow, CurrencySlice, CurrencyState, EligibleIdentity, FlowStep,
    ConvertState, HaltState, IdentityRow, IdentityState, KeyRow, KnownAddressRow, MarketRow,
    MarketState,
    NetworkState, NodeRow, PendingRow,
    ActivityState, PreallocEntry, ReserveEntry, ReviewOutput, SearchHit, SearchState, SeedState,
    Note, SeedWord, SendState, Stat, TxState, Venue, WalletState,
};

/// A conversion being priced.
///
/// # These are the strings the core emits, not a drawing of them
///
/// Two hundred and fifty VRSCTEST into DAI.vETH through Bridge.vETH, on the
/// reserve state the scripted chain publishes. The estimate is what the curve
/// yields, the rate is that estimate divided by what went in, and the floor is
/// three percent under it — all produced by `convert::quote`, checked in
/// `pecu-core`'s own `a_quote_says_which_currency_each_figure_is_in`.
///
/// The previous version of this fixture was invented, and it showed a `Review
/// conversion` button wired to a callback nothing listened to. Both are gone.
pub fn converting(ui: &AppWindow) {
    funded(ui);
    ui.set_screen("convert".into());

    let state = ui.global::<ConvertState>();
    state.set_from_address("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq".into());
    state.set_to_address("iN9vbHXexEh6GTZ45fRoJGKTQThfbgUwMh".into());
    state.set_from_name("VRSCTEST".into());
    state.set_to_name("DAI.vETH".into());
    state.set_from_balance("415.2500 0000".into());
    state.set_pay_draft("250".into());
    state.set_get_estimate("133.5245 8143".into());
    state.set_via("Bridge.vETH".into());
    state.set_rate("1 VRSCTEST = 0.5341 DAI.vETH".into());
    state.set_conversion_fee("0.1250 0000 VRSCTEST".into());
    state.set_network_fee("0.0001 0000 VRSCTEST".into());
    state.set_minimum("129.5188 4398 DAI.vETH".into());
    state.set_slippage("0.58%".into());
    state.set_slippage_tone("positive".into());
    state.set_ready(true);
}

/// The conversion the design's flow calls out: slippage past the point where it
/// should go through without somebody saying so again.
///
/// Its own picture because the warning is the whole content of the state, and a
/// screen whose warnings have never been photographed is a screen whose
/// warnings have never been read.
///
/// A thousand coins, which is what it takes to cost two percent against
/// Bridge.vETH's real reserves. **The demo wallet cannot fund this** — it holds
/// four hundred — so the state is reachable on a chain and not in the scripted
/// build, which is exactly why it needs a fixture. The figures are still the
/// curve's: `slippage_gets_its_tone_from_how_far_the_estimate_fell` asserts
/// this size and this tone.
pub fn converting_thin(ui: &AppWindow) {
    converting(ui);

    let state = ui.global::<ConvertState>();
    state.set_pay_draft("1000".into());
    state.set_get_estimate("525.7011 2000".into());
    state.set_rate("1 VRSCTEST = 0.5257 DAI.vETH".into());
    state.set_conversion_fee("0.5000 0000 VRSCTEST".into());
    state.set_minimum("509.9300 8640 DAI.vETH".into());
    state.set_slippage("2.15%".into());
    state.set_slippage_tone("warning".into());
    state.set_note(note("convert-slippage-high", &["2.15%"]));
}

/// Convert, with the protocol not taking conversions.
///
/// **The state this build is actually in on VRSCTEST**, and it had no picture.
/// `disabledefi` has been in force since block 1 187 000, which is why every
/// conversion is rejected — and the point of the screen is that everything
/// above the button is still true. The pools have reserves, the node prices
/// them, the quote is real. It describes a market nobody can trade in.
pub fn converting_halted(ui: &AppWindow) {
    converting(ui);

    let halt = ui.global::<HaltState>();
    halt.set_severity("critical".into());
    halt.set_note(note("halt-conversions", &[]));
    halt.set_conversions_halted(true);
}

/// The same switch, before its height. A halt that has not landed yet is a
/// warning with a countdown, and it stops nothing — reporting it as active
/// would say trading has stopped when it has not.
pub fn converting_halt_scheduled(ui: &AppWindow) {
    converting(ui);

    let halt = ui.global::<HaltState>();
    halt.set_severity("warning".into());
    halt.set_note(note("halt-conversions", &[]));
    halt.set_in_blocks(40);
}

/// The conversion review: what was actually signed.
///
/// Its own reference image because it is the screen that decides whether money
/// moves, and because almost everything on it is decoded from a
/// CryptoCondition payload — which means it is also the screen where a decoding
/// mistake would show up as a plausible-looking number rather than as a crash.
///
/// The figures are `converting`'s, carried through: two hundred and fifty
/// VRSCTEST into DAI.vETH through Bridge.vETH, the floor three per cent under
/// the estimate the way `convert::floor` sets it.
pub fn converting_review(ui: &AppWindow) {
    converting(ui);

    let state = ui.global::<ConvertState>();
    state.set_step("review".into());
    state.set_ticket(1);
    state.set_review_from("VRSCTEST".into());
    state.set_review_to("DAI.vETH".into());
    state.set_review_via("Bridge.vETH".into());
    state.set_review_pay("250.0000 0000 VRSCTEST".into());
    state.set_review_estimate("133.5245 8143 DAI.vETH".into());
    state.set_review_minimum("129.5188 4398 DAI.vETH".into());
    // The transfer fee as it is written into the payload: 20 010 satoshis.
    // Not a round number, and not this wallet's choice — see
    // `convert::TRANSFER_FEE_SATS`.
    state.set_review_conversion_fee("0.0002 0010".into());
    state.set_review_network_fee("0.0001 0000".into());
    state.set_review_total("250.0003 0010".into());
    state.set_review_balance_after("165.2496 9990".into());
    state.set_review_from_address(ADDRESS.into());
    state.set_review_recipient(ADDRESS.into());

    state.set_outputs(ModelRc::from(Rc::new(VecModel::from(vec![
        ReviewOutput {
            // The delivery address out of the payload, not the protocol
            // constant the script pays. See `send::describe`.
            address: ADDRESS.into(),
            kind: note("output-conversion", &[]),
            amount: "250.0002 0010".into(),
            is_change: false,
        },
        ReviewOutput {
            address: "RWmjzbd4Sy6zK4H4rjHXrpaWTrsJYRr6Nn".into(),
            kind: note("output-payment", &[]),
            amount: "165.2496 9990".into(),
            is_change: true,
        },
    ]))));
}

/// The review with the network's refusal on it.
///
/// **The state this build will actually be in most of the time.** VRSCTEST has
/// conversions paused, so every conversion is rejected — after the wallet has
/// priced it, built it, signed it and had somebody agree to it. The screen has
/// to make it clear that the node said no rather than that the wallet broke,
/// and it has to still be showing the conversion the refusal is about.
pub fn converting_rejected(ui: &AppWindow) {
    converting_review(ui);

    ui.global::<ConvertState>()
        .set_problem(note("convert-refused-by-node", &[]));
}

/// It reached the network. Sent, not finished — what comes out is decided on
/// import, and the screen says so rather than reporting a completed trade.
pub fn converting_sent(ui: &AppWindow) {
    converting(ui);

    let state = ui.global::<ConvertState>();
    state.set_step("sent".into());
    let txid = "6a3f9c1e2b7d4f8a0c5e1937b6d2f4a8c9e0173b5d8f2a4c6e91b3d7f5a8c0e2";
    state.set_txid(txid.into());
    // As the core builds it — see `pecu_chain::Network::explorer`. Spelled out
    // rather than called, for the reason the shortcut hint is: a fixture that
    // asks the environment renders differently on different machines.
    state.set_explorer(format!("https://markets.chainvue.io/testnet/tx/{txid}/").into());
}

/// A leg that cannot be converted at all: the design's "empty reserve".
///
/// `demo.VRSCTEST` is a real currency on the scripted chain that no started
/// basket holds — so this is not a contrived case, it is what the wallet says
/// about a currency somebody actually has.
pub fn converting_refused(ui: &AppWindow) {
    funded(ui);
    ui.set_screen("convert".into());

    let state = ui.global::<ConvertState>();
    state.set_from_address("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq".into());
    state.set_to_address("iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".into());
    state.set_from_name("VRSCTEST".into());
    state.set_to_name("demo.VRSCTEST".into());
    state.set_from_balance("415.2500 0000".into());
    state.set_pay_draft("250".into());
    // Deliberately blank rather than zero. There is no rate, so there is no
    // number — and `0` would be a claim that the conversion yields nothing
    // rather than that it cannot be priced.
    state.set_note(note("convert-no-route", &["VRSCTEST", "demo.VRSCTEST"]));
    state.set_ready(false);
}

/// The currency picker, open over the convert form.
///
/// The list is the markets table, so this is also the only reference image that
/// shows those rows anywhere but the markets screen — which is the claim the
/// picker makes: one source, no second list to disagree with.
pub fn converting_picking(ui: &AppWindow) {
    converting(ui);
    ui.global::<ConvertState>().set_picking("get".into());
}

/// The history screen with every kind of row on it.
///
/// Three of the four kinds cannot be produced by this wallet — see
/// `activity_of`. They are here so the filters, the marks and the subtitles are
/// settled and photographed before the features behind them land, rather than
/// being designed in a hurry on the day they do.
pub fn history(ui: &AppWindow) {
    funded(ui);
    ui.set_screen("activity".into());

    let wallet = ui.global::<WalletState>();
    wallet.set_history(ModelRc::from(Rc::new(VecModel::from(vec![
        dated("in", "+120.0000 0000", "2 hours", "today", 1_187_400),
        activity_of(
            "login",
            "forum.verus.io · as robert.VRSCTEST@",
            "—",
            "3 hours",
            "",
        ),
        dated("out", "−50.0000 0000", "5 hours", "", 1_187_380),
        activity_of(
            "convert",
            "250 VRSCTEST → 134.12 DAI.vETH · via Bridge.vETH",
            "250.0000 0000",
            "7 hours",
            "",
        ),
        activity_of(
            "identity",
            "Recovery address changed",
            "—",
            "1 days",
            "yesterday",
        ),
        dated("in", "+5.0000 0000", "pending", "", 0),
    ]))));

    // Invented, like every other summary figure in this build. The core counts
    // none of these yet.
    ui.global::<ActivityState>()
        .set_stats(ModelRc::from(Rc::new(VecModel::from(vec![
            Stat {
                label: note("stat-transactions-30d", &[]),
                value: "48".into(),
            },
            Stat {
                label: note("stat-sent-30d", &[]),
                value: "3 410.00".into(),
            },
            Stat {
                label: note("stat-received-30d", &[]),
                value: "5 102.00".into(),
            },
            Stat {
                label: note("stat-signins-30d", &[]),
                value: "12".into(),
            },
        ]))));
}

/// The same list with one filter applied, which is the state the filters exist
/// for and the one that shows what they do to a short list.
///
/// # The rows are filtered, not only the chip
///
/// This used to set `filter` and leave all six rows where they were, so the
/// reference image showed "Payments" pressed above a login, a conversion and an
/// identity update. **The wallet cannot reach that state.**
/// `ActivityState.filter` is only the echo that keeps the pressed chip from
/// waiting on a round trip — the list itself arrives already filtered, because
/// `portfolio::rows_from` drops every row of another kind *before* the core
/// sends it, and `ui/vm/state.slint` says why the filtering cannot live in
/// `.slint` at all.
///
/// A picture of a screen the product cannot produce guards nothing: the test
/// stays green through any change to the filter, because the image was never
/// of the filter working. This is the same fault `markets_crowded` was fixed
/// for — a fixture that left a selection in place and quietly stopped being the
/// long-list case — and the reason the native menu bar was built, rendered and
/// taken out again rather than photographed, `docs/LATER.md` §4d.
pub fn history_filtered(ui: &AppWindow) {
    history(ui);
    filter_history(ui, "payment");
    ui.global::<ActivityState>().set_filter("payment".into());
}

/// Keep only the history rows of one kind, which is what the core sends when a
/// filter chip is pressed.
///
/// # The day headings are left alone
///
/// `rows_from` filters first and assigns headings second, to the first row of
/// each day. Dropping the three non-payment rows empties exactly one day —
/// yesterday, whose only row was the identity update — so its heading leaves
/// with it, and every row that survives is one of today's, under the heading
/// the first of them already carries. No surviving row gains or loses a day, so
/// there is nothing to recompute here and recomputing it would mean teaching
/// this crate a second copy of `calendar_day`.
///
/// Refusing loudly when nothing survives, on the same bargain [`when_note`]
/// makes: this runs only while a reference image is being rendered, so a
/// refusal is a failed render rather than anything a user can reach — and a
/// filtered fixture that photographed an empty list would be a second picture
/// of the empty state wearing the filter's name.
fn filter_history(ui: &AppWindow, kind: &str) {
    let wallet = ui.global::<WalletState>();
    let rows = wallet.get_history();
    let kept: Vec<ActivityRow> = (0..rows.row_count())
        .filter_map(|i| rows.row_data(i))
        .filter(|row| row.kind == kind)
        .collect();

    assert!(
        !kept.is_empty(),
        "no history row is a {kind:?}, so filtering to it photographs the empty \
         state rather than the filter",
    );

    wallet.set_history(ModelRc::from(Rc::new(VecModel::from(kept))));
}

/// The markets table, with nothing picked.
///
/// # These figures are invented
///
/// Every price, delta and depth here is written down rather than derived — the
/// core does not price anything yet. They are chosen to exercise the states the
/// screen has to survive rather than to look plausible: two currencies with no
/// price at all, one that has not moved, and one down.
///
/// `—` is the case worth having a picture of. A currency with no converter has
/// no price, and there is no number that says so.
pub fn markets(ui: &AppWindow) {
    funded(ui);
    ui.set_screen("markets".into());
}

/// The table's rows on their own, so the dashboard's markets column can be
/// filled without pretending the whole markets screen is open.
///
/// # Every string here was produced by the code that produces them
///
/// Not drawn. These are the exact rows `pecu-core`'s `market::rows` returns for
/// the scripted chain — the same figures, formatted by the same formatter, in
/// the same order. That matters because the previous version of this fixture
/// showed `$0.54`, `+4.2%` and `$412K`, and the wallet has never been able to
/// produce any of the three: there is no dollar sign on a DAI price, no
/// 24-hour change the SDK can ask for, and no `K` in the quantity formatter.
/// A reference image of figures the product cannot emit reviews a screen that
/// does not exist.
///
/// This crate cannot call `market::rows` to check — `pecu-core` is deliberately
/// not on its dependency list, see `tests/dependency_boundary.rs`. What keeps
/// the two honest is `pecu-core`'s own `opening_the_markets_screen_prices_the
/// _chain_currency`, which asserts the same strings against the same script.
fn market_rows(ui: &AppWindow) {
    let state = ui.global::<MarketState>();
    state.set_quote("DAI.vETH".into());
    state.set_rows(ModelRc::from(Rc::new(VecModel::from(vec![
        // Ordered by what the started baskets hold, which is what the table
        // sorts on. Every figure below is what `market::rows` produces from the
        // scripted chain — read out of it rather than composed here.
        MarketRow {
            name: "Bridge.vETH".into(),
            address: "iSojYsotVzXz4wh2eJriASGo6UidJDDhL2".into(),
            price: "7.09".into(),
            change: "0.00%".into(),
            tone: "unknown".into(),
            pooled: "100 311".into(),
            depth: "94.63".into(),
        },
        MarketRow {
            name: "VRSCTEST".into(),
            address: "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq".into(),
            price: "0.5372".into(),
            change: "0.00%".into(),
            tone: "unknown".into(),
            pooled: "26 968".into(),
            depth: "1 249".into(),
        },
        // Three currencies of one basket, so they are pooled to the same value
        // and fall back to their names. A tie is a real state and this is what
        // it looks like.
        MarketRow {
            name: "DAI.vETH".into(),
            address: "iN9vbHXexEh6GTZ45fRoJGKTQThfbgUwMh".into(),
            price: "1.00".into(),
            change: "0.00%".into(),
            tone: "unknown".into(),
            pooled: "25 078".into(),
            depth: "670.96".into(),
        },
        MarketRow {
            name: "MKR.vETH".into(),
            address: "i3WBJ7xEjTna5345D7gPnK4nKfbEBujZqL".into(),
            price: "1 831".into(),
            change: "0.00%".into(),
            tone: "unknown".into(),
            pooled: "25 078".into(),
            depth: "0.37".into(),
        },
        MarketRow {
            name: "vETH".into(),
            address: "iCtawpxUiCc2sEupt7Z4u8SDAncGZpgSKm".into(),
            price: "2 044".into(),
            change: "0.00%".into(),
            tone: "unknown".into(),
            pooled: "25 078".into(),
            depth: "0.33".into(),
        },
        // The one market on this chain whose price moved. Two hops to a price
        // and two hops to a chart — see `market::series`. It is last because it
        // is the smallest, and it has no `Exit @2%` because nothing measures
        // room in a pool with one reserve at weight one.
        MarketRow {
            name: "vrealv1".into(),
            address: "iBBRjDbPf3wdFpghLotJQ3ESjtPBxn6NS3".into(),
            price: "0.002923".into(),
            change: "+0.23%".into(),
            tone: "positive".into(),
            pooled: "1 890".into(),
            depth: "—".into(),
        },
        // `Bridge.Betelgeuse` used to be here, at the bottom, with four dashes.
        // No started basket trades it, so no conversion can reach it and it is
        // off the table entirely — see `Book::convertible`.
    ]))));
}

/// One currency picked, so the detail half has something in it.
///
/// The same provenance as [`market_rows`]: this is what `market::detail`
/// returns for VRSCTEST on the scripted chain, field for field.
pub fn market_detail(ui: &AppWindow) {
    markets(ui);

    let state = ui.global::<MarketState>();
    state.set_selected("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq".into());
    state.set_detail_name("VRSCTEST".into());
    state.set_detail_subtitle("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq · 2 of 3 venues trading".into());
    state.set_detail_price("0.5372".into());
    // Flat rather than unknown: the price is known at both ends of the window
    // and it is the same price. Bridge.vETH published one state in thirty days.
    state.set_detail_change("0.00%".into());
    state.set_detail_tone("unknown".into());
    state.set_detail_stats(ModelRc::from(Rc::new(VecModel::from(vec![
        Stat {
            label: note("stat-exit-depth", &["VRSCTEST"]),
            value: "1 249".into(),
        },
        Stat {
            label: note("stat-venues", &[]),
            value: "2 of 3".into(),
        },
        Stat {
            label: note("stat-quoted-in", &[]),
            value: "DAI.vETH".into(),
        },
    ]))));
    state.set_detail_venues(ModelRc::from(Rc::new(VecModel::from(vec![
        Venue {
            name: "Bridge.vETH".into(),
            state: "active".into(),
            price: "0.5372".into(),
            change: "0.00%".into(),
            tone: "unknown".into(),
            depth: "1 249".into(),
        },
        // Five dollars where the market says fifty-four cents, because it never
        // launched. Listed and never priced from — the one row on this screen
        // that shows why the venue list is worth the space.
        Venue {
            name: "Bridge.Betelgeuse".into(),
            state: "unstarted".into(),
            price: "5.00".into(),
            change: "—".into(),
            tone: "unknown".into(),
            depth: "13.38".into(),
        },
    ]))));
    // Bridge.vETH published one reading in thirty days, so the detail for
    // VRSCTEST draws the honest empty state rather than a line. `market_moving`
    // below is the one with a chart.
    crate::spark::show(ui, &[]);
    state.set_detail_route("VRSCTEST  →  Bridge.vETH  →  DAI.vETH".into());
    state.set_detail_route_note(note("price-from-notarization", &["1 156 331"]));
}

/// The markets screen as a real chain fills it: forty-nine currencies, not
/// seven.
///
/// **This is what the live wallet looks like and no reference image had it.**
/// The list is what a chain with three hundred currencies on it produces, and
/// no picture here had more than seven rows until this existed.
///
/// The rows past the first few are generated: what matters is the *count*, and
/// forty-nine rows of hand-written fixture would be forty-nine chances to
/// mistype a number that nothing checks.
pub fn markets_crowded(ui: &AppWindow) {
    // Built on `market_moving` for its rows, and then **unselected**.
    //
    // The selection used to be harmless: the list and the detail were side by
    // side, so a picture could show a long list and an open currency at once.
    // They are two views now, and leaving `market_moving`'s selection in place
    // would quietly turn this case into a second photograph of the detail — the
    // long-list case would stop existing and no test would say so.
    market_moving(ui);

    let state = ui.global::<MarketState>();
    state.set_selected(SharedString::new());
    let existing = state.get_rows();
    let mut rows: Vec<MarketRow> = (0..existing.row_count())
        .filter_map(|i| existing.row_data(i))
        .collect();
    for i in 0..42 {
        rows.push(MarketRow {
            address: format!("iFill{i:0>29}").into(),
            name: format!("filler-{i:02}").into(),
            price: format!("{:.6}", 0.004 + f64::from(i) / 1000.0).into(),
            change: "-0.30%".into(),
            tone: "negative".into(),
            // Descending, so the generated rows sit below the real ones and the
            // ordering the table applies is visible in the picture.
            pooled: format!("{}", 900 - i * 10).into(),
            depth: "1 761 472".into(),
        });
    }
    // No re-sort: the rows arrive from the core in the order the table shows,
    // and a fixture that imposed its own would be photographing an arrangement
    // the wallet never produces.
    state.set_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
}

/// A chain that prices nothing at all.
///
/// Reachable on a young chain, or one whose baskets have all been defined and
/// none started. It was a blank card in the corner of a split screen and is a
/// blank *screen* now, so the list grew something to say — and a state the
/// interface can reach is a state worth a picture of.
pub fn markets_empty(ui: &AppWindow) {
    markets(ui);
    let state = ui.global::<MarketState>();
    state.set_selected(SharedString::new());
    state.set_rows(ModelRc::from(Rc::new(VecModel::from(
        Vec::<MarketRow>::new(),
    ))));
}

/// A market whose price actually moved, and therefore has a chart.
///
/// The six readings are `vrealv1`'s from VRSCTEST — one reserve at weight one,
/// so its price is `held / supply` and the supply is the only thing that
/// changes. The **same** figures the scripted chain publishes, which is why
/// this picture and a demo build agree.
pub fn market_moving(ui: &AppWindow) {
    markets(ui);

    let state = ui.global::<MarketState>();
    state.set_selected("iBBRjDbPf3wdFpghLotJQ3ESjtPBxn6NS3".into());
    state.set_detail_name("vrealv1".into());
    state.set_detail_subtitle("iBBRjDbPf3wdFpghLotJQ3ESjtPBxn6NS3 · 1 of 1 venue trading".into());
    state.set_detail_price("0.005441".into());
    state.set_detail_change("+0.23%".into());
    state.set_detail_tone("positive".into());
    state.set_detail_stats(ModelRc::from(Rc::new(VecModel::from(vec![
        Stat {
            label: note("stat-exit-depth", &["vrealv1"]),
            value: "—".into(),
        },
        Stat {
            label: note("stat-supply", &[]),
            value: "646 493".into(),
        },
        Stat {
            label: note("stat-reserves", &[]),
            value: "1".into(),
        },
    ]))));
    state.set_detail_venues(ModelRc::from(Rc::new(VecModel::from(vec![Venue {
        name: "vrealv1".into(),
        state: "active".into(),
        price: "0.005441".into(),
        change: "+0.23%".into(),
        tone: "positive".into(),
        depth: "—".into(),
    }]))));
    state.set_detail_route("vrealv1  →  DAI.vETH".into());
    state.set_detail_route_note(note("price-from-notarization", &["1 184 883"]));

    // The prices these supplies give, as satoshis, oldest first. A day apart,
    // which is what the core sends and what the sparkline's window is.
    let day = 86_400;
    let now = 1_787_000_000_i64;
    let supplies = [
        647_993.435_264_f64,
        647_493.435_264,
        647_293.435_264,
        646_993.435_264,
        646_693.435_264,
        646_493.435_264,
    ];
    let points: Vec<pecu_protocol::ChartPointVm> = supplies
        .iter()
        .enumerate()
        .map(|(index, supply)| pecu_protocol::ChartPointVm {
            t: now - (5 - i64::try_from(index).unwrap_or(0)) * day,
            #[allow(clippy::cast_possible_truncation)]
            sats: (3_517.884_285_f64 / supply * 100_000_000.0).round() as i64,
        })
        .collect();
    crate::spark::show(ui, &points);
}

/// The command palette, open over the dashboard with results.
///
/// Seeded rather than searched: the hits are what the **core** returns, and no
/// core is running behind a reference image. What this photographs is the
/// panel — which is the half that can be got wrong by looking at it.
/// The first payment this wallet made on a real chain: 5 VRSCTEST to
/// `dude.VRSCTEST@`, accepted at block 1197422. Used wherever a fixture needs a
/// transaction id that is not invented.
const LANDED_TXID: &str = "68320bb5eb723ca3ab3f92d26133b4309d03c59e9ce3e93dba85d68379e98883";

pub fn searching(ui: &AppWindow) {
    funded(ui);

    let state = ui.global::<SearchState>();
    state.set_open(true);
    state.set_query("ve".into());
    state.set_hits(ModelRc::from(Rc::new(VecModel::from(vec![
        SearchHit {
            kind: "address".into(),
            label: "Vera".into(),
            sub: "RQxJPwqCvMHqCiZKPyzXvRXjT2Y5vd8Xhi".into(),
            target: "RQxJPwqCvMHqCiZKPyzXvRXjT2Y5vd8Xhi".into(),
        },
        // Paid once, never named. Photographed on purpose: it is the row that
        // shows the same string twice, and the only way to know that reads as a
        // deliberate fallback rather than a rendering fault is to look at it.
        SearchHit {
            kind: "address".into(),
            label: "RVe7T8FwqhLmMzTNbEo3vjFJ8yfXxJ4vDk".into(),
            sub: "RVe7T8FwqhLmMzTNbEo3vjFJ8yfXxJ4vDk".into(),
            target: "RVe7T8FwqhLmMzTNbEo3vjFJ8yfXxJ4vDk".into(),
        },
        SearchHit {
            kind: "currency".into(),
            label: "Bridge.vETH".into(),
            sub: "iBoaN7swKAwXgYf1huA3PxBXi5stcfgGMh".into(),
            target: "iBoaN7swKAwXgYf1huA3PxBXi5stcfgGMh".into(),
        },
    ]))));
}

/// The palette answering a transaction id.
///
/// The head of one, because that is the row worth photographing: it is the
/// shape the panel gained — the abbreviated id over the whole one — and the
/// only way to know that reads as a deliberate pair rather than a duplicated
/// line is to look at it. The query is a prefix for the same reason the core
/// accepts prefixes: sixty-four characters is what a clipboard holds, and the
/// first eight is what a person can read off a receipt.
pub fn searching_transaction(ui: &AppWindow) {
    funded(ui);

    let state = ui.global::<SearchState>();
    state.set_open(true);
    state.set_query(LANDED_TXID[..12].into());
    state.set_hits(ModelRc::from(Rc::new(VecModel::from(vec![SearchHit {
        kind: "transaction".into(),
        label: "68320…8883".into(),
        sub: LANDED_TXID.into(),
        target: LANDED_TXID.into(),
    }]))));
}

/// …and the palette refusing to pretend it did not understand the question.
///
/// A whole transaction id, no rows, and the sentence the core sends instead of
/// silence. The longer of the two sentences on purpose — the one a wallet still
/// paging its history backwards gets, which is most wallets — because it is the
/// one that has to wrap inside the panel without pushing anything off it.
pub fn searching_unknown_transaction(ui: &AppWindow) {
    funded(ui);

    let state = ui.global::<SearchState>();
    state.set_open(true);
    // Not `LANDED_TXID`: this is a well-formed id that this wallet has no row
    // for, which is the entire subject of the picture.
    state.set_query("9f2c41a0b7de5836419ca07d2b8e5f1043c96be27d4a08915fc3b6e2d7085a14".into());
    state.set_hits(ModelRc::from(Rc::new(VecModel::from(Vec::<SearchHit>::new()))));
    state.set_note(note("search-txid-unscanned", &[]));
}

/// A named reason, the way the core sends one.
fn note(code: &str, args: &[&str]) -> Note {
    Note {
        code: code.into(),
        args: ModelRc::from(Rc::new(VecModel::from(
            args.iter().map(|a| SharedString::from(*a)).collect::<Vec<_>>(),
        ))),
    }
}

/// A fresh install: no wallet yet, so the onboarding screen is what shows.
pub fn fresh(ui: &AppWindow) {
    nodes(ui);
    let wallet = ui.global::<WalletState>();
    wallet.set_loading(false);
    wallet.set_exists(false);
}

/// An existing wallet, unlocked — the state the dashboard and network screens
/// are actually about.
pub fn unlocked(ui: &AppWindow) {
    nodes(ui);
    let wallet = ui.global::<WalletState>();
    wallet.set_loading(false);
    wallet.set_exists(true);
    wallet.set_locked(false);
    wallet.set_name("Pecu".into());
    wallet.set_active_key("main".into());
    wallet.set_keys(ModelRc::from(Rc::new(VecModel::from(vec![key(
        "main",
        ADDRESS,
        "generated",
        true,
        true,
    )]))));
}

/// A wallet with more than one key, on the settings screen.
///
/// Three at once because the row has three things it may have to say — this one
/// is in use, this one's phrase has never been written down, this one never had
/// a phrase — and each of them is a different trailing element competing for the
/// same space.
pub fn keys(ui: &AppWindow) {
    settings(ui);
    // Settings is four tabs now, and each fixture has to say which one it is
    // photographing — otherwise three of these images are the same picture of
    // the security tab, which is what they were.
    ui.set_settings_tab(1);

    let wallet = ui.global::<WalletState>();
    wallet.set_keys(ModelRc::from(Rc::new(VecModel::from(vec![
        key("main", ADDRESS, "generated", true, true),
        // Generated and never written down: one power cut from being gone, and
        // the row has to say so.
        key("savings", SECOND_ADDRESS, "generated", false, false),
        // A WIF import has no phrase and never will, so "not backed up" would
        // be a warning about something that cannot be fixed.
        key("cold-storage", THIRD_ADDRESS, "wif", true, false),
    ]))));
}

/// The address book, with the two shapes a row can take: named, and not.
///
/// Both at once because they are laid out differently — an unnamed row puts the
/// address in the title and its summary in the subtitle, and a named one puts
/// the name in the title and pushes the summary to the far side.
pub fn addresses(ui: &AppWindow) {
    settings(ui);
    ui.set_settings_tab(2);

    ui.global::<SendState>()
        .set_known(ModelRc::from(Rc::new(VecModel::from(vec![
            // A VerusID paid by name. The row this whole column exists for:
            // the payment recorded the i-address, and without the name it is
            // twenty bytes nobody recognises.
            KnownAddressRow {
                address: "i4YzoP8ZHnh1gNywV9PAT6Yz3AkfXxJmtP".into(),
                label: SharedString::new(),
                name: "dude.VRSCTEST@".into(),
                summary: "1 payment · last in the last hour".into(),
            },
            KnownAddressRow {
                address: SECOND_ADDRESS.into(),
                label: "the exchange".into(),
                name: SharedString::new(),
                summary: "3 payments · last 2 days ago".into(),
            },
            KnownAddressRow {
                address: THIRD_ADDRESS.into(),
                label: SharedString::new(),
                name: SharedString::new(),
                summary: "1 payment · last in the last hour".into(),
            },
        ]))));
}

/// The General tab: theme, auto-lock, and where the files are.
///
/// Photographed on testnet, like every other settings shot, and on testnet the
/// spending block is not drawn — a spend there cannot cost anything, so there
/// is no true sentence for it to say. The one control in Settings that changes
/// what this wallet may do with real money therefore has its own case:
/// `spending_arm`, below.
pub fn general_settings(ui: &AppWindow) {
    settings(ui);
    ui.set_settings_tab(3);
}

/// The same tab on a chain where a spend costs real money.
///
/// Its own case because the general tab is photographed on testnet, and on
/// testnet the spending block is not drawn at all — there is no true sentence
/// for it to say. That left the highest-consequence control in the wallet with
/// no picture of it anywhere. Verus rather than vARRR: the two words in the
/// block differ on either, and "Verus"/`VRSC` is the pair most people will see.
pub fn spending_arm(ui: &AppWindow) {
    general_settings(ui);

    let net = ui.global::<NetworkState>();
    net.set_requested("Mainnet".into());
    net.set_requested_name("VRSC".into());
    net.set_chain_title("Verus".into());
    net.set_effective("Mainnet".into());
    net.set_endpoint("https://api.verus.services".into());
    net.set_node_state("online".into());
    net.set_spend_needs_opt_in(true);

    // The About block sits directly below the spending one and names both
    // paths, and a picture of a mainnet wallet reading `…/testnet/vault.json`
    // is a picture of a state the application cannot be in.
    ui.global::<WalletState>().set_vault_path(
        "~/Library/Application Support/com.pecu.wallet/mainnet/vault.json".into(),
    );
    ui.global::<AppInfo>()
        .set_log_path("~/Library/Application Support/com.pecu.wallet/mainnet/logs".into());
}

/// The keys section with a rename in progress, which is where the form and the
/// row it belongs to have to sit together without the list jumping.
pub fn renaming_key(ui: &AppWindow) {
    keys(ui);
    let wallet = ui.global::<WalletState>();
    wallet.set_renaming("savings".into());
    wallet.set_rename_draft("main".into());
    wallet.set_key_problem(note("key-name-taken", &["main"]));
}

/// The addresses these pictures are drawn with.
///
/// The first is from the SDK's own fixtures; the other two are derived from
/// fixed scalars (`[11; 32]` and `[23; 32]`). All three have valid checksums and
/// none of them belongs to anybody — which is the whole requirement. These
/// images are committed and looked at, so an address in one must be neither
/// invented (a hand-typed one failed its checksum twice, which nobody notices in
/// a picture and somebody might copy out of it) nor real.
const ADDRESS: &str = "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX";
/// A real Sapling payment address, from the SDK's own `zaddr` test vector.
///
/// Not invented. A made-up `zs1…` of the wrong length would make every picture
/// of this screen a picture of a layout that does not exist — the address wraps,
/// so its length decides how tall the card is.
const SHIELDED_ADDRESS: &str =
    "zs18pytujp8qu73a3fu6g9chl7mfumrr0htyqsh60r3ed4capagqwm8tx2l8f9c5g7w87q4566uph3";
const SECOND_ADDRESS: &str = "RVGTY4w2GrdBFrzGaAASBvT6prBr4MxDfJ";
const THIRD_ADDRESS: &str = "RGZbQcWU9LNSa9rat45UMKaeP1q32NBduM";

fn key(label: &str, address: &str, origin: &str, backed_up: bool, active: bool) -> KeyRow {
    KeyRow {
        label: label.into(),
        address: address.into(),
        origin: origin.into(),
        used: true,
        backed_up,
        active,
    }
}

/// A wallet somebody started using this morning, on a year's axis.
///
/// The shape that exposed the bug this exists to catch: four movements inside
/// one hour, nine hours ago, and a scan that has reached the start of the
/// chain. Every range button used to draw the same picture, because the axis
/// was derived from the readings — and the same four readings are inside every
/// window. Here the year is a year: nothing for fifty-one weeks, then a rise in
/// the last sliver.
pub fn young_wallet(ui: &AppWindow) {
    const HOUR: i64 = 3_600;
    let now = 1_770_000_000;

    funded(ui);

    let points: Vec<pecu_chart::Point> = [
        (9 * HOUR, 0_i64),
        (9 * HOUR - 60, 5_000_000_000),
        (8 * HOUR - 600, 4_889_990_000),
        (8 * HOUR, 5_389_990_000),
        (0, 5_389_990_000),
    ]
    .iter()
    .map(|(ago, sats)| pecu_chart::Point {
        t: now - ago,
        value: *sats,
    })
    .collect();

    // `complete`: the scan reached the chain start, so the balance before the
    // first transaction is known and the window may honestly be drawn back to
    // its own start.
    crate::chart::seed(ui, &points, true, "VRSCTEST", now);
}

/// The dashboard with the wallet complaining.
///
/// Two at once, and one of them repeated, because that is the shape a real
/// session produced: a refusal that happens once and a refresh that fails every
/// fifteen seconds. The second is why these are counted rather than stacked.
pub fn complaining(ui: &AppWindow) {
    use pecu_protocol::{Severity, UiError};

    funded(ui);
    crate::toast::install(ui);

    crate::toast::show(
        ui,
        &UiError::simple(
            "spend_refused",
            pecu_protocol::NoteVm::with(
                "spend-wrong-chain",
                ["Mainnet".to_string(), "Testnet".to_string()],
            ),
            String::new(),
            Severity::Danger,
        ),
    );
    for _ in 0..7 {
        crate::toast::show(
            ui,
            &UiError::simple(
                "history",
                pecu_protocol::NoteVm::plain("history-unreadable"),
                // A node's own words, which is what this line is for.
                "the node closed the connection".to_string(),
                Severity::Warning,
            ),
        );
    }
}

/// The dashboard with a payment whose fate is unknown.
///
/// The state that is hardest to get right and rarest to see, which is exactly
/// why it gets a reference image: the wrong move here — sending again — is the
/// tempting one, and the wording is what stands between someone and paying
/// twice.
pub fn unconfirmed(ui: &AppWindow) {
    funded(ui);

    ui.global::<SendState>()
        .set_pending_rows(ModelRc::from(Rc::new(VecModel::from(vec![PendingRow {
            id: 1,
            txid: "685ffac53fc525a4cefa5ed334139aebace508cbe293a41e6edba096f22517a5".into(),
            to_address: "RWmjzbd4Sy6zK4H4rjHXrpaWTrsJYRr6Nn".into(),
            amount: "50.0000 0000".into(),
            state: "uncertain".into(),
            checks: 2,
        }]))));
}

/// Settings on testnet, where a spend needs no confirmation and the spending
/// block is therefore not drawn. `spending_arm` is the same tab on a chain that
/// costs real money.
pub fn settings(ui: &AppWindow) {
    unlocked(ui);
    ui.set_screen("settings".into());

    let wallet = ui.global::<WalletState>();
    wallet.set_auto_lock(5);
    wallet.set_vault_path(
        "~/Library/Application Support/com.pecu.wallet/testnet/vault.json".into(),
    );
    // The real binary fills this in. Leaving it blank rendered "Logs" beside
    // nothing at all, which is exactly the difference between the snapshot and
    // the shipped app that these images exist to catch.
    ui.global::<AppInfo>()
        .set_log_path("~/Library/Application Support/com.pecu.wallet/testnet/logs".into());
}

/// A transaction opened over the activity list.
///
/// Shown with the raw JSON still on its way and the fee unreported — the state
/// the sheet is actually in for the first moment, and the one where the wording
/// has to be right about what is not known yet.
pub fn tx_detail(ui: &AppWindow) {
    funded(ui);
    ui.set_screen("activity".into());

    let tx = ui.global::<TxState>();
    tx.set_txid("685ffac53fc525a4cefa5ed334139aebace508cbe293a41e6edba096f22517a5".into());
    tx.set_when(note("when-hours", &["2"]));
    tx.set_amount("+12 345.0000 0000 mambo".into());
    tx.set_direction("in".into());
    // A token movement: the amount already names its currency.
    tx.set_amount_is_native(false);
    tx.set_confirmations("187".into());
    tx.set_height("1 187 313".into());
    tx.set_explorer(
        "https://testex.verus.io/tx/685ffac53fc525a4cefa5ed334139aebace508cbe293a41e6edba096f22517a5"
            .into(),
    );
}

/// The send form, on a wallet with more than one key.
///
/// The form has never had a reference image of its own — [`reviewing`] jumps
/// straight to step two — so the labels, the placeholders and the key picker
/// have only ever been checked by reading them.
pub fn sending(ui: &AppWindow) {
    funded(ui);
    ui.set_screen("send".into());

    let wallet = ui.global::<WalletState>();
    wallet.set_keys(ModelRc::from(Rc::new(VecModel::from(vec![
        key("main", ADDRESS, "generated", true, true),
        key("savings", SECOND_ADDRESS, "generated", true, false),
    ]))));
    wallet.set_address(ADDRESS.into());

    // What the ACTIVE key holds, which is deliberately less than the wallet
    // does. `funded` gives this wallet two keys, 12 382.42 spendable and 100
    // maturing between them; `main` holds 8 000 and 40 of that. The send form
    // spends one key, so a fixture that made the two figures equal would
    // photograph a picture that cannot tell a correct form from the one that
    // quoted the wallet's sum — which is the bug these images now hold shut.
    let send = ui.global::<SendState>();
    send.set_key_spendable("8 000.0000 0000".into());
    send.set_key_immature("40.0000 0000".into());

    // Who this wallet has paid, which the send screen now shows beside the
    // form. The same rows the Addresses tab lists — one list, one truth about
    // who has been paid, rather than a second address book that could disagree
    // with the first.
    ui.global::<SendState>()
        .set_known(ModelRc::from(Rc::new(VecModel::from(vec![
            // A VerusID paid by name. The row this whole column exists for:
            // the payment recorded the i-address, and without the name it is
            // twenty bytes nobody recognises.
            KnownAddressRow {
                address: "i4YzoP8ZHnh1gNywV9PAT6Yz3AkfXxJmtP".into(),
                label: SharedString::new(),
                name: "dude.VRSCTEST@".into(),
                summary: "1 payment · last in the last hour".into(),
            },
            KnownAddressRow {
                address: SECOND_ADDRESS.into(),
                label: "the exchange".into(),
                name: SharedString::new(),
                summary: "3 payments · last 2 days ago".into(),
            },
            KnownAddressRow {
                address: THIRD_ADDRESS.into(),
                label: SharedString::new(),
                name: SharedString::new(),
                summary: "1 payment · last in the last hour".into(),
            },
        ]))));
}

/// The review step, showing a payment that has been built and signed.
///
/// Two outputs, because that is what a real payment looks like: the recipient
/// and the change coming back. A fixture with one output would hide the row
/// this screen exists to show.
pub fn reviewing(ui: &AppWindow) {
    funded(ui);
    ui.set_screen("send".into());

    let send = ui.global::<SendState>();
    send.set_step("review".into());
    send.set_ticket(1);
    send.set_amount("50.0000 0000".into());
    send.set_fee("0.0001 0000".into());
    send.set_total("50.0001 0000".into());
    send.set_change("12 332.4199 0000".into());
    send.set_balance_after("12 332.4199 0000".into());
    // Which key is paying. The bridge has always set this; nothing rendered it
    // until the review learned to say so.
    send.set_from_address(ADDRESS.into());
    // The state worth a reference image: the warning is the reason the review
    // step exists at all, and it is the one thing on this screen somebody has
    // to read rather than glance at.
    send.set_first_time_recipient(true);

    send.set_outputs(ModelRc::from(Rc::new(VecModel::from(vec![
        ReviewOutput {
            address: "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX".into(),
            kind: note("output-payment", &[]),
            amount: "50.0000 0000".into(),
            is_change: false,
        },
        ReviewOutput {
            // A real address with a valid checksum, derived in
            // `pecu-core/tests/send_build.rs`. A hand-typed one failed its
            // checksum — which nobody would notice in a picture, and which
            // someone might copy out of it.
            address: "RWmjzbd4Sy6zK4H4rjHXrpaWTrsJYRr6Nn".into(),
            kind: note("output-payment", &[]),
            amount: "12 332.4199 0000".into(),
            is_change: true,
        },
    ]))));
}

/// The payment landed: the id, and the two things anybody does with one.
///
/// **This screen had no reference image at all**, which is how a transaction id
/// that broke after 63 of its 64 characters — leaving one digit alone on a
/// second line — shipped and stayed. A screen nobody photographs is a screen
/// nobody looks at.
///
/// The txid is the real one from the first payment this wallet ever made on
/// VRSCTEST, block 1197422, so the explorer link beside it is one that actually
/// resolves.
pub fn sent(ui: &AppWindow) {
    funded(ui);
    ui.set_screen("send".into());

    let send = ui.global::<SendState>();
    send.set_step("sent".into());
    send.set_txid(LANDED_TXID.into());
    send.set_explorer(format!("https://markets.chainvue.io/testnet/tx/{LANDED_TXID}/").into());
}

/// The broadcast whose outcome nobody knows.
///
/// The most important paragraph in the application is on this screen — somebody
/// who resends here can pay twice — and it had never been rendered either.
pub fn send_uncertain(ui: &AppWindow) {
    sent(ui);
    ui.global::<SendState>().set_step("uncertain".into());
}

/// The same review, for a payment addressed to a VerusID by name.
///
/// Worth its own reference image because it is the one case where the address
/// on screen is not the thing anybody typed. A name is a question put to a node;
/// the outputs are the answer. Showing only one of the two is either asking
/// somebody to check an i-address they have never seen, or asking them to trust
/// a lookup nobody told them happened.
pub fn reviewing_identity(ui: &AppWindow) {
    reviewing(ui);

    let send = ui.global::<SendState>();
    send.set_recipient_name("pecu.VRSCTEST@".into());
    // Paid before, so the screen is not carrying two warnings at once and the
    // identity block is what the image is actually about.
    send.set_first_time_recipient(false);

    send.set_outputs(ModelRc::from(Rc::new(VecModel::from(vec![
        ReviewOutput {
            // A real i-address from `api.verustest.net`, so it parses.
            address: "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".into(),
            kind: note("output-to-verusid", &[]),
            amount: "50.0000 0000".into(),
            is_change: false,
        },
        ReviewOutput {
            address: "RWmjzbd4Sy6zK4H4rjHXrpaWTrsJYRr6Nn".into(),
            kind: note("output-payment", &[]),
            amount: "12 332.4199 0000".into(),
            is_change: true,
        },
    ]))));
}

/// The receive screen, with a real QR for a real address.
///
/// The address is from the SDK's own fixtures — a valid transparent address
/// that this project has never held a key for. Rendering an invented one would
/// prove nothing about the encoder, and rendering a live one would put someone's
/// address in a committed image.
/// Receiving on a key that arrived as a WIF.
///
/// Worth its own picture because the shielded column is *present* and says
/// something rather than being absent: a person looking for a z-address has to
/// find out why there is none, and "the panel is missing" is not an answer.
/// This is also the state a wallet lands in after importing a private key,
/// which is a normal thing to have done.
pub fn receiving_without_a_phrase(ui: &AppWindow) {
    receiving(ui);

    let wallet = ui.global::<WalletState>();
    wallet.set_shielded_address(slint::SharedString::new());
    wallet.set_shielded_address_spoken(slint::SharedString::new());
    wallet.set_shielded_note(Note {
        code: "shielded-needs-a-phrase".into(),
        ..Default::default()
    });
    wallet.set_keys(ModelRc::from(Rc::new(VecModel::from(vec![key(
        "imported",
        ADDRESS,
        "imported-wif",
        true,
        true,
    )]))));
}

pub fn receiving(ui: &AppWindow) {
    unlocked(ui);

    // A name this wallet controls, because the screen now offers it as the
    // other way to be paid. Without one the panel is correctly absent — which
    // is a state worth having too, but not the one this picture is of.
    ui.global::<IdentityState>()
        .set_rows(ModelRc::from(Rc::new(VecModel::from(vec![IdentityRow {
            name: "robert.VRSCTEST@".into(),
            address: "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".into(),
            status: "Active".into(),
            tone: "online".into(),
            note: Note::default(),
            mine: true,
        }]))));

    let wallet = ui.global::<WalletState>();
    wallet.set_ticker("VRSCTEST".into());
    // Two keys, so the picture includes the selector. A single-key wallet
    // hides it, and the state worth a reference image is the one with a
    // control in it that can put a different address on the QR.
    wallet.set_keys(ModelRc::from(Rc::new(VecModel::from(vec![
        key("main", ADDRESS, "generated", true, true),
        key("savings", SECOND_ADDRESS, "generated", true, false),
    ]))));
    wallet.set_address(ADDRESS.into());
    wallet.set_address_spoken(crate::spoken(ADDRESS).into());
    wallet.set_shielded_address(SHIELDED_ADDRESS.into());
    wallet.set_shielded_address_spoken(crate::spoken(SHIELDED_ADDRESS).into());
    ui.global::<NetworkState>().set_effective("Testnet".into());

    // Scale factor 1: the offscreen renderer draws at exactly the size it is
    // told, so a snapshot must not pretend to be on a Retina display.
    if let Some(image) = crate::qr::encode(&wallet.get_address(), 236.0, 1.0) {
        wallet.set_qr_side(crate::qr::logical_side(&image, 1.0));
        wallet.set_qr(image);
    }
}

/// A wallet with money in it — **mock mode on**, so the picture says so.
pub fn funded(ui: &AppWindow) {
    unlocked(ui);

    // A wallet with figures on it has read a converter too, and the dashboard
    // now has a column for that. Without this the markets column sits at its
    // empty state beside two full ones, which photographs as a bug.
    market_rows(ui);

    let net = ui.global::<NetworkState>();
    net.set_mock_mode(true);
    // A wallet with figures on it has been read from somewhere. Leaving these
    // blank put "Not connected · no block height" underneath a full dashboard,
    // which is a state the wallet cannot actually be in.
    net.set_effective("Testnet".into());
    net.set_tip("1 187 500".into());
    net.set_latency("84 ms".into());

    // The oracle has been asked and answered nothing is switched off. Left
    // out, the severity is the empty string, which the interface reads as "not
    // asked yet" and says so under the asset rows — a different sentence, and
    // not the one a wallet that has read a block height and a converter is in.
    // `converting_halted` and `converting_halt_scheduled` override it.
    ui.global::<HaltState>().set_severity("clear".into());

    let wallet = ui.global::<WalletState>();
    wallet.set_ticker("VRSCTEST".into());
    wallet.set_total("12 482.4200 0000".into());
    wallet.set_spendable("12 382.4200 0000".into());
    wallet.set_immature("100.0000 0000".into());
    // Empty, not "0.0000 0000" — the bridge blanks a figure that is zero rather
    // than passing a zero through, so a fixture that passed one would be
    // photographing a state the wallet cannot produce.
    wallet.set_pending("".into());
    wallet.set_incoming("5.0000 0000".into());
    wallet.set_has_breakdown(true);
    // Set beside the assets rather than derived from them, because the bridge
    // carries it as its own fact and a fixture that computed it would be
    // photographing a different wallet.
    wallet.set_holds_tokens(true);

    wallet.set_assets(ModelRc::from(Rc::new(VecModel::from(vec![
        AssetRow {
            name: "VRSCTEST".into(),
            amount: "12 482.4200 0000".into(),
            secondary: SharedString::new(),
            currency_id: VRSCTEST.into(),
            native: true,
            // No scan has happened in this wallet, so the row is the same two
            // figures the headline is and says so. `funded_with_shielded`
            // replaces the row rather than setting a flag on this one, because
            // the flag and the amount are one statement.
            counts_shielded: false,
            // 12 482.42 at the 0.5372 DAI.vETH the MARKETS column beside it is
            // showing, which is the arithmetic a reviewer can check across the
            // two cards of this one picture. Every figure in `market_rows`
            // comes out of `market::rows` over the scripted chain; this is
            // what `Book::value_of` does with the same book, to the rounding
            // `format::approx_sats` applies.
            value: "6 705.88".into(),
        },
        AssetRow {
            name: "Bridge.vETH".into(),
            // The i-address under the name: a token whose name is missing, or
            // whose name is trying to look like something else, is still told
            // apart by the part that cannot lie.
            //
            // Derived from a fixed twenty bytes, so the checksum is real. A
            // hand-typed one here failed to parse, which nobody would notice in
            // a picture and somebody might copy out of it.
            secondary: "iBoaN7swKAwXgYf1huA3PxBXi5stcfgGMh".into(),
            currency_id: "iBoaN7swKAwXgYf1huA3PxBXi5stcfgGMh".into(),
            amount: "48.5000 0000".into(),
            native: false,
            // There is one shielded pool and it holds the chain's own currency,
            // so no token row can ever have one folded in.
            counts_shielded: false,
            // 48.50 at the 7.09 DAI.vETH on the row two cards over. The second
            // row is priced for a reason: a picture where only the chain's own
            // currency had a value would not show whether the column works for
            // the rows it was asked for, which are the token rows.
            value: "343.88".into(),
        },
    ]))));

    let recent = vec![
        activity("in", "+120.0000 0000", "2 hours", false),
        activity("out", "−50.0000 0000", "yesterday", false),
        activity("in", "+5.0000 0000", "pending", true),
    ];
    wallet.set_activity(ModelRc::from(Rc::new(VecModel::from(recent))));

    // The Activity screen sees the same transactions WITH their day headings —
    // which the dashboard's excerpt deliberately drops, because six rows are
    // not a day.
    wallet.set_tip_height(1_187_500);
    // The scan has not reached the start of the chain, so "Load older" is
    // offered — the state the Activity screen is in almost all of the time.
    wallet.set_history_complete(false);
    wallet.set_history(ModelRc::from(Rc::new(VecModel::from(vec![
        dated("in", "+120.0000 0000", "2 hours", "today", 1_187_400),
        dated("in", "+5.0000 0000", "pending", "", 0),
        dated("out", "−50.0000 0000", "yesterday", "yesterday", 1_186_200),
        dated(
            "in",
            "+12 345.0000 0000 mambo",
            "3 days",
            "9 7",
            1_184_000,
        ),
    ]))));

    balance_history(ui);
}

/// The balance over time behind [`funded`].
///
/// Twelve movements over three months, because the shape this has to lay out
/// well is a staircase with runs of very different lengths — a chart drawn from
/// evenly spaced points would look fine here and wrong in the wallet, since x
/// is proportional to time rather than to position in the list.
///
/// It is part of `funded` rather than a fixture of its own so that every screen
/// showing a wallet with history also shows a chart. A dashboard with four
/// transactions on it and "no history to chart yet" above them is a state that
/// cannot happen.
fn balance_history(ui: &AppWindow) {
    // A day, in seconds, and hundredths of a coin — the unit these readings are
    // legible in. Satoshis would be eight zeros per line and the shape of the
    // series would be impossible to read off the source.
    const DAY: i64 = 86_400;
    const CENTI: i64 = 1_000_000;

    // A fixed "now", so the picture is the same every time it is rendered. A
    // real timestamp would make every snapshot differ from the last one by
    // however long passed between them, and the visual test would be noise.
    let now = 1_770_000_000;

    let movements: [(i64, i64); 12] = [
        (90, 0),
        (88, 40_000),
        (74, 38_000),
        (73, 120_000),
        (55, 115_000),
        (40, 580_000),
        (39, 575_000),
        (22, 990_000),
        (14, 985_000),
        (9, 1_240_000),
        (2, 1_248_242),
        (0, 1_248_242),
    ];

    let points: Vec<pecu_chart::Point> = movements
        .iter()
        .map(|(days_ago, centi)| pecu_chart::Point {
            t: now - days_ago * DAY,
            value: centi * CENTI,
        })
        .collect();

    crate::chart::seed(ui, &points, false, "VRSCTEST", now);
}

fn dated(direction: &str, amount: &str, when: &str, group: &str, height: i32) -> ActivityRow {
    ActivityRow {
        height,
        pending: height == 0,
        ..activity_in(direction, amount, when, height == 0, group)
    }
}

fn activity(direction: &str, amount: &str, when: &str, pending: bool) -> ActivityRow {
    activity_in(direction, amount, when, pending, "")
}

fn activity_in(
    direction: &str,
    amount: &str,
    when: &str,
    pending: bool,
    group: &str,
) -> ActivityRow {
    ActivityRow {
        // Not a real txid, and not 64 hex characters — the same reasoning as
        // the mock chain's `mock…` ids: it must be impossible to mistake for
        // something you could look up.
        txid: "fixture-not-a-real-transaction".into(),
        txid_short: "fixtu…ction".into(),
        direction: direction.into(),
        amount: amount.into(),
        when: when_note(when),
        pending,
        height: 0,
        group: group_note(group),
        kind: "payment".into(),
        note: SharedString::new(),
    }
}

/// A relative time, as the core spells one.
///
/// The fixtures used to write a clock — "14:02" — and the wallet has never
/// produced one: `portfolio::when` says "2 hours ago", in the row's own
/// language. A reference image of a format the product cannot emit is a picture
/// of a different wallet.
// Panicking is the point. These run only when a reference image is being
// rendered, so a refusal here is a failed render rather than anything a user
// could reach — and the alternative, returning an empty note, is what let
// "2 hours ago" become the code `when-hours ago` and print itself on the
// dashboard for one commit.
#[allow(clippy::panic)]
fn when_note(shorthand: &str) -> Note {
    match shorthand {
        "pending" => note("when-pending", &[]),
        "now" => note("when-just-now", &[]),
        "yesterday" => note("when-yesterday", &[]),
        other => match other.split_once(' ') {
            Some((count, unit))
                if matches!(unit, "minutes" | "hours" | "days" | "months") =>
            {
                note(&format!("when-{unit}"), &[count])
            }
            // Anything else is a fixture writing a shape the core does not
            // emit. Refused loudly rather than turned into a code nobody wrote
            // a sentence for — "2 hours ago" became `when-hours ago`, and the
            // dashboard printed exactly that.
            _ => panic!("a fixture asked for a time this wallet cannot say: {other:?}"),
        },
    }
}

/// A day heading, likewise.
#[allow(clippy::panic)]
fn group_note(shorthand: &str) -> Note {
    match shorthand {
        "" => Note::default(),
        "today" => note("day-today", &[]),
        "yesterday" => note("day-yesterday", &[]),
        "pending" => note("day-pending", &[]),
        // "9 7" — the ninth of month index seven, which is August. The index
        // rather than the name, because that is what the core sends: naming
        // the month is `note.slint`'s job, and a fixture that named it here
        // would photograph a heading the wallet cannot produce.
        other => match other.split_once(' ') {
            Some((day, month)) => note("day-this-year", &[day, month]),
            None => panic!("a fixture asked for a day heading in a shape the core does not send: {other:?}"),
        },
    }
}

/// A row that is not a payment: a login, an identity action, a conversion.
///
/// None of these can be produced by this wallet yet — history is read from
/// address deltas, and none of the three is one. They exist so the filters and
/// the row styling have something to be photographed against, and so the shapes
/// are settled before the features arrive.
fn activity_of(kind: &str, note: &str, amount: &str, when: &str, group: &str) -> ActivityRow {
    ActivityRow {
        txid: "fixture-not-a-real-transaction".into(),
        txid_short: "fixtu…ction".into(),
        direction: "self".into(),
        amount: amount.into(),
        when: when_note(when),
        pending: false,
        height: 1_187_400,
        group: group_note(group),
        kind: kind.into(),
        note: note.into(),
    }
}

/// The restore form, with the message shown when a phrase fails its checksum.
///
/// The error state is the one worth a reference image: the happy path is an
/// empty form, and what has to stay readable is the sentence someone reads
/// after mistyping one word of twenty-four.
///
pub fn restoring(ui: &AppWindow) {
    fresh(ui);
    ui.set_restoring(true);
    ui.global::<WalletState>()
        .set_problem(note("phrase-checksum", &[]));
}

/// A wallet that exists and is locked — the screen most sessions start on.
///
/// It had no reference image at all, which is how a screen ends up being the
/// one nobody has looked at. Every launch after the first one lands here.
pub fn locked(ui: &AppWindow) {
    unlocked(ui);
    let wallet = ui.global::<WalletState>();
    wallet.set_locked(true);
    wallet.set_name("Pecu".into());
}

/// The same screen after a wrong passphrase.
///
/// Its own case because the refusal is the whole point of it: a message that
/// lands somewhere nobody looks is the same as no message.
pub fn locked_refused(ui: &AppWindow) {
    locked(ui);
    ui.global::<WalletState>()
        .set_problem(note("passphrase-wrong", &[]));
}

/// The send form with more asked for than the wallet holds.
///
/// The most common way a payment fails, and the one place the amount field has
/// something to say. Photographed because a note nobody has seen rendered is a
/// note that can be the wrong length, the wrong colour, or absent.
/// Shielding: paying a `zs…` out of the transparent balance.
///
/// The state that needs a picture most. Three things only exist together here
/// — the source selector, a shielded destination, and the sentence naming what
/// the combination does — and each of them is meaningless without the others.
/// A dashboard whose wallet has looked at its shielded pool and found some.
///
/// Its own fixture rather than a change to `funded`, because the state worth a
/// picture is the *pair*: a public balance beside a private one. A wallet that
/// has never scanned shows no shielded column at all, and that is the ordinary
/// case the other images already cover.
pub fn funded_with_shielded(ui: &AppWindow) {
    funded(ui);
    let wallet = ui.global::<WalletState>();
    wallet.set_shielded_balance("2.5000 0000".into());
    // Both, because they are separate questions: whether a scan happened, and
    // whether it found anything. A picture with only the first would show a
    // column the wallet does not draw.
    wallet.set_shielded_any(true);

    // And the ASSETS row moves with it, which is the whole reason this picture
    // is worth taking twice. The headline stays at 12 482.42 — spendable and
    // maturing — while the row underneath reads 12 484.92, because a holdings
    // list answers "what do I hold in this currency" and the private half of it
    // is still held. The two figures differ on purpose and each says which it
    // is: this is the image where a reviewer can check that they do.
    //
    // Edited in place rather than rebuilt, so the token row beside it stays
    // exactly the one `funded` photographs.
    wallet.get_assets().set_row_data(
        0,
        AssetRow {
            name: "VRSCTEST".into(),
            amount: "12 484.9200 0000".into(),
            secondary: SharedString::new(),
            currency_id: VRSCTEST.into(),
            native: true,
            counts_shielded: true,
            // The value moves with the amount, because it is the amount priced:
            // 12 484.92 at 0.5372 rather than 12 482.42 at it. A picture where
            // the shielded pool was folded into the holding and not into what
            // the holding is worth would be two figures on one row disagreeing
            // about what is in the row.
            value: "6 707.22".into(),
        },
    );
}

pub fn shielding(ui: &AppWindow) {
    sending(ui);

    let send = ui.global::<SendState>();
    send.set_shielded_available(true);
    send.set_shielded_balance("2.5000 0000".into());
    send.set_shielded_scanned(true);
    send.set_from_shielded(false);
    send.set_to_draft(SHIELDED_ADDRESS.into());
    send.set_amount_draft("1.5".into());
    send.set_to_valid(true);
    send.set_to_note(Note {
        code: "address-shielded".into(),
        ..Default::default()
    });
    send.set_amount_valid(true);
    send.set_ready(true);
    send.set_route("shield".into());
}

/// Paying out of the shielded balance to a public address — an unshield.
///
/// Worth its own image because it is the route with the sharpest consequence:
/// the amount and the recipient become public at the moment it lands, and the
/// sentence has to say so before the button is pressed rather than after.
pub fn unshielding(ui: &AppWindow) {
    shielding(ui);

    let send = ui.global::<SendState>();
    send.set_from_shielded(true);
    send.set_to_draft(SECOND_ADDRESS.into());
    send.set_to_note(Note {
        code: "address-transparent".into(),
        ..Default::default()
    });
    send.set_route("unshield".into());
}

pub fn sending_too_much(ui: &AppWindow) {
    sending(ui);
    let send = ui.global::<SendState>();
    send.set_to_draft(SECOND_ADDRESS.into());
    send.set_to_valid(true);
    // Codes the core actually emits, and the label as its own field.
    //
    // These used to be invented sentences — "Paid before · the exchange", and
    // an amount note with wording no version of this wallet has ever produced.
    // The reference image was therefore a picture of text the product cannot
    // show, which is the failure the fixtures warn about in three other places.
    // A code cannot be invented: an unknown one renders as itself.
    send.set_to_note(note("address-transparent", &[]));
    send.set_to_label("the exchange".into());
    send.set_amount_draft("99 999.0000 0000".into());
    send.set_amount_valid(false);
    // The figure the core would quote: what the *active key* can spend, which
    // is what `validate_draft` now measures a draft against.
    send.set_amount_note(note("amount-above-spendable", &["8 000.0000 0000"]));
}

/// Emptying a key: the amount field is gone and a sentence stands in its place.
///
/// The state the reference image exists to hold honest. Two things have to be
/// visible in it and neither can be checked by a unit test: that there is no
/// box to type a number into — the invariant the old "no Max button" comment
/// was protecting, now held by absence rather than by argument — and that the
/// maturing balance is named. Somebody sweeping a key before they decommission
/// the machine holding it needs to be told what is staying behind.
///
/// The figure in that sentence comes from `sending`, and it is the **active
/// key's** 40 coins rather than the wallet's 100. That is the whole reason this
/// image is worth keeping: the two numbers are different, so a form that went
/// back to quoting the wallet-wide total would move these pixels.
pub fn sending_everything(ui: &AppWindow) {
    sending(ui);
    let send = ui.global::<SendState>();
    send.set_to_draft(SECOND_ADDRESS.into());
    send.set_to_valid(true);
    send.set_to_note(note("address-transparent", &[]));
    send.set_to_label("the exchange".into());
    send.set_send_all(true);
    // What the core says about a send-all: nothing to object to, and nothing
    // typed to object about.
    send.set_amount_valid(true);
    send.set_amount_note(Note::default());
    send.set_ready(true);
}

/// The network screen with everything that can be wrong with a node.
///
/// One offline, one answering for the wrong chain, one behind the tip. Only the
/// healthy case had an image, so the three states the node list exists to
/// distinguish were never looked at together.
pub fn network_trouble(ui: &AppWindow) {
    unlocked(ui);
    ui.set_screen("settings".into());
    ui.set_settings_tab(4);

    let net = ui.global::<NetworkState>();
    // One node, behind the chain.
    //
    // This fixture used to hold three: the testnet endpoint, `api.verus.services`
    // marked as being on the wrong chain, and one somebody had typed in. Neither
    // of the last two can happen any more — the shipped list is one endpoint per
    // chain, and there is no form to add another. Photographing states the
    // wallet can no longer reach would be photographing a different program.
    //
    // What is left is the trouble that is still real: the one endpoint there is,
    // catching up.
    net.set_nodes(ModelRc::from(Rc::new(VecModel::from(vec![NodeRow {
        id: 0,
        label: "VRSCTEST (public)".into(),
        url: "https://api.verustest.net".into(),
        status: "degraded".into(),
        network: "Testnet".into(),
        tip: "1 187 102".into(),
        latency: "612 ms".into(),
        note: note("node-catching-up", &["1 187 102", "1 187 500"]),
        builtin: true,
        active: true,
    }]))));
    net.set_requested("Testnet".into());
    net.set_requested_name("VRSCTEST".into());
    net.set_chain_title("Testnet".into());
    net.set_effective("Testnet".into());
    net.set_syncing(true);
    net.set_endpoint("https://api.verustest.net".into());
    net.set_latency("612 ms".into());
    // The title bar's dot follows the ACTIVE node, and the bridge derives it
    // from that node's status rather than from the row. Left unset, this image
    // showed a calm grey dot above a node list full of trouble — a difference
    // between the picture and the running application, which is the one thing
    // these images exist to catch.
    net.set_node_state("degraded".into());
}

/// A wallet whose phrase has never been written down, on the dashboard.
pub fn backup_due(ui: &AppWindow) {
    unlocked(ui);
    ui.global::<WalletState>().set_backup_key("main".into());
}

/// The phrase screen, concealed — which is how it looks unless the reveal
/// button is being physically held.
///
/// **No real words here, and that is deliberate.** These fixtures are rendered
/// to PNGs that get committed and looked at; twenty-four checked-in BIP-39
/// words would be indistinguishable at a glance from someone's actual phrase,
/// and the first person to find them in a repository has no way to know it was
/// a fixture. The concealed state is also the honest default.
pub fn backup_phrase(ui: &AppWindow) {
    backup_due(ui);
    let seed = ui.global::<SeedState>();
    seed.set_words(ModelRc::from(Rc::new(VecModel::from(masked(24)))));
    seed.set_challenge(ModelRc::from(Rc::new(VecModel::from(vec![3, 11, 19]))));
    seed.set_step("phrase".into());
}

/// The passphrase re-prompt, which is the gate in front of every reveal.
///
/// It had no reference image, which was survivable while it was a step on the
/// way back to an unfinished backup and is not now: the same gate now stands in
/// front of every key's words for the rest of the wallet's life, and it is the
/// only thing between an unlocked wallet and them.
///
/// Seeded on the dashboard-banner route — a key the wallet is nagging about,
/// which is why `backup_due` is underneath. The re-read route reaches the same
/// gate and photographs the same thing: what the two routes differ about is how
/// the sitting ends, and [`backup_reread`] is the picture of that.
pub fn backup_passphrase(ui: &AppWindow) {
    backup_due(ui);
    let seed = ui.global::<SeedState>();
    seed.set_label("main".into());
    seed.set_step("passphrase".into());
}

/// Reading a phrase again, from a row on the keys screen.
///
/// The same words behind the same mask as [`backup_phrase`] — and no real ones
/// here either, for the reason spelled out there. What differs is the heading
/// and the way out: there is nothing to prove at the end of a re-read, so the
/// pair of buttons that ends in a quiz is one button that closes the screen.
pub fn backup_reread(ui: &AppWindow) {
    backup_phrase(ui);
    // Reached from a key row rather than from the banner. This key's phrase was
    // written down long ago, so the dashboard has nothing to nag about — and
    // showing it again does not give it something.
    ui.global::<WalletState>().set_backup_key(SharedString::new());

    let seed = ui.global::<SeedState>();
    seed.set_label("main".into());
    seed.set_re_reading(true);
}

/// The confirmation step, after a wrong answer — the state worth looking at,
/// because it is the one with an error in it.
pub fn backup_verify(ui: &AppWindow) {
    backup_phrase(ui);
    let seed = ui.global::<SeedState>();
    seed.set_step("verify".into());
    seed.set_problem(
        note("phrase-mismatch", &[]),
    );
}

fn masked(count: i32) -> Vec<SeedWord> {
    (1..=count)
        .map(|index| SeedWord {
            index,
            word: "••••••".into(),
        })
        .collect()
}

/// The network screen with nodes that have actually been asked something.
///
/// The default fixture leaves every node at "unknown", which is true for about
/// two seconds after launch and hides everything this screen is for: the status
/// column, the latency, and the Remove button that only a user-added endpoint
/// gets. All three are laid out here at once, because a row that looks right
/// alone can still collide with the one beside it.
/// The VerusIDs list, showing all four states at once.
///
/// Four, because the two a two-state wallet cannot express are exactly the two
/// worth a reference image: a lock with no countdown, and a countdown running.
/// The i-addresses are real ones from VRSCTEST, so they parse.
pub fn identities(ui: &AppWindow) {
    unlocked(ui);
    ui.set_screen("identities".into());
    // These are testnet identities; the ticker has to say so, or the detail
    // sheet prices what one holds in the wrong currency.
    ui.global::<WalletState>().set_ticker("VRSCTEST".into());

    let rows = vec![
        IdentityRow {
            name: "robert.VRSCTEST@".into(),
            address: "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".into(),
            status: "Active".into(),
            tone: "online".into(),
            note: Note::default(),
            mine: true,
        },
        IdentityRow {
            name: "vault.VRSCTEST@".into(),
            address: "i5Qcj82gvrHdHCCvTwy2yCFeMz3s3dgB6m".into(),
            status: "Locked".into(),
            tone: "degraded".into(),
            note: note("identity-locked", &["100"]),
            mine: true,
        },
        IdentityRow {
            name: "moving.VRSCTEST@".into(),
            address: "i87QZVSS7SosM5choTJE7Dy4SNRt5vAEhr".into(),
            status: "Unlocking".into(),
            tone: "degraded".into(),
            note: note("identity-unlocking", &["1 188 900"]),
            mine: true,
        },
    ];

    let state = ui.global::<IdentityState>();
    state.set_rows(ModelRc::from(Rc::new(VecModel::from(rows))));

    // Somebody else's, looked up — in its own section, under a heading that
    // says whose it is not.
    //
    // This is the state a real session found: an identity that is not yours
    // used to sit in the list above, under a sentence claiming it was found by
    // asking which names your keys control, and stayed there until the wallet
    // was restarted.
    state.set_looked_up(ModelRc::from(Rc::new(VecModel::from(vec![IdentityRow {
        name: "stranger.VRSCTEST@".into(),
        address: "i92nDT1FzULuXGGXbCt8VHC4qpYc2R1Bfr".into(),
        status: "Revoked".into(),
        tone: "offline".into(),
        note: note("identity-revoked", &[]),
        mine: false,
    }]))));
}

/// The currencies screen with both halves populated.
///
/// Two currencies and four identities, deliberately mismatched: a fixed-supply
/// token, a mintable basket, and two identities that could still define one —
/// plus one that could not, because this wallet does not hold its keys. A
/// fixture where every identity had a currency would never render the picker,
/// and one where none did would never render the list.
pub fn currencies(ui: &AppWindow) {
    unlocked(ui);
    ui.set_screen("currencies".into());
    ui.global::<WalletState>().set_ticker("VRSCTEST".into());

    let state = ui.global::<CurrencyState>();
    state.set_rows(ModelRc::from(Rc::new(VecModel::from(vec![
        CurrencyRow {
            name: "demo.VRSCTEST".into(),
            address: "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".into(),
            kind: "Token".into(),
            tone: "online".into(),
            note: Note::default(),
            mintable: false,
            start_block: "1 170 000".into(),
            started: true,
        },
        CurrencyRow {
            name: "market.VRSCTEST".into(),
            address: "i5Qcj82gvrHdHCCvTwy2yCFeMz3s3dgB6m".into(),
            kind: "Basket".into(),
            tone: "online".into(),
            note: note("kind-basket", &[]),
            mintable: true,
            start_block: "1 171 402".into(),
            // Launched and not yet begun — the twenty-minute window in which a
            // currency exists and does nothing. A list where every row had
            // started would never render the line that says so.
            started: false,
        },
    ]))));

    state.set_eligible(ModelRc::from(Rc::new(VecModel::from(vec![
        EligibleIdentity {
            name: "demo.VRSCTEST@".into(),
            address: "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".into(),
            refusal: note("eligible-already-defines", &["demo.VRSCTEST"]),
        },
        // The second currency's own identity. Present with a refusal rather
        // than absent: every identity appears in this list, and one that had
        // silently vanished because it already defines something would describe
        // a wallet the core cannot produce.
        EligibleIdentity {
            name: "market.VRSCTEST@".into(),
            address: "i5Qcj82gvrHdHCCvTwy2yCFeMz3s3dgB6m".into(),
            refusal: note("eligible-already-defines", &["market.VRSCTEST"]),
        },
        // The two refusals that are about the identity's state rather than
        // about a currency. Both are refused in the picker now, and neither had
        // an image: a launch under a revoked identity is refused by the flow
        // several screens later, and one under a timelocked identity is refused
        // by nothing at all until the node sees it.
        EligibleIdentity {
            name: "vault.VRSCTEST@".into(),
            address: "i87QZVSS7SosM5choTJE7Dy4SNRt5vAEhr".into(),
            refusal: note("eligible-timelocked", &[]),
        },
        EligibleIdentity {
            name: "gone.VRSCTEST@".into(),
            address: "i87QZVSS7SosM5choTJE7Dy4SNRt5vAEhr".into(),
            refusal: note("eligible-revoked", &[]),
        },
        EligibleIdentity {
            name: "spare.VRSCTEST@".into(),
            address: "i92nDT1FzULuXGGXbCt8VHC4qpYc2R1Bfr".into(),
            refusal: Note::default(),
        },
        EligibleIdentity {
            name: "borrowed.VRSCTEST@".into(),
            // Derived from the fixed scalar `[0x2b; 32]`, the way the R
            // addresses above it are. It used to carry VRSCTEST's own currency
            // id, which is a real address belonging to the chain itself — and
            // the reserve picker now shows that id under the name it actually
            // has, so the same twenty bytes appeared twice on one screen under
            // two different names.
            address: "i5irTLNFVvjQESy3bMjxoG7CXg8Ntmdc9V".into(),
            refusal: note("eligible-cannot-sign", &[]),
        },
    ]))));
}

/// The form's third question: a basket with three uneven reserves.
///
/// Three and uneven on purpose: two equal slices demonstrate nothing about a
/// proportional bar, and the interesting case is the one where the eye cannot
/// check the arithmetic. The weights add to 100% here, so the bar is full and
/// the total reads green — the short-bar case is its own fixture.
///
/// This is also the base every other currency-form fixture is built on, so it
/// fills in a complete draft and the answers the core would give about it. Each
/// of the others changes the step it is on and nothing else, which is what keeps
/// five reference images describing one wallet rather than five.
pub fn defining_currency(ui: &AppWindow) {
    currencies(ui);

    let state = ui.global::<CurrencyState>();
    state.set_defining(true);
    state.set_form_step("setup".into());
    state.set_kind("basket".into());
    // The one in the list with no refusal. It used to be `vault@`, which the
    // picker now refuses for being timelocked — so the image showed a selection
    // the wallet would not make.
    state.set_identity("i92nDT1FzULuXGGXbCt8VHC4qpYc2R1Bfr".into());
    state.set_identity_name("spare.VRSCTEST@".into());
    state.set_start_delay("20".into());
    state.set_mintable(true);

    state.set_reserve_rows(ModelRc::from(Rc::new(VecModel::from(vec![
        reserve("VRSCTEST", VRSCTEST, "50"),
        reserve("Bridge.vETH", BRIDGE, "30"),
        reserve("vUSDC.vETH", VUSDC, "20"),
    ]))));

    state.set_supply_rows(ModelRc::from(Rc::new(VecModel::from(vec![
        PreallocEntry {
            recipient: "demo@".into(),
            amount: "750000".into(),
        },
        PreallocEntry {
            recipient: "vault@".into(),
            amount: "250000".into(),
        },
    ]))));

    // What core would answer. Written out rather than computed here, because a
    // fixture that did its own arithmetic would photograph a picture the core
    // cannot produce.
    state.set_slices(ModelRc::from(Rc::new(VecModel::from(vec![
        slice("VRSCTEST", 50.0, 0.0, "50%", "accent"),
        slice("Bridge.vETH", 30.0, 50.0, "30%", "positive"),
        slice("vUSDC.vETH", 20.0, 80.0, "20%", "warning"),
    ]))));
    state.set_weights_total("100%".into());

    state.set_supply_slices(ModelRc::from(Rc::new(VecModel::from(vec![
        slice("demo@", 75.0, 0.0, "75%", "accent"),
        slice("vault@", 25.0, 75.0, "25%", "positive"),
    ]))));
    state.set_supply_total("1 000 000.0000 0000".into());
    state.set_start_block("1 187 520".into());
    state.set_fee("200.0000 0000".into());

    state.set_preview(ModelRc::from(Rc::new(VecModel::from(vec![
        field("field-kind", "Basket"),
        // Both, because they answer different halves of one check: the name
        // is what was chosen, the address is what goes on the chain, and
        // nobody can verify an address they never typed.
        field("field-defined-under", "spare.VRSCTEST@"),
        field("field-its-address", "i92nDT1FzULuXGGXbCt8VHC4qpYc2R1Bfr"),
        field("field-starts-at-block", "1 187 520"),
        sentence("field-supply-can-grow", "field-mintable-yes"),
        field("field-reserves", "VRSCTEST 50%, Bridge.vETH 30%, vUSDC.vETH 20%"),
        field("field-starting-supply", "1 000 000.0000 0000 VRSCTEST"),
    ]))));

    // One transaction, because this is the short path: an identity that already
    // exists, one definition, one fee. The three-step list belongs to the path
    // that claims a name — `claiming_a_name_for_a_currency` carries that one.
    state.set_steps(ModelRc::from(Rc::new(VecModel::from(vec![step(
        "Define the currency",
        "later",
        true,
    )]))));

    problems(&state, Vec::new());
}

/// The first question, on a fresh form: what is being made.
pub fn currency_kind(ui: &AppWindow) {
    defining_currency(ui);
    ui.global::<CurrencyState>().set_form_step("kind".into());
}

/// The second question, unanswered — the identity picker with its refusals.
///
/// Its own image because this is the list that decides whether the screen is
/// usable at all on a given wallet, and four of its five rows are refusals with
/// a different reason each. A picture of it collapsed to the one chosen row says
/// nothing about the four that were not.
pub fn currency_identity(ui: &AppWindow) {
    defining_currency(ui);

    let state = ui.global::<CurrencyState>();
    state.set_form_step("under".into());
    state.set_identity("".into());
    state.set_identity_name("".into());
}

/// Choosing a reserve out of what the chain has.
///
/// Its own image because the whole point of the overlay is that a reserve is
/// **chosen and not typed** — the definition carries an i-address, and a name in
/// that field was accepted by every check on the form and then refused by the
/// builder after the launch had been agreed to.
///
/// Four rows, one of each thing a row has to be able to say: a plain token, a
/// basket, an NFT, and a currency that has not started yet — which is the one
/// fact about a reserve that is invisible in its name and changes what putting
/// it in a basket means.
pub fn currency_reserve_picker(ui: &AppWindow) {
    defining_currency(ui);

    let state = ui.global::<CurrencyState>();
    // The third row, which is the one with a weight of 20% in the form behind.
    state.set_picking_reserve(2);
    state.set_choices_query("".into());
    state.set_choices(ModelRc::from(Rc::new(VecModel::from(vec![
        pick("VRSCTEST", VRSCTEST, "Token", ""),
        pick("Bridge.vETH", BRIDGE, "Basket", ""),
        pick("vUSDC.vETH", VUSDC, "Token", ""),
        pick(
            "stamp137",
            "iJMWZJ9KMTpado8MqdcGsCwDtWC8qqYvUP",
            "NFT",
            "Starts at block 1 189 000",
        ),
    ]))));
    // More than fit, said rather than hidden.
    state.set_choices_more(286);
}

/// An NFT at the same step, which is the short version of it.
///
/// Its own image for two reasons. It is the only kind with nothing to fill in —
/// no reserves, no supply — so it is the one case where the two questions that
/// are always asked, minting and the start height, are visible without
/// scrolling, and they had never been photographed. And it carries the caveat
/// that no NFT has ever been accepted by a node from this SDK, which is a
/// warning rather than a refusal and has to look like one.
pub fn currency_nft(ui: &AppWindow) {
    defining_currency(ui);

    let state = ui.global::<CurrencyState>();
    state.set_kind("nft".into());
    state.set_mintable(false);
    state.set_reserve_rows(ModelRc::from(Rc::new(VecModel::from(
        Vec::<ReserveEntry>::new(),
    ))));
    state.set_supply_rows(ModelRc::from(Rc::new(VecModel::from(
        Vec::<PreallocEntry>::new(),
    ))));
    state.set_slices(ModelRc::from(Rc::new(VecModel::from(
        Vec::<CurrencySlice>::new(),
    ))));
    state.set_supply_slices(ModelRc::from(Rc::new(VecModel::from(
        Vec::<CurrencySlice>::new(),
    ))));
    state.set_preview(ModelRc::from(Rc::new(VecModel::from(vec![
        field("field-kind", "NFT"),
        field("field-defined-under", "spare.VRSCTEST@"),
        field("field-its-address", "i92nDT1FzULuXGGXbCt8VHC4qpYc2R1Bfr"),
        field("field-starts-at-block", "1 187 520"),
        sentence("field-supply-can-grow", "field-mintable-no"),
    ]))));

    problems(
        &state,
        vec![CurrencyProblem {
            blocking: false,
            text: note("draft-nft-never-sent", &[]),
        }],
    );
}

/// The last question: the whole definition, at full width, before it is signed.
pub fn currency_review(ui: &AppWindow) {
    defining_currency(ui);
    ui.global::<CurrencyState>().set_form_step("review".into());
}

/// The same form with weights that do not add up — the case the bar exists for.
///
/// The one basket mistake that cannot be fixed after the launch, and the one
/// the SDK's Rust core does not refuse. The bar draws short and the total reads
/// amber, which is the picture and the number saying the same thing.
/// An identity registered and paid for, with nothing defined under it.
///
/// The state a crash between the two transactions leaves behind, and the one
/// worth a reference image: it is not a loading state, it is a bill somebody
/// has already settled with nothing to show for it — and that identity can
/// never be used for a different currency.
pub fn launch_pending(ui: &AppWindow) {
    currencies(ui);

    let state = ui.global::<CurrencyState>();
    state.set_pending_note(note("launch-name-ready", &["market.VRSCTEST@"]));
    state.set_pending_can_continue(true);
    // Progress, not a plan: the first three have happened and two of them were
    // paid for. This is the state where the money is already gone and the thing
    // it was for does not exist yet.
    state.set_pending_steps(ModelRc::from(Rc::new(VecModel::from(vec![
        step("Choose a name", "done", false),
        step("Register the identity", "done", true),
        step("Wait for it to confirm", "done", false),
        step("Define the currency", "now", true),
    ]))));
    // Last, because a non-empty identity is what opens the panel.
    state.set_pending_identity("market.VRSCTEST@".into());
}

/// The other way in: claiming a name as part of the launch.
///
/// Its own image because it is the entry point that makes this screen usable on
/// a wallet with no spare identity — which is every wallet, the first time —
/// and because the press at the end of it is the only one on this screen that
/// spends money **before** showing a review. The two fees are stated together
/// for that reason.
pub fn claiming_a_name_for_a_currency(ui: &AppWindow) {
    defining_currency(ui);

    let state = ui.global::<CurrencyState>();
    state.set_form_step("under".into());
    state.set_claiming(true);
    state.set_identity("".into());
    state.set_identity_name("".into());
    state.set_new_name("livecoin".into());
    state.set_new_name_fee("100.0000 0000".into());
    state.set_new_authority("other".into());
    state.set_new_revocation("vault.VRSCTEST@".into());
    state.set_new_recovery("vault.VRSCTEST@".into());
    // Three steps, not one: this path is three transactions with a wait in the
    // middle, and the review says so before anything is pressed.
    state.set_steps(ModelRc::from(Rc::new(VecModel::from(vec![
        step("Register the identity", "later", true),
        step("Wait for it to confirm", "later", false),
        step("Define the currency", "later", true),
    ]))));
    state.set_preview(ModelRc::from(Rc::new(VecModel::from(vec![
        field("field-kind", "Basket"),
        field("field-defined-under", "livecoin@ (to be claimed)"),
        field("field-starts-at-block", "1 187 520"),
        sentence("field-supply-can-grow", "field-mintable-yes"),
        field("field-reserves", "VRSCTEST 50%, Bridge.vETH 30%, vUSDC.vETH 20%"),
        field("field-starting-supply", "1 000 000.0000 0000 VRSCTEST"),
    ]))));
}

/// The extra question that path asks, with neither answer given yet.
///
/// Its own image because the unanswered state is the point of the step: leaving
/// both authorities blank is what makes an identity permanently unrevokable, and
/// on the flat form that outcome was what happened to somebody who read nothing.
/// A picture of it already answered would not show that.
pub fn currency_authority(ui: &AppWindow) {
    claiming_a_name_for_a_currency(ui);

    let state = ui.global::<CurrencyState>();
    state.set_form_step("authority".into());
    state.set_new_authority("".into());
    state.set_new_revocation("".into());
    state.set_new_recovery("".into());
}

/// The launch review: the last moment before something irreversible.
///
/// Its own image because it is the screen that spends two hundred coins, and it
/// was written and wired without ever being looked at. The three figures are
/// what the review exists for — the fee, the half that becomes the currency's
/// reserve deposit, and the half that is burned with no output at all.
pub fn launching_currency(ui: &AppWindow) {
    defining_currency(ui);

    let state = ui.global::<CurrencyState>();
    state.set_launch_name("market.VRSCTEST".into());
    state.set_launch_description(note("launch-defines-once", &["market.VRSCTEST"]));
    // VRSCTEST's own figures, and the halves add back to the fee — the property
    // `currency::cost` is tested for.
    state.set_launch_fee("200.0000 0000".into());
    state.set_launch_deposit("100.0000 0000".into());
    state.set_launch_burned("100.0000 0000".into());
    // The same height the form behind this one is showing. They are the same
    // number in the running wallet — the review reads it back off the signed
    // bytes rather than off the form — and a reference image where the two
    // disagree teaches that they are allowed to.
    state.set_launch_start_block("1 187 520".into());
    // Last, because a non-zero ticket is what opens the review.
    state.set_launch_ticket(4);
}

pub fn currency_weights_wrong(ui: &AppWindow) {
    defining_currency(ui);

    let state = ui.global::<CurrencyState>();
    state.set_reserve_rows(ModelRc::from(Rc::new(VecModel::from(vec![
        reserve("VRSCTEST", VRSCTEST, "40"),
        reserve("Bridge.vETH", BRIDGE, "30"),
    ]))));
    state.set_slices(ModelRc::from(Rc::new(VecModel::from(vec![
        slice("VRSCTEST", 40.0, 0.0, "40%", "accent"),
        slice("Bridge.vETH", 30.0, 40.0, "30%", "positive"),
    ]))));
    state.set_weights_total("70%".into());
    problems(
        &state,
        vec![CurrencyProblem {
            blocking: true,
            text: note("draft-weights-wrong", &["70%"]),
        }],
    );
}

/// Set the problem list, the count the pinned bar reads, and whether the form
/// is ready — together, because they are three readings of one fact.
///
/// Set separately for one render and they disagreed: the bar said "nothing is
/// blocking it" beside a disabled button and a basket whose weights added to
/// 70%. The count exists because Slint cannot filter a model in a binding, and
/// a derived value nobody derives is a value that drifts.
fn problems(state: &CurrencyState, rows: Vec<CurrencyProblem>) {
    let blocking = rows.iter().filter(|problem| problem.blocking).count();
    state.set_problems(ModelRc::from(Rc::new(VecModel::from(rows))));
    state.set_blocking_count(i32::try_from(blocking).unwrap_or(i32::MAX));
    state.set_ready(blocking == 0);
}

/// The i-addresses the reserve fixtures use.
///
/// `VRSCTEST` is the real one, read out of the SDK's own recorded
/// `listcurrencies` reply — it is the chain's own currency and belongs to
/// nobody. The other two are derived from fixed scalars (`[0x3d; 32]` and
/// `[0x4f; 32]`) the way the R addresses at the top of this file are: those
/// currencies exist on VRSCTEST but their addresses are not in anything this
/// wallet has recorded, and inventing one by hand is how a checksum ends up
/// wrong in a picture somebody copies out of.
const VRSCTEST: &str = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq";
const BRIDGE: &str = "iPy29AAA5AUgVPzDvCWtWkNTVCyyMn8X4X";
const VUSDC: &str = "i3XUvfqfZ3eT6iwNgyVEVzFVimMrziG5Ws";

fn pick(name: &str, address: &str, kind: &str, starts_at: &str) -> CurrencyPick {
    CurrencyPick {
        name: name.into(),
        address: address.into(),
        kind: kind.into(),
        note: if starts_at.is_empty() {
            Note::default()
        } else {
            note("currency-starts-at", &[starts_at])
        },
    }
}

fn reserve(name: &str, address: &str, weight: &str) -> ReserveEntry {
    ReserveEntry {
        // The address is what the definition carries; the name is what anybody
        // reads. Both, because neither can be derived from the other here.
        currency: address.into(),
        name: name.into(),
        weight: weight.into(),
    }
}

/// One slice of a bar, with where it starts written out.
///
/// The offset is given rather than accumulated, for the reason every figure in
/// this file is: a fixture that did its own arithmetic would photograph a
/// picture the core cannot produce.
fn slice(label: &str, percent: f32, offset: f32, display: &str, tone: &str) -> CurrencySlice {
    CurrencySlice {
        label: label.into(),
        percent,
        offset_percent: offset,
        percent_display: display.into(),
        tone: tone.into(),
    }
}

fn field(label: &str, value: &str) -> CurrencyField {
    CurrencyField {
        label: note(label, &[]),
        value: value.into(),
        value_note: Note::default(),
        // Every field in a currency definition is permanent, which is the
        // reason the panel exists.
        permanent: true,
    }
}

/// The two lines whose value is a sentence rather than a figure.
fn sentence(label: &str, value: &str) -> CurrencyField {
    CurrencyField {
        label: note(label, &[]),
        value: slint::SharedString::new(),
        value_note: note(value, &[]),
        permanent: true,
    }
}

fn step(label: &str, state: &str, costs: bool) -> FlowStep {
    FlowStep {
        label: note(label, &[]),
        state: state.into(),
        costs,
    }
}

/// The screen before any identity defines anything — which is where every
/// wallet starts, and the state the empty text was written for.
pub fn currencies_empty(ui: &AppWindow) {
    unlocked(ui);
    ui.set_screen("currencies".into());
    ui.global::<WalletState>().set_ticker("VRSCTEST".into());

    ui.global::<CurrencyState>()
        .set_eligible(ModelRc::from(Rc::new(VecModel::from(vec![
            EligibleIdentity {
                name: "demo.VRSCTEST@".into(),
                address: "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".into(),
                refusal: Note::default(),
            },
        ]))));
}

/// The claim form, open.
///
/// Its own case because the form is now behind a button. Without this the
/// three fields, the authority warning and the name rule would have no
/// reference image at all — a screen that exists and that nothing looks at.
pub fn claiming_a_name(ui: &AppWindow) {
    identities(ui);
    let state = ui.global::<IdentityState>();
    state.set_claiming(true);
    state.set_claim_step("name".into());
    state.set_name_draft("pecu".into());
    state.set_name_fee("100.0000 0000".into());
}

/// The question that used to be two blank fields.
///
/// Unanswered on purpose. This is the state the whole step exists to create: on
/// the old flat form, the dangerous answer — nobody can revoke it, ever — was
/// what happened to somebody who read nothing and pressed the button. Here
/// there is nothing to press until one of the two rows is chosen.
pub fn claiming_authority(ui: &AppWindow) {
    claiming_a_name(ui);
    ui.global::<IdentityState>()
        .set_claim_step("authority".into());
}

/// The last screen before the first fee.
///
/// Shown with the unrevokable answer chosen, because that is the one worth
/// having an image of: the review has to say so plainly, and it is the only
/// place left that can.
pub fn claiming_review(ui: &AppWindow) {
    claiming_a_name(ui);
    let state = ui.global::<IdentityState>();
    state.set_claim_step("review".into());
    state.set_claim_authority("none".into());
}

/// One VerusID in full, with the warning that outranks everything on it.
///
/// The identity is its own recovery authority — the shape a fresh registration
/// lands on by default — so the sheet has to say it cannot be revoked. That is
/// the state this image exists for.
pub fn identity_detail(ui: &AppWindow) {
    identities(ui);

    let state = ui.global::<IdentityState>();
    state.set_name("robert.VRSCTEST@".into());
    state.set_status("Locked".into());
    state.set_tone("degraded".into());
    state.set_balance("48.5000 0000".into());
    state.set_signatures_required("1 of 1".into());
    state.set_control_note(note("control-enough", &["1", "1"]));
    state.set_revocation_authority("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq".into());
    state.set_recovery_authority("iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".into());
    state.set_cannot_be_revoked(true);
    state.set_timelock_note(note("timelock-held", &["100"]));

    state.set_primary_addresses(ModelRc::from(Rc::new(VecModel::from(vec![
        slint::SharedString::from("RK9izAySZHQAaCEkRmVV4Xtu73uV5sqsZy"),
    ]))));

    // One key this wallet can name, one it cannot — which is the permanent
    // case, not a pending one.
    state.set_content(ModelRc::from(Rc::new(VecModel::from(vec![
        ContentEntry {
            key: "iJ1BsyA9mx5RVk3ePK2WDgFcFCcfsXkBbA".into(),
            name: "vrsc::identity.profile".into(),
            first: true,
            text: "first value, must survive".into(),
            hex: "66697273742076616c75652c206d7573742073757276697665".into(),
            size: "25 bytes".into(),
            structured: "".into(),
        },
        ContentEntry {
            key: "i87QZVSS7SosM5choTJE7Dy4SNRt5vAEhr".into(),
            name: "".into(),
            first: true,
            text: SharedString::new(),
            hex: "018787a1035a9bd4179a3e0538ba9f90be7f231b69b0b588bac7b83800".into(),
            size: "29 bytes".into(),
            structured: "".into(),
        },
    ]))));

    // This wallet holds the key, so the controls that change it are offered.
    // A fact, not something read out of the sentence above it.
    state.set_can_sign(true);

    // Last, because this is what opens the sheet.
    state.set_address("iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".into());
}

/// The authority form inside the sheet, open.
///
/// Its own case because the form is now behind a button. Closing the sheet
/// shuts it again — see `on_close_identity` — so without this the fields, the
/// warning about handing an authority away and the Review button have no
/// reference image at all.
pub fn identity_authorities(ui: &AppWindow) {
    identity_detail(ui);
    ui.global::<IdentityState>().set_changing_authorities(true);
}

/// A change built and signed, waiting to be sent.
///
/// The last moment before something irreversible, which is why it is its own
/// layer rather than one heading among eight — and why the sentence on it has
/// to say what handing an authority away costs.
pub fn identity_change_review(ui: &AppWindow) {
    identity_detail(ui);

    let state = ui.global::<IdentityState>();
    state.set_change_description(note(
        "change-recovery",
        &["iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq"],
    ));
    state.set_change_fee("0.0001 0000".into());
    // Last, because a non-zero ticket is what opens the review.
    state.set_change_ticket(1);
}

/// The revocation review — the one change nobody can undo from here.
///
/// Worth its own reference image because it is the only place in this
/// application, besides the spending switch, where a word has to be typed. If
/// that ever renders as an ordinary confirmation, the picture says so.
pub fn identity_revoke_review(ui: &AppWindow) {
    identity_detail(ui);

    let state = ui.global::<IdentityState>();
    state.set_change_description(note("change-revoke", &[]));
    state.set_change_fee("0.0001 0000".into());
    // The word the core requires, read from the protocol rather than typed out
    // again — a fixture that spelled it itself would keep photographing
    // "revoke" after the rule had changed to something else.
    state.set_change_confirmation(pecu_protocol::REVOKE_CONFIRMATION.into());
    state.set_change_ticket(2);
}

/// A name claim waiting for its commitment to confirm.
///
/// The state the whole panel exists for: two transactions with a deadline
/// between them, and a window of about twenty blocks that cannot be extended.
/// A progress spinner without that fact on it hides the only thing somebody
/// could act on.
pub fn registering(ui: &AppWindow) {
    identities(ui);

    let state = ui.global::<IdentityState>();
    state.set_reg_step("waiting".into());
    state.set_reg_name("pecu".into());
    state.set_reg_note(note("claim-waiting", &["0"]));
    state.set_reg_deadline(note("claim-deadline", &["1 188 674", "18"]));
    state.set_reg_fee("100.0000 0000".into());
    state.set_reg_cannot_be_revoked(true);
    // Where it has got to: the first transaction is mined, the wait is running,
    // and the one that costs the rest is still ahead.
    state.set_reg_steps(ModelRc::from(Rc::new(VecModel::from(vec![
        step("Claim the name", "done", true),
        step("Wait for it to confirm", "now", false),
        step("Register it", "later", true),
    ]))));
}

pub fn network(ui: &AppWindow) {
    unlocked(ui);
    ui.set_screen("settings".into());
    ui.set_settings_tab(4);

    // One endpoint, online. That is the whole list on a chain now — the
    // wrong-chain row and the hand-typed one that used to be here describe a
    // wallet this build cannot produce.
    let nodes = vec![NodeRow {
        status: "online".into(),
        network: "Testnet".into(),
        tip: "1 187 500".into(),
        latency: "84 ms".into(),
        ..node(0, "VRSCTEST (public)", "https://api.verustest.net", true)
    }];

    let net = ui.global::<NetworkState>();
    net.set_nodes(ModelRc::from(Rc::new(VecModel::from(nodes))));
    net.set_effective("Testnet".into());
    // The footer reads its height from here, not from the active row, so
    // without this the picture says "no block height" beside an online node.
    net.set_tip("1 187 500".into());
    net.set_latency("84 ms".into());
}

fn nodes(ui: &AppWindow) {
    // The real binary shows the pinned SDK revision here. Leaving the fixture
    // blank rendered "sdk unknown", which is a difference between the snapshot
    // and the shipped app — exactly what these images are supposed to catch.
    ui.global::<AppInfo>()
        .set_sdk_rev(pecu_protocol::SDK_REV[..8].into());
    // Pinned, **not** `crate::search_shortcut()`.
    //
    // That function is `cfg!(target_os = ...)`, so using it here would make the
    // reference images say `⌘K` on a Mac and `Ctrl+K` on Linux CI — an image
    // that depends on who rendered it is not a reference. The same rule the
    // renderer applies to the language, for the same reason.
    ui.global::<AppInfo>().set_search_shortcut("⌘K".into());

    // One endpoint, because the shipped list is per chain now: a wallet on
    // testnet is offered the testnet node and nothing else. The second row here
    // used to be `api.verus.services`, marked `WrongNetwork` the moment it
    // answered — correct, and an endpoint that could never be used.
    let nodes = vec![node(
        0,
        "VRSCTEST (public)",
        "https://api.verustest.net",
        true,
    )];

    let net = ui.global::<NetworkState>();
    net.set_nodes(ModelRc::from(Rc::new(VecModel::from(nodes))));
    // Two strings for one chain, as the core sends them: the label a person
    // reads and the name every comparison is made against. This fixture used to
    // put "VRSCTEST" in the first, which is what the sentence at the top of the
    // screen then showed — a string production never puts there.
    net.set_requested("Testnet".into());
    net.set_requested_name("VRSCTEST".into());
    net.set_chain_title("Testnet".into());
    net.set_endpoint("https://api.verustest.net".into());

    // The chains the core offers. Names and titles both, because the button
    // shows one and sends the other — see `ChainChoiceVm`.
    let chains: Vec<ChainChoice> = [
        ("VRSCTEST", "Testnet"),
        ("VRSC", "Verus"),
        // Spelled the way the chains' own daemons spell them, because the name
        // is what goes back to the core and is compared there by exact string.
        ("vARRR", "Pirate Chain"),
        ("CHIPS", "CHIPS"),
        ("vDEX", "vDEX"),
    ]
    .into_iter()
    .map(|(name, title)| ChainChoice {
        name: name.into(),
        title: title.into(),
    })
    .collect();
    net.set_chains(ModelRc::from(Rc::new(VecModel::from(chains))));
}

fn node(id: i32, label: &str, url: &str, active: bool) -> NodeRow {
    NodeRow {
        id,
        label: label.into(),
        url: url.into(),
        // Nothing has been probed, so nothing claims to be reachable, and the
        // network stays blank — that is decided by `chain_info().name`, never
        // by the hostname.
        status: "unknown".into(),
        network: SharedString::new(),
        tip: SharedString::new(),
        latency: SharedString::new(),
        note: Note::default(),
        builtin: true,
        active,
    }
}
