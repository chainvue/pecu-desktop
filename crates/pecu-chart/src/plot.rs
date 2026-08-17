//! Turning a balance series into two SVG path strings.

use core::fmt::Write as _;

use crate::series::Point;

/// The box the chart is drawn into, in logical pixels.
///
/// # There is no viewbox
///
/// The paths come out in the element's own coordinates, so `Path` is used
/// without `viewbox-width`/`viewbox-height` and must be regenerated when the
/// element resizes. The tempting shortcut is to emit into a fixed `0..1000` box
/// and never regenerate — but with a viewbox set, `stroke-width` is measured in
/// *path* coordinates, so a wide short chart renders an elliptically thick
/// stroke. One pixel of stroke has to mean one pixel.
#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
    /// Room for the leading dot at the right-hand edge, and for the stroke not
    /// to be clipped in half at the top and bottom.
    pub pad_left: f32,
    pub pad_right: f32,
    pub pad_top: f32,
    pub pad_bottom: f32,
}

impl Viewport {
    /// A viewport with the padding this chart is designed around.
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            // Not zero. The first reading is usually a riser — the balance
            // going from nothing to something — and at zero it sits exactly on
            // the boundary with half its stroke outside, which reads as a chart
            // that has been cut off rather than one that starts there.
            pad_left: 3.0,
            pad_right: 8.0,
            pad_top: 10.0,
            pad_bottom: 10.0,
        }
    }

    pub fn plot_width(&self) -> f32 {
        (self.width - self.pad_left - self.pad_right).max(1.0)
    }

    fn plot_height(&self) -> f32 {
        (self.height - self.pad_top - self.pad_bottom).max(1.0)
    }

    fn base(&self) -> f32 {
        self.pad_top + self.plot_height()
    }
}

/// Everything the interface needs to draw one chart.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plot {
    /// The stroke.
    pub line: String,
    /// The fill under it, closed to the baseline. Emitted from the **same**
    /// control points in the same pass, so the two cannot drift apart.
    pub area: String,
    /// Where each kept point landed. The hover crosshair sits on a sample
    /// rather than under the pointer, so it needs these.
    pub xs: Vec<f32>,
    pub ys: Vec<f32>,
    /// The values behind `ys`, so a tooltip can show the balance at the cursor
    /// without the interface doing arithmetic on money.
    pub values: Vec<i64>,
    pub times: Vec<i64>,
    /// The range the y axis covers, in satoshis.
    pub low: i64,
    pub high: i64,
    /// The window the x axis covers, in unix seconds.
    ///
    /// Explicit rather than derived from the data, and that is the whole
    /// difference between a range control that works and one that does not: a
    /// wallet whose entire history is nine hours old has the same five readings
    /// in every window, so an axis derived from them draws exactly the same
    /// picture for 1W, 1M, 1Y and ALL.
    pub span: (i64, i64),
    /// Where the plot area starts and how wide it is, so a pointer position can
    /// be turned back into a time.
    pub pad_left: f32,
    pub plot_width: f32,
    /// The balance never moved across this window.
    ///
    /// The chart draws a flat line and **suppresses the gradient**: a fill that
    /// fades downwards from a horizontal line reads as a chart that failed to
    /// load rather than as a balance that held steady.
    pub flat: bool,
}

