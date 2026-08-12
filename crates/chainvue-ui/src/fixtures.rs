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
    ActivityRow, AppInfo, AppWindow, AssetRow, NetworkState, NodeRow, PendingRow, ReviewOutput,
    SeedState, SeedWord, SendState, WalletState,
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
    wallet.set_address("RQr2cUkF46n7y8WRzDkd1iV9gHusSSQuzX".into());
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
    ui.global::<NetworkState>().set_mock_mode(true);

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
