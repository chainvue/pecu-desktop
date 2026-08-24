//! Carrying events from the core onto the UI thread.
//!
//! The core runs on Tokio and knows nothing about Slint. This is the one place
//! that knows about both, and it is deliberately thin: take an `Event`, turn it
//! into Slint property writes, and nothing else. No decisions are made here —
//! anything that needs a judgement belongs in the core, where it is testable
//! without a window.

use std::rc::Rc;

use pecu_protocol::{
    ContentEntryVm, Event, HistoryRowVm, IdentityDetailVm, IdentityVm, KeyOrigin, ListDelta,
    NodeVm, PortfolioVm, Reachability, SendOutcomeVm, TxDirection,
};
use pecu_ui::prelude::*;
use pecu_ui::{
    qr, seed, ActivityRow, AppWindow, AssetRow, ContentEntry, IdentityRow, IdentityState, KeyRow,
    KnownAddressRow, NetworkState, NodeRow, PendingRow, ReviewOutput, SeedState, SendState,
    TxState, WalletState,
};
use slint::{Model, ModelRc, SharedString, VecModel, Weak};
use tokio::sync::mpsc;

/// The box the receive QR is drawn to fit, in logical pixels. The code itself
/// comes out slightly smaller, because its module size has to be a whole number
/// of physical pixels — see `pecu_ui::qr`.
const QR_SIDE: f32 = 236.0;

