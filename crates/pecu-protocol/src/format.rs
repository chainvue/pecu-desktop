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

/// A derived price, to four significant digits.
///
/// # Why not the satoshi formatter
///
/// Because prices on this chain span eight orders of magnitude — a testnet
/// basket unit is worth `0.000004` of a dollar and a wrapped bitcoin `5 888` of
/// them — and one decimal rule cannot serve both. `0.00000400` in a column
/// reads as zero; `5 887.9122 2600` reads as a balance rather than a price and
/// claims a precision the reserve ratio it came from does not have.
///
/// Four significant digits, then: enough to tell two currencies apart, few
/// enough that the number is the same width wherever it lands. Trailing zeros
/// are trimmed, but never past two decimals for a value under a thousand — a
/// price that renders as `7.09` beside one that renders as `7.1` is two
/// different columns.
///
/// A non-zero value never formats as zero. Below a satoshi — the smallest
/// thing this chain represents at all — there are no digits left to show, so it
/// says `< 0.00000001` rather than rounding down to a claim that it is free.
pub fn price(value: f64) -> String {
    if !value.is_finite() {
        return "—".to_string();
    }
    let magnitude = value.abs();
    if magnitude == 0.0 {
        return "0.00".to_string();
    }

    // Four significant digits: add decimal places until the value scaled by
    // them has four digits in front of the point. By multiplication rather than
    // through a logarithm and a cast — the cast is the part that would be
    // wrong, silently, at the ends of the range this has to cover.
    let mut places = 0_usize;
    let mut scaled = magnitude;
    while scaled < 1000.0 && places < 8 {
        scaled *= 10.0;
        places += 1;
    }
    let shown = format!("{magnitude:.places$}");

    // A value too small for four significant digits at eight places rounds to
    // zero, which is the one thing a price must never say by accident. Eight is
    // where the chain itself stops, so there is no further place to go to —
    // what is left to say is that it is smaller than that.
    if shown.chars().all(|ch| !ch.is_ascii_digit() || ch == '0') {
        let sign = if value < 0.0 { "−" } else { "" };
        return format!("{sign}< 0.00000001");
    }

    let floor = if magnitude >= 1000.0 { 0 } else { 2 };
    let shown = trim(&shown, floor);

    let (whole, rest) = match shown.split_once('.') {
        Some((whole, rest)) => (whole, format!(".{rest}")),
        None => (shown.as_str(), String::new()),
    };
    let sign = if value < 0.0 { "−" } else { "" };
    format!("{sign}{}{rest}", group(whole))
}

/// A derived quantity, rounded to something a person can compare.
///
/// Used for depths and supplies, which are answers to "roughly how much" and
/// span from a third of one coin to forty million. Whole units above a
/// thousand, two places below — the fraction of a large depth is noise, and the
/// fraction of a small one is the whole figure.
///
/// **Not for anything anybody can spend.** This rounds, and it takes a float.
/// Amounts go through [`coins`].
pub fn approx(value: f64) -> String {
    if !value.is_finite() {
        return "—".to_string();
    }
    let magnitude = value.abs();
    let places = if magnitude >= 1000.0 { 0 } else { 2 };
    let shown = format!("{magnitude:.places$}");
    let (whole, rest) = match shown.split_once('.') {
        Some((whole, rest)) => (whole, format!(".{rest}")),
        None => (shown.as_str(), String::new()),
    };
    let sign = if value < 0.0 { "−" } else { "" };
    format!("{sign}{}{rest}", group(whole))
}

/// Drop trailing zeros, keeping at least `floor` decimal places.
fn trim(shown: &str, floor: usize) -> String {
    let Some((whole, decimals)) = shown.split_once('.') else {
        return shown.to_string();
    };
    let kept = decimals.trim_end_matches('0');
    let kept = &decimals[..kept.len().max(floor).min(decimals.len())];
    if kept.is_empty() {
        whole.to_string()
    } else {
        format!("{whole}.{kept}")
    }
}