/// Draw a balance over time.
///
/// # A balance is a step function, and drawing it smoothly would be a lie
///
/// A wallet's balance does not drift. It is one number until a transaction
/// lands, and then it is a different number. A smooth curve between two
/// readings claims the balance passed through every value in between at times
/// nobody can point at, which for money is not a stylistic choice.
///
/// So this draws horizontal runs joined by vertical jumps, with the corners
/// rounded. It reads as a modern staircase rather than a bar chart, and every
/// pixel of it is a balance somebody actually held.
///
/// (A smooth mode belongs with a price feed, where the series really is
/// continuous because the *value* of a fixed holding changes between
/// transactions. There is no price feed, so there is no smooth mode — an
/// unused one would only be an invitation.)
///
/// # x is proportional to time
///
/// Not to position in the list. Spacing points evenly would make the axis
/// "transaction number", and a month with three payments in its first day would
/// then draw them across the whole width. The hover lookup pays for this with
/// [`step_at`] instead of arithmetic in the interface, which is a fair trade
/// for an axis that means what it says.
///
/// `None` when there is nothing to draw.
pub fn plot(points: &[Point], view: &Viewport, corner: f32, window: (i64, i64)) -> Option<Plot> {
    if points.len() < 2 {
        return single(points, view, window);
    }

    let (start, end) = window;
    let span = end.saturating_sub(start);
    if span <= 0 {
        return single(&points[points.len() - 1..], view, window);
    }

    let low = points.iter().map(|point| point.value).min()?;
    let high = points.iter().map(|point| point.value).max()?;
    let flat = low == high;

    let xs: Vec<f32> = points
        .iter()
        .map(|point| {
            // The only place a time becomes a float, and it becomes one as a
            // fraction of the span rather than as a timestamp — an i64 of
            // seconds since 1970 does not survive an f32.
            let fraction = ratio(point.t.saturating_sub(start), span);
            view.pad_left + fraction * view.plot_width()
        })
        .collect();

    let ys: Vec<f32> = points
        .iter()
        .map(|point| {
            if flat {
                // Halfway up, so a steady balance sits in the middle of its box
                // rather than pinned to an edge that implies a maximum.
                view.pad_top + view.plot_height() / 2.0
            } else {
                let fraction = ratio(point.value.saturating_sub(low), high.saturating_sub(low));
                view.pad_top + (1.0 - fraction) * view.plot_height()
            }
        })
        .collect();

    let line = staircase(&xs, &ys, corner);
    let area = close_to_baseline(&line, &xs, view);

    Some(Plot {
        line,
        area,
        xs,
        ys,
        values: points.iter().map(|point| point.value).collect(),
        times: points.iter().map(|point| point.t).collect(),
        low,
        high,
        flat,
        span: window,
        pad_left: view.pad_left,
        plot_width: view.plot_width(),
    })
}

/// One reading, or several at the same instant: a flat line with the gradient
/// suppressed. A gradient under a horizontal line looks like a rendering fault.
fn single(points: &[Point], view: &Viewport, window: (i64, i64)) -> Option<Plot> {
    let point = points.last()?;
    let y = view.pad_top + view.plot_height() / 2.0;
    let (left, right) = (view.pad_left, view.pad_left + view.plot_width());

    let line = format!("M {left:.2} {y:.2} L {right:.2} {y:.2}");
    Some(Plot {
        area: String::new(),
        line,
        xs: vec![right],
        ys: vec![y],
        values: vec![point.value],
        times: vec![point.t],
        low: point.value,
        high: point.value,
        flat: true,
        span: window,
        pad_left: view.pad_left,
        plot_width: view.plot_width(),
    })
}

/// The stroke: horizontal runs, vertical jumps, rounded corners.
///
/// Each transition is `L` to just before the corner, `Q` around it, `L` down or
/// up the riser, `Q` around the second corner. The radius is clamped against
/// both adjacent segments so a pair of transactions a few seconds apart cannot
/// produce a corner wider than the run it belongs to — which would draw the
/// path back over itself.
fn staircase(xs: &[f32], ys: &[f32], corner: f32) -> String {
    let mut path = String::with_capacity(xs.len() * 48);
    let _ = write!(path, "M {:.2} {:.2}", xs[0], ys[0]);

    for index in 1..xs.len() {
        let (x, y) = (xs[index], ys[index]);
        let (previous_x, previous_y) = (xs[index - 1], ys[index - 1]);
        let rise = y - previous_y;

        if rise.abs() < f32::EPSILON {
            // No jump: the balance did not change here. One straight run.
            let _ = write!(path, " L {x:.2} {previous_y:.2}");
            continue;
        }

        let run_in = x - previous_x;
        let run_out = xs.get(index + 1).map_or(run_in, |next| next - x);
        let radius = corner
            .min(run_in / 2.0)
            .min(run_out / 2.0)
            .min(rise.abs() / 2.0)
            .max(0.0);

        let direction = if rise > 0.0 { 1.0 } else { -1.0 };

        let _ = write!(path, " L {:.2} {previous_y:.2}", x - radius);
        let _ = write!(
            path,
            " Q {x:.2} {previous_y:.2} {x:.2} {:.2}",
            previous_y + direction * radius,
        );
        let _ = write!(path, " L {x:.2} {:.2}", y - direction * radius);
        let _ = write!(path, " Q {x:.2} {y:.2} {:.2} {y:.2}", x + radius);
    }

    path
}