/// How many transactions the dashboard's short list shows. Mirrors
/// `pecu_core::portfolio::RECENT`, which the UI cannot name — this crate
/// deliberately does not depend on the core.
const RECENT: usize = 6;

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
///
/// Long because it is a dispatch table: one arm per event, each either a write
/// or a call. Splitting it would put the list of what the wallet can say in two
/// places, which is the one property worth keeping — a new `Event` variant
/// breaks this match, and that is the point.
#[allow(clippy::too_many_lines)]
fn apply(ui: &AppWindow, event: Event) {
    match event {
        Event::Network(vm) => apply_network(ui, &vm),

        Event::Wallet(vm) => apply_wallet(ui, vm),

        // What the window looked like last time. Arrives before any figure, so
        // there is no frame of the wrong theme.
        Event::Appearance {
            dark,
            reduce_motion,
            window,
        } => {
            ui.global::<pecu_ui::Theme>().set_dark(dark);
            ui.global::<pecu_ui::Motion>().set_enabled(!reduce_motion);
            // Logical pixels, which is what the window reported when it was
            // written down — so a wallet moved between a Retina display and an
            // ordinary one comes back the same size in inches rather than in
            // device pixels.
            if let Some((width, height)) = window {
                // Exact for every window size a display can have: an `f32` holds
                // integers up to 2^24 without loss, and the core refuses
                // anything below 880 x 620 on the way in.
                #[allow(clippy::cast_precision_loss)]
                ui.window()
                    .set_size(slint::LogicalSize::new(width as f32, height as f32));
            }
        }

        Event::Locked { reason } => {
            tracing::info!(?reason, "locked");
            let state = ui.global::<WalletState>();
            state.set_locked(true);
            state.set_busy(false);
            // Core drops the phrase on lock. The screen goes with it, and the
            // words it was holding are overwritten rather than merely hidden.
            seed::close(ui);
            // And whatever the wallet was complaining about belonged to a
            // session that has ended. Leaving it over the unlock form would be
            // shouting at whoever turns up next.
            pecu_ui::toast::clear(ui);
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
                ui.global::<SeedState>()
                    .set_problem(note(&pecu_protocol::NoteVm::plain("phrase-mismatch")));
            }
        }

        Event::AddressBook(rows) => apply_address_book(ui, &rows),

        Event::Portfolio(vm) => apply_portfolio(ui, &vm),

        // Readings, not a picture. The geometry is computed in `pecu-ui`,
        // which is the only thing that knows how big the chart element is.
        Event::Chart(vm) => pecu_ui::chart::set_series(ui, &vm),

        Event::History { delta, .. } => apply_history(ui, delta),

        Event::SendValidation(vm) => {
            let send = ui.global::<SendState>();
            send.set_to_valid(vm.to_valid);
            send.set_to_note(note(&vm.to_note));
            send.set_to_label(vm.to_label.into());
            send.set_amount_valid(vm.amount_valid);
            send.set_amount_note(note(&vm.amount_note));
            send.set_ready(vm.ready);
            send.set_route(route_name(vm.route).into());
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

        Event::Identities { yours, looked_up } => apply_identities(ui, &yours, &looked_up),

        Event::Currencies { yours, eligible } => apply_currencies(ui, &yours, &eligible),

        Event::Markets { rows, quote } => {
            let state = ui.global::<pecu_ui::MarketState>();
            state.set_quote(quote.into());
            let rows: Vec<pecu_ui::MarketRow> = rows
                .iter()
                .map(|row| pecu_ui::MarketRow {
                    name: row.name.clone().into(),
                    address: row.address.clone().into(),
                    price: row.price.clone().into(),
                    change: row.change.clone().into(),
                    tone: row.tone.clone().into(),
                    pooled: row.pooled.clone().into(),
                    depth: row.depth.clone().into(),
                })
                .collect();
            state.set_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
        }

        Event::MarketDetail(detail) => apply_market_detail(ui, detail.as_deref()),

        Event::ChainHalt(halt) => {
            let state = ui.global::<pecu_ui::HaltState>();
            state.set_severity(halt.severity.clone().into());
            state.set_note(note(&halt.note));
            state.set_conversions_halted(halt.conversions_halted);
            state.set_in_blocks(i32::try_from(halt.in_blocks).unwrap_or(i32::MAX));
            state.set_stale(halt.stale);
        }

        Event::ConvertQuote(quote) => {
            let state = ui.global::<pecu_ui::ConvertState>();
            state.set_from_name(quote.from.clone().into());
            state.set_to_name(quote.to.clone().into());
            state.set_from_balance(quote.from_balance.clone().into());
            state.set_get_estimate(quote.get.clone().into());
            state.set_via(quote.via.clone().into());
            state.set_rate(quote.rate.clone().into());
            state.set_conversion_fee(quote.conversion_fee.clone().into());
            state.set_network_fee(quote.network_fee.clone().into());
            state.set_minimum(quote.minimum.clone().into());
            state.set_slippage(quote.slippage.clone().into());
            state.set_slippage_tone(quote.slippage_tone.clone().into());
            state.set_note(note(&quote.note));
            state.set_ready(quote.ready);
        }

        Event::ConvertPrepared(vm) => apply_convert_review(ui, &vm),
        Event::ConvertResult(outcome) => apply_convert_outcome(ui, outcome),

        Event::SearchHits { query, hits } => {
            let state = ui.global::<pecu_ui::SearchState>();
            // Drop a reply to a query nobody is running any more. Two
            // keystrokes in flight arrive in order, but the older answer
            // landing second would put a shorter list back on screen — and the
            // person typing would see their own results disappear.
            if state.get_query() != query.as_str() {
                return;
            }
            let rows: Vec<pecu_ui::SearchHit> = hits
                .iter()
                .map(|hit| pecu_ui::SearchHit {
                    kind: hit.kind.clone().into(),
                    label: hit.label.clone().into(),
                    sub: hit.sub.clone().into(),
                    target: hit.target.clone().into(),
                })
                .collect();
            state.set_hits(slint::ModelRc::new(slint::VecModel::from(rows)));
        }

        Event::CurrencyDraftChecked(vm) => apply_currency_draft(ui, &vm),

        Event::CurrencyChoices(vm) => {
            let state = ui.global::<pecu_ui::CurrencyState>();
            let rows: Vec<pecu_ui::CurrencyPick> = vm
                .rows
                .iter()
                .map(|row| pecu_ui::CurrencyPick {
                    name: row.name.clone().into(),
                    address: row.address.clone().into(),
                    kind: row.kind.clone().into(),
                    note: note(&row.note),
                })
                .collect();
            state.set_choices(ModelRc::from(Rc::new(VecModel::from(rows))));
            state.set_choices_more(i32::try_from(vm.more).unwrap_or(i32::MAX));
            state.set_choices_busy(vm.loading);
            state.set_choices_problem(note(&vm.problem));
        }

        Event::LaunchPending(pending) => {
            let state = ui.global::<pecu_ui::CurrencyState>();
            match pending {
                Some(vm) => {
                    state.set_pending_note(note(&vm.note));
                    state.set_pending_can_continue(vm.can_continue);
                    state.set_pending_steps(steps_of(&vm.steps));
                    // The form has been submitted, so it closes — which puts
                    // the panel that says what is now unfinished on screen
                    // instead. Leaving the form up would invite a second press
                    // on a decision that has already claimed a name; core
                    // refuses that, and a screen should not be offering it.
                    close_currency_form(ui);
                    // Last, because a non-empty identity is what opens it.
                    state.set_pending_identity(vm.identity.clone().into());
                }
                None => state.set_pending_identity(SharedString::new()),
            }
        }

        Event::LaunchPrepared(review) => {
            let state = ui.global::<pecu_ui::CurrencyState>();
            state.set_launch_busy(false);
            match review {
                Some(vm) => {
                    state.set_launch_name(vm.name.clone().into());
                    state.set_launch_description(note(&vm.description));
                    state.set_launch_fee(vm.fee_display.clone().into());
                    state.set_launch_deposit(vm.deposit_display.clone().into());
                    state.set_launch_burned(vm.burned_display.clone().into());
                    state.set_launch_start_block(vm.start_block.clone().into());
                    // Last, because a non-zero ticket is what opens the review.
                    state.set_launch_ticket(i32::try_from(vm.ticket).unwrap_or(i32::MAX));
                }
                // Zero closes it. One field decides, so the overlay and the
                // ticket cannot disagree about whether there is a launch.
                None => state.set_launch_ticket(0),
            }
        }

        Event::LaunchDone(done) => {
            tracing::info!(txid = %done.txid, "a currency launch was sent");
            let state = ui.global::<pecu_ui::CurrencyState>();
            state.set_launch_busy(false);
            state.set_launch_ticket(0);
            // The form has done its job. Closing it puts the fresh list back on
            // screen, which is where the new currency appears.
            close_currency_form(ui);
        }

        Event::IdentityChangePrepared {
            ticket,
            description,
            fee_display,
            confirmation,
        } => {
            let state = ui.global::<IdentityState>();
            state.set_change_description(note(&description));
            state.set_change_fee(fee_display.into());
            state.set_change_confirmation(confirmation.into());
            state.set_change_typed(SharedString::new());
            // Last, because a non-zero ticket is what opens the review.
            state.set_change_ticket(i32::try_from(ticket).unwrap_or(i32::MAX));
        }

        Event::IdentityChanged { txid } => {
            tracing::info!(%txid, "an identity change was sent");
            let state = ui.global::<IdentityState>();
            state.set_change_ticket(0);
            state.set_change_busy(false);
            state.set_new_revocation(SharedString::new());
            state.set_new_recovery(SharedString::new());
            state.set_lock_blocks(SharedString::new());
            state.set_change_typed(SharedString::new());
        }

        Event::NameChecked {
            name,
            problem,
            fee_display,
        } => {
            // Two screens ask this question — the identity form and the
            // currency form, which claims a name as part of a launch — and the
            // core answers one at a time. The answer goes to whichever of them
            // is still holding the name it is about, which is what the `name`
            // field is carried for: a reply that arrives after the field moved
            // on describes something nobody is looking at.
            let currency = ui.global::<pecu_ui::CurrencyState>();
            if currency.get_new_name().as_str() == name {
                currency.set_new_name_problem(note(&problem));
                currency.set_new_name_fee(fee_display.clone().into());
            }

            let identity = ui.global::<IdentityState>();
            if identity.get_name_draft().as_str() == name {
                identity.set_name_problem(note(&problem));
                identity.set_name_fee(fee_display.into());
            }
        }

        Event::Registration(claim) => apply_registration(ui, claim.as_deref()),

        Event::IdentityMissing { typed, reason } => {
            tracing::info!(%typed, reason = %reason.code, "a VerusID lookup found nothing");
            ui.global::<IdentityState>()
                .set_lookup_problem(note(&reason));
        }

        // The sheet is open exactly when the address is non-empty, so `None`
        // closes it with one write rather than a second flag that could
        // disagree with the first.
        Event::IdentityDetail(Some(vm)) => apply_identity_detail(ui, &vm),
        Event::IdentityDetail(None) => close_identity(ui),

        Event::ContentKeyDerived { uri, key, present } => {
            tracing::debug!(%uri, %key, present, "derived a VDXF key");
            let state = ui.global::<IdentityState>();
            state.set_derived_key(key.into());
            state.set_derived_present(present);
        }

        Event::TxDetail(vm) => apply_tx_detail(ui, vm),

        Event::HistoryExhausted(complete) => {
            ui.global::<WalletState>().set_history_complete(complete);
        }

        Event::Busy { task, on } => {
            tracing::debug!(?task, on, "busy");
            // The Refresh control says "Reading…" while a read is in flight.
            // Without this it would look inert for the several seconds a
            // balance actually takes.
            match task {
                pecu_protocol::TaskKind::RefreshingBalance => {
                    ui.global::<WalletState>().set_busy(on);
                }
                pecu_protocol::TaskKind::PreparingSend | pecu_protocol::TaskKind::Broadcasting => {
                    ui.global::<SendState>().set_busy(on);
                }
                pecu_protocol::TaskKind::Converting => {
                    ui.global::<pecu_ui::ConvertState>().set_busy(on);
                }
                pecu_protocol::TaskKind::LoadingHistory => {
                    ui.global::<WalletState>().set_loading_history(on);
                }
                _ => {}
            }
        }

        Event::Notice(error) => apply_notice(ui, &error),

        // Core only ever replaces this list wholesale.
        Event::Pending(other) => tracing::debug!(?other, "unexpected pending delta"),
    }
}

