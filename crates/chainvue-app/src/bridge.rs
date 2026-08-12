//! Carrying events from the core onto the UI thread.
//!
//! The core runs on Tokio and knows nothing about Slint. This is the one place
//! that knows about both, and it is deliberately thin: take an `Event`, turn it
//! into Slint property writes, and nothing else. No decisions are made here —
//! anything that needs a judgement belongs in the core, where it is testable
//! without a window.

use std::rc::Rc;

use chainvue_protocol::{
    Event, HistoryRowVm, ListDelta, NodeVm, PortfolioVm, Reachability, SendOutcomeVm, TxDirection,
};
use chainvue_ui::prelude::*;
use chainvue_ui::{
    qr, seed, ActivityRow, AppWindow, AssetRow, NetworkState, NodeRow, PendingRow, ReviewOutput,
    SeedState, SendState, WalletState,
};
use slint::{Model, ModelRc, SharedString, VecModel, Weak};
use tokio::sync::mpsc;

/// The box the receive QR is drawn to fit, in logical pixels. The code itself
/// comes out slightly smaller, because its module size has to be a whole number
/// of physical pixels — see `chainvue_ui::qr`.
const QR_SIDE: f32 = 236.0;

/// Forward every event to the window until the core stops.
pub fn pump(
    handle: &tokio::runtime::Handle,
    weak: Weak<AppWindow>,
    mut events: mpsc::UnboundedReceiver<Event>,
) {
    handle.spawn(async move {
        while let Some(event) = events.recv().await {
            // `upgrade_in_event_loop` hops to the UI thread and does nothing if
            // the window has already gone — which is exactly what should happen
            // during shutdown.
            let _ = weak.upgrade_in_event_loop(move |ui| apply(&ui, event));
        }
    });
}

/// Apply one event.
///
/// Total by construction: no `unwrap`, no panic. A panic on the UI thread kills
/// the window, so an event that cannot be applied is dropped and logged rather
/// than taking the wallet with it.
fn apply(ui: &AppWindow, event: Event) {
    match event {
        Event::Network(vm) => apply_network(ui, &vm),

        Event::Wallet(vm) => apply_wallet(ui, vm),

        Event::Locked { reason } => {
            tracing::info!(?reason, "locked");
            let state = ui.global::<WalletState>();
            state.set_locked(true);
            state.set_busy(false);
            // Core drops the phrase on lock. The screen goes with it, and the
            // words it was holding are overwritten rather than merely hidden.
            seed::close(ui);
        }

        // A backup has started: how many words there will be, and which of them
        // will be asked for. No word has arrived yet — the grid is laid out
        // masked, and stays that way until the reveal button is held.
        Event::PhraseChallenge {
            positions,
            word_count,
        } => {
            seed::open(ui, &positions, word_count);
        }

        Event::SeedWords(words) => {
            if words.is_empty() {
                seed::conceal(ui);
            } else {
                seed::reveal(ui, &words);
            }
        }

        Event::PhraseConfirmed(correct) => {
            if correct {
                // Core has already recorded the backup and dropped the phrase.
                seed::close(ui);
            } else {
                // Never which word. Saying that would make this screen a
                // checker someone could feed guesses to one position at a time.
                ui.global::<SeedState>().set_problem(
                    "That does not match the phrase we showed you. Check what you wrote down."
                        .into(),
                );
            }
        }

        Event::Portfolio(vm) => apply_portfolio(ui, &vm),

        Event::History { delta, .. } => {
            let state = ui.global::<WalletState>();
            match delta {
                // The dashboard shows a fixed window of the newest rows, so it
                // only ever receives a whole replacement. Paging deltas arrive
                // with the Activity screen, which is what they are for.
                ListDelta::Replace(rows) => {
                    let rows: Vec<ActivityRow> = rows.iter().map(activity_row).collect();
                    state.set_activity(ModelRc::from(Rc::new(VecModel::from(rows))));
                }
                other => tracing::debug!(?other, "history delta not rendered yet"),
            }
        }

        Event::SendValidation(vm) => {
            let send = ui.global::<SendState>();
            send.set_to_valid(vm.to_valid);
            send.set_to_note(vm.to_note.into());
            send.set_amount_valid(vm.amount_valid);
            send.set_amount_note(vm.amount_note.into());
            send.set_ready(vm.ready);
        }

        Event::SendPrepared(vm) => apply_review(ui, &vm),
        Event::SendResult(outcome) => apply_outcome(ui, outcome),

        Event::Pending(ListDelta::Replace(rows)) => {
            let rows: Vec<PendingRow> = rows
                .iter()
                .map(|row| PendingRow {
                    id: i32::try_from(row.id).unwrap_or(i32::MAX),
                    txid: row.txid.clone().into(),
                    to_address: row.to_address.clone().into(),
                    amount: row.amount_display.clone().into(),
                    state: row.state.clone().into(),
                    checks: i32::try_from(row.checks).unwrap_or(i32::MAX),
                })
                .collect();
            ui.global::<SendState>()
                .set_pending_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
        }

        Event::Busy { task, on } => {
            tracing::debug!(?task, on, "busy");
            // The Refresh control says "Reading…" while a read is in flight.
            // Without this it would look inert for the several seconds a
            // balance actually takes.
            match task {
                chainvue_protocol::TaskKind::RefreshingBalance => {
                    ui.global::<WalletState>().set_busy(on);
                }
                chainvue_protocol::TaskKind::PreparingSend
                | chainvue_protocol::TaskKind::Broadcasting => {
                    ui.global::<SendState>().set_busy(on);
                }
                _ => {}
            }
        }

        Event::Notice(error) => {
            tracing::warn!(code = error.code, title = %error.title, "notice");
            // Until a toast host exists, an onboarding failure at least has to
            // appear on the screen it came from — a button that does nothing is
            // indistinguishable from a broken app.
            let state = ui.global::<WalletState>();
            state.set_busy(false);
            state.set_problem(error.title.into());
        }

        other => {
            tracing::debug!(?other, "event not rendered yet");
        }
    }
}

