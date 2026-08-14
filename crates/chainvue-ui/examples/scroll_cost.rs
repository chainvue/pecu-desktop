//! What a large transaction history costs to put on screen.
//!
//! ```sh
//! cargo run -p chainvue-ui --example scroll_cost --release
//! ```
//!
//! # Why this exists as a measurement and not an assertion
//!
//! The plan's acceptance line for the activity list is "a 50k-row synthetic
//! history scrolls at 60 fps", written on the strength of Slint's `ListView`
//! virtualising since 1.16. The screen does not use a `ListView`. It is a plain
//! `for` over the model inside a `Card`, which instantiates **every row** as a
//! live component — so the cost is linear in the length of somebody's history,
//! and it is paid on the UI thread before the screen appears.
//!
//! Numbers rather than an argument, because "it will be slow" is a guess and
//! "2.9 seconds at fifty thousand" is a decision.
//!
//! Release build, or the figures mean nothing.
//!
//! # Why only build and layout are timed
//!
//! Rendering cannot be measured here past about a thousand rows: the software
//! renderer works in i16 fixed-point coordinates, and a column of 64px rows
//! overflows them somewhere north of 32 767 pixels — it panics with
//! `Overflow -32066`. That failure is itself the finding. A virtualised list is
//! never that tall, because it never builds the rows nobody is looking at.

// A measuring harness, not shipped code. A fixture that does not behave should
// stop the run loudly rather than produce a number nobody can trust.
#![allow(clippy::expect_used)]

use chainvue_ui::{snapshot, ActivityRow, AppWindow, WalletState};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};
use std::rc::Rc;
use std::time::Instant;

fn main() {
    let window = snapshot::install().expect("offscreen platform");

    println!("build and layout of the activity screen, by history length\n");
    for count in [100usize, 1_000, 10_000, 50_000] {
        let ui = AppWindow::new().expect("a window");
        chainvue_ui::chart::install(&ui);

        let wallet = ui.global::<WalletState>();
        wallet.set_loading(false);
        wallet.set_exists(true);
        wallet.set_locked(false);

        let model = Instant::now();
        let rows: Vec<ActivityRow> = (0..count).map(row).collect();
        let model = model.elapsed();

        wallet.set_history(ModelRc::from(Rc::new(VecModel::from(rows))));
        ui.set_screen("activity".into());
        window.set_size(PhysicalSize::new(snapshot::WIDTH, snapshot::HEIGHT));

        // `show` is where the components are built and laid out. On a real
        // window this happens on the thread the event loop owns, so every
        // millisecond here is a millisecond the interface is not answering.
        let laid_out = Instant::now();
        ui.show().expect("show");
        let laid_out = laid_out.elapsed();
        ui.hide().expect("hide");

        println!("{count:>6} rows   model {model:>9.1?}   build + layout {laid_out:>9.1?}");
    }
}

fn row(index: usize) -> ActivityRow {
    ActivityRow {
        txid: format!("{index:064x}").into(),
        txid_short: "abcd…9876".into(),
        direction: if index.is_multiple_of(3) { "out" } else { "in" }.into(),
        amount: "12.5000 0000".into(),
        when: "2 hours ago".into(),
        pending: false,
        height: 1_000_000 - i32::try_from(index).unwrap_or(0),
        // A day heading every twentieth row, so the taller variant of the row
        // is represented rather than measuring the cheapest one.
        group: if index.is_multiple_of(20) {
            "13 August"
        } else {
            ""
        }
        .into(),
    }
}
