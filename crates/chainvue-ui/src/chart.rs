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

use chainvue_chart::{Plot, Point, Range, Viewport};
use chainvue_protocol::ChartVm;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::{AppWindow, ChartState};

/// What the chart is currently drawing, and what it was given to draw it from.
#[derive(Default)]
struct Chart {
    /// Every reading the core has sent, oldest first. The window and the
    /// downsampling are applied on the way to a path, never to this.
    series: Vec<Point>,
    /// How far back the wallet has actually scanned, in seconds.
    covers: i64,
    /// The scan reached the start of the chain.
    complete: bool,
    ticker: String,
    range: Range,
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
    pinned_now: Option<i64>,
}

thread_local! {
    static CHART: RefCell<Chart> = RefCell::new(Chart {
        // Everything the wallet has, which is the only range that is honest
        // before anything has been scanned.
        range: Range::All,
        ..Chart::default()
    });
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

    state.set_range_labels(ModelRc::from(std::rc::Rc::new(VecModel::from(
        Range::ORDER
            .iter()
            .map(|range| SharedString::from(range.label()))
            .collect::<Vec<_>>(),
    ))));

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
        state.on_set_range(move |index| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let Ok(index) = usize::try_from(index) else {
                return;
            };
            let Some(range) = Range::ORDER.get(index).copied() else {
                return;
            };
            CHART.with_borrow_mut(|chart| {
                chart.range = range;
                chart.generation = chart.generation.wrapping_add(1);
            });
            // The cursor was pointing at a sample that may not exist in the
            // new window. Dropping it is the only honest option.
            clear_cursor(&ui);
            refresh(&ui);
        });
    }

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
pub fn seed(ui: &AppWindow, points: &[Point], covers: i64, complete: bool, ticker: &str, now: i64) {
    CHART.with_borrow_mut(|chart| {
        chart.series = points.to_vec();
        chart.covers = covers;
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
        chart.covers = vm.covers_seconds;
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
        // The same window and the same thinning the paths will use, so the
        // figure in the header is computed from exactly the points that get
        // drawn rather than from a slightly different set.
        let now = chart.pinned_now.unwrap_or_else(now);
        let windowed = match chart.range.seconds() {
            Some(seconds) => chainvue_chart::since(&chart.series, now, seconds),
            None => chart.series.clone(),
        };
        let thinned = chainvue_chart::downsample(&windowed, chainvue_chart::MAX_POINTS);
        Some(
            thinned
                .iter()
                .map(|point| point.value)
                .collect::<Vec<i64>>(),
        )
    });

    let Some(values) = summary else {
        state.set_has_data(false);
        state.set_change(SharedString::new());
        state.set_caption(SharedString::new());
        return;
    };

    state.set_has_data(true);

    let (change, tone) = change_over(&values);
    state.set_change(change.into());
    state.set_change_tone(tone.into());

    CHART.with_borrow(|chart| {
        state.set_ticker(chart.ticker.as_str().into());
        state.set_range(index_of(chart.range));
        state.set_range_enabled(ModelRc::from(std::rc::Rc::new(VecModel::from(
            Range::ORDER
                .iter()
                .map(|range| offerable(*range, chart))
                .collect::<Vec<_>>(),
        ))));
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
            chart.plot = chainvue_chart::build(&chart.series, now, chart.range, &view);
            chart.plotted_for = (width, height);
            chart.plotted_generation = chart.generation;
        }

        chart.plot.clone()
    })
}

/// Whether a range button can be filled honestly.
///
/// A range the scan does not reach would draw a month's axis with a week of
/// history on it, and say nothing about the three weeks it has never looked
/// at. Offering it and then quietly showing less is worse than not offering it:
/// the flat stretch on the left would read as "you had nothing", which is a
/// claim about somebody's money that the wallet cannot make.
///
/// Once the scan has reached the start of the chain every range is honest,
/// because then a flat stretch on the left really does mean the wallet was
/// empty.
fn offerable(range: Range, chart: &Chart) -> bool {
    if chart.series.len() < 2 {
        return false;
    }
    match range.seconds() {
        None => true,
        Some(seconds) => chart.complete || chart.covers >= seconds,
    }
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
    let amount = chainvue_protocol::coins(delta);

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
        format!("{readings} {noun} · as far back as ChainVue has looked so far").into()
    }
}