/// The review, built from the transaction that was actually signed.
fn apply_review(ui: &AppWindow, vm: &chainvue_protocol::SendReviewVm) {
    let send = ui.global::<SendState>();
    send.set_ticket(i32::try_from(vm.ticket).unwrap_or(i32::MAX));
    send.set_amount(vm.amount_display.clone().into());
    send.set_fee(vm.fee_display.clone().into());
    send.set_total(vm.total_display.clone().into());
    send.set_change(vm.change_display.clone().into());
    send.set_balance_after(vm.balance_after_display.clone().into());
    send.set_from_address(vm.from_address.clone().into());

    let outputs: Vec<ReviewOutput> = vm
        .outputs
        .iter()
        .map(|output| ReviewOutput {
            // Empty means the script could not be decoded. The screen
            // shows that as such rather than hiding the row.
            address: output.address.clone().unwrap_or_default().into(),
            kind: output.kind.clone().into(),
            amount: output.amount_display.clone().into(),
            is_change: output.is_change,
        })
        .collect();
    send.set_outputs(ModelRc::from(Rc::new(VecModel::from(outputs))));

    send.set_problem(SharedString::new());
    send.set_step("review".into());
}

/// What happened when the bytes were handed over.
fn apply_outcome(ui: &AppWindow, outcome: SendOutcomeVm) {
    let send = ui.global::<SendState>();
    match outcome {
        SendOutcomeVm::Sent { txid, .. } => {
            send.set_txid(txid.into());
            send.set_problem(SharedString::new());
            send.set_step("sent".into());
        }
        // Not an error, and not a success. The bytes are on disk and the
        // resolution is to ask the node — never to send again.
        SendOutcomeVm::Uncertain { txid, pending_id } => {
            tracing::warn!(%txid, pending_id, "broadcast outcome unknown");
            send.set_txid(txid.into());
            send.set_step("uncertain".into());
        }
        SendOutcomeVm::Failed(error) => {
            send.set_problem(error.title.into());
            // Stays on whichever step it failed on, so the form or the
            // review is still there to correct.
        }
    }
}