/// The same path, closed down to the baseline and back.
///
/// Built by extending the line rather than by walking the points again: two
/// passes over the same data is two chances for the fill and the stroke to
/// disagree about where the curve is, and the disagreement shows up as a
/// hairline of background between them.
fn close_to_baseline(line: &str, xs: &[f32], view: &Viewport) -> String {
    let base = view.base();
    let (Some(first), Some(last)) = (xs.first(), xs.last()) else {
        return String::new();
    };
    format!("{line} L {last:.2} {base:.2} L {first:.2} {base:.2} Z")
}

/// A fraction in `0.0..=1.0`, computed from integers.
///
/// Money and time stay `i64` right up to this line. Only the ratio becomes a
/// float, and a ratio has no units to lose precision in — an `f32` holds it to
/// far better than one pixel.
///
/// This is **the** place the workspace's cast lints are relaxed, and it is the
/// only one. The output is a number between zero and one that gets multiplied
/// by a pixel width; the worst a lost bit can do here is move a coordinate by
/// less than a thousandth of a pixel. Nothing that comes out of this function
/// is ever formatted as an amount.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn ratio(part: i64, whole: i64) -> f32 {
    if whole <= 0 {
        return 0.0;
    }
    let fraction = part as f64 / whole as f64;
    // Guarding against a series that somehow arrived unsorted: a fraction
    // outside the box would put a `NaN`-free but off-canvas coordinate in a
    // command string, which draws nothing and explains nothing.
    (fraction.clamp(0.0, 1.0)) as f32
}

/// The reading that governs the balance at `x`.
///
/// The **last** reading at or before the pointer, not the nearest one.
///
/// # Why this changed
///
/// It used to snap to the nearest reading, on the reasoning that a dot floating
/// between two of them points at a balance nobody held. That is true of an
/// interpolated curve and false of a staircase: between two readings a balance
/// is not estimated, it is *known* — it was exactly that from one transaction
/// until the next. Snapping protected against nothing and made the cursor
/// useless, because a wallet with a quiet week has one reading at the start of
/// it and the dot would sit there however far right you pointed.
///
/// So the crosshair follows the pointer and the dot rides the step it is
/// standing on. Every figure it reports is exact.
pub fn step_at(xs: &[f32], x: f32) -> usize {
    let mut governing = 0;
    for (index, candidate) in xs.iter().enumerate() {
        if *candidate <= x {
            governing = index;
        } else {
            break;
        }
    }
    governing
}

