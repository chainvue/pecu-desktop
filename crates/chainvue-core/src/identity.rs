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
) -> chainvue_protocol::IdentityVm {
    let state = status(record, tip);
    chainvue_protocol::IdentityVm {
        name: qualified(&record.name, chain_name),
        address: record.identity_address.clone(),
        status: state.label().to_string(),
        tone: state.tone().to_string(),
        note: note(&state),
        mine,
    }
}

/// One row, from a full record — which is what a lookup returns.
pub fn row_of(record: &IdentityRecord, tip: u32, mine: &[String]) -> chainvue_protocol::IdentityVm {
    let at = as_at_address(record);
    let state = status(&at, tip);
    chainvue_protocol::IdentityVm {
        name: record.fully_qualified_name.clone(),
        address: record.identity_address.clone(),
        status: state.label().to_string(),
        tone: state.tone().to_string(),
        note: note(&state),
        mine: holds_a_key(record, mine) > 0,
    }
}

/// The sentence under a status, for the states where the word alone is not
/// enough. Empty for Active — a row explaining "Active" is a row nobody reads.
fn note(state: &Status) -> String {
    match state {
        Status::Active => String::new(),
        Status::Locked { delay } => {
            format!("Funds held. Unlocking starts a {delay}-block wait.")
        }
        Status::Unlocking { at } => format!("Unlocks at block {at}"),
        Status::Revoked => "Only its recovery authority can bring it back.".to_string(),
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
pub fn read(chain: &chainvue_chain::Chain, name_or_id: &str) -> Result<Detail, RpcError> {
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
) -> chainvue_protocol::IdentityDetailVm {
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

    chainvue_protocol::IdentityDetailVm {
        name: record.fully_qualified_name.clone(),
        address: record.identity_address.clone(),
        status: state.label().to_string(),
        tone: state.tone().to_string(),
        signatures_required: format!("{required} of {}", primary.len()),
        can_sign: held >= required as usize,
        control_note: if held == 0 {
            "This wallet holds none of the keys. You can read this identity but not change it."
                .to_string()
        } else if held >= required as usize {
            format!("This wallet holds {held} of the {required} signatures needed.")
        } else {
            format!(
                "This wallet holds {held} of the {required} signatures needed — not enough on its own."
            )
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
fn timelock_note(state: &Status) -> String {
    match state {
        Status::Locked { delay } => format!(
            "Locked. The funds cannot be spent, and nothing is counting down yet — \
             unlocking publishes a height {delay} blocks out and the wait starts then."
        ),
        Status::Unlocking { at } => format!(
            "Counting down. The funds stay held until block {at}; the transaction that \
             started this did not end the lock."
        ),
        Status::Revoked => "Revoked. Only its recovery authority can bring it back.".to_string(),
        Status::Active => "Not locked.".to_string(),
    }
}

/// A content map, keyed and decoded for display.
fn entries(
    map: &BTreeMap<String, Vec<ContentValue>>,
    names: Option<&Names>,
) -> Vec<chainvue_protocol::ContentEntryVm> {
    map.iter()
        .map(|(key, values)| chainvue_protocol::ContentEntryVm {
            name: names
                .and_then(|table| table.name_of(key))
                .unwrap_or_default()
                .to_string(),
            key: key.clone(),
            values: values
                .iter()
                .map(|raw| {
                    let shown = value(raw);
                    chainvue_protocol::ContentValueVm {
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
    pub fn describe(&self) -> String {
        match self {
            Self::Authorities {
                revocation,
                recovery,
            } => {
                let mut parts = Vec::new();
                if let Some(who) = revocation {
                    parts.push(format!("revocation to {who}"));
                }
                if let Some(who) = recovery {
                    parts.push(format!("recovery to {who}"));
                }
                if parts.is_empty() {
                    "Nothing to change.".to_string()
                } else {
                    format!(
                        "Points {} — after this, only they can take that action, \
                         and this wallet cannot take it back.",
                        parts.join(" and ")
                    )
                }
            }
            Self::Lock { delay } => format!(
                "Holds the funds. Nothing counts down until somebody asks to unlock, \
                 and then the wait is {delay} blocks."
            ),
            Self::Unlock { .. } => unlock_note(),
            Self::Revoke => "Revokes the identity. It can no longer be updated or spent \
                 from by its own keys, and only its recovery authority can bring it \
                 back — so if that authority is the identity itself, nothing can."
                .to_string(),
            Self::Recover => "Clears the revocation and hands the identity back to its \
                 primary addresses."
                .to_string(),
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
        broadcaster: &chainvue_chain::Permitted<'_>,
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
/// The same shape the mainnet spending switch uses, and for the same reason: a
/// revocation cannot be undone without the recovery authority, and an identity
/// that is its own recovery authority cannot be recovered at all. A button that
/// only needs to be clicked is one that gets clicked.
pub const REVOKE_CONFIRMATION: &str = "revoke";

/// What unlocking actually does, said plainly.
///
/// The single most misleading operation in this whole feature. Consensus
/// measures the published unlock height from the transaction's own expiry
/// rather than from the tip, so the identity stays locked until the chain
/// passes it. A screen that says "Unlocked" when the transaction confirms is
/// lying for as long as the delay lasts.
pub fn unlock_note() -> String {
    "Starts the countdown. It does not unlock the identity: the funds stay held \
     until the chain reaches the height this publishes, and that height is \
     measured from this transaction rather than from now."
        .to_string()
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
pub fn name_problem(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if name.len() > 64 {
        return Some("Too long — 64 characters at most.".to_string());
    }
    let bad: String = name
        .chars()
        .filter(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-'))
        .collect();
    if !bad.is_empty() {
        return Some(format!(
            "Only lowercase letters, digits, `_` and `-`. Remove: {bad}"
        ));
    }
    None
}

/// What a registration in progress looks like on screen.
///
/// The deadline is the part worth the trouble. A commitment expires roughly
/// twenty blocks after it is signed, the expiry is inside the bytes the
/// signature covers so it cannot be extended, and missing it spends the fee for
/// nothing. A progress spinner that does not say that is hiding the only fact
/// somebody could act on.
pub fn registration_view(
    record: &crate::registration::Record,
    status: Option<&verus_sdk::network::CommitmentStatus>,
    tip: u32,
    busy: bool,
) -> chainvue_protocol::RegistrationVm {
    use verus_sdk::network::CommitmentStatus;

    let expiry = record.pending.expiry_height();
    let deadline = match expiry {
        Some(at) if at > tip => {
            let blocks = at - tip;
            format!("Must confirm before block {at} — about {blocks} minutes")
        }
        Some(at) => format!("The deadline passed at block {at}"),
        None => String::new(),
    };

    let (step, note) = match status {
        None => match record.step {
            crate::registration::Step::Reserved => (
                "reserved",
                "Signed and written down. Nothing has been sent yet.".to_string(),
            ),
            crate::registration::Step::Committed => (
                "committed",
                "The claim is on its way. Waiting for it to be mined.".to_string(),
            ),
        },
        Some(CommitmentStatus::Waiting { confirmations }) => (
            "waiting",
            format!(
                "The claim is on the chain with {confirmations} confirmations. \
                 One is enough to register."
            ),
        ),
        Some(CommitmentStatus::Ready(_)) => (
            "ready",
            "The claim has confirmed. The name can be registered now.".to_string(),
        ),
        Some(CommitmentStatus::Reorged { detail }) => (
            "waiting",
            format!("The chain moved underneath the claim: {detail}"),
        ),
        Some(CommitmentStatus::CommitmentGone) => (
            "lost",
            "The claim is no longer on the chain. Its fee is spent and the name \
             was not registered."
                .to_string(),
        ),
        Some(CommitmentStatus::Expired { expiry_height, .. }) => (
            "expired",
            format!(
                "The claim expired at block {expiry_height}. Its fee is spent, \
                 the name was not registered, and the same claim cannot be sent \
                 again — the expiry is inside the bytes it was signed with."
            ),
        ),
        // `CommitmentStatus` is `#[non_exhaustive]`, so a variant added upstream
        // lands here rather than failing to compile. Saying so is better than
        // guessing which of the others it resembles.
        Some(_) => (
            "waiting",
            "The node reported something this build does not recognise.".to_string(),
        ),
    };

    chainvue_protocol::RegistrationVm {
        name: record.name.clone(),
        step: step.to_string(),
        note,
        deadline,
        fee_display: crate::portfolio::coins(record.pending.registration_fee),
        address: String::new(),
        busy,
        cannot_be_revoked: record.pending.recovery_authority.is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(
            view.deadline.contains(&expiry.to_string()),
            "the deadline does not name the block: {}",
            view.deadline
        );

        // Past it. The wording changes rather than the number vanishing —
        // "about -3 minutes" would be worse than saying it has gone.
        let view = registration_view(&record, None, expiry + 5, false);
        assert!(view.deadline.contains("passed"), "{}", view.deadline);

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
        assert!(view.note.contains("fee is spent"), "{}", view.note);
        assert!(
            view.note.contains("cannot be sent again"),
            "the wording invites a retry that cannot work: {}",
            view.note
        );

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
            name: "chainvue".to_string(),
            key_label: "main".to_string(),
            step: crate::registration::Step::Committed,
            pending: verus_flows::prepare_registration_with_salt(
                &reader,
                &key,
                "chainvue",
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
        let unlock = Change::Unlock { extra_blocks: 20 }.describe();
        assert!(
            unlock.contains("does not unlock"),
            "unlocking is described as unlocking: {unlock}",
        );
        assert!(unlock.contains("stay held"), "{unlock}");

        // Locking says the wait does not start until somebody asks — the state
        // whose whole point is that it has no end of its own.
        let lock = Change::Lock { delay: 100 }.describe();
        assert!(lock.contains("Nothing counts down"), "{lock}");
        assert!(lock.contains("100 blocks"), "{lock}");

        // Handing an authority away is the one that cannot be taken back, and
        // the sentence has to say so before it is signed.
        let away = Change::Authorities {
            revocation: None,
            recovery: Some("iGRp1CGkuro3LtGazX8W1PRjVupPVfe8Pv".to_string()),
        };
        let text = away.describe();
        assert!(text.contains("cannot take it back"), "{text}");
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
        assert!(nothing.describe().contains("Nothing to change"));
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

        let revoke = Change::Revoke.describe();
        assert!(
            revoke.contains("recovery authority"),
            "the sentence does not say who can undo it: {revoke}",
        );
        assert!(
            revoke.contains("nothing can"),
            "the sentence does not say when nobody can: {revoke}",
        );

        // Recovery says what it restores, and deliberately does not offer to
        // move the primary addresses — which a recovery legitimately may do,
        // and which would be the most dangerous default in the application.
        assert!(Change::Recover.describe().contains("primary addresses"));
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
