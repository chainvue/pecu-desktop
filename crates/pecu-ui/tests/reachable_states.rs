//! A fixture may only photograph a state the wallet can actually reach.
//!
//! # Why this is a test and not a reference image
//!
//! A reference image cannot catch this. `tests/visual.rs` asks whether the
//! interface still draws what it drew last time — it has no opinion about
//! whether what it drew was ever reachable, so a fixture describing an
//! impossible screen produces a green test forever. That is precisely how
//! `history_filtered` sat for as long as it did: it pressed the "Payments"
//! chip and left a login, a conversion and an identity update on the list, and
//! nothing was red, because nothing here was ever asking that question.
//!
//! # The one invariant, and why it is this one
//!
//! `ActivityState.filter` is an **echo**. The core does the filtering and
//! re-sends the list — `portfolio::rows_from` drops every row of another kind
//! before it is sent, and `ui/vm/state.slint` records why that cannot be done
//! in `.slint`. So on any screen the wallet can produce, a filter other than
//! "all" implies every row on the list is of that kind. A fixture that sets one
//! without the other is describing a different wallet.
//!
//! This runs over **every** case in `snapshot::CASES` rather than over
//! `history_filtered` alone. The fixture that broke this was not written wrong
//! on purpose; it was written before the filtering moved into the core and was
//! never revisited. The next one will arrive the same way, so the guard is
//! placed where a new case walks into it rather than where this one was.
//!
//! # What this does not check
//!
//! Every other way a fixture could invent a state. The figures on the history
//! summary strip are admitted inventions, the day headings are hand-written
//! rather than derived from `calendar_day`, and three of the four row kinds
//! cannot be produced by this wallet at all — all three are deliberate and
//! documented where they are written. This asserts one relationship between two
//! pieces of state, which is the one the product enforces on itself.

// Clippy's test exemptions cover `#[test]` functions, not the helpers beside
// them, and this whole file is test code.
#![allow(clippy::expect_used)]

use pecu_ui::{ActivityState, AppWindow, WalletState};
use slint::{ComponentHandle, Model};

/// The window each fixture is applied to, wired as `snapshot::render` wires it.
///
/// The chart and sparkline callbacks are installed because several fixtures
/// seed them, and an uninstalled chart is not a state any of them expect. The
/// window is never shown and nothing is drawn: this reads view state, so laying
/// it out would only make it slower.
fn seeded(seed: fn(&AppWindow)) -> AppWindow {
    let ui = AppWindow::new().expect("a window");
    pecu_ui::chart::install(&ui);
    pecu_ui::spark::install(&ui);
    seed(&ui);
    ui
}

#[test]
fn a_filtered_history_fixture_holds_only_rows_of_that_kind() {
    i_slint_backend_testing::init_no_event_loop();

    let mut impossible = Vec::new();
    let mut checked = 0usize;

    for &(_screen, label, seed) in pecu_ui::snapshot::CASES {
        let ui = seeded(seed);

        let filter = ui.global::<ActivityState>().get_filter();
        // "all" is the unfiltered list and the default, and says nothing about
        // the rows. Empty is not a filter the chips can produce; treated the
        // same rather than failed, because the claim here is about the rows.
        if filter.is_empty() || filter == "all" {
            continue;
        }
        checked += 1;

        let rows = ui.global::<WalletState>().get_history();
        let strays: Vec<String> = (0..rows.row_count())
            .filter_map(|i| rows.row_data(i))
            .filter(|row| row.kind != filter)
            .map(|row| row.kind.to_string())
            .collect();

        if !strays.is_empty() {
            impossible.push(format!(
                "{label}: the filter is {filter:?} and {} of {} rows are not — {}",
                strays.len(),
                rows.row_count(),
                strays.join(", "),
            ));
        }
    }

    assert!(
        impossible.is_empty(),
        "these fixtures photograph a filtered history the wallet cannot \
         produce:\n  {}\n\n\
         The core filters the list before it sends it, so a filter other than \
         \"all\" leaves only rows of that kind. Filter the fixture's rows too, \
         or set the filter back to \"all\".",
        impossible.join("\n  "),
    );

    // A silent zero would mean this file had stopped testing anything — the
    // failure mode of a loop that finds its subject by inspection rather than
    // by name. `history-filtered` is that subject today.
    assert!(
        checked > 0,
        "no fixture sets a history filter, so this test checked nothing",
    );
}
