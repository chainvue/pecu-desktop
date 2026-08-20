//! Whether the protocol is currently letting anybody trade.
//!
//! # Why a successful quote proves nothing
//!
//! Verus can switch conversions off chain-wide, at consensus level. While that
//! is on, a conversion is rejected during validation and never reaches a block
//! — and **nothing on the read path changes shape**. `listcurrencies` still
//! lists pools with reserves in them, `getcurrencystate` still answers, and
//! `estimateconversion` still quotes a price.
//!
//! So a wallet that decides the market is healthy because the numbers came back
//! shows green straight through a total halt, and builds a transaction the
//! network is going to throw away. The switch has to be read directly, and this
//! module is that read.
//!
//! # Where it lives
//!
//! Each chain designates a notification oracle identity — see
//! [`pecu_chain::Oracle`] — whose content map holds at most one upgrade
//! descriptor under a key derived per chain. Clearing the switch is an identity
//! update that removes the key, so **absent is the healthy state**.

use pecu_protocol::NoteVm;

/// One value out of the oracle's content map, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor {
    /// The record's own version.
    pub version: u64,
    /// The daemon version this upgrade wants, as `a.b.c.d`.
    pub minimum_daemon: [u8; 4],
    /// Which switch, by its twenty bytes, spelled the way the table below is.
    pub upgrade: String,
    /// The height it takes effect at. Compare against the tip; see [`Status`].
    pub activation_height: u32,
    /// Epoch seconds, and zero in everything observed. Gate on the height.
    pub activation_time: u64,
}

/// How loud a switch is.
///
/// Ordered, and the order is the combining rule: worst wins. `Unknown` outranks
/// `Info` **deliberately** — a chain nobody could reach might be halted, and the
/// one thing this must never do is let missing data read as working.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Nothing is switched off. You asked, and the key was absent.
    Clear,
    /// Routine housekeeping. Eleven of the fourteen switches are this.
    Info,
    /// Something is off, or a halt is scheduled and has not landed.
    Warning,
    /// Could not be asked. **Never** the same as clear.
    Unknown,
    /// Trading is halted now.
    Critical,
}

impl Severity {
    /// What the interface calls it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Unknown => "unknown",
            Self::Critical => "critical",
        }
    }
}

/// One row of the switch table.
struct Switch {
    /// The daemon's name for it.
    ///
    /// Never put in front of anybody — see [`Status::note`] — and written to
    /// the **log**, which is a different audience. `disabledefi` is exactly
    /// what somebody reading a bug report needs and exactly what a person
    /// staring at a dead button does not.
    name: &'static str,
    severity: Severity,
    /// The note that says what stops working.
    says: &'static str,
    /// Whether a conversion is refused while this is in force.
    halts_conversion: bool,
}

/// The switches, by the byte-reversed id.
///
/// A convenience, and **not the source of truth**: an id that is not here is
/// reported under its raw hex at `Info` rather than dropped. Silently
/// discarding one is how a future halt goes unannounced.
fn switch(id: &str) -> Option<Switch> {
    let row = |name, severity, says, halts_conversion| {
        Some(Switch {
            name,
            severity,
            says,
            halts_conversion,
        })
    };
    match id {
        "ba88b5b0691b237fbe909fba38053e9a17d49b5a" => {
            row("disabledefi", Severity::Critical, "halt-conversions", true)
        }
        "c8eb1ce97cc65b44b7c3b86315ef4419d5279ad9" => row(
            "disablepbaascrosschain",
            Severity::Warning,
            "halt-pbaas-bridge",
            false,
        ),
        "cb287a8f91a05bf453d6052b22e7f1bdd9e84ff9" => row(
            "disablegatewaycrosschain",
            Severity::Warning,
            "halt-eth-gateway",
            false,
        ),
        "bce0fbf688ce745d4f4128ce624e8ec4ea637cfd" => row(
            "disableearnednotarizations",
            Severity::Info,
            "chain-housekeeping",
            false,
        ),
        "5469a8ccb954da2412c617730f6b7eb6d41996ac" => row(
            "resetnotarizationmodulo",
            Severity::Info,
            "chain-housekeeping",
            false,
        ),
        "b005d51d22735a9334679c293cbea2620a43e7f6" => row(
            "magicnumberfix",
            Severity::Info,
            "chain-housekeeping",
            false,
        ),
        "faf4d61c493863a443e48a03939b58cab1c82748" => row(
            "pbaascrosschainproofupgrade",
            Severity::Info,
            "chain-housekeeping",
            false,
        ),
        "aea035194dc1c14d73ac4bc00be438e0ba3a8635" => row(
            "bridgecleanupwindowclosed",
            Severity::Info,
            "chain-housekeeping",
            false,
        ),
        "71e1f55da6f5f0ec2711321aff4ca9409b379a4e" => row(
            "forceidentityunlock",
            Severity::Info,
            "chain-housekeeping",
            false,
        ),
        "a8fec0b8428df1e86092b814c8ee2a710b74aac5" => row(
            "forceidentityupgrade",
            Severity::Info,
            "chain-housekeeping",
            false,
        ),
        "81a1256d556e417d2b09f6b80b943e491d21689d" => row(
            "enableoptimizedethproof",
            Severity::Info,
            "chain-housekeeping",
            false,
        ),
        "352486742ed39608a2bcf5d401bfe969f3f99d33" => row(
            "optionalpbaasupgrade",
            Severity::Info,
            "chain-housekeeping",
            false,
        ),
        "0dcb3fe723a57fb058daf7a01082c81b83530495" => row(
            "preconvertreservetransferprecheck",
            Severity::Info,
            "chain-housekeeping",
            false,
        ),
        "b237a49606639ecdba9993d89de592c88c35de1a" => row(
            "importpreconvertreservetransferprecheck",
            Severity::Info,
            "chain-housekeeping",
            false,
        ),
        _ => None,
    }
}