/// Put the crosshair on the sample nearest `x`.
///
/// On the **sample**, not under the pointer: a dot floating between two
/// readings points at a balance that was never held.
fn cursor(ui: &AppWindow, x: f32) {
    let state = ui.global::<ChartState>();

    let found = CHART.with_borrow(|chart| {
        let plot = chart.plot.as_ref()?;
        if plot.xs.is_empty() {
            return None;
        }
        let index = chainvue_chart::index_at(&plot.xs, x);
        Some((
            index,
            *plot.xs.get(index)?,
            *plot.ys.get(index)?,
            *plot.values.get(index)?,
            *plot.times.get(index)?,
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
    // `chainvue_protocol::format`.
    state.set_hover_amount(chainvue_protocol::coins(value).into());
    state.set_hover_when(when(time).into());
}

fn clear_cursor(ui: &AppWindow) {
    let state = ui.global::<ChartState>();
    state.set_hover(-1);
    state.set_hover_amount(SharedString::new());
    state.set_hover_when(SharedString::new());
}

/// `12 Aug, 14:32` in the machine's own timezone.
///
/// A block timestamp is UTC seconds; which day and hour that is, is a question
/// about where the reader is sitting.
fn when(seconds: i64) -> String {
    use chrono::{Local, TimeZone};

    Local.timestamp_opt(seconds, 0).single().map_or_else(
        || "unknown".to_string(),
        |when| when.format("%-d %b, %H:%M").to_string(),
    )
}

fn index_of(range: Range) -> i32 {
    i32::try_from(
        Range::ORDER
            .iter()
            .position(|candidate| *candidate == range)
            .unwrap_or(0),
    )
    .unwrap_or(0)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chart(covers: i64, complete: bool, points: usize) -> Chart {
        Chart {
            series: (0..points)
                .map(|i| Point {
                    t: i64::try_from(i).unwrap_or(0) * 86_400,
                    value: 100,
                })
                .collect(),
            covers,
            complete,
            ..Chart::default()
        }
    }

    /// The rule that keeps the chart from claiming to know a month it has
    /// never looked at.
    #[test]
    fn a_range_the_scan_does_not_reach_is_not_offered() {
        let week = 7 * 86_400;
        let chart = chart(week, false, 10);

        assert!(offerable(Range::Week, &chart));
        assert!(!offerable(Range::Month, &chart));
        assert!(!offerable(Range::Year, &chart));
        // Everything the wallet has is always an honest answer.
        assert!(offerable(Range::All, &chart));
    }

    /// Once the scan has reached the start of the chain, a flat stretch on the
    /// left really does mean the wallet was empty — so every range is honest.
    #[test]
    fn a_complete_scan_can_fill_any_range() {
        let chart = chart(7 * 86_400, true, 10);
        for range in Range::ORDER {
            assert!(offerable(range, &chart), "{range:?}");
        }
    }

    /// Nothing to draw means nothing to offer.
    #[test]
    fn an_empty_series_offers_no_range_at_all() {
        let chart = chart(0, true, 0);
        for range in Range::ORDER {
            assert!(!offerable(range, &chart), "{range:?}");
        }
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
        let complete = caption(&chart(0, true, 3), 3);
        assert!(complete.contains("whole history"), "{complete}");

        let partial = caption(&chart(0, false, 3), 3);
        assert!(partial.contains("as far back"), "{partial}");

        // Singular when there is one of them. A "1 readings" is the sort of
        // thing nobody fixes because nobody writes it down.
        assert!(caption(&chart(0, true, 1), 1).contains("1 reading ·"));
    }
}
