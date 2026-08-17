//! Chart geometry: a balance over time, as two path strings.
//!
//! # What this crate does not depend on
//!
//! Slint, the core, the SDK. Nothing. Chart geometry is arithmetic, and
//! arithmetic can be checked against a golden string — which matters here more
//! than it looks, because the failures this code can have are *invisible*. A
//! path with its steps in the wrong order, or a corner radius wider than the
//! run it belongs to, produces a picture that is plainly a chart and quietly
//! the wrong one. Nobody spots that by looking; a string comparison spots it
//! every time.
//!
//! # Money stays an integer
//!
//! Every value in here is `i64` satoshis and every time is `i64` seconds. The
//! only thing that becomes a float is a **ratio** — a position within the range,
//! which has no units and only ever has to be right to within a pixel. No
//! `f32` ever turns back into an amount, and nothing displayed as money passes
//! through one.
//!
//! # The three decisions worth knowing about
//!
//! **A balance is drawn as a step function**, because that is what it is. See
//! [`plot::plot`].
//!
//! **The series is built backwards** from the balance the wallet knows right
//! now, rather than forwards from a starting balance it does not. See
//! [`series::from_deltas`].
//!
//! **x is proportional to time**, not to position in the list. See
//! [`plot::plot`].

pub mod lttb;
pub mod plot;
pub mod series;

pub use lttb::downsample;
pub use plot::{plot, step_at, time_at, Plot, Viewport};
pub use series::{from_deltas, Point};

/// The most points worth drawing.
///
/// Past this the samples are closer together than a pixel, so the extra ones
/// cost path length and rendering time to draw a line that is already there.
/// [`downsample`] keeps the shape rather than every point — see its docs for
/// why that is not the same as taking every nth.
pub const MAX_POINTS: usize = 240;

/// The radius each step's corners are rounded by, in logical pixels.
///
/// Enough to read as drawn rather than as plotted; small enough that the flat
/// runs are still visibly flat. It is clamped against the space available at
/// every corner, so this is a maximum rather than a promise.
pub const CORNER: f32 = 4.0;

/// Everything from a raw history to a finished pair of paths.
///
/// # There is one view, and it is all of it
///
/// This had a range control — 1W, 1M, 3M, 1Y, ALL — carried over from the shape
/// of a price chart. That control answers a real question when a continuous
/// series spans years and you need to zoom into part of it. A wallet's balance
/// series is not that: it is bounded by how far the scan has looked, so "all of
/// it" is already a bounded window, and the narrower buttons were mostly
/// answering a question nobody had.
///
/// Removing them removed the machinery that made them honest — which range the
/// scan could fill, and what to draw for the part of a window reaching back
/// further than the wallet exists. Both bugs this chart has had lived in there.
///
/// The axis therefore spans the readings: earliest to now.
pub fn build(points: &[Point], now: i64, view: &Viewport) -> Option<Plot> {
    let window = (points.first().map_or(now, |first| first.t), now);
    let thinned = downsample(points, MAX_POINTS);
    plot(&thinned, view, CORNER, window)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The axis covers the readings, earliest to now — not whatever the last
    /// reading happened to be. A wallet quiet for a week must show that week.
    #[test]
    fn the_axis_runs_from_the_earliest_reading_to_now() {
        let now = 1_800_000_000;
        let points = vec![
            Point {
                t: now - 90 * 86_400,
                value: 0,
            },
            Point {
                t: now - 89 * 86_400,
                value: 500,
            },
        ];

        let plot = build(&points, now, &Viewport::new(400.0, 200.0)).expect("a plot");
        assert_eq!(plot.span, (now - 90 * 86_400, now));
        // The last reading is 89 days ago, so the flat run to `now` is most of
        // the width — which is the truth about a wallet nobody has touched.
        let last = plot.xs.last().copied().unwrap_or_default();
        assert!(last < plot.plot_width * 0.02, "{last}");
    }

    #[test]
    fn a_long_history_is_thinned_to_the_budget() {
        let points: Vec<Point> = (0..5_000)
            .map(|i| Point {
                t: i * 60,
                value: i,
            })
            .collect();
        let plot = build(&points, 5_000 * 60, &Viewport::new(800.0, 240.0)).expect("plot");

        assert_eq!(plot.values.len(), MAX_POINTS);
    }
}
