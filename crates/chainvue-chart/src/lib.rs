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
pub use plot::{index_at, plot, Plot, Viewport};
pub use series::{from_deltas, since, span, Point};

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

/// How far back a range covers, in seconds.
///
/// `None` is "everything the wallet has scanned", which is the only one of
/// these that is always honest — the rest depend on the history reaching back
/// far enough, and the interface disables the ones it cannot fill.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Range {
    Week,
    Month,
    Quarter,
    Year,
    /// The default, and the only one that is honest before anything has
    /// been scanned.
    #[default]
    All,
}

impl Range {
    pub const ORDER: [Self; 5] = [
        Self::Week,
        Self::Month,
        Self::Quarter,
        Self::Year,
        Self::All,
    ];

    pub fn seconds(self) -> Option<i64> {
        const DAY: i64 = 86_400;
        match self {
            Self::Week => Some(7 * DAY),
            Self::Month => Some(30 * DAY),
            Self::Quarter => Some(90 * DAY),
            Self::Year => Some(365 * DAY),
            Self::All => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Week => "1W",
            Self::Month => "1M",
            Self::Quarter => "3M",
            Self::Year => "1Y",
            Self::All => "ALL",
        }
    }
}

/// Everything from a raw history to a finished pair of paths.
///
/// One function so the order cannot be got wrong: window first, then thin, then
/// plot. Thinning before windowing would spend the budget of 240 points on
/// history the window is about to throw away.
pub fn build(points: &[Point], now: i64, range: Range, view: &Viewport) -> Option<Plot> {
    let windowed = match range.seconds() {
        Some(seconds) => series::since(points, now, seconds),
        None => points.to_vec(),
    };
    let thinned = downsample(&windowed, MAX_POINTS);
    plot(&thinned, view, CORNER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ranges_are_in_order_and_all_is_unbounded() {
        let bounded: Vec<i64> = Range::ORDER.iter().filter_map(|r| r.seconds()).collect();
        assert!(
            bounded.windows(2).all(|pair| pair[0] < pair[1]),
            "{bounded:?}"
        );
        assert_eq!(Range::All.seconds(), None);
    }

    /// The window has to be applied before the thinning, or the budget of 240
    /// points is spent on history that is about to be discarded — leaving a
    /// week's worth of chart drawn from three surviving samples.
    #[test]
    fn a_narrow_range_keeps_its_detail() {
        // A year of daily readings.
        let points: Vec<Point> = (0..365)
            .map(|day| Point {
                t: day * 86_400,
                value: day * 100,
            })
            .collect();
        let now = 365 * 86_400;

        let week = build(&points, now, Range::Week, &Viewport::new(400.0, 200.0)).expect("plot");
        // Seven days of readings all survive: the window cut it down long
        // before the downsampler had anything to do.
        assert!(week.values.len() >= 7, "{}", week.values.len());
        assert!(
            week.high - week.low < 1_000,
            "a week's range covered a year's worth of movement",
        );
    }

    #[test]
    fn a_long_history_is_thinned_to_the_budget() {
        let points: Vec<Point> = (0..5_000)
            .map(|i| Point {
                t: i * 60,
                value: i,
            })
            .collect();
        let plot = build(
            &points,
            5_000 * 60,
            Range::All,
            &Viewport::new(800.0, 240.0),
        )
        .expect("plot");

        assert_eq!(plot.values.len(), MAX_POINTS);
    }
}
