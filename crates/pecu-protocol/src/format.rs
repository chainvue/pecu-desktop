//! How an amount is written down.
//!
//! # Why this lives in the protocol crate
//!
//! Because two crates need it and neither may depend on the other. The core
//! formats every figure it sends; the interface formats the one figure the core
//! cannot pre-format, which is the balance under a chart cursor — that is
//! chosen by a pointer moving at sixty hertz, and asking the core for it would
//! be a round trip per frame.
//!
//! One formatter, in the one crate both sides already have. The alternative is
//! two implementations of the same rule that agree until the day they do not,
//! and the day they do not is a screen showing two different numbers for the
//! same money.

use crate::SATS_PER_COIN;

/// `1248242000000` → `12 482.4200 0000`.
///
/// Grouped thousands, then the satoshi digits in two blocks of four. A bare
/// `12482.42000000` is a number nobody can read at a glance, and eight decimal
/// places are not optional in a currency where the last one is a meaningful
/// unit — trimming trailing zeros would make two amounts of different precision
/// line up differently in a column.
///
/// Takes satoshis as an `i64` and never sees a float. The sign is dropped:
/// direction is the caller's to say, because "+" and "−" mean different things
/// on a balance and on a movement.
pub fn coins(sats: i64) -> String {
    magnitude(u128::from(sats.unsigned_abs()))
}

/// The same rule for an unsigned count.
///
/// The SDK's `Amount` holds a `u64`, and a `u64` of satoshis runs past
/// `i64::MAX` — twice over. Saturating on the way in would silently halve an
/// amount, so the two signatures both exist and both call the same worker. The
/// duplication worth avoiding is the *rule*, not a two-line wrapper.
pub fn coins_u64(sats: u64) -> String {
    magnitude(u128::from(sats))
}

fn magnitude(sats: u128) -> String {
    let per_coin = u128::from(SATS_PER_COIN.unsigned_abs());
    let whole = sats / per_coin;
    let fraction = sats % per_coin;

    // Always eight digits, zero-padded, then split four and four.
    let fraction = format!("{fraction:08}");
    format!(
        "{}.{} {}",
        group(&whole.to_string()),
        &fraction[..4],
        &fraction[4..],
    )
}

/// `12482` → `12 482`.
///
/// A thin space would be better typography; a normal space is what every font
/// in this build actually has, and a glyph that renders as nothing is worse
/// than a slightly wide one.
pub fn group(digits: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_are_grouped_and_padded_to_eight_places() {
        assert_eq!(coins(0), "0.0000 0000");
        assert_eq!(coins(1), "0.0000 0001");
        assert_eq!(coins(SATS_PER_COIN), "1.0000 0000");
        assert_eq!(coins(1_248_242_000_000), "12 482.4200 0000");
        assert_eq!(coins(100_000_000_000), "1 000.0000 0000");
    }

    /// The sign belongs to the caller: "+" and "−" mean different things on a
    /// balance and on a movement, and a formatter that guessed would be wrong
    /// half the time.
    #[test]
    fn the_sign_is_not_this_function_s_business() {
        assert_eq!(coins(-1_248_242_000_000), coins(1_248_242_000_000));
    }

    /// Not one satoshi, at either end of the range.
    #[test]
    fn nothing_is_lost_at_the_extremes() {
        for sats in [i64::MAX, i64::MIN + 1, i64::MIN, -1, 1, 99_999_999] {
            let shown = coins(sats);
            assert_eq!(
                digits_of(&shown),
                u128::from(sats.unsigned_abs()),
                "{shown}"
            );
        }

        // And the unsigned entry point, which has to reach past `i64::MAX` —
        // that is the whole reason it exists.
        for sats in [u64::MAX, u64::MAX / 2, 0, 1] {
            let shown = coins_u64(sats);
            assert_eq!(digits_of(&shown), u128::from(sats), "{shown}");
        }
    }

    /// Every digit in a formatted amount, as one number. Nothing may be lost
    /// to grouping or to the decimal split.
    fn digits_of(shown: &str) -> u128 {
        shown
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap_or_default()
    }

    #[test]
    fn thousands_are_grouped_from_the_right() {
        assert_eq!(group("0"), "0");
        assert_eq!(group("999"), "999");
        assert_eq!(group("1000"), "1 000");
        assert_eq!(group("1187149"), "1 187 149");
    }
}