#[cfg(test)]
// The price cases are the figures VRSCTEST actually quotes, written the way the
// daemon writes them. Separators would obscure exactly what is being checked.
#[allow(clippy::unreadable_literal)]
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

    /// The figures are the ones VRSCTEST actually quotes, so the widths in
    /// this list are the widths the markets column has to hold.
    #[test]
    fn a_price_keeps_four_significant_digits_at_any_magnitude() {
        assert_eq!(price(5887.912226), "5 888");
        assert_eq!(price(2044.397409), "2 044");
        assert_eq!(price(55.991938), "55.99");
        assert_eq!(price(7.090256), "7.09");
        assert_eq!(price(1.0), "1.00");
        assert_eq!(price(0.537226), "0.5372");
        assert_eq!(price(0.098340), "0.09834");
        assert_eq!(price(0.002923), "0.002923");
    }

    /// The one thing a price may never do is claim something is free.
    #[test]
    fn a_price_smaller_than_four_digits_can_show_is_not_rounded_to_nothing() {
        assert_eq!(price(0.000004), "0.000004");
        assert_eq!(price(0.00000001), "0.00000001");
        // Below the smallest unit the chain has. Not zero, and not a rounded
        // number that would read as one.
        assert_eq!(price(0.000000001), "< 0.00000001");
        assert_eq!(price(-0.000000001), "−< 0.00000001");
    }

    #[test]
    fn zero_is_a_price_and_infinity_is_not() {
        assert_eq!(price(0.0), "0.00");
        assert_eq!(price(f64::NAN), "—");
        assert_eq!(price(f64::INFINITY), "—");
        assert_eq!(approx(f64::NAN), "—");
    }

    #[test]
    fn a_quantity_is_whole_above_a_thousand_and_exact_below_it() {
        assert_eq!(approx(0.37), "0.37");
        assert_eq!(approx(13.38), "13.38");
        assert_eq!(approx(1248.93), "1 249");
        assert_eq!(approx(94095.41), "94 095");
        assert_eq!(approx(40_399_999.56), "40 400 000");
    }

    #[test]
    fn thousands_are_grouped_from_the_right() {
        assert_eq!(group("0"), "0");
        assert_eq!(group("999"), "999");
        assert_eq!(group("1000"), "1 000");
        assert_eq!(group("1187149"), "1 187 149");
    }
}

/// A name the chain supplied, made safe to put in front of somebody.
///
/// Named `safe_name` and not `chain_name`, which in this workspace already
/// means the name *of a chain* — `Core::chain_name` returns "VRSCTEST". Two
/// things one letter apart in the same file is how the wrong one gets called.
///
/// # Why any of this is necessary
///
/// **Names on Verus are permissionless.** A currency's `fullyqualifiedname` and
/// a VerusID's name are chosen by whoever registered them, and they reach this
/// wallet's most consequential surfaces: the row somebody clicks to pay, the
/// review they read before signing, the list of people they have paid before.
///
/// Two families of character make that dangerous, and neither is visible:
///
///   * **Bidi controls** re-order the text around them. `RLO` inside a name
///     makes the rest of the line render right-to-left, so a name can be
///     constructed to display as an entirely different one. This is the
///     [Trojan Source] class of attack, and a wallet is exactly the place it
///     pays off.
///   * **Zero-width and other invisible formatting characters** let two
///     distinct names render identically, so one identity can be made to look
///     like another that the reader already trusts.
///
/// Control characters are removed with them: a newline in a name breaks a row
/// in half, and a name is a single line by definition.
///
/// # What this does not do
///
/// It does not shorten, transliterate, or reject anything else. Emoji are
/// legitimate and common — VRSCTEST has `Token🏭Factory` and `🌲Token AMM1🌲` —
/// and a wallet that mangled those would be wrong far more often than it was
/// right. Nor does it make a name safe to *identify* by: **everything in this
/// workspace keys on the i-address**, and this is about what is displayed, not
/// about what is decided.
///
/// An empty result is a real answer, and the callers already have the right
/// fallback for it: a name that was nothing but invisible characters becomes
/// the empty string, and `name_of` then shows the i-address, which is ugly and
/// cannot be forged.
///
/// [Trojan Source]: https://trojansource.codes
#[must_use]
pub fn safe_name(raw: &str) -> String {
    raw.chars().filter(|c| !is_deceptive(*c)).collect()
}

