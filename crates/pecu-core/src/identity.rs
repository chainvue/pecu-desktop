//! Reading VerusIDs: what state one is in, and what it has published.
//!
//! # Why the timelock is not read here
//!
//! An identity's lock is a flag and a number that mean different things
//! together — `unlock_after` is an absolute height when the identity is
//! unlocked and a relative delay when it is locked, and the same integer reads
//! as either. The SDK's `Timelock::of` is the one place that rule lives, so
//! this module builds an `Identity` carrying the two fields and asks it, rather
//! than restating three lines that would then have to be kept in step by hand.
//!
//! That costs a blank identity per call and buys the guarantee that a flag
//! added upstream changes this wallet's answer too.
//!
//! # Why content values are guessed at, carefully
//!
//! A VDXF key is a one-way hash of a name. The SDK says the consequence
//! plainly: for a key you did not create, *"you cannot recover the name, and
//! without the name there is nothing to tell you how its values are encoded"*.
//! So there is no decoder, and there cannot be one.
//!
//! What is possible is the forward direction — hash a name you already know and
//! see whether it matches — and a guess about the bytes that is honest about
//! being a guess. Real identities on VRSCTEST publish hex-encoded UTF-8, so
//! showing text when the bytes are valid UTF-8 is right far more often than it
//! is wrong, and the raw hex stays one click away either way.

use std::collections::BTreeMap;

use pecu_protocol::NoteVm;
use verus_sdk::currency::CurrencyId;
use verus_sdk::identity::{Identity, Timelock};
use verus_sdk::network::{ContentValue, IdentityAtAddress, IdentityRecord, RpcError};

/// What state an identity is in, as a person would describe it.
///
/// Four, not two. A wallet that renders "active" and "revoked" cannot say the
/// two things that actually stop somebody: that the funds are held, and that
/// the clock releasing them is already running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Ordinary. Signable, spendable, updatable.
    Active,
    /// Locked with a delay that has not started. Nobody has asked to unlock it
    /// yet; when somebody does, the wait is this many blocks.
    Locked { delay: u32 },
    /// The countdown is running. Funds are held until this height.
    Unlocking { at: u32 },
    /// Revoked by its revocation authority. Only recovery brings it back.
    Revoked,
}

impl Status {
    /// The word for a status pill.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Locked { .. } => "Locked",
            Self::Unlocking { .. } => "Unlocking",
            Self::Revoked => "Revoked",
        }
    }

    /// Which pill colour, in the vocabulary the UI already uses for nodes.
    pub fn tone(&self) -> &'static str {
        match self {
            Self::Active => "online",
            Self::Locked { .. } | Self::Unlocking { .. } => "degraded",
            Self::Revoked => "offline",
        }
    }
}

/// Read an identity's state from the two raw fields and the chain tip.
///
/// `tip` decides only whether a running countdown has finished; it is not
/// consulted for a lock that has not started, because such a lock has no end.
pub fn status(record: &IdentityAtAddress, tip: u32) -> Status {
    if record.is_revoked() {
        return Status::Revoked;
    }
    match timelock(record.flags, record.timelock) {
        Timelock::DelayAfterUnlock(delay) => Status::Locked { delay },
        Timelock::UntilBlock(at) if at > tip => Status::Unlocking { at },
        // No lock, or one whose height has passed. The two are the same state:
        // nothing clears `unlock_after` when a countdown elapses, so a stale
        // height is an identity that is simply unlocked — not one perpetually
        // about to be.
        Timelock::None | Timelock::UntilBlock(_) => Status::Active,
    }
}

/// The SDK's own reading of the flag-and-number pair.
///
/// Built rather than restated. `Timelock::of` looks at exactly `flags` and
/// `unlock_after`, so a blank identity carrying those two answers the same
/// question the real one would — and if the rule ever grows a third input, this
/// stops compiling instead of quietly disagreeing.
fn timelock(flags: u32, unlock_after: u32) -> Timelock {
    Timelock::of(&Identity {
        version: 3,
        flags,
        unlock_after,
        primary_addresses: Vec::new(),
        min_sigs: 1,
        parent: [0; 20],
        name: String::new(),
        content_multimap: Vec::new(),
        content_map: Vec::new(),
        revocation_authority: [0; 20],
        recovery_authority: [0; 20],
        private_addresses: Vec::new(),
        system_id: [0; 20],
    })
}

/// One published value, ready to put on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Value {
    /// The bytes as text, when they are valid UTF-8 and worth reading as such.
    /// Empty otherwise.
    pub text: String,
    /// Always present: the raw bytes as hex.
    pub hex: String,
    /// How many bytes, for the values that are not text.
    pub bytes: usize,
    /// The daemon recognised this key and rendered it. Its JSON, pretty.
    pub structured: String,
}

/// Turn a published value into something displayable, without claiming to know
/// what it means.
///
/// `Structured` is the daemon saying it recognised the key — the only structure
/// anybody gets for free, and it cannot be converted back to bytes, so it is
/// carried as its own field rather than replacing the hex.
pub fn value(raw: &ContentValue) -> Value {
    match raw {
        ContentValue::Structured(json) => Value {
            text: String::new(),
            hex: String::new(),
            bytes: 0,
            structured: serde_json::to_string_pretty(json).unwrap_or_default(),
        },
        ContentValue::Bytes(bytes) => Value {
            text: readable(bytes),
            hex: hex::encode(bytes),
            bytes: bytes.len(),
            structured: String::new(),
        },
    }
}

