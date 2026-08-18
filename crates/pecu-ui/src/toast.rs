//! What the wallet said, and nobody had anywhere to read.
//!
//! # Why this exists
//!
//! Notices used to be routed to a per-screen string, and every code without a
//! screen of its own fell through to one rendered only on the unlock form. So
//! while the wallet was open, these happened in complete silence:
//!
//! * a spend refused because mainnet spending is off, or the node is on another
//!   chain — you press Send and nothing at all occurs;
//! * a payment that could not be written to the pending ledger, and was
//!   therefore not sent;
//! * a history that would not load, which looks exactly like "no transactions";
//! * a node the wallet failed over from.
//!
//! That was found by reading a log after a real session, where "could not read
//! this wallet's activity" had been raised every few seconds for eight minutes
//! behind a screen showing a cached list.
//!
//! # De-duplication is the point, not a nicety
//!
//! A refresh failing every fifteen seconds is one problem. Forty cards saying
//! so is a wallet nobody can use, and the honest rendering — one card with a
//! count — is also the readable one.

use std::cell::RefCell;

use pecu_protocol::{Severity, UiError};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::{AppWindow, ToastRow, ToastState};

/// How many are kept. Past this the oldest goes: a stack taller than the window
/// hides the newest thing, which is the one that just happened.
const MOST: usize = 4;

thread_local! {
    /// Same reasoning as the chart's state — one window, one thread, and
    /// threading a handle through every event's signature would put toast
    /// plumbing in the type of everything the wallet can say.
    static TOASTS: RefCell<Vec<Notice>> = const { RefCell::new(Vec::new()) };
}

struct Notice {
    code: &'static str,
    note: pecu_protocol::NoteVm,
    detail: String,
    severity: Severity,
    count: u32,
}

/// Wire the dismiss callbacks. Called once, with the window.
pub fn install(ui: &AppWindow) {
    TOASTS.with_borrow_mut(Vec::clear);

    {
        let weak = ui.as_weak();
        ui.global::<ToastState>().on_dismiss(move |index| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if let Ok(index) = usize::try_from(index) {
                TOASTS.with_borrow_mut(|toasts| {
                    if index < toasts.len() {
                        toasts.remove(index);
                    }
                });
            }
            publish(&ui);
        });
    }

    {
        let weak = ui.as_weak();
        ui.global::<ToastState>().on_dismiss_all(move || {
            TOASTS.with_borrow_mut(Vec::clear);
            if let Some(ui) = weak.upgrade() {
                publish(&ui);
            }
        });
    }

    publish(ui);
}

/// Say something.
///
/// A notice with the same code as one already showing bumps its count rather
/// than stacking. The code is the identity of a *problem*; the twentieth
/// occurrence is not new information.
pub fn show(ui: &AppWindow, error: &UiError) {
    TOASTS.with_borrow_mut(|toasts| {
        if let Some(existing) = toasts.iter_mut().find(|notice| notice.code == error.code) {
            existing.count = existing.count.saturating_add(1);
            // The newest wording wins: the same code can carry a different
            // sentence, and the one that just happened is the true one.
            existing.note.clone_from(&error.message);
            existing.detail.clone_from(&error.detail);
            return;
        }

        toasts.push(Notice {
            code: error.code,
            note: error.message.clone(),
            detail: error.detail.clone(),
            severity: error.severity,
            count: 1,
        });

        // Oldest first, because the newest is the one somebody is looking for.
        while toasts.len() > MOST {
            toasts.remove(0);
        }
    });

    publish(ui);
}

/// Clear everything.
///
/// Called when the wallet locks: a problem raised about a session that has
/// ended is not a problem anybody can act on, and leaving it on the unlock
/// screen would be shouting at the wrong person.
pub fn clear(ui: &AppWindow) {
    TOASTS.with_borrow_mut(Vec::clear);
    publish(ui);
}

fn publish(ui: &AppWindow) {
    let rows: Vec<ToastRow> = TOASTS.with_borrow(|toasts| {
        toasts
            .iter()
            .map(|notice| ToastRow {
                code: notice.code.into(),
                note: crate::Note {
                    code: notice.note.code.as_str().into(),
                    args: slint::ModelRc::new(slint::VecModel::from(
                        notice.note.args
                            .iter()
                            .map(|arg| slint::SharedString::from(arg.as_str()))
                            .collect::<Vec<_>>(),
                    )),
                },
                detail: notice.detail.as_str().into(),
                severity: match notice.severity {
                    Severity::Info => "info",
                    Severity::Warning => "warning",
                    Severity::Danger => "danger",
                }
                .into(),
                count: i32::try_from(notice.count).unwrap_or(i32::MAX),
            })
            .collect()
    });

    ui.global::<ToastState>()
        .set_rows(ModelRc::from(std::rc::Rc::new(VecModel::from(rows))));
}

/// How many are showing. For the tests, and for anything that needs to know
/// whether the wallet currently has something to say.
pub fn count(ui: &AppWindow) -> usize {
    ui.global::<ToastState>().get_rows().row_count()
}

/// The codes currently showing, oldest first.
pub fn codes(ui: &AppWindow) -> Vec<SharedString> {
    ui.global::<ToastState>()
        .get_rows()
        .iter()
        .map(|row| row.code)
        .collect()
}