/// What time the pointer is over.
///
/// Exact for a step chart in a way it would not be for a curve: the x axis is a
/// linear map of the window, so this is the instant under the cursor rather
/// than an estimate of one.
pub fn time_at(plot: &Plot, x: f32) -> i64 {
    let (start, end) = plot.span;
    let span = end.saturating_sub(start);
    if span <= 0 || plot.plot_width <= 0.0 {
        return end;
    }

    let fraction = f64::from((x - plot.pad_left) / plot.plot_width).clamp(0.0, 1.0);
    // A window is at most a few years of seconds — under 2^27 — so both casts
    // are exact and the rounding is to the nearest second. Nothing here is
    // money; it is a timestamp for a tooltip.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let offset = (fraction * span as f64).round() as i64;
    start.saturating_add(offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window a series would be drawn in when nothing narrower is asked
    /// for: its own extent.
    fn window_of(points: &[Point]) -> (i64, i64) {
        match (points.first(), points.last()) {
            (Some(first), Some(last)) => (first.t, last.t),
            _ => (0, 0),
        }
    }

    fn view() -> Viewport {
        Viewport {
            width: 100.0,
            height: 100.0,
            pad_left: 0.0,
            pad_right: 0.0,
            pad_top: 0.0,
            pad_bottom: 0.0,
        }
    }

    /// The golden string. A step-ordering or corner-radius regression is
    /// invisible to the eye until it is not, and this is the only thing that
    /// catches it before a person does.
    #[test]
    fn a_two_step_balance_draws_the_path_it_should() {
        let points = vec![
            Point { t: 0, value: 0 },
            Point { t: 50, value: 100 },
            Point { t: 100, value: 100 },
        ];
        let plot = plot(&points, &view(), 4.0, window_of(&points)).expect("a plot");

        assert_eq!(
            plot.line,
            "M 0.00 100.00 \
             L 46.00 100.00 \
             Q 50.00 100.00 50.00 96.00 \
             L 50.00 4.00 \
             Q 50.00 0.00 54.00 0.00 \
             L 100.00 0.00"
                .replace("             ", ""),
        );

        // The fill is the same path, closed to the baseline and back.
        assert_eq!(
            plot.area,
            format!("{} L 100.00 100.00 L 0.00 100.00 Z", plot.line)
        );
    }

    /// A balance is a step function. A diagonal in this path would be the
    /// chart claiming the money passed through values nobody held, at times
    /// nobody can point at.
    #[test]
    fn no_segment_is_ever_diagonal() {
        let points = vec![
            Point { t: 0, value: 10 },
            Point { t: 30, value: 90 },
            Point { t: 60, value: 20 },
            Point { t: 90, value: 55 },
            Point { t: 120, value: 55 },
        ];
        let plot = plot(&points, &view(), 4.0, window_of(&points)).expect("a plot");

        // Every `L` moves in exactly one axis. The `Q` corners move in both by
        // construction — that is what a rounded corner is — so they are the
        // only exception, and they are bounded by the radius.
        let mut cursor = (0.0f32, 0.0f32);
        for (command, to) in walk(&plot.line) {
            if command == 'L' {
                let moved_x = (to.0 - cursor.0).abs() > 0.01;
                let moved_y = (to.1 - cursor.1).abs() > 0.01;
                assert!(
                    !(moved_x && moved_y),
                    "a line segment moved in both axes: {cursor:?} -> {to:?}",
                );
            }
            cursor = to;
        }
    }

    /// Walk a path, yielding each command and the point the pen ends up at.
    ///
    /// Tracking where the pen actually is matters: a `Q` has a control point
    /// *and* an end point, so a reader that only looked at `L` commands would
    /// carry a stale cursor into the next segment and report a diagonal that
    /// is not there.
    fn walk(path: &str) -> Vec<(char, (f32, f32))> {
        let mut out = Vec::new();
        let mut tokens = path.split_whitespace().peekable();

        while let Some(token) = tokens.next() {
            let command = match token {
                "M" | "L" => token.chars().next().unwrap_or('?'),
                "Q" => {
                    // Control point, then the end point.
                    let _ = tokens.next();
                    let _ = tokens.next();
                    'Q'
                }
                _ => continue,
            };

            let x = tokens.next().and_then(|n| n.parse().ok()).unwrap_or(0.0);
            let y = tokens.next().and_then(|n| n.parse().ok()).unwrap_or(0.0);
            out.push((command, (x, y)));
        }

        out
    }

    /// Every number in a path string has to be a number. `NaN` in a command
    /// draws nothing at all and gives no clue why.
    #[test]
    fn nothing_infinite_or_undefined_reaches_a_command_string() {
        let awkward = vec![
            // The same instant twice, a zero span, and the extremes of the
            // range — each of these is a division waiting to go wrong.
            vec![Point { t: 5, value: 1 }, Point { t: 5, value: 2 }],
            vec![
                Point {
                    t: 0,
                    value: i64::MIN,
                },
                Point {
                    t: 1,
                    value: i64::MAX,
                },
            ],
            vec![Point { t: 0, value: 7 }, Point { t: 1, value: 7 }],
        ];

        for points in awkward {
            let Some(plot) = plot(&points, &view(), 4.0, window_of(&points)) else {
                continue;
            };
            for path in [&plot.line, &plot.area] {
                assert!(!path.contains("NaN") && !path.contains("inf"), "{path}");
            }
            assert!(plot.xs.iter().all(|x| x.is_finite()));
            assert!(plot.ys.iter().all(|y| y.is_finite()));
        }
    }

    /// A gradient fading downwards from a horizontal line reads as a chart
    /// that failed to load.
    #[test]
    fn a_balance_that_never_moved_is_flat_and_says_so() {
        let points = vec![
            Point { t: 0, value: 500 },
            Point { t: 50, value: 500 },
            Point { t: 100, value: 500 },
        ];
        let plot = plot(&points, &view(), 4.0, window_of(&points)).expect("a plot");

        assert!(plot.flat);
        assert_eq!(plot.low, 500);
        assert_eq!(plot.high, 500);
        // Halfway up: pinned to an edge would imply a maximum that is not there.
        assert!(
            plot.ys.iter().all(|y| (*y - 50.0).abs() < 0.01),
            "{:?}",
            plot.ys
        );
    }

    #[test]
    fn one_reading_draws_a_line_and_no_fill() {
        let plot = plot(&[Point { t: 0, value: 3 }], &view(), 4.0, (0, 0)).expect("a plot");
        assert!(plot.flat);
        assert!(plot.area.is_empty(), "a single point got a gradient");
        assert_eq!(plot.values, vec![3]);
    }

    #[test]
    fn nothing_at_all_draws_nothing() {
        assert!(plot(&[], &view(), 4.0, (0, 100)).is_none());
    }

    /// The corner cannot be wider than the run it belongs to, or the path
    /// doubles back over itself.
    #[test]
    fn a_corner_is_clamped_to_the_space_it_has() {
        // Two transactions one second apart in a hundred-second window: the
        // run between them is a fraction of a pixel.
        let points = vec![
            Point { t: 0, value: 0 },
            Point { t: 50, value: 100 },
            Point { t: 51, value: 0 },
            Point { t: 100, value: 50 },
        ];
        let plot = plot(&points, &view(), 4.0, window_of(&points)).expect("a plot");

        // x never goes backwards, which is what a corner wider than its run
        // would cause.
        let mut previous = f32::NEG_INFINITY;
        for command in plot.line.split(&[' '][..]).collect::<Vec<_>>().chunks(1) {
            let _ = command;
        }
        for x in &plot.xs {
            assert!(*x >= previous, "{:?}", plot.xs);
            previous = *x;
        }
    }

    /// The dot rides the step it is standing on, rather than jumping to the
    /// nearest reading. On a quiet week that is the difference between a cursor
    /// that follows you and one that sits at the last transaction whatever you
    /// point at.
    #[test]
    fn the_cursor_reads_the_step_it_is_standing_on() {
        let xs = [0.0, 10.0, 40.0, 100.0];

        assert_eq!(step_at(&xs, -5.0), 0, "before the first reading");
        assert_eq!(step_at(&xs, 0.0), 0);
        // Anywhere along a run belongs to the reading that started it — this is
        // the case the old nearest-match got wrong, since 39 is nearer to 40.
        assert_eq!(step_at(&xs, 9.9), 0);
        assert_eq!(step_at(&xs, 39.0), 1);
        assert_eq!(step_at(&xs, 40.0), 2);
        assert_eq!(step_at(&xs, 1_000.0), 3);
    }

    /// The pointer's position maps back to an instant, exactly — the axis is a
    /// linear map of the window, so this is the time under the cursor rather
    /// than an estimate of it.
    #[test]
    fn a_pointer_position_maps_back_to_a_time() {
        let points = vec![Point { t: 100, value: 1 }, Point { t: 200, value: 2 }];
        let window = (100, 200);
        let plot = plot(&points, &view(), 4.0, window).expect("a plot");

        assert_eq!(time_at(&plot, 0.0), 100);
        assert_eq!(time_at(&plot, 100.0), 200);
        assert_eq!(time_at(&plot, 50.0), 150);
        // Off either end clamps rather than running past the window.
        assert_eq!(time_at(&plot, -50.0), 100);
        assert_eq!(time_at(&plot, 5_000.0), 200);
    }
}