/// Something the core wants said, put on the screen that asked for it.
///
/// Routing by code rather than showing everything in one place: a refusal has
/// to appear next to the control that caused it, or it is indistinguishable
/// from the button having done nothing. This is the stand-in for a proper toast
/// host, and the codes it knows about are the ones with a screen of their own.
fn apply_notice(ui: &AppWindow, error: &pecu_protocol::UiError) {
    let network = ui.global::<NetworkState>();

    match error.code {
        // The node was accepted. The only signal the add form waits for — it
        // deliberately does not clear itself on submit, so that a refused
        // address survives to be corrected rather than retyped.
        "node_added" => {
            network.set_problem(pecu_ui::Note::default());
            network.set_draft_url(SharedString::new());
            network.set_draft_label(SharedString::new());
        }

        // ── Inline, next to the control that caused it ──────────────────
        //
        // These four have a form on screen with a field to correct, and a
        // message beside that field beats one in the corner. Everything else
        // falls through to a toast.
        "add_node" | "node_connect" => {
            tracing::warn!(code = error.code, reason = %error.message.code, "notice");
            network.set_problem(note(&error.message));
        }

        "add_key" | "rename_key" => {
            tracing::warn!(code = error.code, reason = %error.message.code, "notice");
            ui.global::<WalletState>()
                .set_key_problem(note(&error.message));
        }

        // ── The unlock and restore forms ────────────────────────────────
        //
        // These happen while the shell is not on screen, so a toast would be
        // rendered over an onboarding screen that has a better place for it.
        "unlock" | "import_key" | "wallet_create" => {
            tracing::warn!(code = error.code, reason = %error.message.code, "notice");
            let state = ui.global::<WalletState>();
            state.set_busy(false);
            state.set_problem(note(&error.message));
        }

        // The passphrase re-prompt on the backup screen.
        "reveal_backup" => {
            tracing::warn!(code = error.code, reason = %error.message.code, "notice");
            ui.global::<SeedState>().set_problem(note(&error.message));
        }

        // ── Everything else ─────────────────────────────────────────────
        //
        // Which used to mean "nowhere". A refused spend, a payment that could
        // not be recorded, a history that would not load, a node failover — all
        // of them landed on a property rendered only by the unlock form, so
        // while the wallet was open they happened in silence.
        _ => {
            tracing::warn!(code = error.code, reason = %error.message.code, "notice");
            ui.global::<WalletState>().set_busy(false);
            pecu_ui::toast::show(ui, error);
        }
    }
}

/// Who this wallet has paid.
fn apply_address_book(ui: &AppWindow, rows: &[pecu_protocol::KnownAddressVm]) {
    let send = ui.global::<SendState>();

    let rows: Vec<KnownAddressRow> = rows
        .iter()
        .map(|row| KnownAddressRow {
            address: row.address.clone().into(),
            label: row.label.clone().into(),
            name: row.name.clone().into(),
            summary: row.summary.clone().into(),
        })
        .collect();
    send.set_known(ModelRc::from(Rc::new(VecModel::from(rows))));

    // The naming form is finished when a fresh book arrives, because the core
    // only sends one after it has acted. Derived from the wallet's answer
    // rather than from a "that worked" message — the same reasoning as the key
    // forms, and the same consequence: a refusal would leave the form as it is.
    if !send.get_naming().is_empty() {
        send.set_naming(SharedString::new());
        send.set_name_draft(SharedString::new());
    }
}

/// One transaction, opened.
fn apply_tx_detail(ui: &AppWindow, vm: pecu_protocol::TxDetailVm) {
    let tx = ui.global::<TxState>();
    tx.set_txid(vm.txid.into());
    tx.set_when(note(&vm.when_display));
    tx.set_amount(vm.net_display.into());
    tx.set_direction(
        match vm.direction {
            TxDirection::Incoming => "in",
            TxDirection::Outgoing => "out",
            TxDirection::Self_ => "self",
        }
        .into(),
    );
    tx.set_amount_is_native(vm.amount_is_native);
    tx.set_height(vm.height.to_string().into());
    tx.set_confirmations(
        vm.confirmations
            .map(|count| count.to_string())
            .unwrap_or_default()
            .into(),
    );
    // Empty means the node did not report one, which the sheet says in
    // words rather than showing a zero.
    tx.set_fee(vm.fee_display.unwrap_or_default().into());
    tx.set_explorer(vm.explorer_url.unwrap_or_default().into());
    tx.set_raw(vm.raw_json.unwrap_or_default().into());

    let lines: Vec<SharedString> = vm
        .currency_lines
        .iter()
        .map(|line| SharedString::from(line.as_str()))
        .collect();
    tx.set_currency_lines(ModelRc::from(Rc::new(VecModel::from(lines))));
}

