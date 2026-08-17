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
}
