//! Thinning a long series without erasing what makes it interesting.

use crate::series::Point;

/// Largest-Triangle-Three-Buckets.
///
/// # Why not just take every nth point
///
/// Because the story in a wallet's history is usually one or two large
/// movements, and stride sampling deletes whichever ones happen to fall between
/// strides. A chart that quietly loses the day someone was paid is worse than
/// no chart: it is a picture of the right shape with the wrong events in it.
///
/// LTTB keeps a point from every bucket, choosing the one that forms the
/// largest triangle with the previous kept point and the average of the next
/// bucket. That is a proxy for "how much does this point change the visible
/// outline", so spikes survive and flat runs collapse — which is exactly the
/// trade a chart wants.
///
/// The first and last points are always kept. They are the two the reader
/// checks against the balance on screen.
///
/// Returns the input unchanged when it is already short enough, or when
/// `threshold` is under three — with fewer than three there are no interior
/// buckets and the algorithm has nothing to say.
pub fn downsample(points: &[Point], threshold: usize) -> Vec<Point> {
    if threshold < 3 || points.len() <= threshold {
        return points.to_vec();
    }

    let mut kept = Vec::with_capacity(threshold);
    kept.push(points[0]);

    // Every bucket covers the same number of *interior* points; the first and
    // last are outside the buckets because they are always kept.
    let interior = points.len() - 2;
    let buckets = threshold - 2;
    let mut previous = points[0];

    for bucket in 0..buckets {
        let start = 1 + bucket * interior / buckets;
        let end = 1 + (bucket + 1) * interior / buckets;
        let (next_start, next_end) = if bucket + 1 == buckets {
            // The last bucket's "next" is the final point on its own.
            (points.len() - 1, points.len())
        } else {
            (end, 1 + (bucket + 2) * interior / buckets)
        };

        let Some(chosen) = pick(
            &points[start..end],
            previous,
            average(&points[next_start..next_end]),
        ) else {
            continue;
        };
        kept.push(chosen);
        previous = chosen;
    }

    kept.push(points[points.len() - 1]);
    kept
}

/// The point in `bucket` making the largest triangle with `previous` and
/// `next`.
///
/// The area is computed in `f64` because it is a comparison between candidates
/// and never becomes a displayed quantity. Every value that reaches a screen
/// stays an `i64` all the way — see the crate docs.
///
/// Hence the allow: past 2^53 satoshis these casts lose their low bits, and it
/// does not matter. The result is only ever fed to `>`, so the worst a rounded
/// area can do is pick a neighbouring point out of the same bucket — a
/// different pixel, not a different number. The value that gets *drawn* is
/// copied from the chosen `Point` and never touches an `f64`.
#[allow(clippy::cast_precision_loss)]
fn pick(bucket: &[Point], previous: Point, next: (f64, f64)) -> Option<Point> {
    let (ax, ay) = (previous.t as f64, previous.value as f64);
    let (cx, cy) = next;

    let mut best: Option<(f64, Point)> = None;
    for candidate in bucket {
        let (bx, by) = (candidate.t as f64, candidate.value as f64);
        // Twice the triangle's area. The factor of two is the same for every
        // candidate, so dividing it out would only cost an instruction.
        let area = ((ax - cx) * (by - ay) - (ax - bx) * (cy - ay)).abs();
        if best.is_none_or(|(previous_area, _)| area > previous_area) {
            best = Some((area, *candidate));
        }
    }

    best.map(|(_, point)| point)
}

/// The centre of a bucket, as the third corner of the triangle.
///
/// Same reasoning as [`pick`]: this is one input to a comparison and is never
/// shown to anybody.
#[allow(clippy::cast_precision_loss)]
fn average(points: &[Point]) -> (f64, f64) {
    if points.is_empty() {
        return (0.0, 0.0);
    }
    let count = points.len() as f64;
    let t: f64 = points.iter().map(|point| point.t as f64).sum();
    let value: f64 = points.iter().map(|point| point.value as f64).sum();
    (t / count, value / count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(count: i64) -> Vec<Point> {
        (0..count).map(|i| Point { t: i, value: i }).collect()
    }

    #[test]
    fn a_short_series_is_left_alone() {
        let points = line(10);
        assert_eq!(downsample(&points, 240), points);
        assert_eq!(downsample(&points, 10), points);
    }

    #[test]
    fn a_long_series_comes_down_to_the_threshold() {
        let points = line(5_000);
        let thinned = downsample(&points, 240);

        assert_eq!(thinned.len(), 240);
        assert_eq!(thinned.first(), points.first());
        assert_eq!(thinned.last(), points.last());
    }

    /// The property the whole algorithm exists for. One large receive in an
    /// otherwise flat history **is** the story, and stride sampling would drop
    /// it whenever it fell between strides.
    #[test]
    fn a_single_spike_survives_being_thinned() {
        let mut points = line(1_000);
        for point in &mut points {
            point.value = 100;
        }
        points[437].value = 9_000;

        let thinned = downsample(&points, 50);
        assert!(
            thinned.iter().any(|point| point.value == 9_000),
            "the spike was thinned away",
        );
    }

    /// Time only ever moves forwards, and a downsampler that reordered points
    /// would draw a path that doubles back on itself.
    #[test]
    fn thinning_keeps_the_order() {
        let points = line(3_000);
        let thinned = downsample(&points, 120);

        let times: Vec<i64> = thinned.iter().map(|point| point.t).collect();
        assert!(times.windows(2).all(|pair| pair[0] < pair[1]), "{times:?}");
    }

    /// Under three there are no interior buckets, so there is nothing to
    /// choose between and the honest answer is the input.
    #[test]
    fn a_threshold_too_small_to_mean_anything_changes_nothing() {
        let points = line(100);
        assert_eq!(downsample(&points, 2).len(), 100);
        assert_eq!(downsample(&points, 0).len(), 100);
    }
}