/// Read one Verus varint.
///
/// **Not CompactSize**, and reading it as one produces plausible garbage rather
/// than an obvious failure — you get a height, it is simply the wrong one.
///
/// It is a big-endian base-128 run where a set continuation bit *also
/// increments* the accumulated value, which is what makes every length's
/// encoding unique. `0x80 0x00` is 1, not 0.
///
/// `None` on a run that walks off the end, or one long enough to overflow —
/// nine bytes is already past `u64`.
fn varint(bytes: &[u8], at: usize) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    let mut index = at;
    for _ in 0..9 {
        let byte = *bytes.get(index)?;
        index += 1;
        value = value
            .checked_mul(128)?
            .checked_add(u64::from(byte & 0x7f))?;
        if byte & 0x80 == 0 {
            return Some((value, index));
        }
        value = value.checked_add(1)?;
    }
    None
}

/// Decode one descriptor.
///
/// `None` on anything that does not parse, and the caller skips it: one
/// unreadable entry must not take the whole status with it.
pub fn parse(bytes: &[u8]) -> Option<Descriptor> {
    let (version, at) = varint(bytes, 0)?;
    let (minimum, at) = varint(bytes, at)?;
    let minimum_daemon = u32::try_from(minimum).ok()?.to_be_bytes();

    let raw = bytes.get(at..at + 20)?;
    // **Little-endian on the wire.** Reversed before it is compared against
    // anything, because the table is written the way the daemon prints an id.
    let mut id = [0u8; 20];
    for (slot, byte) in id.iter_mut().zip(raw.iter().rev()) {
        *slot = *byte;
    }
    let at = at + 20;

    let (activation_height, at) = varint(bytes, at)?;
    let (activation_time, _) = varint(bytes, at)?;

    Some(Descriptor {
        version,
        minimum_daemon,
        upgrade: hex::encode(id),
        activation_height: u32::try_from(activation_height).ok()?,
        activation_time,
    })
}

/// What a chain's oracle says, reduced to what a screen needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub severity: Severity,
    /// What stops working, in the interface's words — never the switch's name.
    /// `disabledefi` means nothing to anybody; "conversions are halted" does.
    pub note: NoteVm,
    /// Whether a conversion would be rejected right now.
    pub conversions_halted: bool,
    /// Blocks until the worst switch lands. **Zero means it is in force**, and
    /// the note says what it stops either way — the interface composes "in 40
    /// blocks" around the sentence rather than the core writing two of them.
    pub in_blocks: u32,
}

impl Status {
    /// There is nowhere to ask on this chain. Not a failure, and not a clear
    /// answer either.
    pub fn unconfigured() -> Self {
        Self {
            severity: Severity::Clear,
            note: NoteVm::plain("halt-unconfigured"),
            conversions_halted: false,
            in_blocks: 0,
        }
    }

    /// The read failed. **Never collapse this into clear** — a chain nobody
    /// could reach might be halted.
    pub fn unknown() -> Self {
        Self {
            severity: Severity::Unknown,
            note: NoteVm::plain("halt-unknown"),
            conversions_halted: false,
            in_blocks: 0,
        }
    }

    /// The key is absent, which is the healthy state.
    pub fn clear() -> Self {
        Self {
            severity: Severity::Clear,
            note: NoteVm::none(),
            conversions_halted: false,
            in_blocks: 0,
        }
    }
}