/// Whether a character can change what the text around it looks like without
/// being seen itself.
fn is_deceptive(c: char) -> bool {
    // A name is one line, so anything Unicode calls a control goes.
    if c.is_control() {
        return true;
    }

    matches!(c,
        // Explicit bidirectional formatting: the marks, the embeddings and
        // overrides with their terminator, and the isolates.
        '\u{061C}'                  // Arabic letter mark
        | '\u{200E}'..='\u{200F}'    // LRM, RLM
        | '\u{202A}'..='\u{202E}'    // LRE, RLE, PDF, LRO, RLO
        | '\u{2066}'..='\u{2069}'    // LRI, RLI, FSI, PDI
        // Zero-width and other invisibles that join, separate or simply hide.
        | '\u{00AD}'                // soft hyphen
        | '\u{180E}'                // Mongolian vowel separator
        | '\u{200B}'..='\u{200D}'    // ZWSP, ZWNJ, ZWJ
        | '\u{2060}'..='\u{2064}'    // word joiner and the invisible operators
        | '\u{FEFF}'                // zero-width no-break space
    )
}

#[cfg(test)]
mod name_tests {
    use super::safe_name;

    /// A right-to-left override makes the text after it render backwards, so a
    /// name can be built to display as a different one. This is the attack the
    /// function exists for.
    #[test]
    fn a_bidi_override_is_removed() {
        let deceptive = "safe\u{202E}dlog\u{202C}";
        assert_eq!(safe_name(deceptive), "safedlog");
        assert!(!safe_name(deceptive).chars().any(char::is_control));
    }

    /// Two names that render identically and are not the same name.
    #[test]
    fn zero_width_characters_cannot_hide_a_difference() {
        assert_eq!(safe_name("dude\u{200B}.VRSCTEST@"), "dude.VRSCTEST@");
        assert_eq!(safe_name("du\u{200D}de.VRSCTEST@"), "dude.VRSCTEST@");
        assert_eq!(safe_name("\u{FEFF}Bridge.vETH"), "Bridge.vETH");
    }

    /// A newline in a name would break the row it is drawn in.
    #[test]
    fn a_name_is_one_line() {
        assert_eq!(safe_name("Bridge\n.vETH"), "Bridge.vETH");
        assert_eq!(safe_name("Bridge\t.vETH"), "Bridge.vETH");
    }

    /// **Emoji stay.** They are legitimate and common — these two are real
    /// currencies on VRSCTEST — and a wallet that mangled them would be wrong
    /// far more often than it was right.
    #[test]
    fn the_names_a_real_chain_actually_has_are_left_alone() {
        for name in [
            "Token\u{1F3ED}Factory",
            "\u{1F332}Token AMM1\u{1F332}",
            "Bridge.vETH",
            "dude.VRSCTEST@",
            "VRSC-BTC-USD-EUR-GOLD",
            "kali-3-seed162-broken_press-mandala.mcp3",
        ] {
            assert_eq!(safe_name(name), name, "{name} was altered");
        }
    }

    /// A name that was nothing but invisibles becomes nothing, and the callers
    /// already fall back to the i-address — which is ugly and cannot be forged.
    #[test]
    fn a_name_made_only_of_invisibles_is_empty() {
        assert!(safe_name("\u{200B}\u{200C}\u{FEFF}").is_empty());
    }
}
