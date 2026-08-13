//! Turning a list of transactions into a balance over time.

/// One reading: what the balance was, and when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    /// Unix seconds.
    pub t: i64,
    /// Satoshis. Integer all the way through — see the crate docs.
    pub value: i64,
}

/// Build the balance over time by walking **backwards** from what it is now.
///
/// # Why backwards
///
/// The wallet knows two things exactly: the balance right now, and every
/// transaction inside the window it has scanned. It does **not** know the
/// balance before that window — there may be a hundred thousand blocks of
/// history underneath it.
///
/// Walking forwards would mean starting from a number nobody knows and hoping
/// it comes out right at the end. Walking backwards starts from the one figure
/// that is certainly true and subtracts each delta on the way down, so every
/// point is anchored to a fact rather than to an assumption. Getting this the
/// wrong way round produces a chart that is uniformly wrong by whatever the
/// wallet held before the window, which is exactly the kind of error that looks
/// plausible.
///
/// `deltas` is `(block_time, net_native)` in **oldest-first** order, which is
/// the order the SDK returns history in.
///
/// The series that comes back is oldest-first and has one point per
/// transaction, plus one at `now` carrying the current balance — without that
/// last one a chart would stop at the most recent transaction and imply the
/// balance had not existed since.
pub fn from_deltas(now: i64, balance_now: i64, deltas: &[(i64, i64)]) -> Vec<Point> {
    if deltas.is_empty() {
        return Vec::new();
    }

    let mut points = Vec::with_capacity(deltas.len() + 1);
    points.push(Point {
        t: now,
        value: balance_now,
    });

    // Newest first: after this transaction the balance was `running`, so before
    // it the balance was `running - net`.
    let mut running = balance_now;
    for (time, net) in deltas.iter().rev() {
        let before = running.saturating_sub(*net);
        points.push(Point {
            t: *time,
            value: before,
        });
        running = before;
    }

    points.reverse();

    // A block timestamp is miner-chosen and only loosely monotonic — the SDK
    // says so where it defines the field. Two blocks can carry timestamps that
    // go backwards by a few seconds, which would make a segment run right to
    // left. Sorting by time is the only thing that guarantees the path moves
    // one way, and it is stable so same-second transactions keep chain order.
    points.sort_by_key(|point| point.t);
    points
}

/// Keep only the points inside the last `seconds`.
///
/// The point immediately before the window is kept and moved to its edge, so
/// the chart starts at the balance that was actually held when the window
/// opened rather than at the first transaction inside it. Without that, a
/// month with one transaction in it would draw a line starting on the day of
/// that transaction and say nothing about the three weeks before.
///
/// # `extend`: what to do when the window reaches further back than the wallet
///
/// Set it when the scan has reached the start of the chain. Then the earliest
/// reading is the balance before this wallet's first transaction — a real
/// figure, usually zero — and the window can honestly be drawn back to its own
/// start at that value. A year's axis on a nine-hour-old wallet then shows a
/// year of nothing and a rise at the right, which is exactly what happened.
///
/// Leave it clear when the scan has not reached the start. The wallet then does
/// not know what was held before its earliest reading, and drawing a flat run
/// back to the window's edge would be inventing one — the interface refuses
/// those ranges for the same reason.
pub fn since(points: &[Point], now: i64, seconds: i64, extend: bool) -> Vec<Point> {
    let cutoff = now.saturating_sub(seconds);

    let first_inside = points.iter().position(|point| point.t >= cutoff);
    let Some(first_inside) = first_inside else {
        let _ = extend;
        // Everything is older than the window. The balance has not moved
        // inside it, so the window is one flat line at what it is now.
        return match points.last() {
            Some(last) => vec![
                Point {
                    t: cutoff,
                    value: last.value,
                },
                Point {
                    t: now,
                    value: last.value,
                },
            ],
            None => Vec::new(),
        };
    };

    let mut kept: Vec<Point> = Vec::with_capacity(points.len() - first_inside + 1);
    if let Some(before) = first_inside.checked_sub(1).and_then(|i| points.get(i)) {
        kept.push(Point {
            t: cutoff,
            value: before.value,
        });
    } else if extend {
        // The window opens before anything this wallet has ever done, and the
        // scan reached the chain start — so what was held then is known.
        if let Some(first) = points.first().filter(|first| first.t > cutoff) {
            kept.push(Point {
                t: cutoff,
                value: first.value,
            });
        }
    }
    kept.extend_from_slice(&points[first_inside..]);
    kept
}