/// The activity list, and the dashboard's excerpt of it.
fn apply_history(ui: &AppWindow, delta: ListDelta<HistoryRowVm>) {
    let state = ui.global::<WalletState>();
    match delta {
        // The dashboard shows a fixed window of the newest rows, so it
        // only ever receives a whole replacement. Paging deltas arrive
        // with the Activity screen, which is what they are for.
        ListDelta::Replace(rows) => {
            let rows: Vec<ActivityRow> = rows.iter().map(activity_row).collect();

            // The dashboard shows a handful; Activity shows all of
            // them. Two models rather than one sliced, because Slint's
            // `for` has no window — and the first rows need their day
            // heading cleared, since a six-row excerpt is not a day.
            let recent: Vec<ActivityRow> = rows
                .iter()
                .take(RECENT)
                .map(|row| ActivityRow {
                    group: pecu_ui::Note::default(),
                    ..row.clone()
                })
                .collect();

            state.set_activity(ModelRc::from(Rc::new(VecModel::from(recent))));
            state.set_history(ModelRc::from(Rc::new(VecModel::from(rows))));
        }
        other => tracing::debug!(?other, "history delta not rendered yet"),
    }
}

/// A name claim in progress, or none.
fn apply_registration(ui: &AppWindow, claim: Option<&pecu_protocol::RegistrationVm>) {
    let state = ui.global::<IdentityState>();
    let Some(vm) = claim else {
        // An empty step is what says there is no claim, so this is one write
        // rather than a second flag that could disagree with it. The drafts go
        // too — a finished claim must not leave its name in the box, where the
        // next press would try to register it again.
        state.set_reg_step(SharedString::new());
        state.set_name_draft(SharedString::new());
        state.set_revocation_draft(SharedString::new());
        state.set_recovery_draft(SharedString::new());
        state.set_name_fee(SharedString::new());
        state.set_reg_busy(false);
        // And the wizard, back to its first question with the second one
        // unanswered — the same reset Escape and Cancel do. A claim that just
        // finished must not leave the form ready to press through again.
        state.set_claim_step("name".into());
        state.set_claim_authority(SharedString::new());
        return;
    };

    state.set_reg_name(vm.name.clone().into());
    state.set_reg_note(note(&vm.note));
    state.set_reg_deadline(note(&vm.deadline));
    state.set_reg_fee(vm.fee_display.clone().into());
    state.set_reg_address(vm.address.clone().into());
    state.set_reg_busy(vm.busy);
    state.set_reg_cannot_be_revoked(vm.cannot_be_revoked);
    state.set_reg_steps(steps_of(&vm.steps));
    state.set_reg_step(vm.step.clone().into());
}

/// Close the detail sheet, and forget the answer it was showing.
fn close_identity(ui: &AppWindow) {
    let state = ui.global::<IdentityState>();
    state.set_address(SharedString::new());
    state.set_derived_key(SharedString::new());
    state.set_key_draft(SharedString::new());
}

/// The VerusIDs lists — yours, and the ones somebody looked up.
fn apply_identities(ui: &AppWindow, yours: &[IdentityVm], looked_up: &[IdentityVm]) {
    let state = ui.global::<IdentityState>();
    state.set_rows(identity_rows(yours));
    state.set_looked_up(identity_rows(looked_up));
}

/// Shut the define form and put it back on its first question.
///
/// Three statements rather than one, and all three matter: a form reopened after
/// a launch has been sent must not resume on the review it was left on, and the
/// authority answer has no safe default — leaving it set would carry one
/// launch's decision into the next one silently.
fn close_currency_form(ui: &AppWindow) {
    let state = ui.global::<pecu_ui::CurrencyState>();
    state.set_defining(false);
    state.set_form_step("kind".into());
    state.set_new_authority(SharedString::new());
}

/// Both halves of the currency walk, from one event.
///
/// One currency in detail, or nothing selected.
///
/// `None` clears the selection as well as the fields. Leaving `selected` set
/// while the panel emptied would leave a row highlighted next to a blank half
/// screen, which reads as a screen that failed rather than as one showing
/// nothing.
fn apply_market_detail(ui: &AppWindow, detail: Option<&pecu_protocol::MarketDetailVm>) {
    let state = ui.global::<pecu_ui::MarketState>();

    let Some(detail) = detail else {
        pecu_ui::spark::show(ui, &[]);
        state.set_selected(slint::SharedString::new());
        state.set_detail_name(slint::SharedString::new());
        state.set_detail_stats(slint::ModelRc::new(slint::VecModel::from(Vec::<
            pecu_ui::Stat,
        >::new())));
        state.set_detail_venues(slint::ModelRc::new(slint::VecModel::from(Vec::<
            pecu_ui::Venue,
        >::new())));
        return;
    };

    pecu_ui::spark::show(ui, &detail.series);
    state.set_detail_name(detail.name.clone().into());
    state.set_detail_subtitle(detail.subtitle.clone().into());
    state.set_detail_price(detail.price.clone().into());
    state.set_detail_change(detail.change.clone().into());
    state.set_detail_tone(detail.tone.clone().into());
    state.set_detail_route(detail.route.clone().into());
    state.set_detail_route_note(note(&detail.route_note));

    let figures: Vec<pecu_ui::Stat> = detail
        .stats
        .iter()
        .map(|stat| pecu_ui::Stat {
            label: note(&stat.label),
            value: stat.value.clone().into(),
        })
        .collect();
    state.set_detail_stats(slint::ModelRc::new(slint::VecModel::from(figures)));

    let venues: Vec<pecu_ui::Venue> = detail
        .venues
        .iter()
        .map(|venue| pecu_ui::Venue {
            name: venue.name.clone().into(),
            state: venue.state.clone().into(),
            price: venue.price.clone().into(),
            change: venue.change.clone().into(),
            tone: venue.tone.clone().into(),
            depth: venue.depth.clone().into(),
        })
        .collect();
    state.set_detail_venues(slint::ModelRc::new(slint::VecModel::from(venues)));
}