/// The wallet's own state: whether it exists, whether it is open, its keys.
fn apply_wallet(ui: &AppWindow, vm: chainvue_protocol::WalletVm) {
    let state = ui.global::<WalletState>();
    state.set_exists(vm.exists);
    state.set_locked(vm.locked);
    state.set_name(vm.name.into());
    state.set_busy(false);
    // A successful transition clears whatever went wrong last time.
    state.set_problem(SharedString::new());
    state.set_loading(false);
    state.set_address(
        vm.keys
            .first()
            .map(|k| k.address.clone())
            .unwrap_or_default()
            .into(),
    );
    state.set_backup_key(vm.needs_backup.unwrap_or_default().into());
    // `None` is "never", which the settings screen shows as 0.
    state.set_auto_lock(
        vm.auto_lock_minutes
            .map_or(0, |m| i32::try_from(m).unwrap_or(i32::MAX)),
    );

    // The QR follows the address. Regenerated here rather than bound in
    // Slint because the module size depends on the window's scale
    // factor, which only Rust can read.
    let address = state.get_address().to_string();
    let scale = ui.window().scale_factor();
    if let Some(image) = qr::encode(&address, QR_SIDE, scale) {
        state.set_qr_side(qr::logical_side(&image, scale));
        state.set_qr(image);
    }
}

/// Node health, the active endpoint, and which chain it says it is on.
fn apply_network(ui: &AppWindow, vm: &chainvue_protocol::NetworkVm) {
    let state = ui.global::<NetworkState>();
    state.set_requested(vm.requested.clone().into());
    state.set_effective(vm.effective.clone().unwrap_or_default().into());
    state.set_tip(vm.tip.map(thousands).unwrap_or_default().into());
    state.set_syncing(vm.syncing);
    state.set_allow_mainnet_spend(vm.allow_mainnet_spend);
    state.set_mock_mode(vm.mock_mode);

    // The status pill in the title bar follows the ACTIVE node, and the
    // endpoint line follows it too, so the footer and the pill can never
    // describe different nodes.
    let active = vm
        .active_node
        .and_then(|id| vm.nodes.iter().find(|n| n.id == id));
    state.set_node_state(
        active
            .map_or("unknown", |n| reachability_label(n.status))
            .into(),
    );
    state.set_endpoint(
        active
            .map_or_else(|| "not configured".to_string(), |n| n.url.clone())
            .into(),
    );
    state.set_latency(
        active
            .and_then(|n| n.latency_ms)
            .map(|ms| format!("{ms} ms"))
            .unwrap_or_default()
            .into(),
    );

    update_nodes(&state, &vm.nodes, vm.active_node);
}

/// The figures, already formatted. Nothing here does arithmetic on money —
/// every string arrived finished from `chainvue_core::portfolio`.
fn apply_portfolio(ui: &AppWindow, vm: &PortfolioVm) {
    let state = ui.global::<WalletState>();
    let balance = &vm.balance;

    state.set_total(balance.total_display.clone().into());
    state.set_spendable(balance.spendable_display.clone().into());
    state.set_immature(balance.immature_display.clone().into());
    state.set_pending(balance.pending_display.clone().into());
    state.set_stale(vm.stale);
    // The native asset row carries the chain's own name, which is what the
    // hero figure should be labelled with too.
    if let Some(native) = vm.assets.iter().find(|asset| asset.native) {
        state.set_ticker(native.name.clone().into());
    }
    state.set_loading(false);

    // The breakdown line appears only when it says something the total does
    // not. A wallet with everything spendable stays quiet rather than printing
    // "0.0000 0000 maturing" under every balance.
    let quiet = is_zero(&balance.immature_sats)
        && is_zero(&balance.pending_out_sats)
        && is_zero(&balance.pending_in_sats);
    state.set_has_breakdown(!quiet);

    // Formatted by core. This only decides whether there is anything to say.
    state.set_incoming(if is_zero(&balance.pending_in_sats) {
        SharedString::new()
    } else {
        balance.incoming_display.clone().into()
    });

    let assets: Vec<AssetRow> = vm
        .assets
        .iter()
        .map(|asset| AssetRow {
            name: asset.name.clone().into(),
            amount: asset.amount_display.clone().into(),
            // The i-address under the name, so a token whose name is missing —
            // or whose name is trying to look like something else — can still
            // be told apart by the part that cannot lie.
            secondary: if asset.native {
                SharedString::new()
            } else {
                asset.currency_id.clone().into()
            },
            native: asset.native,
        })
        .collect();
    state.set_assets(ModelRc::from(Rc::new(VecModel::from(assets))));
}

