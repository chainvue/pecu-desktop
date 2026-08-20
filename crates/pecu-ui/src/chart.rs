//! The balance chart: everything between a list of readings and two paths.
//!
//! # Why the geometry is computed here and not in the core
//!
//! Because the only thing that knows how wide the chart is, is the chart. A
//! plot made in the core would need the element's size reported back through
//! the actor on every resize, would arrive a frame late, and would put a
//! round trip between a pointer moving and a crosshair following it.
//!
//! What the core sends is *readings* — when, and how much — which is a
//! statement about the wallet. What happens here is arithmetic on a rectangle,
//! which is a statement about a window. The two belong on different sides.
//!
//! # Why the state is a thread-local
//!
//! Slint's event loop owns the main thread and every component lives on it, so
//! there is exactly one of these and it is only ever touched from one thread.
//! The alternative — threading an `Rc` through `bridge::apply`, which is a free
//! function taking `&AppWindow` — would put chart plumbing in the signature of
//! every event the wallet can emit.

use std::cell::RefCell;

use pecu_chart::{Plot, Point, Viewport};
use pecu_protocol::ChartVm;
use slint::{ComponentHandle, SharedString};

use crate::{AppWindow, ChartState};

/// What the chart is currently drawing, and what it was given to draw it from.
#[derive(Default)]
struct Chart {
    /// Every reading the core has sent, oldest first. The window and the
    /// downsampling are applied on the way to a path, never to this.
    series: Vec<Point>,
    /// The scan reached the start of the chain.
    complete: bool,
    ticker: String,
    /// The last plot, and the size it was made for. Kept so that the two path
    /// bindings — the stroke and the fill — do not each recompute the same
    /// geometry, and so the hover lookup has coordinates to search.
    plot: Option<Plot>,
    plotted_for: (f32, f32),
    /// Bumped whenever the readings or the range change, so a memoised plot for
    /// the right *size* but the wrong *data* is not reused.
    generation: u64,
    plotted_generation: u64,
    /// Set only by [`seed`], so a rendered reference image is the same every
    /// time it is produced. `None` in the wallet, where "now" is the clock.
    ///
    /// It pins the timezone as well as the second — see [`stamp`]. Pinning one
    /// without the other is not reproducible, which is what CI found on its
    /// first run.
    pinned_now: Option<i64>,
}

thread_local! {
    static CHART: RefCell<Chart> = RefCell::new(Chart::default());
}

/// Wire the callbacks the chart element drives.
///
/// Called once, when the window is built. Each body does arithmetic and writes
/// properties — no I/O, nothing that can block, and nothing that talks to the
/// core.
pub fn install(ui: &AppWindow) {
    let state = ui.global::<ChartState>();

    // Reset. In the wallet this runs once and there is nothing to clear; in the
    // snapshot renderer it runs once per window inside a single process, and
    // without this a screen that draws no chart would inherit the readings of
    // whichever one was rendered before it.
    CHART.with_borrow_mut(|chart| *chart = Chart::default());
    clear_cursor(ui);

    // The two paths are BINDINGS on the element's own width and height, not
    // properties pushed from a resize handler.
    //
    // A `pure callback` used in a binding re-evaluates when its arguments
    // change, so the geometry follows the element exactly — including the very
    // first layout, which is the case a `changed width` handler cannot cover:
    // it fires only once a size has been assigned, and until then there is
    // nothing to plot against. (It also fires only under a running event loop,
    // which the offscreen renderer does not have.)
    //
    // Both callbacks go through one memo, so asking for the stroke and the fill
    // at the same size computes the geometry once.
    state.on_line_at(|width, height| {
        plot_at(width, height).map_or_else(SharedString::new, |plot| plot.line.as_str().into())
    });

    state.on_area_at(|width, height| {
        plot_at(width, height).map_or_else(SharedString::new, |plot| {
            // A gradient fading downwards from a horizontal line reads as a
            // chart that failed to load, so a balance that never moved gets no
            // fill at all.
            if plot.flat {
                SharedString::new()
            } else {
                plot.area.as_str().into()
            }
        })
    });

    // Where the line ends: the "now" dot. Same memo, so this costs a lookup.
    state.on_end_x_at(|width, height| {
        plot_at(width, height)
            .and_then(|plot| plot.xs.last().copied())
            .unwrap_or(0.0)
    });
    state.on_end_y_at(|width, height| {
        plot_at(width, height)
            .and_then(|plot| plot.ys.last().copied())
            .unwrap_or(0.0)
    });

    {
        let weak = ui.as_weak();
        state.on_hovered(move |x| {
            if let Some(ui) = weak.upgrade() {
                cursor(&ui, x);
            }
        });
    }

    {
        let weak = ui.as_weak();
        state.on_left(move || {
            if let Some(ui) = weak.upgrade() {
                clear_cursor(&ui);
            }
        });
    }
}

