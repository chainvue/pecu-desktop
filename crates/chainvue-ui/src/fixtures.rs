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

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::{
    ActivityRow, AppInfo, AppWindow, AssetRow, KeyRow, KnownAddressRow, NetworkState, NodeRow,
    PendingRow, ReviewOutput, SeedState, SeedWord, SendState, TxState, WalletState,
};

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
    wallet.set_name("ChainVue".into());
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

    ui.global::<SendState>()
        .set_known(ModelRc::from(Rc::new(VecModel::from(vec![
            KnownAddressRow {
                address: SECOND_ADDRESS.into(),
                label: "the exchange".into(),
                summary: "3 payments · last 2 days ago".into(),
            },
            KnownAddressRow {
                address: THIRD_ADDRESS.into(),
                label: SharedString::new(),
                summary: "1 payment · last in the last hour".into(),
            },
        ]))));
}

/// The keys section with a rename in progress, which is where the form and the
/// row it belongs to have to sit together without the list jumping.
pub fn renaming_key(ui: &AppWindow) {
    keys(ui);
    let wallet = ui.global::<WalletState>();
    wallet.set_renaming("savings".into());
    wallet.set_rename_draft("main".into());
    wallet.set_key_problem("There is already a key called `main`".into());
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

    let points: Vec<chainvue_chart::Point> = [
        (9 * HOUR, 0_i64),
        (9 * HOUR - 60, 5_000_000_000),
        (8 * HOUR - 600, 4_889_990_000),
        (8 * HOUR, 5_389_990_000),
        (0, 5_389_990_000),
    ]
    .iter()
    .map(|(ago, sats)| chainvue_chart::Point {
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
    use chainvue_protocol::{Severity, UiError};

    funded(ui);
    crate::toast::install(ui);

    crate::toast::show(
        ui,
        &UiError::simple(
            "spend_refused",
            "This node is on Mainnet, not Testnet",
            "ChainVue will not sign against a chain you did not choose.",
            Severity::Danger,
        ),
    );
    for _ in 0..7 {
        crate::toast::show(
            ui,
            &UiError::simple(
                "history",
                "Could not read this wallet\'s activity",
                "The balance is still current. Only the transaction list failed to load.",
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

/// Settings, with the mainnet guard in its default state: off.
pub fn settings(ui: &AppWindow) {
    unlocked(ui);
    ui.set_screen("settings".into());

    let wallet = ui.global::<WalletState>();
    wallet.set_auto_lock(5);
    wallet.set_vault_path(
        "~/Library/Application Support/com.chainvue.wallet/testnet/vault.json".into(),
    );
    // The real binary fills this in. Leaving it blank rendered "Logs" beside
    // nothing at all, which is exactly the difference between the snapshot and
    // the shipped app that these images exist to catch.
    ui.global::<AppInfo>()
        .set_log_path("~/Library/Application Support/com.chainvue.wallet/testnet/logs".into());
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
    tx.set_when("2 hours ago".into());
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
    // The state worth a reference image: the warning is the reason the review
    // step exists at all, and it is the one thing on this screen somebody has
    // to read rather than glance at.
    send.set_first_time_recipient(true);

    send.set_outputs(ModelRc::from(Rc::new(VecModel::from(vec![
        ReviewOutput {
            address: "RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX".into(),
            kind: "Payment".into(),
            amount: "50.0000 0000".into(),
            is_change: false,
        },
        ReviewOutput {
            // A real address with a valid checksum, derived in
            // `chainvue-core/tests/send_build.rs`. A hand-typed one failed its
            // checksum — which nobody would notice in a picture, and which
            // someone might copy out of it.
            address: "RWmjzbd4Sy6zK4H4rjHXrpaWTrsJYRr6Nn".into(),
            kind: "Payment".into(),
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
pub fn receiving(ui: &AppWindow) {
    unlocked(ui);

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

    let net = ui.global::<NetworkState>();
    net.set_mock_mode(true);
    // A wallet with figures on it has been read from somewhere. Leaving these
    // blank put "Not connected · no block height" underneath a full dashboard,
    // which is a state the wallet cannot actually be in.
    net.set_effective("Testnet".into());
    net.set_tip("1 187 500".into());
    net.set_latency("84 ms".into());

    let wallet = ui.global::<WalletState>();
    wallet.set_ticker("VRSCTEST".into());
    wallet.set_total("12 482.4200 0000".into());
    wallet.set_spendable("12 382.4200 0000".into());
    wallet.set_immature("100.0000 0000".into());
    wallet.set_pending("0.0000 0000".into());
    wallet.set_incoming("5.0000 0000".into());
    wallet.set_has_breakdown(true);

    wallet.set_assets(ModelRc::from(Rc::new(VecModel::from(vec![
        AssetRow {
            name: "VRSCTEST".into(),
            amount: "12 482.4200 0000".into(),
            secondary: SharedString::new(),
            native: true,
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
            amount: "48.5000 0000".into(),
            native: false,
        },
    ]))));

    let recent = vec![
        activity("in", "+120.0000 0000", "2 hours ago", false),
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
        dated("in", "+120.0000 0000", "2 hours ago", "Today", 1_187_400),
        dated("in", "+5.0000 0000", "pending", "", 0),
        dated("out", "−50.0000 0000", "yesterday", "Yesterday", 1_186_200),
        dated(
            "in",
            "+12 345.0000 0000 mambo",
            "3 days ago",
            "9 August",
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

    let points: Vec<chainvue_chart::Point> = movements
        .iter()
        .map(|(days_ago, centi)| chainvue_chart::Point {
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
        when: when.into(),
        pending,
        height: 0,
        group: group.into(),
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
        .set_problem("There is a typo in that phrase — one word is wrong or out of order.".into());
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

/// The confirmation step, after a wrong answer — the state worth looking at,
/// because it is the one with an error in it.
pub fn backup_verify(ui: &AppWindow) {
    backup_phrase(ui);
    let seed = ui.global::<SeedState>();
    seed.set_step("verify".into());
    seed.set_problem(
        "That does not match the phrase we showed you. Check what you wrote down.".into(),
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
pub fn network(ui: &AppWindow) {
    unlocked(ui);

    let nodes = vec![
        NodeRow {
            status: "online".into(),
            network: "Testnet".into(),
            tip: "1 187 500".into(),
            latency: "84 ms".into(),
            ..node(0, "VRSCTEST (public)", "https://api.verustest.net", true)
        },
        NodeRow {
            // Answering perfectly about the wrong chain. Degraded, not online —
            // and the note is what says which, since the colour alone cannot.
            status: "degraded".into(),
            network: "Mainnet".into(),
            tip: "3 402 118".into(),
            latency: "132 ms".into(),
            note: "this node is on Mainnet".into(),
            ..node(1, "VRSC (public)", "https://api.verus.services", false)
        },
        NodeRow {
            builtin: false,
            status: "offline".into(),
            note: "connection refused".into(),
            ..node(1000, "my node", "https://my-node.example:27486", false)
        },
    ];

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
        .set_sdk_rev(chainvue_protocol::SDK_REV[..8].into());

    let nodes = vec![
        node(0, "VRSCTEST (public)", "https://api.verustest.net", true),
        node(1, "VRSC (public)", "https://api.verus.services", false),
    ];

    let net = ui.global::<NetworkState>();
    net.set_nodes(ModelRc::from(Rc::new(VecModel::from(nodes))));
    net.set_requested("Testnet".into());
    net.set_endpoint("https://api.verustest.net".into());
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
        note: SharedString::new(),
        builtin: true,
        active,
    }
}