/// The bytes as text, if reading them that way is defensible.
///
/// Valid UTF-8 is not enough on its own — a 32-byte hash is valid UTF-8 often
/// enough to matter, and rendering one as mojibake is worse than rendering it
/// as hex. So control characters disqualify it, which is what separates a
/// published string from a published digest that happens to decode.
fn readable(bytes: &[u8]) -> String {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return String::new();
    };
    if text.is_empty()
        || text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return String::new();
    }
    text.to_string()
}

/// VDXF names this wallet can recognise, hashed forward.
///
/// The map is key i-address → the URI it came from. Built once, offline: the
/// derivation is a hash of a name and needs no node.
///
/// It will never be complete, and that is the nature of the thing rather than a
/// gap to close — anybody can publish under any name. An unmatched key is shown
/// as its i-address and said to be unrecoverable, which is true.
pub struct Names(BTreeMap<String, String>);

/// The URIs worth trying. Verus's own conventions, plus the ones a wallet is
/// most likely to meet.
const KNOWN: &[&str] = &[
    "vrsc::identity.profile",
    "vrsc::identity.public",
    "vrsc::identity.contact",
    "vrsc::identity.email",
    "vrsc::identity.name",
    "vrsc::identity.avatar",
    "vrsc::system.currency.export",
    "vrsc::system.currency.import",
];

impl Names {
    /// Derive the table for one chain.
    ///
    /// `chain_id` is the chain's own i-address, which the wallet already knows
    /// from `chain_info`. A wrong chain here derives keys that match nothing,
    /// which shows up as every key being unnamed rather than as a wrong name.
    pub fn derive(chain_name: &str, chain_id: CurrencyId) -> Self {
        let mut table = BTreeMap::new();
        for uri in KNOWN {
            if let Ok(key) = verus_sdk::vdxf::qualified_key(uri, chain_name, chain_id) {
                table.insert(key_address(key), (*uri).to_string());
            }
        }
        Self(table)
    }

    /// The URI a key came from, if this wallet knows one that hashes to it.
    pub fn name_of(&self, key_address: &str) -> Option<&str> {
        self.0.get(key_address).map(String::as_str)
    }

    /// Derive one key from a URI somebody typed, for the try-a-key search.
    ///
    /// The forward direction is the only direction there is. The SDK is explicit
    /// that this is a search rather than a lookup, and the interface says so.
    ///
    /// # Errors
    ///
    /// If the URI is not one the SDK's own resolver accepts.
    pub fn derive_one(uri: &str, chain_name: &str, chain_id: CurrencyId) -> Result<String, String> {
        verus_sdk::vdxf::qualified_key(uri, chain_name, chain_id)
            .map(key_address)
            .map_err(|error| error.to_string())
    }
}

/// The chain's own currency id, from the i-address a node reports it as.
///
/// The wallet already caches this as `native_currency`; this exists for the
/// cases that have the string and not the id, and for the tests.
pub fn currency_of(i_address: &str) -> CurrencyId {
    i_address
        .parse::<verus_sdk::verus_keys::Address>()
        .map_or_else(
            |_| CurrencyId::from_bytes([0; 20]),
            |a| CurrencyId::from_bytes(a.hash()),
        )
}

// ── Turning what the chain said into what a person reads ────────────────────

/// One row of the list, from the address-scoped lookup.
///
/// `chain_name` qualifies the bare name the reply carries — `demo` becomes
/// `demo.VRSCTEST@` — but only when the identity's parent really is the chain.
/// A sub-identity under some other parent keeps its bare name rather than being
/// given a qualification that is wrong.
pub fn row(
    record: &IdentityAtAddress,
    tip: u32,
    chain_name: Option<&str>,
    mine: bool,
) -> pecu_protocol::IdentityVm {
    let state = status(record, tip);
    pecu_protocol::IdentityVm {
        name: qualified(&record.name, chain_name),
        address: record.identity_address.clone(),
        status: state.label().to_string(),
        tone: state.tone().to_string(),
        note: note(&state),
        mine,
    }
}

/// One row, from a full record — which is what a lookup returns.
pub fn row_of(record: &IdentityRecord, tip: u32, mine: &[String]) -> pecu_protocol::IdentityVm {
    let at = as_at_address(record);
    let state = status(&at, tip);
    pecu_protocol::IdentityVm {
        name: pecu_protocol::format::safe_name(&record.fully_qualified_name),
        address: record.identity_address.clone(),
        status: state.label().to_string(),
        tone: state.tone().to_string(),
        note: note(&state),
        mine: holds_a_key(record, mine) > 0,
    }
}

/// The sentence under a status, for the states where the word alone is not
/// enough. Empty for Active — a row explaining "Active" is a row nobody reads.
fn note(state: &Status) -> NoteVm {
    match state {
        Status::Active => NoteVm::none(),
        Status::Locked { delay } => NoteVm::with("identity-locked", [delay.to_string()]),
        Status::Unlocking { at } => NoteVm::with("identity-unlocking", [at.to_string()]),
        Status::Revoked => NoteVm::plain("identity-revoked"),
    }
}