/// How far back the series goes, in seconds.
pub fn span(points: &[Point]) -> i64 {
    match (points.first(), points.last()) {
        (Some(first), Some(last)) => last.t.saturating_sub(first.t),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_balance_is_reconstructed_backwards_from_what_it_is_now() {
        // Received 100, then 50, then spent 30. Balance now: 120.
        let deltas = [(1_000, 100), (2_000, 50), (3_000, -30)];
        let points = from_deltas(4_000, 120, &deltas);

        assert_eq!(
            points,
            vec![
                // Before the first transaction there was nothing.
                Point { t: 1_000, value: 0 },
                Point {
                    t: 2_000,
                    value: 100
                },
                Point {
                    t: 3_000,
                    value: 150
                },
                Point {
                    t: 4_000,
                    value: 120
                },
            ],
        );
    }

    /// The window is what the wallet has scanned, not the whole chain. A
    /// series that starts above zero is the normal case and must not be
    /// "corrected" to start at nothing.
    #[test]
    fn a_partial_window_keeps_the_balance_it_started_with() {
        // The wallet already held 500 before anything in this window.
        let deltas = [(1_000, 100), (2_000, -40)];
        let points = from_deltas(3_000, 560, &deltas);

        assert_eq!(points[0].value, 500);
        assert_eq!(points[1].value, 600);
        assert_eq!(points[2].value, 560);
    }

    #[test]
    fn no_transactions_is_no_series() {
        assert!(from_deltas(1_000, 42, &[]).is_empty());
    }

    /// Block timestamps are miner-chosen and only loosely monotonic. A pair
    /// that goes backwards would draw a segment running right to left.
    #[test]
    fn timestamps_that_go_backwards_are_put_in_order() {
        let deltas = [(1_000, 100), (990, 50), (2_000, 10)];
        let points = from_deltas(3_000, 160, &deltas);

        let times: Vec<i64> = points.iter().map(|point| point.t).collect();
        assert!(times.windows(2).all(|pair| pair[0] <= pair[1]), "{times:?}");
    }

    /// The last point carries the balance at `now`, not at the last
    /// transaction — otherwise a chart stops on the day of the most recent
    /// payment and implies the money stopped existing there.
    #[test]
    fn the_series_runs_up_to_now() {
        let points = from_deltas(9_999, 77, &[(1_000, 77)]);
        assert_eq!(
            points.last().copied(),
            Some(Point {
                t: 9_999,
                value: 77
            })
        );
    }

    #[test]
    fn a_window_keeps_the_balance_held_when_it_opened() {
        let points = vec![
            Point { t: 100, value: 10 },
            Point { t: 200, value: 20 },
            Point { t: 900, value: 30 },
        ];
        // A window covering the last 750 seconds: only the t=900 point is
        // inside it, but the balance at the edge was 20.
        let kept = since(&points, 1_000, 750, false);

        assert_eq!(kept[0], Point { t: 250, value: 20 });
        assert_eq!(kept[1], Point { t: 900, value: 30 });
        assert_eq!(kept.len(), 2);
    }

    /// A window wider than the wallet's whole history, on a wallet whose whole
    /// history is known. The balance before the first transaction is a real
    /// figure, so the window can be drawn back to its own start at that value —
    /// which is what makes 1Y look different from 1W.
    #[test]
    fn a_complete_history_can_be_drawn_back_to_the_window_s_start() {
        let points = vec![
            Point { t: 9_000, value: 0 },
            Point {
                t: 9_100,
                value: 500,
            },
            Point {
                t: 10_000,
                value: 500,
            },
        ];

        // A window reaching back to t=0, on a wallet whose earliest reading is
        // t=9000. Extended: the line starts at the window's edge, at nothing.
        let kept = since(&points, 10_000, 10_000, true);
        assert_eq!(kept.first().copied(), Some(Point { t: 0, value: 0 }));
        assert_eq!(kept.len(), 4);

        // Not extended, because the scan has not reached the chain start and
        // nobody knows what was held at t=0. The series is left alone.
        let kept = since(&points, 10_000, 10_000, false);
        assert_eq!(kept, points);
    }

    /// A quiet month is still a month. It draws as a flat line at what the
    /// balance is, not as an empty chart.
    #[test]
    fn a_window_with_nothing_in_it_is_flat_rather_than_empty() {
        let points = vec![Point { t: 100, value: 42 }];
        let kept = since(&points, 100_000, 1_000, false);

        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|point| point.value == 42));
        assert_eq!(kept[0].t, 99_000);
        assert_eq!(kept[1].t, 100_000);
    }
}