/// Put the chart into a known state, for fixtures and snapshots.
///
/// `now` is pinned rather than read from the clock. Without that, every
/// rendered reference image would differ from the last one by however long
/// passed between them — the window is measured backwards from the present, so
/// a moving present moves every point.
///
/// Not `#[cfg(test)]`: `examples/render_shots.rs` and `tests/visual.rs` are
/// both outside this crate's test build, and they are the two callers.
pub fn seed(ui: &AppWindow, points: &[Point], complete: bool, ticker: &str, now: i64) {
    CHART.with_borrow_mut(|chart| {
        chart.series = points.to_vec();
        chart.complete = complete;
        chart.ticker = ticker.to_string();
        chart.pinned_now = Some(now);
        chart.generation = chart.generation.wrapping_add(1);
    });
    clear_cursor(ui);
    refresh(ui);
}

/// Take a new set of readings from the core.
pub fn set_series(ui: &AppWindow, vm: &ChartVm) {
    CHART.with_borrow_mut(|chart| {
        chart.series = vm
            .points
            .iter()
            .map(|point| Point {
                t: point.t,
                value: point.sats,
            })
            .collect();
        chart.complete = vm.complete;
        chart.ticker.clone_from(&vm.ticker);
        chart.generation = chart.generation.wrapping_add(1);
    });

    clear_cursor(ui);
    refresh(ui);
}

/// Everything about the chart that does **not** depend on how big it is.
///
/// Which readings fall inside the window, what moved across it, how many there
/// are and which range buttons can be filled — none of that is a question about
/// pixels, so none of it waits for a layout. That matters: the empty state has
/// to be right before the element has ever been measured, and a chart that said
/// "no history" for one frame on every window resize would be worse than one
/// that never drew at all.
///
/// The paths are the only size-dependent part, and they are bindings — see
/// [`install`].
fn refresh(ui: &AppWindow) {
    let state = ui.global::<ChartState>();

    let summary = CHART.with_borrow(|chart| {
        if chart.series.len() < 2 {
            return None;
        }
        // The same thinning the paths will use, so the figure in the header is
        // computed from exactly the points that get drawn.
        let thinned = pecu_chart::downsample(&chart.series, pecu_chart::MAX_POINTS);
        let values: Vec<i64> = thinned.iter().map(|point| point.value).collect();
        let from = chart.series.first().map_or(0, |first| first.t);
        Some((values, from))
    });

    let Some((values, from)) = summary else {
        state.set_has_data(false);
        state.set_change(SharedString::new());
        state.set_caption(SharedString::new());
        state.set_span_from(SharedString::new());
        state.set_span_range(SharedString::new());
        return;
    };

    state.set_has_data(true);

    let (change, tone) = change_over(&values);
    state.set_change(change.into());
    state.set_change_tone(tone.into());

    // Where the axis begins. There are no gridlines and no tick labels, so
    // without this the chart cannot say what period it covers at all — a
    // question the range buttons used to answer by implication and which
    // nothing answered once they were gone.
    state.set_span_from(axis_label(from, now_of(&state)).into());
    state.set_span_range(span_range(&values).into());

    CHART.with_borrow(|chart| {
        state.set_ticker(chart.ticker.as_str().into());
        state.set_caption(caption(chart, values.len()));
    });
}

/// The plot for a given size, computed at most once per (size, data) pair.
///
/// Called from two bindings — the stroke and the fill — which always ask for
/// the same size, so without the memo every layout would do the work twice.
fn plot_at(width: f32, height: f32) -> Option<Plot> {
    CHART.with_borrow_mut(|chart| {
        if width < 1.0 || height < 1.0 || chart.series.len() < 2 {
            chart.plot = None;
            return None;
        }

        let fresh = chart.plot.is_some()
            && chart.plotted_generation == chart.generation
            && (chart.plotted_for.0 - width).abs() < 0.5
            && (chart.plotted_for.1 - height).abs() < 0.5;

        if !fresh {
            let view = Viewport::new(width, height);
            let now = chart.pinned_now.unwrap_or_else(now);
            chart.plot = pecu_chart::build(&chart.series, now, &view);
            chart.plotted_for = (width, height);
            chart.plotted_generation = chart.generation;
        }

        chart.plot.clone()
    })
}