/// Written together because they arrive together — see `Event::Currencies`. Two
/// separate handlers would let the list and the picker be repainted a frame
/// apart, describing wallets a second apart.
fn apply_currencies(
    ui: &AppWindow,
    yours: &[pecu_protocol::CurrencyVm],
    eligible: &[pecu_protocol::EligibleIdentityVm],
) {
    let state = ui.global::<pecu_ui::CurrencyState>();

    let rows: Vec<pecu_ui::CurrencyRow> = yours
        .iter()
        .map(|row| pecu_ui::CurrencyRow {
            name: row.name.clone().into(),
            address: row.address.clone().into(),
            kind: row.kind.clone().into(),
            tone: row.tone.clone().into(),
            note: note(&row.note),
            mintable: row.mintable,
            start_block: row.start_block.clone().into(),
            started: row.started,
        })
        .collect();
    state.set_rows(ModelRc::from(Rc::new(VecModel::from(rows))));

    let picker: Vec<pecu_ui::EligibleIdentity> = eligible
        .iter()
        .map(|entry| pecu_ui::EligibleIdentity {
            name: entry.name.clone().into(),
            address: entry.address.clone().into(),
            refusal: note(&entry.refusal),
        })
        .collect();
    state.set_eligible(ModelRc::from(Rc::new(VecModel::from(picker))));

    // The walk is finished by the time this arrives — it is what the walk
    // produced. Nothing else clears the flag.
    state.set_busy(false);
}

/// The verdict on a draft, and everything the picture is drawn from.
///
/// Every figure here arrived finished. Nothing in this function divides
/// anything — the bars, the captions beside them and the preview are three
/// renderings of one arithmetic that happened in `currency::check`, and a
/// second division here is how a bar and its own caption end up disagreeing.
fn apply_currency_draft(ui: &AppWindow, vm: &pecu_protocol::CurrencyDraftVm) {
    let state = ui.global::<pecu_ui::CurrencyState>();

    let problems: Vec<pecu_ui::CurrencyProblem> = vm
        .problems
        .iter()
        .map(|problem| pecu_ui::CurrencyProblem {
            blocking: problem.blocking,
            text: note(&problem.text),
        })
        .collect();
    let blocking = vm
        .problems
        .iter()
        .filter(|problem| problem.blocking)
        .count();
    state.set_problems(ModelRc::from(Rc::new(VecModel::from(problems))));
    state.set_blocking_count(i32::try_from(blocking).unwrap_or(i32::MAX));

    state.set_slices(slices_of(&vm.slices));
    state.set_supply_slices(slices_of(&vm.supply_slices));
    state.set_weights_total(vm.weights_total.clone().into());
    state.set_supply_total(vm.supply_total.clone().into());
    state.set_start_block(vm.start_block.clone().into());
    state.set_fee(vm.fee_display.clone().into());

    let preview: Vec<pecu_ui::CurrencyField> = vm
        .preview
        .iter()
        .map(|field| pecu_ui::CurrencyField {
            label: note(&field.label),
            value_note: note(&field.value_note),
            value: field.value.clone().into(),
            permanent: field.permanent,
        })
        .collect();
    state.set_preview(ModelRc::from(Rc::new(VecModel::from(preview))));
    state.set_steps(steps_of(&vm.steps));

    // Last, because this is what turns the Review button on.
    state.set_ready(vm.ready);
}

