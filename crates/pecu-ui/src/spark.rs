//! The markets sparkline: a price series, as one path.
//!
//! # Why this is not `chart.rs`
//!
//! They draw the same kind of thing and answer different questions. The balance
//! chart has a cursor, an axis in words, a fill, a "now" dot and a caption
//! about how far the scan has looked — because somebody reads it to find out
//! what happened to their money and when. This is a thumbnail beside a price:
//! no cursor, no labels, no fill, and it is redrawn every time a different row
//! is selected.
//!
//! Sharing `ChartState` would have meant one global holding two charts' worth
//! of state, with the hover of one reachable from the other. What is shared is
//! the part worth sharing — `pecu_chart`'s geometry, so both lines are plotted
//! by one implementation.
//!
//! # Why the state is a thread-local
//!
//! The same reason `chart.rs` gives: Slint's event loop owns the main thread,
//! there is exactly one window, and threading a handle through `bridge::apply`
//! would put plotting in the signature of every event the wallet can emit.

use std::cell::RefCell;

use pecu_chart::{Point, Viewport};
use slint::{ComponentHandle, SharedString};

use crate::{AppWindow, MarketState};

thread_local! {
    static SERIES: RefCell<Vec<Point>> = const { RefCell::new(Vec::new()) };
}

/// Wire the path callback. **Once per window.**
pub fn install(ui: &AppWindow) {
    // Reset, for the same reason the balance chart resets: the snapshot
    // renderer builds many windows in one process, and without this a screen
    // showing no market would draw whichever series was rendered before it.
    SERIES.with_borrow_mut(Vec::clear);
    ui.global::<MarketState>().set_has_series(false);

    // A binding on the element's own size rather than a property pushed from a
    // resize handler — see `chart.rs` for why that distinction is not
    // cosmetic.
    ui.global::<MarketState>()
        .on_spark_at(|width, height| path(width, height).unwrap_or_default());
}

/// What the selected market did, as points the plotter understands.
///
/// Cleared when a market has none, which is what draws the empty state rather
/// than a stale line belonging to the row above.
pub fn show(ui: &AppWindow, points: &[pecu_protocol::ChartPointVm]) {
    SERIES.with_borrow_mut(|series| {
        series.clear();
        series.extend(points.iter().map(|point| Point {
            t: point.t,
            value: point.sats,
        }));
    });

    // Two readings, not one. A single point is a dot with nothing to join it
    // to, and `pecu_chart` draws it as a line from itself to itself — which
    // renders as a horizontal rule and reads as a price that never moved.
    ui.global::<MarketState>().set_has_series(points.len() >= 2);
}

/// The stroke, for an element of this size.
fn path(width: f32, height: f32) -> Option<SharedString> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }

    SERIES.with_borrow(|series| {
        // The window ends at the newest reading rather than at the clock. A
        // sparkline whose right-hand edge is "now" leaves a growing gap after
        // the last sample, and an idle pool would draw a line that shrinks away
        // from the edge as the day goes on.
        let now = series.last()?.t;
        let view = Viewport {
            width,
            height,
            // Enough that a stroke is not clipped in half at the top or the
            // bottom. No room reserved at the sides: there is no dot to fit.
            pad_left: 0.0,
            pad_right: 0.0,
            pad_top: 2.0,
            pad_bottom: 2.0,
        };
        let plot = pecu_chart::build(series, now, &view)?;
        Some(SharedString::from(plot.line.as_str()))
    })
}