/// Satoshi counts cross as decimal strings, so "nothing" is a string test.
fn is_zero(sats: &str) -> bool {
    sats.is_empty() || sats.chars().all(|c| c == '0')
}

fn activity_row(row: &HistoryRowVm) -> ActivityRow {
    ActivityRow {
        txid: row.txid.clone().into(),
        direction: match row.direction {
            TxDirection::Incoming => "in",
            TxDirection::Outgoing => "out",
            TxDirection::Self_ => "self",
        }
        .into(),
        amount: row.net_display.clone().into(),
        when: row.when_display.clone().into(),
        pending: row.pending,
        height: i32::try_from(row.height).unwrap_or(i32::MAX),
    }
}

/// Patch the node model in place.
///
/// `set_row_data` rather than rebuilding the vector: a rebuild tears down and
/// recreates every element, which throws away in-flight transitions. It matters
/// more on the activity list, but the habit is worth establishing where the
/// lists are still small.
fn update_nodes(state: &NetworkState<'_>, nodes: &[NodeVm], active: Option<u32>) {
    let rows = state.get_nodes();

    let same_shape = rows.row_count() == nodes.len()
        && nodes
            .iter()
            .enumerate()
            .all(|(i, n)| rows.row_data(i).is_some_and(|r| r.id == to_i32(n.id)));

    if !same_shape {
        let fresh: Vec<NodeRow> = nodes.iter().map(|n| to_row(n, active)).collect();
        state.set_nodes(ModelRc::from(Rc::new(VecModel::from(fresh))));
        return;
    }

    for (index, node) in nodes.iter().enumerate() {
        let next = to_row(node, active);
        if rows.row_data(index).as_ref() != Some(&next) {
            rows.set_row_data(index, next);
        }
    }
}

fn to_row(node: &NodeVm, active: Option<u32>) -> NodeRow {
    NodeRow {
        id: to_i32(node.id),
        label: node.label.clone().into(),
        url: node.url.clone().into(),
        status: reachability_label(node.status).into(),
        // Blank until the node itself says which chain it is on.
        network: node
            .network
            .clone()
            .map(SharedString::from)
            .unwrap_or_default(),
        tip: node
            .tip
            .map(thousands)
            .map(SharedString::from)
            .unwrap_or_default(),
        latency: node
            .latency_ms
            .map(|ms| SharedString::from(format!("{ms} ms")))
            .unwrap_or_default(),
        note: node
            .note
            .clone()
            .map(SharedString::from)
            .unwrap_or_default(),
        builtin: node.builtin,
        active: active == Some(node.id),
    }
}

fn reachability_label(status: Reachability) -> &'static str {
    match status {
        Reachability::Unknown => "unknown",
        Reachability::Probing => "probing",
        Reachability::Online => "online",
        Reachability::Degraded => "degraded",
        Reachability::Offline => "offline",
    }
}

fn to_i32(id: u32) -> i32 {
    i32::try_from(id).unwrap_or(i32::MAX)
}

/// `1187149` → `1 187 149`. A seven-digit block height is unreadable otherwise.
fn thousands(value: u32) -> String {
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
    use super::thousands;

    #[test]
    fn block_heights_are_grouped() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1 000");
        assert_eq!(thousands(1_187_149), "1 187 149");
    }
}