/// What moved across the window on screen.
///
/// Computed on `i64` satoshis and reported as a percentage in basis points, so
/// no float is involved in a figure anybody reads. A starting balance of zero
/// has no percentage — dividing by it would produce either infinity or a
/// number somebody would believe.
fn change_over(values: &[i64]) -> (String, &'static str) {
    let (Some(first), Some(last)) = (values.first(), values.last()) else {
        return (String::new(), "neutral");
    };

    let delta = last.saturating_sub(*first);
    if delta == 0 {
        return ("no change".to_string(), "neutral");
    }

    let sign = if delta > 0 { "+" } else { "−" };
    let tone = if delta > 0 { "positive" } else { "negative" };
    let amount = pecu_protocol::coins(delta);

    if *first == 0 {
        return (format!("{sign}{amount}"), tone);
    }

    // Basis points, so one integer division carries two decimal places.
    let points = delta
        .saturating_mul(10_000)
        .checked_div(first.abs())
        .unwrap_or(0)
        .abs();
    (
        format!(
            "{sign}{amount}  ·  {sign}{}.{:02}%",
            points / 100,
            points % 100
        ),
        tone,
    )
}

/// The sentence under the chart.
///
/// It says what the picture is of, which matters more here than usual: a
/// balance chart over a window the wallet has not fully scanned is true about
/// the window and silent about everything before it, and the difference is
/// invisible in the drawing.
fn caption(chart: &Chart, readings: usize) -> SharedString {
    let noun = if readings == 1 { "reading" } else { "readings" };

    if chart.complete {
        format!("{readings} {noun} · the whole history of this wallet").into()
    } else {
        format!("{readings} {noun} · as far back as Pecu has looked so far").into()
    }
}

/// Put the crosshair where the pointer is, on the step it is standing on.
///
/// Follows the pointer in x rather than snapping to a reading. Between two
/// readings a balance is not estimated — it was exactly that from one
/// transaction until the next — so the dot riding the step reports a figure
/// that is as exact as the reading itself, and the cursor stops jumping to the
/// last transaction however far right somebody points.
fn cursor(ui: &AppWindow, x: f32) {
    let state = ui.global::<ChartState>();

    let found = CHART.with_borrow(|chart| {
        let plot = chart.plot.as_ref()?;
        if plot.xs.is_empty() {
            return None;
        }

        // Clamped to the plot area: a dot outside it points at a time the axis
        // does not cover.
        let x = x.clamp(plot.pad_left, plot.pad_left + plot.plot_width);
        let index = pecu_chart::step_at(&plot.xs, x);

        Some((
            index,
            x,
            *plot.ys.get(index)?,
            *plot.values.get(index)?,
            pecu_chart::time_at(plot, x),
        ))
    });

    let Some((index, x, y, value, time)) = found else {
        return;
    };

    let Ok(index) = i32::try_from(index) else {
        return;
    };
    state.set_hover(index);
    state.set_hover_x(x);
    state.set_hover_y(y);
    // Formatted through the same function the core uses — see
    // `pecu_protocol::format`.
    state.set_hover_amount(pecu_protocol::coins(value).into());
    state.set_hover_when(when(time).into());
}

fn clear_cursor(ui: &AppWindow) {
    let state = ui.global::<ChartState>();
    state.set_hover(-1);
    state.set_hover_amount(SharedString::new());
    state.set_hover_when(SharedString::new());
}

/// What the top and the bottom of the plot are worth.
///
/// # Why a chart without this is misleading rather than merely bare
///
/// The line is scaled to the values it contains, not to zero — which is the
/// right choice for a balance that never goes near zero, and it means the
/// *shape* carries no magnitude at all. A wallet that moved by one coin and one
/// that moved by ten thousand draw the identical staircase. Somebody reading
/// the picture without a number is reading a picture of nothing.
///
/// Empty when the balance never moved: "12 482.4200 0000 – 12 482.4200 0000" is
/// two copies of a figure already at the top of the screen, and the flat line
/// above it is not ambiguous about anything.
fn span_range(values: &[i64]) -> String {
    let (Some(low), Some(high)) = (values.iter().min(), values.iter().max()) else {
        return String::new();
    };
    if low == high {
        return String::new();
    }
    format!(
        "{} – {}",
        pecu_protocol::coins(*low),
        pecu_protocol::coins(*high),
    )
}

/// Where the axis begins, at whatever precision the span justifies.
///
/// A minute is the right precision for a cursor sitting on one transaction and
/// noise on an axis covering three months — "4 Nov, 03:40 → now" invites
/// somebody to read a significance into 03:40 that the label does not have.
/// Under two days the time is the interesting part; past a year the year is.
fn axis_label(from: i64, now: i64) -> String {
    const DAY: i64 = 86_400;
    let span = now.saturating_sub(from);

    let format = if span < 2 * DAY {
        "%-d %b, %H:%M"
    } else if span < 365 * DAY {
        "%-d %b"
    } else {
        "%-d %b %Y"
    };

    stamp(from, format)
}