/// The launch diagram, as the interface's own struct.
///
/// Shared by the plan on the form and the progress in the unfinished panel, so
/// the two cannot render the same steps differently.
fn steps_of(steps: &[pecu_protocol::FlowStepVm]) -> ModelRc<pecu_ui::FlowStep> {
    let rows: Vec<pecu_ui::FlowStep> = steps
        .iter()
        .map(|step| pecu_ui::FlowStep {
            label: note(&step.label),
            state: step.state.clone().into(),
            costs: step.costs,
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

fn slices_of(slices: &[pecu_protocol::CurrencySliceVm]) -> ModelRc<pecu_ui::CurrencySlice> {
    let rows: Vec<pecu_ui::CurrencySlice> = slices
        .iter()
        .map(|slice| pecu_ui::CurrencySlice {
            label: slice.label.clone().into(),
            percent: slice.percent,
            offset_percent: slice.offset_percent,
            percent_display: slice.percent_display.clone().into(),
            tone: slice.tone.clone().into(),
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

fn identity_rows(rows: &[IdentityVm]) -> ModelRc<IdentityRow> {
    let rows: Vec<IdentityRow> = rows
        .iter()
        .map(|row| IdentityRow {
            name: row.name.clone().into(),
            address: row.address.clone().into(),
            status: row.status.clone().into(),
            tone: row.tone.clone().into(),
            note: note(&row.note),
            mine: row.mine,
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

/// One VerusID, in full.
fn apply_identity_detail(ui: &AppWindow, vm: &IdentityDetailVm) {
    let state = ui.global::<IdentityState>();
    state.set_name(vm.name.clone().into());
    state.set_status(vm.status.clone().into());
    state.set_tone(vm.tone.clone().into());
    state.set_signatures_required(vm.signatures_required.clone().into());
    state.set_control_note(note(&vm.control_note));
    state.set_can_sign(vm.can_sign);
    state.set_revocation_authority(vm.revocation_authority.clone().into());
    state.set_recovery_authority(vm.recovery_authority.clone().into());
    state.set_cannot_be_revoked(vm.cannot_be_revoked);
    state.set_timelock_note(note(&vm.timelock_note));
    state.set_balance(vm.balance_display.clone().into());

    let primary: Vec<SharedString> = vm
        .primary_addresses
        .iter()
        .map(|address| address.clone().into())
        .collect();
    state.set_primary_addresses(ModelRc::from(Rc::new(VecModel::from(primary))));

    state.set_content(flatten(&vm.content));
    state.set_content_history(flatten(&vm.content_history));

    // A fresh sheet answers no stale question: the derived key belonged to the
    // identity that was open before this one.
    state.set_derived_key(SharedString::new());
    state.set_derived_present(false);

    // Last, because this is what opens the sheet — every field above is in
    // place before anything renders.
    state.set_address(vm.address.clone().into());
}

/// A content map, flattened into one row per value.
///
/// The key and its name go on the first row of each key only, the same way a
/// day heading rides on the first transaction of its day. Grouping needs to see
/// the row before this one, which a `for` over a model cannot — so it is
/// decided here, once, rather than guessed at in the interface.
fn flatten(entries: &[ContentEntryVm]) -> ModelRc<ContentEntry> {
    let mut rows = Vec::new();
    for entry in entries {
        for (index, value) in entry.values.iter().enumerate() {
            let first = index == 0;
            rows.push(ContentEntry {
                key: if first {
                    entry.key.clone().into()
                } else {
                    SharedString::new()
                },
                name: if first {
                    entry.name.clone().into()
                } else {
                    SharedString::new()
                },
                first,
                text: value.text.clone().into(),
                hex: value.hex.clone().into(),
                size: value.size.clone().into(),
                structured: value.structured.clone().into(),
            });
        }
    }
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

/// The review, built from the transaction that was actually signed.
fn apply_review(ui: &AppWindow, vm: &pecu_protocol::SendReviewVm) {
    let send = ui.global::<SendState>();
    send.set_ticket(i32::try_from(vm.ticket).unwrap_or(i32::MAX));
    send.set_amount(vm.amount_display.clone().into());
    send.set_fee(vm.fee_display.clone().into());
    send.set_total(vm.total_display.clone().into());
    send.set_change(vm.change_display.clone().into());
    send.set_balance_after(vm.balance_after_display.clone().into());
    send.set_from_address(vm.from_address.clone().into());
    send.set_first_time_recipient(vm.first_time_recipient);
    send.set_recipient_name(vm.recipient_name.clone().into());
    send.set_corroboration(note(&vm.corroboration));

    let outputs: Vec<ReviewOutput> = vm
        .outputs
        .iter()
        .map(|output| ReviewOutput {
            // Empty means the script could not be decoded. The screen
            // shows that as such rather than hiding the row.
            address: output.address.clone().unwrap_or_default().into(),
            kind: note(&output.kind),
            amount: output.amount_display.clone().into(),
            is_change: output.is_change,
        })
        .collect();
    send.set_outputs(ModelRc::from(Rc::new(VecModel::from(outputs))));

    send.set_problem(pecu_ui::Note::default());
    send.set_step("review".into());
}

/// A conversion, signed and not sent.
///
/// Every figure comes off the view model rather than being derived here, for
/// the reason `apply_review` follows: the interface formats nothing about
/// money, and a second place that did would be a second place that could
/// disagree with the core about what was signed.
fn apply_convert_review(ui: &AppWindow, vm: &pecu_protocol::ConvertReviewVm) {
    let convert = ui.global::<pecu_ui::ConvertState>();
    convert.set_ticket(i32::try_from(vm.ticket).unwrap_or(i32::MAX));
    convert.set_review_from(vm.from.clone().into());
    convert.set_review_to(vm.to.clone().into());
    convert.set_review_via(vm.via.clone().into());
    convert.set_review_pay(vm.pay_display.clone().into());
    convert.set_review_estimate(vm.estimate_display.clone().into());
    convert.set_review_minimum(vm.minimum_display.clone().into());
    convert.set_review_conversion_fee(vm.conversion_fee_display.clone().into());
    convert.set_review_network_fee(vm.network_fee_display.clone().into());
    convert.set_review_total(vm.total_display.clone().into());
    convert.set_review_balance_after(vm.balance_after_display.clone().into());
    convert.set_review_from_address(vm.from_address.clone().into());
    convert.set_review_recipient(vm.recipient.clone().into());

    let outputs: Vec<ReviewOutput> = vm
        .outputs
        .iter()
        .map(|output| ReviewOutput {
            // Empty means the script could not be decoded. The screen shows
            // that as such rather than hiding the row.
            address: output.address.clone().unwrap_or_default().into(),
            kind: note(&output.kind),
            amount: output.amount_display.clone().into(),
            is_change: output.is_change,
        })
        .collect();
    convert.set_outputs(ModelRc::from(Rc::new(VecModel::from(outputs))));

    convert.set_problem(pecu_ui::Note::default());
    convert.set_step("review".into());
}

/// What happened when a conversion's bytes were handed over.
///
/// The failure arm stays on whichever step it failed on, which is the whole
/// design of it: while the network has conversions paused the refusal arrives
/// after the review has been read and agreed to, and moving off that screen
/// would take away the thing the refusal is about.
fn apply_convert_outcome(ui: &AppWindow, outcome: SendOutcomeVm) {
    let convert = ui.global::<pecu_ui::ConvertState>();
    match outcome {
        SendOutcomeVm::Sent {
            txid, explorer_url, ..
        } => {
            convert.set_txid(txid.into());
            convert.set_explorer(explorer_url.into());
            convert.set_problem(pecu_ui::Note::default());
            convert.set_step("sent".into());
        }
        SendOutcomeVm::Uncertain {
            txid,
            pending_id,
            explorer_url,
        } => {
            tracing::warn!(%txid, pending_id, "conversion broadcast outcome unknown");
            convert.set_txid(txid.into());
            convert.set_explorer(explorer_url.into());
            convert.set_step("uncertain".into());
        }
        SendOutcomeVm::Failed(error) => {
            convert.set_problem(note(&error.message));
        }
    }
}

/// What happened when the bytes were handed over.
fn apply_outcome(ui: &AppWindow, outcome: SendOutcomeVm) {
    let send = ui.global::<SendState>();
    match outcome {
        SendOutcomeVm::Sent {
            txid, explorer_url, ..
        } => {
            send.set_txid(txid.into());
            send.set_explorer(explorer_url.into());
            send.set_problem(pecu_ui::Note::default());
            send.set_step("sent".into());
        }
        // Not an error, and not a success. The bytes are on disk and the
        // resolution is to ask the node — never to send again.
        SendOutcomeVm::Uncertain {
            txid,
            pending_id,
            explorer_url,
        } => {
            tracing::warn!(%txid, pending_id, "broadcast outcome unknown");
            send.set_txid(txid.into());
            send.set_explorer(explorer_url.into());
            send.set_step("uncertain".into());
        }
        SendOutcomeVm::Failed(error) => {
            send.set_problem(note(&error.message));
            // Stays on whichever step it failed on, so the form or the
            // review is still there to correct.
        }
    }
}

/// The wallet's own state: whether it exists, whether it is open, its keys.
fn apply_wallet(ui: &AppWindow, vm: pecu_protocol::WalletVm) {
    let state = ui.global::<WalletState>();
    state.set_exists(vm.exists);
    state.set_locked(vm.locked);
    state.set_name(vm.name.clone().into());
    state.set_busy(false);
    // A successful transition clears whatever went wrong last time.
    state.set_problem(pecu_ui::Note::default());
    state.set_loading(false);

    // The ACTIVE key's address, not the first one. Receive shows this and Send
    // pays from it, so getting it from position rather than from the wallet's
    // own answer would quietly put someone else's address on the QR.
    let active = vm
        .active_key
        .as_ref()
        .and_then(|label| vm.keys.iter().find(|key| &key.label == label))
        .or_else(|| vm.keys.first());
    state.set_address(
        active
            .map(|key| key.address.clone())
            .unwrap_or_default()
            .into(),
    );
    state.set_active_key(vm.active_key.clone().unwrap_or_default().into());
    // For speech only — see `pecu_ui::spoken`.
    state.set_address_spoken(pecu_ui::spoken(&state.get_address()).into());

    // The shielded address comes from the wallet rather than from a key row:
    // it is not stored anywhere, it is derived from the active key's phrase,
    // and only the core has the vault open to do that.
    state.set_shielded_address(vm.shielded_address.clone().into());
    state.set_shielded_address_spoken(pecu_ui::spoken(&vm.shielded_address).into());
    state.set_shielded_note(vm.shielded_note.as_ref().map(note).unwrap_or_default());

    // The send form needs to know whether there is a second balance to offer.
    // An address is enough to answer that: a key with no shielded address has
    // no shielded balance and never will, so the selector is absent rather than
    // present-and-disabled.
    let send = ui.global::<SendState>();
    send.set_shielded_available(!vm.shielded_address.is_empty());
    // Expanded here rather than carried as three fields: `.slint` has no sum
    // type, so the one value the core reasons about becomes the two questions
    // each screen actually asks.
    send.set_shielded_balance(vm.shielded_funds.balance().into());
    send.set_shielded_scanned(vm.shielded_funds.scanned());
    // The active key's own money, which is what this form spends. Blanked when
    // nothing is maturing by the same helper the dashboard's breakdown uses —
    // a zero here would be the wallet stating that none of this key's coins are
    // immature, which is a claim, not a blank.
    send.set_key_spendable(vm.key_funds.spendable_display.clone().into());
    send.set_key_immature(shown(
        &vm.key_funds.immature_sats,
        &vm.key_funds.immature_display,
    ));
    state.set_shielded_balance(vm.shielded_funds.balance().into());
    state.set_shielded_any(vm.shielded_funds.any());
    state.set_shielded_scanning(vm.shielded_scan.is_some());
    state.set_shielded_scan_percent(
        vm.shielded_scan
            .and_then(|p| i32::try_from(p).ok())
            .unwrap_or(0),
    );

    let keys: Vec<KeyRow> = vm
        .keys
        .iter()
        .map(|key| KeyRow {
            label: key.label.clone().into(),
            address: key.address.clone().into(),
            origin: match key.origin {
                KeyOrigin::Generated => "generated",
                KeyOrigin::ImportedPhrase => "phrase",
                KeyOrigin::ImportedWif => "wif",
            }
            .into(),
            used: key.used,
            backed_up: key.backed_up,
            active: Some(&key.label) == vm.active_key.as_ref(),
        })
        .collect();
    state.set_keys(ModelRc::from(Rc::new(VecModel::from(keys))));
    close_finished_key_forms(&state, &vm);
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

/// Close a key form once the thing it was asking for has happened.
///
/// Derived from the wallet's own answer rather than from a "that worked"
/// message: the rename form is open on a label, so it is finished exactly when
/// no key has that label any more. The add form is finished when a key with the
/// name it was typing exists.
///
/// Doing it this way means a **refused** operation leaves both forms exactly as
/// they were — which is the point. A name the vault would not accept must not
/// disappear off the screen along with the sentence explaining why.
fn close_finished_key_forms(state: &WalletState<'_>, vm: &pecu_protocol::WalletVm) {
    let has = |label: &str| vm.keys.iter().any(|key| key.label == label);

    let renaming = state.get_renaming();
    if !renaming.is_empty() && !has(&renaming) {
        state.set_renaming(SharedString::new());
        state.set_rename_draft(SharedString::new());
        state.set_key_problem(pecu_ui::Note::default());
    }

    let adding = state.get_new_key_draft();
    if !adding.is_empty() && has(&adding) {
        state.set_new_key_draft(SharedString::new());
        state.set_key_problem(pecu_ui::Note::default());
    }
}

/// Node health, the active endpoint, and which chain it says it is on.
fn apply_network(ui: &AppWindow, vm: &pecu_protocol::NetworkVm) {
    let state = ui.global::<NetworkState>();
    state.set_requested(vm.requested.clone().into());
    let chains: Vec<pecu_ui::ChainChoice> = vm
        .chains
        .iter()
        .map(|chain| pecu_ui::ChainChoice {
            name: chain.name.clone().into(),
            title: chain.title.clone().into(),
        })
        .collect();
    state.set_chains(slint::ModelRc::new(slint::VecModel::from(chains)));
    // The chooser follows the wallet, never the other way round: it marks the
    // button whose name matches `requested_name`, so a switch the core refused
    // leaves the highlight where it was. Matched on the name rather than on a
    // position or a display label, because those two describe a fixed number of
    // chains and this build already offers five.
    state.set_requested_name(vm.requested_name.clone().into());
    state.set_chain_title(vm.chain_title.clone().into());
    state.set_effective(vm.effective.clone().unwrap_or_default().into());
    state.set_tip(vm.tip.map(thousands).unwrap_or_default().into());
    state.set_syncing(vm.syncing);
    // One enum in, two flags out. The interface needs to answer two questions
    // separately — draw the block at all, and which half of it — and the third
    // combination is unreachable by construction here.
    state.set_spend_needs_opt_in(vm.spend_gate != pecu_protocol::SpendGate::NotNeeded);
    state.set_allow_spending(vm.spend_gate == pecu_protocol::SpendGate::Open);
    state.set_light_server(vm.light_server.clone().into());
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
/// every string arrived finished from `pecu_core::portfolio`.
fn apply_portfolio(ui: &AppWindow, vm: &PortfolioVm) {
    let state = ui.global::<WalletState>();
    let balance = &vm.balance;

    state.set_total(balance.total_display.clone().into());
    state.set_spendable(balance.spendable_display.clone().into());
    state.set_stale(vm.stale);
    // The native asset row carries the chain's own name, which is what the
    // hero figure should be labelled with too.
    if let Some(native) = vm.assets.iter().find(|asset| asset.native) {
        state.set_ticker(native.name.clone().into());
    }
    state.set_loading(false);

    // The breakdown appears only when it says something the total does not. A
    // wallet with everything spendable stays quiet rather than printing
    // "0.0000 0000 maturing" under every balance.
    //
    // **Each figure is blanked on its own, not just the group.** Only
    // `incoming` used to be, so a wallet with money arriving and nothing
    // maturing printed a zero anyway — the group was non-quiet and the maturing
    // figure was drawn unconditionally. A zero beside three real numbers is not
    // neutral: it is the wallet stating that none of your coins are immature,
    // in the same breath and the same weight as the coins that are.
    let quiet = is_zero(&balance.immature_sats)
        && is_zero(&balance.pending_out_sats)
        && is_zero(&balance.pending_in_sats);
    state.set_has_breakdown(!quiet);

    state.set_immature(shown(&balance.immature_sats, &balance.immature_display));
    // Money that has left and not settled. Counted by `quiet` since the first
    // version, and then never drawn — so a wallet whose only unusual state was
    // an unconfirmed payment out showed a breakdown that did not mention it.
    state.set_pending(shown(&balance.pending_out_sats, &balance.pending_display));
    state.set_incoming(shown(&balance.pending_in_sats, &balance.incoming_display));

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
            currency_id: asset.currency_id.clone().into(),
            native: asset.native,
        })
        .collect();
    // Folded here rather than in the interface, for the same reason
    // `has_breakdown` is: Slint cannot fold a model inside an expression. Why
    // it is carried at all is argued at the property declaration.
    state.set_holds_tokens(vm.assets.iter().any(|asset| !asset.native));
    state.set_assets(ModelRc::from(Rc::new(VecModel::from(assets))));
}

/// Satoshi counts cross as decimal strings, so "nothing" is a string test.
fn is_zero(sats: &str) -> bool {
    sats.is_empty() || sats.chars().all(|c| c == '0')
}

/// A figure, or nothing at all when there is nothing to say.
///
/// The core formats; this only decides whether the line is drawn. Zero is not
/// neutral on a balance: printed beside real numbers it is the wallet stating
/// that none of your coins are maturing, in the same breath and the same weight
/// as the coins that are.
///
/// A free function rather than a closure because two handlers want it — the
/// dashboard's wallet-wide breakdown and the send form's per-key one — and the
/// rule has to be the same on both or the same wallet says two different things
/// about the same zero.
fn shown(zero: &str, display: &str) -> SharedString {
    if is_zero(zero) {
        SharedString::new()
    } else {
        display.into()
    }
}

/// A named reason, on its way to the words.
///
/// The core decides which sentence applies and supplies its values; the text
/// lives in `components/note.slint`. See `NoteVm` for why it stopped writing
/// prose.
/// The route, as the interface names it.
///
/// A string rather than an int, because it is read in `.slint` comparisons
/// where a number would be a magic constant on both sides.
const fn route_name(route: pecu_protocol::Route) -> &'static str {
    match route {
        pecu_protocol::Route::Transparent => "transparent",
        pecu_protocol::Route::Shield => "shield",
        pecu_protocol::Route::Private => "private",
        pecu_protocol::Route::Unshield => "unshield",
    }
}

fn note(vm: &pecu_protocol::NoteVm) -> pecu_ui::Note {
    pecu_ui::Note {
        code: vm.code.clone().into(),
        args: slint::ModelRc::new(slint::VecModel::from(
            vm.args
                .iter()
                .map(|arg| slint::SharedString::from(arg.as_str()))
                .collect::<Vec<_>>(),
        )),
    }
}

fn activity_row(row: &HistoryRowVm) -> ActivityRow {
    ActivityRow {
        txid: row.txid.clone().into(),
        txid_short: row.txid_short.clone().into(),
        direction: match row.direction {
            TxDirection::Incoming => "in",
            TxDirection::Outgoing => "out",
            TxDirection::Self_ => "self",
        }
        .into(),
        amount: row.net_display.clone().into(),
        when: note(&row.when_display),
        pending: row.pending,
        height: i32::try_from(row.height).unwrap_or(i32::MAX),
        group: note(&row.group),
        kind: row.kind.clone().into(),
        note: row.note.clone().into(),
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
        note: note(&node.note),
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