/// `name` qualified into `name.CHAIN@`, when that is true.
fn qualified(name: &str, chain_name: Option<&str>) -> String {
    match chain_name {
        Some(chain) if !name.is_empty() => format!("{name}.{chain}@"),
        _ => name.to_string(),
    }
}

/// Read the two timelock fields back out of a full record.
///
/// The flags and the timelock live in the identity object either way; this
/// borrows the same reading the list uses, so a lookup and a listing cannot
/// disagree about whether something is locked.
fn as_at_address(record: &IdentityRecord) -> IdentityAtAddress {
    IdentityAtAddress {
        identity_address: record.identity_address.clone(),
        name: record.identity["name"].as_str().unwrap_or_default().into(),
        parent: record.identity["parent"]
            .as_str()
            .unwrap_or_default()
            .into(),
        flags: u32::try_from(record.identity["flags"].as_u64().unwrap_or(0)).unwrap_or(0),
        timelock: u32::try_from(record.identity["timelock"].as_u64().unwrap_or(0)).unwrap_or(0),
        outpoint: record.outpoint,
        identity: record.identity.clone(),
    }
}

/// How many of the identity's primary addresses this wallet holds a key for.
fn holds_a_key(record: &IdentityRecord, mine: &[String]) -> usize {
    record.identity["primaryaddresses"]
        .as_array()
        .map_or(0, |all| {
            all.iter()
                .filter_map(|one| one.as_str())
                .filter(|one| mine.iter().any(|held| held == one))
                .count()
        })
}

/// A VDXF key as the i-address a content map is keyed by.
///
/// **Not hex.** The same identity object spells its `contentmap` keys as hex and
/// its `contentmultimap` keys as i-addresses, and comparing a derived key
/// against the wrong rendering finds nothing, silently.
fn key_address(key: [u8; 20]) -> String {
    verus_sdk::verus_keys::Address::new(verus_sdk::verus_keys::AddressKind::Identity, key)
        .to_string()
}

// ── Reading one, in full ────────────────────────────────────────────────────

/// Everything the detail sheet is built from. **Blocking.**
pub struct Detail {
    pub record: verus_sdk::network::IdentityRecord,
    /// What the identity holds now.
    pub content: BTreeMap<String, Vec<ContentValue>>,
    /// Every value ever published under each key, which is a different
    /// question and a different request. Empty when the node will not answer —
    /// a public endpoint behind a method allowlist is a normal thing to meet,
    /// and it must cost the history view rather than the whole sheet.
    pub history: BTreeMap<String, Vec<ContentValue>>,
    /// Native value held by the identity itself, which is not the same money as
    /// the key that controls it.
    pub balance: verus_sdk::money::Amount,
}

/// Look one identity up completely. **Blocking** — dispatch it off the actor.
///
/// Three requests at most, and only the first is allowed to fail the read: an
/// identity nobody can fetch is nothing to show, while a history the node
/// declines and a balance it cannot total are both survivable and are reported
/// as absent rather than as an error.
///
/// # Errors
///
/// When the identity itself cannot be read — including `-5`, which is the
/// daemon saying it does not exist.
pub fn read(chain: &pecu_chain::Chain, name_or_id: &str) -> Result<Detail, RpcError> {
    use verus_sdk::network::ChainReader;

    let record = chain.identity(name_or_id)?;

    // The current content, off the identity object itself. Deliberately not
    // `identity_content`, which answers about the whole chain's worth of
    // history — see the two fields on `Detail`.
    // Reached through `verus_rpc` rather than the `network` facade, which does
    // not re-export it. Same function either way.
    let content = verus_sdk::verus_rpc::content_multimap(&record.identity).unwrap_or_default();

    let history = chain
        .identity_content(name_or_id)
        .map(|whole| whole.content_multimap)
        .unwrap_or_default();

    // What the identity itself holds, which is not the money of the key that
    // controls it. Outputs rather than a total, so they are summed here — and
    // a sum that would overflow is reported as nothing rather than as a wrong
    // number.
    let balance = verus_sdk::network::identity_held(chain, &record.identity_address)
        .ok()
        .and_then(|held| verus_sdk::money::Amount::checked_sum(held.iter().map(|u| u.satoshis)))
        .unwrap_or(verus_sdk::money::Amount::ZERO);

    Ok(Detail {
        record,
        content,
        history,
        balance,
    })
}