/// Render a timestamp in the timezone its reader is sitting in.
///
/// A block timestamp is UTC seconds; which day and hour that is, is a question
/// about where the reader is. In the wallet the reader is somewhere, so it is
/// `Local`.
///
/// A reference image has no reader and no somewhere. `pinned_now` already
/// means "this rendering has to come out the same every time it is produced",
/// and the zone is the other half of making that true: the same pinned second
/// is 02:40 in UTC and 03:40 in Berlin, and a label carrying `%H:%M` is then a
/// different picture on a machine that sits somewhere else. Two chart screens
/// disagreed that way between this project's desk and a runner, which is how
/// it was noticed at all — a test that only passes in one timezone is a test
/// nobody outside that timezone can trust.
fn stamp(seconds: i64, format: &str) -> String {
    use chrono::{DateTime, Local, TimeZone, Utc};

    // A generic function rather than a closure: the two branches hand it
    // `DateTime<Utc>` and `DateTime<Local>`, and a closure would be fixed to
    // whichever it saw first.
    fn render<Tz: TimeZone>(at: Option<DateTime<Tz>>, format: &str) -> String
    where
        Tz::Offset: std::fmt::Display,
    {
        at.map_or_else(
            || "unknown".to_string(),
            |at| at.format(format).to_string(),
        )
    }

    if CHART.with_borrow(|chart| chart.pinned_now.is_some()) {
        render(Utc.timestamp_opt(seconds, 0).single(), format)
    } else {
        render(Local.timestamp_opt(seconds, 0).single(), format)
    }
}

/// The clock the chart is measuring against — pinned in a fixture, real
/// otherwise, so a reference image does not move with the calendar.
fn now_of(_state: &ChartState<'_>) -> i64 {
    CHART.with_borrow(|chart| chart.pinned_now.unwrap_or_else(now))
}

/// `12 Aug, 14:32`, in the zone [`stamp`] decides on.
fn when(seconds: i64) -> String {
    stamp(seconds, "%-d %b, %H:%M")
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chart(complete: bool) -> Chart {
        Chart {
            complete,
            ..Chart::default()
        }
    }

    /// A pinned rendering is the same picture wherever it is drawn.
    ///
    /// It was not. The clock was pinned and the timezone was not, so a fixed
    /// second came out as 03:40 at this project's desk and 02:40 on a runner,
    /// and two chart references were only correct in one timezone — which the
    /// first CI run found by being the first machine to sit somewhere else.
    ///
    /// 1 770 000 000 is 2 February 2026, 02:40 UTC. Asserting the UTC spelling
    /// is the whole point: this test passes in Berlin, in Tokyo and on a
    /// runner, or it is not testing what it claims to.
    #[test]
    fn a_pinned_chart_is_labelled_in_utc() {
        CHART.with_borrow_mut(|chart| chart.pinned_now = Some(1_770_000_000));
        assert_eq!(stamp(1_770_000_000, "%-d %b, %H:%M"), "2 Feb, 02:40");
        assert_eq!(stamp(1_770_000_000, "%-d %b %Y"), "2 Feb 2026");
        CHART.with_borrow_mut(|chart| chart.pinned_now = None);
    }

    #[test]
    fn the_change_is_reported_with_a_sign_and_a_percentage() {
        let values = vec![100_000_000, 150_000_000];
        let (text, tone) = change_over(&values);
        assert_eq!(tone, "positive");
        assert!(text.starts_with("+0.5000 0000"), "{text}");
        assert!(text.contains("+50.00%"), "{text}");

        let values = vec![200_000_000, 150_000_000];
        let (text, tone) = change_over(&values);
        assert_eq!(tone, "negative");
        assert!(text.contains("−25.00%"), "{text}");
    }

    /// Dividing by a starting balance of zero produces either infinity or a
    /// number somebody would believe. Neither belongs on screen.
    #[test]
    fn a_balance_that_started_at_nothing_has_no_percentage() {
        let values = vec![0, 150_000_000];
        let (text, tone) = change_over(&values);
        assert_eq!(tone, "positive");
        assert!(!text.contains('%'), "{text}");
        assert!(!text.contains("inf"), "{text}");
    }

    #[test]
    fn a_balance_that_did_not_move_says_so_rather_than_showing_a_zero() {
        let values = vec![7, 7];
        assert_eq!(change_over(&values), ("no change".to_string(), "neutral"));
    }

    /// The caption has to distinguish "this is everything" from "this is what
    /// we have looked at", because the drawing cannot.
    #[test]
    fn the_caption_says_whether_the_history_is_complete() {
        let complete = caption(&chart(true), 3);
        assert!(complete.contains("whole history"), "{complete}");

        let partial = caption(&chart(false), 3);
        assert!(partial.contains("as far back"), "{partial}");

        // Singular when there is one of them. A "1 readings" is the sort of
        // thing nobody fixes because nobody writes it down.
        assert!(caption(&chart(true), 1).contains("1 reading ·"));
    }
}