/// Read every descriptor under the oracle's key and reduce them to one status.
///
/// `values` is what the content map held; an empty slice is [`Status::clear`].
pub fn read(values: &[Vec<u8>], tip: u32) -> Status {
    let mut worst = Status::clear();

    for bytes in values {
        let Some(descriptor) = parse(bytes) else {
            // One unreadable entry is not the whole answer, and it is not
            // silence either: something is published that this build cannot
            // read, which is worth a line.
            let candidate = Status {
                severity: Severity::Info,
                note: NoteVm::plain("halt-unreadable"),
                conversions_halted: false,
                in_blocks: 0,
            };
            if candidate.severity > worst.severity {
                worst = candidate;
            }
            continue;
        };

        let active = descriptor.activation_height <= tip;
        let in_blocks = descriptor.activation_height.saturating_sub(tip);

        let (severity, note, halts) = match switch(&descriptor.upgrade) {
            Some(found) => {
                tracing::info!(
                    switch = found.name,
                    active,
                    height = descriptor.activation_height,
                    "the protocol has a change published on this chain",
                );
                (
                    // Pending demotes a halt to a warning and leaves everything
                    // else alone. Capping *everything* pending at warning made a
                    // scheduled `magicnumberfix`, which stops nothing, light the
                    // same alarm as an imminent trading halt.
                    if active || found.severity == Severity::Info {
                        found.severity
                    } else {
                        Severity::Warning
                    },
                    // The effect, always — whether it is in force or landing in
                    // forty blocks. **When** is `in_blocks`, and the interface
                    // composes the two. A note whose argument was another note's
                    // code would need `note.slint` to nest, and the one thing that
                    // file is is flat.
                    NoteVm::plain(found.says),
                    active && found.halts_conversion,
                )
            }
            // Reported under its raw hex rather than dropped. The table is a
            // convenience; the chain is the source of truth, and a switch
            // nobody has written down yet is exactly the one worth seeing.
            None => (
                Severity::Info,
                NoteVm::with("halt-unrecognised", [descriptor.upgrade.clone()]),
                false,
            ),
        };

        if severity > worst.severity || (halts && !worst.conversions_halted) {
            worst = Status {
                severity: severity.max(worst.severity),
                note,
                conversions_halted: halts || worst.conversions_halted,
                in_blocks: if active { 0 } else { in_blocks },
            };
        } else if halts {
            worst.conversions_halted = true;
        }
    }

    worst
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The descriptor VRSCTEST's oracle was publishing on 2026-08-20, read off
    /// the chain and pasted here whole.
    ///
    /// The one fixture in this file that nobody composed: if the encoding is
    /// misread in any of its five fields, this test says so.
    const TESTNET: &str = "018787a1035a9bd4179a3e0538ba9f90be7f231b69b0b588bac7b83800";

    const DISABLEDEFI: &str = "ba88b5b0691b237fbe909fba38053e9a17d49b5a";

    /// The trap the specification names: it is **not** CompactSize.
    ///
    /// A set continuation bit adds one as well as shifting, which is what makes
    /// each length's encoding unique. Read as CompactSize these give different
    /// numbers, and every one of them looks like a plausible block height.
    #[test]
    fn the_continuation_bit_adds_one() {
        assert_eq!(varint(&[0x00], 0), Some((0, 1)));
        assert_eq!(varint(&[0x01], 0), Some((1, 1)));
        assert_eq!(varint(&[0x7f], 0), Some((127, 1)));
        // The one the specification calls out by name.
        assert_eq!(varint(&[0x80, 0x00], 0), Some((128, 2)));
        assert_eq!(varint(&[0x80, 0x01], 0), Some((129, 2)));
    }

    /// A run that never ends, or ends past the buffer, is a refusal rather than
    /// a panic or a number.
    #[test]
    fn a_varint_that_runs_off_the_end_is_refused() {
        assert_eq!(varint(&[], 0), None);
        assert_eq!(varint(&[0x80], 0), None);
        assert_eq!(varint(&[0xff; 12], 0), None);
    }

    /// Every field of the real descriptor, including the byte order of the id.
    #[test]
    fn the_testnet_descriptor_reads_field_for_field() {
        let bytes = hex::decode(TESTNET).expect("the fixture is hex");
        let found = parse(&bytes).expect("it parses");

        assert_eq!(found.version, 1);
        assert_eq!(found.minimum_daemon, [1, 2, 17, 3], "1.2.17.3");
        assert_eq!(found.upgrade, DISABLEDEFI);
        assert_eq!(found.activation_height, 1_187_000);
        assert_eq!(found.activation_time, 0);
    }

    /// The id is little-endian on the wire. Forgetting to reverse it matches
    /// nothing in the table, so every switch would report as unrecognised —
    /// which reads as "nothing is wrong" on a chain that is halted.
    #[test]
    fn the_upgrade_id_is_reversed_before_it_is_matched() {
        let bytes = hex::decode(TESTNET).expect("hex");
        let found = parse(&bytes).expect("parses");
        let on_the_wire = "5a9bd4179a3e0538ba9f90be7f231b69b0b588ba";

        assert_eq!(found.upgrade, DISABLEDEFI);
        assert_ne!(found.upgrade, on_the_wire);
        assert!(switch(&found.upgrade).is_some(), "the table did not match");
        assert!(switch(on_the_wire).is_none());
    }

    /// In force, and conversions are refused.
    #[test]
    fn a_halt_below_the_tip_is_in_force() {
        let bytes = vec![hex::decode(TESTNET).expect("hex")];
        let status = read(&bytes, 1_197_271);

        assert_eq!(status.severity, Severity::Critical);
        assert!(status.conversions_halted);
        assert_eq!(status.note.code, "halt-conversions");
        assert_eq!(status.in_blocks, 0);
    }

    /// The same descriptor, before its height. A halt that has not landed is a
    /// warning with a countdown — reporting it as active would say trading has
    /// stopped when it has not, and the reverse is worse.
    #[test]
    fn a_halt_above_the_tip_is_scheduled_and_not_in_force() {
        let bytes = vec![hex::decode(TESTNET).expect("hex")];
        let status = read(&bytes, 1_186_960);

        assert_eq!(status.severity, Severity::Warning);
        assert!(
            !status.conversions_halted,
            "a scheduled halt must not stop anybody trading yet",
        );
        assert_eq!(
            status.note.code, "halt-conversions",
            "the note says what stops; `in_blocks` says when",
        );
        assert_eq!(status.in_blocks, 40);
    }

    /// An absent key is the healthy state, and it is the common one.
    #[test]
    fn nothing_published_is_clear() {
        let status = read(&[], 1_000_000);
        assert_eq!(status.severity, Severity::Clear);
        assert!(!status.conversions_halted);
        assert!(status.note.code.is_empty());
    }

    /// Missing data must never read as working.
    #[test]
    fn unknown_outranks_info_and_clear() {
        assert!(Severity::Unknown > Severity::Info);
        assert!(Severity::Unknown > Severity::Clear);
        assert!(Severity::Critical > Severity::Unknown);
        assert!(Severity::Warning > Severity::Info);
    }

    /// A switch nobody has written down is shown, not swallowed.
    #[test]
    fn an_unrecognised_switch_is_reported_under_its_hex() {
        // The real descriptor with one byte of the id changed.
        let mut bytes = hex::decode(TESTNET).expect("hex");
        bytes[5] ^= 0xff;

        let status = read(&[bytes], 1_197_271);
        assert_eq!(status.severity, Severity::Info);
        assert_eq!(status.note.code, "halt-unrecognised");
        assert!(!status.conversions_halted);
        assert_eq!(status.note.args.len(), 1);
    }

    /// One unreadable entry does not take the whole status with it — and it is
    /// not silence either.
    #[test]
    fn an_unreadable_entry_is_skipped_and_still_said() {
        let good = hex::decode(TESTNET).expect("hex");
        let status = read(&[vec![0xff, 0xff], good], 1_197_271);

        assert_eq!(
            status.severity,
            Severity::Critical,
            "the real one still read"
        );
        assert!(status.conversions_halted);
    }

    /// Housekeeping is not an alarm, scheduled or not.
    ///
    /// Eleven of the fourteen switches are `info`, and an earlier rule that
    /// capped everything pending at warning made a scheduled `magicnumberfix`
    /// — which stops nothing — light the same lamp as an imminent halt.
    #[test]
    fn scheduled_housekeeping_stays_quiet() {
        let magic = "b005d51d22735a9334679c293cbea2620a43e7f6";
        assert_eq!(
            switch(magic).expect("in the table").severity,
            Severity::Info
        );

        let mut bytes = hex::decode(TESTNET).expect("hex");
        let raw: Vec<u8> = hex::decode(magic).expect("hex").into_iter().rev().collect();
        bytes[5..25].copy_from_slice(&raw);

        // Well above the tip, so it is pending.
        let status = read(&[bytes], 1_000);
        assert_eq!(status.severity, Severity::Info);
        assert!(!status.conversions_halted);
    }

    /// The two bridge switches stop bridging and **not** conversions. Turning
    /// the whole screen off for one of those would be wrong in the expensive
    /// direction: it would stop somebody trading when they can.
    #[test]
    fn a_bridge_halt_does_not_stop_conversions() {
        for id in [
            "c8eb1ce97cc65b44b7c3b86315ef4419d5279ad9",
            "cb287a8f91a05bf453d6052b22e7f1bdd9e84ff9",
        ] {
            let found = switch(id).expect("in the table");
            assert_eq!(found.severity, Severity::Warning);
            assert!(
                !found.halts_conversion,
                "{} stopped conversions",
                found.name
            );
        }
    }
}