/// Everything the detail sheet shows, in the words it shows it in.
///
/// This is where the three facts that make VerusIDs dangerous get said out
/// loud, because none of them is visible in the raw fields:
///
/// * an identity whose recovery authority is itself **cannot be revoked**, and
///   that is the default a fresh registration lands on;
/// * a lock with no countdown does not end on its own;
/// * a countdown ends at a height, not "soon".
pub fn detail(
    read: &Detail,
    mine: &[String],
    names: Option<&Names>,
) -> pecu_protocol::IdentityDetailVm {
    let record = &read.record;
    let at = as_at_address(record);
    let state = status(&at, 0);

    let primary: Vec<String> = record.identity["primaryaddresses"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter_map(|one| one.as_str())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    let required =
        u32::try_from(record.identity["minimumsignatures"].as_u64().unwrap_or(1)).unwrap_or(1);
    let held = holds_a_key(record, mine);

    let recovery = record.identity["recoveryauthority"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    pecu_protocol::IdentityDetailVm {
        name: pecu_protocol::format::safe_name(&record.fully_qualified_name),
        address: record.identity_address.clone(),
        status: state.label().to_string(),
        tone: state.tone().to_string(),
        signatures_required: format!("{required} of {}", primary.len()),
        can_sign: held >= required as usize,
        control_note: if held == 0 {
            NoteVm::plain("control-none")
        } else if held >= required as usize {
            NoteVm::with("control-enough", [held.to_string(), required.to_string()])
        } else {
            NoteVm::with("control-short", [held.to_string(), required.to_string()])
        },
        primary_addresses: primary,
        revocation_authority: record.identity["revocationauthority"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        // The default a registration lands on, and the one nobody is told
        // about: consensus refuses a revocation whose subject is its own
        // recovery authority, so the identity has no way back from one.
        cannot_be_revoked: recovery == record.identity_address,
        recovery_authority: recovery,
        timelock_note: timelock_note(&state),
        balance_display: crate::portfolio::coins(read.balance),
        content: entries(&read.content, names),
        content_history: entries(&read.history, names),
    }
}

/// The lock, in a sentence somebody can act on.
fn timelock_note(state: &Status) -> NoteVm {
    match state {
        Status::Locked { delay } => NoteVm::with("timelock-held", [delay.to_string()]),
        Status::Unlocking { at } => NoteVm::with("timelock-counting", [at.to_string()]),
        Status::Revoked => NoteVm::plain("timelock-revoked"),
        Status::Active => NoteVm::plain("timelock-none"),
    }
}

/// A content map, keyed and decoded for display.
fn entries(
    map: &BTreeMap<String, Vec<ContentValue>>,
    names: Option<&Names>,
) -> Vec<pecu_protocol::ContentEntryVm> {
    map.iter()
        .map(|(key, values)| pecu_protocol::ContentEntryVm {
            name: names
                .and_then(|table| table.name_of(key))
                .unwrap_or_default()
                .to_string(),
            key: key.clone(),
            values: values
                .iter()
                .map(|raw| {
                    let shown = value(raw);
                    pecu_protocol::ContentValueVm {
                        size: if shown.bytes == 0 {
                            String::new()
                        } else {
                            format!("{} bytes", shown.bytes)
                        },
                        text: shown.text,
                        hex: shown.hex,
                        structured: shown.structured,
                    }
                })
                .collect(),
        })
        .collect()
}

// ── Changing one ────────────────────────────────────────────────────────────

/// What somebody asked to change about an identity.
///
/// Deliberately not a mirror of the SDK's `IdentityChange`. That type can
/// express things this wallet does not offer yet — the content map, the private
/// addresses — and an enum of the operations actually on screen is what keeps
/// the review able to describe what is about to happen in a sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Point revocation, recovery, or both at another identity.
    ///
    /// The one that matters most: an identity that is its own recovery
    /// authority cannot be revoked, and this is how that gets fixed.
    Authorities {
        revocation: Option<String>,
        recovery: Option<String>,
    },
    /// Hold the funds until somebody asks, and then for this many blocks.
    Lock { delay: u32 },
    /// Start the countdown. **Does not unlock anything** — see [`unlock_note`].
    Unlock { extra_blocks: u32 },
    /// Revoke it. Signed by the revocation authority's keys, not the
    /// identity's, and undone only by the recovery authority.
    Revoke,
    /// Bring a revoked identity back. Signed by the recovery authority.
    Recover,
}

impl Change {
    /// What this will do, in a sentence, before it is signed.
    pub fn describe(&self) -> NoteVm {
        match self {
            Self::Authorities {
                revocation,
                recovery,
            } => match (revocation.as_deref(), recovery.as_deref()) {
                (None, None) => NoteVm::plain("change-nothing"),
                (Some(who), None) => NoteVm::with("change-revocation", [who.to_string()]),
                (None, Some(who)) => NoteVm::with("change-recovery", [who.to_string()]),
                (Some(revoke), Some(recover)) => NoteVm::with(
                    "change-both-authorities",
                    [revoke.to_string(), recover.to_string()],
                ),
            },
            Self::Lock { delay } => NoteVm::with("change-lock", [delay.to_string()]),
            Self::Unlock { .. } => unlock_note(),
            Self::Revoke => NoteVm::plain("change-revoke"),
            Self::Recover => NoteVm::plain("change-recover"),
        }
    }

    /// Whether this moves who controls the identity, which the SDK gates behind
    /// an explicit opt-in and which deserves its own confirmation here too.
    pub fn changes_authority(&self) -> bool {
        matches!(self, Self::Authorities { .. })
    }

    /// Whether this needs a word typed before it is sent.
    ///
    /// Only revocation. Everything else here is undoable by somebody who holds
    /// the keys; a revocation is undoable only by the recovery authority, and
    /// on a self-authority identity by nobody.
    pub fn needs_typed_confirmation(&self) -> bool {
        matches!(self, Self::Revoke)
    }
}

/// A signed change waiting to be sent.
///
/// An enum rather than three maps, because the three flows return three
/// different `Unsent<T>` and the ticket that reaches the UI must not have to
/// know which. What the UI does with any of them is identical: show it, then
/// send it or throw it away.
pub enum Prepared {
    Updated(verus_sdk::network::Unsent<verus_sdk::network::Updated>),
    Revoked(verus_sdk::network::Unsent<verus_sdk::network::Revoked>),
    Recovered(verus_sdk::network::Unsent<verus_sdk::network::Recovered>),
}

impl Prepared {
    pub fn fee(&self) -> verus_sdk::money::Amount {
        match self {
            Self::Updated(unsent) => unsent.outcome.fee,
            Self::Revoked(unsent) => unsent.outcome.fee,
            Self::Recovered(unsent) => unsent.outcome.fee,
        }
    }

    /// Send it. Takes a broadcaster, which needs a permit — the same gate every
    /// other write in this application goes through.
    pub fn broadcast(
        self,
        broadcaster: &pecu_chain::Permitted<'_>,
    ) -> Result<String, verus_sdk::network::FlowError> {
        match self {
            Self::Updated(unsent) => unsent.broadcast(broadcaster).map(|done| done.txid),
            Self::Revoked(unsent) => unsent.broadcast(broadcaster).map(|done| done.txid),
            Self::Recovered(unsent) => unsent.broadcast(broadcaster).map(|done| done.txid),
        }
    }
}

/// The word somebody has to type before a revocation is sent.
///
/// Defined in `pecu-protocol` and re-exported here, because it is a term of
/// the contract rather than a detail of this module: the core prints it in an
/// event, the interface prompts with it and gates a button on it, and the core
/// checks what comes back. Four readers, one of them across the boundary — so
/// it lives in the one crate both halves are allowed to name.
pub use pecu_protocol::REVOKE_CONFIRMATION;

/// What unlocking actually does, said plainly.
///
/// The single most misleading operation in this whole feature. Consensus
/// measures the published unlock height from the transaction's own expiry
/// rather than from the tip, so the identity stays locked until the chain
/// passes it. A screen that says "Unlocked" when the transaction confirms is
/// lying for as long as the delay lasts.
pub fn unlock_note() -> NoteVm {
    NoteVm::plain("change-unlock")
}

/// Turn a described change into the SDK's own shape.
///
/// # Errors
///
/// If an authority was named and does not parse as an i-address. Names are
/// resolved before this is called, because resolving costs a request and this
/// runs on the actor.
pub fn as_sdk_change(change: &Change) -> Result<verus_sdk::network::IdentityChange, String> {
    use verus_sdk::network::IdentityChange;

    Ok(match change {
        Change::Authorities {
            revocation,
            recovery,
        } => {
            let mut built = IdentityChange::new();
            if let Some(who) = revocation {
                built = built.with_revocation_authority(authority_hash(who)?);
            }
            if let Some(who) = recovery {
                built = built.with_recovery_authority(authority_hash(who)?);
            }
            // The SDK refuses an authority move without this, and it is right
            // to: the field decides who owns the identity from then on.
            built.allowing_authority_change()
        }
        Change::Lock { delay } => IdentityChange::new()
            .with_timelock(verus_sdk::identity::Timelock::DelayAfterUnlock(*delay)),
        // These three are not `IdentityChange`s at all: each needs to read the
        // identity first — the current delay, or the authority that has to
        // sign — so the SDK gives each its own function.
        Change::Unlock { .. } | Change::Revoke | Change::Recover => {
            return Err("this change does not go through an identity change".to_string())
        }
    })
}

fn authority_hash(who: &str) -> Result<[u8; 20], String> {
    who.parse::<verus_sdk::verus_keys::Address>()
        .map(|address| address.hash())
        .map_err(|error| format!("{who} is not an identity address: {error}"))
}

// ── Claiming a name ─────────────────────────────────────────────────────────

/// What the SDK will accept as an identity name, checked offline.
///
/// The rule is the SDK's, and it is deliberately narrower than consensus: a
/// name with whitespace, a dot or a capital letter derives an id the registrant
/// may not expect, and that mistake is only visible after the fee is spent.
/// There is no exported validator, so this states the rule the SDK's own
/// `validate_name` applies — and [`check_name`] is tested against
/// `NameReservation::new`, which is what actually enforces it.
pub fn name_problem(name: &str) -> Option<NoteVm> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if name.len() > 64 {
        return Some(NoteVm::plain("name-too-long"));
    }
    let bad: String = name
        .chars()
        .filter(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-'))
        .collect();
    if !bad.is_empty() {
        return Some(NoteVm::with("name-bad-characters", [bad]));
    }
    None
}

/// The three transactions a claim is made of, and how far it has got.
///
/// # Why the middle one is a step rather than a spinner
///
/// Because it has a deadline. The commitment expires about twenty blocks after
/// it is signed, the expiry is inside the bytes the signature covers so it
/// cannot be extended, and missing it spends the fee for nothing. A wait with a
/// clock on it is a step somebody may have to act on.
///
/// `step` is [`pecu_protocol::RegistrationVm::step`]. An empty string means
/// nothing has been sent — which is what the review draws, as a plan.
///
/// # Why a failed claim gets no diagram
///
/// `expired` and `lost` are not positions along this journey; they are the
/// journey stopping. Marking the last reached step `failed` and the rest
/// `later` would draw a path somebody might still walk. The note above it
/// already says the fee is spent and the name was not registered, which is the
/// whole of what is true.
pub fn progress(step: &str) -> Vec<pecu_protocol::FlowStepVm> {
    const STEPS: [(&str, bool); 3] = [
        ("step-claim-the-name", true),
        ("step-wait-to-confirm", false),
        ("step-register-it", true),
    ];

    // How many are behind us, and whether the one we are on has been reached at
    // all. `reserved` is signed and not sent, so nothing is done yet.
    let reached: usize = match step {
        "" | "reserved" => 0,
        "committed" | "waiting" => 1,
        "ready" | "registering" => 2,
        "done" => 3,
        // Terminal without finishing.
        _ => return Vec::new(),
    };

    // Nothing sent at all: every step is ahead, which is what a review shows.
    let pending = step.is_empty();

    STEPS
        .iter()
        .enumerate()
        .map(|(index, (label, costs))| {
            let state = if pending {
                "later"
            } else {
                match index.cmp(&reached) {
                    std::cmp::Ordering::Less => "done",
                    std::cmp::Ordering::Equal => "now",
                    std::cmp::Ordering::Greater => "later",
                }
            };
            pecu_protocol::FlowStepVm::new(NoteVm::plain(label), state, *costs)
        })
        .collect()
}

pub fn registration_view(
    record: &crate::registration::Record,
    status: Option<&verus_sdk::network::CommitmentStatus>,
    tip: u32,
    busy: bool,
) -> pecu_protocol::RegistrationVm {
    use verus_sdk::network::CommitmentStatus;

    let expiry = record.pending.expiry_height();
    let deadline = match expiry {
        Some(at) if at > tip => {
            NoteVm::with("claim-deadline", [at.to_string(), (at - tip).to_string()])
        }
        Some(at) => NoteVm::with("claim-deadline-passed", [at.to_string()]),
        None => NoteVm::none(),
    };

    let (step, note) = match status {
        None => match record.step {
            crate::registration::Step::Reserved => {
                ("reserved", NoteVm::plain("claim-signed-not-sent"))
            }
            crate::registration::Step::Committed => {
                ("committed", NoteVm::plain("claim-on-its-way"))
            }
        },
        Some(CommitmentStatus::Waiting { confirmations }) => (
            "waiting",
            NoteVm::with("claim-waiting", [confirmations.to_string()]),
        ),
        Some(CommitmentStatus::Ready(_)) => ("ready", NoteVm::plain("claim-confirmed")),
        Some(CommitmentStatus::Reorged { detail }) => {
            ("waiting", NoteVm::with("claim-reorged", [detail.clone()]))
        }
        Some(CommitmentStatus::CommitmentGone) => ("lost", NoteVm::plain("claim-gone")),
        Some(CommitmentStatus::Expired { expiry_height, .. }) => (
            "expired",
            NoteVm::with("claim-expired", [expiry_height.to_string()]),
        ),
        // `CommitmentStatus` is `#[non_exhaustive]`, so a variant added upstream
        // lands here rather than failing to compile. Saying so is better than
        // guessing which of the others it resembles.
        Some(_) => ("waiting", NoteVm::plain("claim-unrecognised")),
    };

    pecu_protocol::RegistrationVm {
        name: record.name.clone(),
        step: step.to_string(),
        note,
        deadline,
        fee_display: crate::portfolio::coins(record.pending.registration_fee),
        address: String::new(),
        busy,
        cannot_be_revoked: record.pending.recovery_authority.is_none(),
        steps: progress(step),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chain's steps: three transactions, two of which are paid for.
    #[test]
    fn a_claim_is_three_transactions_and_two_fees() {
        let planned = progress("");
        assert_eq!(planned.len(), 3);
        assert!(
            planned.iter().all(|step| step.state == "later"),
            "a review drew a transaction as though it had happened",
        );
        assert_eq!(
            planned.iter().filter(|step| step.costs).count(),
            2,
            "the diagram did not say which of the three cost money",
        );

        // Signed and written down, not sent: still nothing done.
        assert_eq!(progress("reserved")[0].state, "now");
        assert_eq!(
            progress("reserved")
                .iter()
                .filter(|s| s.state == "done")
                .count(),
            0,
        );

        assert_eq!(progress("committed")[1].state, "now");
        assert_eq!(progress("waiting")[1].state, "now");
        assert_eq!(progress("ready")[2].state, "now");
        assert!(progress("done").iter().all(|step| step.state == "done"));
    }

    /// A claim that ended without finishing is not a claim in progress.
    ///
    /// `expired` and `lost` are the two ways this flow ends with the fee spent
    /// and no name. Drawing them as a step along the way — done, now or later —
    /// would be untrue in all three spellings, and "later" in particular would
    /// suggest waiting is still worth something.
    #[test]
    fn a_claim_that_died_draws_no_journey() {
        assert!(progress("expired").is_empty());
        assert!(progress("lost").is_empty());
    }

    fn record(flags: u32, timelock: u32) -> IdentityAtAddress {
        IdentityAtAddress {
            identity_address: "iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".to_string(),
            name: "demo".to_string(),
            parent: String::new(),
            flags,
            timelock,
            outpoint: (verus_sdk::money::Txid::from_internal([0; 32]), 0),
            identity: serde_json::json!({}),
        }
    }

    /// The four states, including the two a two-state wallet cannot express.
    #[test]
    fn a_lock_that_has_not_started_reads_differently_from_one_counting_down() {
        const LOCKED: u32 = 0x2;
        const REVOKED: u32 = 0x8000;
        let tip = 1_000_000;

        assert_eq!(status(&record(0, 0), tip), Status::Active);
        assert_eq!(status(&record(REVOKED, 0), tip), Status::Revoked);

        // The flag set: the number is a DELAY, and no height it could be
        // compared against means anything.
        assert_eq!(
            status(&record(LOCKED, 100), tip),
            Status::Locked { delay: 100 },
            "a delay was read as a height",
        );

        // The flag clear and the number in the future: an absolute height.
        assert_eq!(
            status(&record(0, tip + 500), tip),
            Status::Unlocking { at: tip + 500 },
        );

        // The same field in the past is inert. Nothing clears it when the
        // countdown elapses, so treating a stale height as "still unlocking"
        // would leave an ordinary identity permanently mislabelled.
        assert_eq!(status(&record(0, tip - 500), tip), Status::Active);

        // Revocation outranks a lock: an identity that is both is one nobody
        // can use, and saying "Locked" would suggest waiting is the fix.
        assert_eq!(status(&record(REVOKED | LOCKED, 100), tip), Status::Revoked);
    }

    /// Text when the bytes are text, hex when they are not — and hex always.
    #[test]
    fn a_published_value_is_shown_as_text_only_when_that_is_defensible() {
        // What VRSCTEST identities actually carry: hex-encoded UTF-8.
        let words = ContentValue::Bytes(b"first value, must survive".to_vec());
        let shown = value(&words);
        assert_eq!(shown.text, "first value, must survive");
        assert!(shown.hex.starts_with("66697273"), "{}", shown.hex);

        // A digest. Valid UTF-8 is not the test — control bytes are — because a
        // hash rendered as mojibake is worse than one rendered as hex.
        let digest = ContentValue::Bytes(vec![0x01, 0x87, 0x87, 0xa1, 0x03]);
        let shown = value(&digest);
        assert!(shown.text.is_empty(), "a digest was shown as text");
        assert_eq!(shown.bytes, 5);
        assert_eq!(shown.hex, "018787a103");

        // Ascii that happens to contain a NUL is not a string either.
        let padded = ContentValue::Bytes(b"name\0\0\0".to_vec());
        assert!(value(&padded).text.is_empty());

        // The daemon's own rendering is carried whole, and does not pretend to
        // be bytes — it cannot be turned back into any.
        let structured = ContentValue::Structured(serde_json::json!({"version": 1}));
        let shown = value(&structured);
        assert!(shown.structured.contains("\"version\""));
        assert!(shown.hex.is_empty());
    }

    /// The claim's own state is read from the two fields that carry it.
    ///
    /// The deadline is the fact this view exists to surface: the window is about
    /// twenty blocks, the expiry lives inside the bytes the claim was signed
    /// with so it cannot be extended, and missing it spends the fee for
    /// nothing. A panel that shows a spinner and not that is hiding the only
    /// thing somebody could act on.
    #[test]
    fn a_claim_says_when_it_runs_out_and_what_that_costs() {
        use verus_sdk::network::CommitmentStatus;

        let record = planted();
        let expiry = record.pending.expiry_height().expect("an expiry");

        // Waiting, with room left.
        let view = registration_view(&record, None, expiry - 12, false);
        assert_eq!(view.step, "committed");
        assert_eq!(view.deadline.code, "claim-deadline");
        assert_eq!(
            view.deadline.args.first().map(String::as_str),
            Some(expiry.to_string().as_str()),
            "the deadline does not name the block: {:?}",
            view.deadline
        );

        // Past it. The wording changes rather than the number vanishing —
        // "about -3 minutes" would be worse than saying it has gone.
        let view = registration_view(&record, None, expiry + 5, false);
        assert_eq!(
            view.deadline.code, "claim-deadline-passed",
            "{:?}",
            view.deadline
        );

        // And when the node says so outright, the panel says what it cost. Not
        // "try again": the same claim cannot be sent again, because the expiry
        // is inside the bytes it was signed with.
        let view = registration_view(
            &record,
            Some(&CommitmentStatus::Expired {
                expiry_height: expiry,
                tip: expiry + 1,
            }),
            expiry + 1,
            false,
        );
        assert_eq!(view.step, "expired");
        // The code, not the sentence: what this test is about is that an
        // expired claim is reported as expired rather than as something a
        // retry could fix, and the words for that live in `note.slint`.
        assert_eq!(view.note.code, "claim-expired");
        assert_eq!(view.note.args, vec![expiry.to_string()]);

        // Both authorities left at the default, so the identity would land
        // unrevokable — and the screen has to be able to say so while somebody
        // is still looking at it.
        assert!(view.cannot_be_revoked);
    }

    /// A reservation built against a scripted chain, salt and all.
    fn planted() -> crate::registration::Record {
        use verus_flows::testing::ScriptedReader;
        use verus_sdk::verus_keys::PrivateKey;

        let key = PrivateKey::from_bytes(&[7u8; 32], true).expect("a fixed scalar is a key");
        let reader = ScriptedReader::new(1_000_000)
            .with_utxo(&key.address().to_string(), 999_000, 200 * 100_000_000)
            .with_policy(verus_sdk::network::CurrencyPolicy {
                currency_id: "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq".to_string(),
                name: "VRSCTEST".to_string(),
                id_registration_fee: verus_sdk::money::Amount::from_sat(100 * 100_000_000),
                id_referral_levels: 3,
                id_import_fee: verus_sdk::money::Amount::ZERO,
                currency_registration_fee: verus_sdk::money::Amount::ZERO,
                proof_protocol: 1,
            });

        crate::registration::Record {
            name: "pecu".to_string(),
            key_label: "main".to_string(),
            step: crate::registration::Step::Committed,
            pending: verus_flows::prepare_registration_with_salt(
                &reader,
                &key,
                "pecu",
                &verus_flows::RegistrationOptions::default(),
                [0x5a; 32],
            )
            .expect("the reservation builds"),
        }
    }

    /// Unlocking says what it does, and what it does is not unlocking.
    ///
    /// The most misleading operation in this feature. Consensus measures the
    /// published unlock height from the transaction's own expiry rather than
    /// from the tip, so the identity stays locked until the chain passes it. A
    /// screen that says "Unlocked" when the transaction confirms is lying for as
    /// long as the delay lasts, and this is the wording that stops it.
    #[test]
    fn the_change_descriptions_do_not_promise_what_they_cannot_do() {
        // The **code**, not the sentence — the words are in `note.slint`
        // now, and `pecu-ui`'s `note_coverage.rs` is what guarantees each of
        // these has one. What this pins is that the four changes are told apart
        // at all: an unlock described with the lock's reason would promise to
        // unlock something that stays held.
        assert_eq!(
            Change::Unlock { extra_blocks: 20 }.describe().code,
            "change-unlock",
        );

        let lock = Change::Lock { delay: 100 }.describe();
        assert_eq!(lock.code, "change-lock");
        assert_eq!(lock.args, vec!["100".to_string()], "the wait is not named");

        // Handing an authority away is the one that cannot be taken back, and
        // the sentence has to say so before it is signed.
        let away = Change::Authorities {
            revocation: None,
            recovery: Some("iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".to_string()),
        };
        let text = away.describe();
        assert_eq!(text.code, "change-recovery");
        assert_eq!(
            text.args,
            vec!["iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".to_string()]
        );
        assert!(away.changes_authority());

        // And an authority change carries the SDK's explicit opt-in, without
        // which it would be refused after signing rather than before.
        let built = as_sdk_change(&away).expect("it builds");
        assert!(built.allow_authority_change);
        assert!(built.recovery_authority.is_some());
        assert!(
            built.revocation_authority.is_none(),
            "it changed one nobody asked about"
        );

        // Nothing to do is said rather than silently building an empty change,
        // which the SDK refuses anyway.
        let nothing = Change::Authorities {
            revocation: None,
            recovery: None,
        };
        assert_eq!(nothing.describe().code, "change-nothing");
    }

    /// Only the revocation asks for a word, and its sentence says why.
    ///
    /// Every other change here can be undone by whoever holds the keys. A
    /// revocation can be undone only by the recovery authority — and on an
    /// identity that is its own recovery authority, by nobody. That asymmetry
    /// is the whole reason for the extra step, and the wording has to carry it
    /// or the step is just friction.
    #[test]
    fn only_the_change_that_cannot_be_undone_asks_for_a_word() {
        assert!(Change::Revoke.needs_typed_confirmation());

        for ordinary in [
            Change::Lock { delay: 10 },
            Change::Unlock { extra_blocks: 10 },
            Change::Recover,
            Change::Authorities {
                revocation: None,
                recovery: Some("iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".to_string()),
            },
        ] {
            assert!(
                !ordinary.needs_typed_confirmation(),
                "{ordinary:?} asks for a word it does not need",
            );
        }

        assert_eq!(Change::Revoke.describe().code, "change-revoke");

        // Recovery says what it restores, and deliberately does not offer to
        // move the primary addresses — which a recovery legitimately may do,
        // and which would be the most dangerous default in the application.
        assert_eq!(Change::Recover.describe().code, "change-recover");
    }

    /// The forward hash agrees with what the daemon derives for the same URI.
    ///
    /// Pinned against `getvdxfid("vrsc::identity.profile")` on VRSCTEST, read
    /// from the live endpoint on 2026-08-13. This is the whole basis for naming
    /// a key at all: if the derivation drifts, every key silently becomes
    /// unnamed, which looks exactly like an identity that published nothing
    /// recognisable.
    #[test]
    fn a_known_name_hashes_to_the_key_the_daemon_derives() {
        const VRSCTEST: &str = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq";
        const PROFILE: &str = "iJ1BsyA9mx5RVk3ePK2WDgFcFCcfsXkBbA";

        let chain = currency_of(VRSCTEST);
        let one = Names::derive_one("vrsc::identity.profile", "VRSCTEST", chain)
            .expect("a known URI derives");
        assert_eq!(one, PROFILE);

        let table = Names::derive("VRSCTEST", chain);
        assert_eq!(table.name_of(PROFILE), Some("vrsc::identity.profile"));
        // A key nobody here published a name for stays unnamed rather than
        // being guessed at.
        assert_eq!(table.name_of("i87QZVSS7SosM5choTJE7Dy4SNRt5vAEhr"), None);
    }
}
