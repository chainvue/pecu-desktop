//! What the wallet knows about its shielded funds.
//!
//! # What this layer is, and what the SDK already did
//!
//! The hard parts are not here. Trial-decrypting compact blocks, counting note
//! positions, checking that a range of blocks really chains, joining a note to
//! the nullifier that spent it — all of that is `verus_flows::shielded`, and it
//! is checked there against captured chain data. What is here is the wallet's
//! side of the conversation: hold the viewing key, remember what has been
//! scanned, ask for the tail, fold it in, and know what to do when the server
//! says the chain moved.
//!
//! # What is written to disk, and how
//!
//! A [`ScanResult`] is the wallet's shielded history: every note paid to it,
//! with amounts and heights. The wallet's databases are plain SQLite, encrypted
//! by nothing — only the vault is. Writing note data there in clear would put a
//! shielded balance and its whole history in a file any other process on the
//! machine can read, which is precisely the property somebody chose a shielded
//! address to avoid. A wallet that leaks that at rest has not delivered privacy;
//! it has delivered the appearance of it.
//!
//! For a long time the answer was to write nothing, and rescan on every launch.
//! That is honest and it is unusable: a VRSCTEST scan from Sapling activation
//! is about twelve hundred requests and three minutes, and paying it at every
//! start makes the shielded balance something the wallet is always in the
//! middle of finding out.
//!
//! So it is written, [`Kept`] first and then sealed under the vault's data key
//! — the same key the recovery phrase is under, held only while the wallet is
//! open. The database row carries ciphertext and a timestamp; which account it
//! belongs to is inside. `tests/shielded_kept.rs` reads the file back and
//! greps it for the address, the value and the height rather than trusting this
//! paragraph.
//!
//! What that does **not** protect against is anything with the passphrase or
//! with the running process, which is the same boundary as every key here.
//!
//! # A balance here is a claim by a server
//!
//! Everything below rests on what one lightwalletd said. See
//! [`pecu_chain::light`] for what that is worth and what it is not.

use pecu_chain::LightServer;
use pecu_keystore::ShieldedView;
use verus_sdk::light::{
    dfvk_from_bytes, min_relay_fee, scan, scan_after, DetectedNote, DiversifiableFullViewingKey,
    LightClient, LightTransport, ScanResult, ShieldedRecipient, ShieldedSpent, SpendRequest,
    TransparentRecipient, MAX_SPEND_NOTES,
};
use verus_sdk::money::Amount;
use verus_sdk::network::{FlowError, Unsent};
use verus_sdk::verus_sapling::params::SaplingParams;

/// Why a shielded sync did not complete.
#[derive(Debug, thiserror::Error)]
pub enum ShieldedError {
    /// The 128 bytes did not reconstruct a viewing key.
    ///
    /// A wallet bug rather than a chain condition: the bytes came from
    /// [`ShieldedView`], which derived them.
    #[error("the shielded viewing key is unusable: {0}")]
    BadViewingKey(String),

    /// The server is behind where this wallet has already scanned.
    ///
    /// **Not** a reorg, and the difference decides what to do: a lagging server
    /// serves the same chain with less of it, so the answer is to wait. Treated
    /// as a reorg it would throw away good notes and rescan for nothing.
    #[error("the light server is behind: it has {theirs}, and this wallet has scanned to {ours}")]
    ServerBehind { ours: u64, theirs: u64 },

    /// The chain moved under a completed scan, further back than the
    /// checkpoints reach.
    ///
    /// Recoverable only by scanning again from further back, which is the
    /// caller's decision because it means discarding work.
    #[error("the chain was reorged deeper than this scan can verify a rollback to")]
    ReorgTooDeep,

    /// Anything the scan itself reported.
    #[error("the shielded scan failed: {0}")]
    Scan(String),

    /// A kept scan belongs to a different account than the one now loaded.
    ///
    /// Not corruption and not an attack — switching keys inside one wallet
    /// reaches this — but folding it in would report one account's money under
    /// another's address, so it is refused and the scan starts again.
    #[error("that scan was made for a different shielded account")]
    WrongAccount,

    /// A kept scan could not be read back.
    #[error("the kept shielded scan is unreadable: {0}")]
    Unreadable(String),

    /// The destination is neither a shielded nor a transparent address.
    #[error("that is not an address this wallet can pay")]
    BadAddress,

    /// The amount could not be read, or is nothing.
    #[error("that is not an amount")]
    BadAmount,

    /// Nothing has been scanned, so the question cannot be answered yet.
    ///
    /// Distinct from having no funds, and the difference matters: a wallet that
    /// has not looked does not know that it is empty, and saying so would be a
    /// claim it has not earned.
    #[error("this wallet has not scanned for shielded funds yet")]
    NothingScanned,

    /// The notes do not cover it.
    ///
    /// Carries what is reachable **in one spend** as well as the balance,
    /// because those differ and the difference is not obvious: a bundle carries
    /// at most a fixed number of notes, so a balance spread thinly across many
    /// small ones cannot all move at once.
    #[error(
        "this wallet holds {held} shielded, and {needed} is needed — but only {reachable} is \
         reachable in one payment, from {notes} notes"
    )]
    NotEnough {
        held: String,
        needed: String,
        reachable: String,
        notes: usize,
    },
}

/// What one sync did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// First block this sync covered.
    pub from: u64,
    /// Last block this sync covered, inclusive.
    pub to: u64,
    /// How many blocks it had to roll back before it could continue.
    ///
    /// Zero in the ordinary case. Non-zero means a reorg was detected and
    /// handled, which is worth surfacing rather than hiding: it explains a
    /// balance that moved without the owner doing anything.
    pub rewound: u64,
}

/// The shielded side of one key.
///
/// Holds viewing material only — see [`ShieldedView`]. Nothing reachable from
/// here can spend, and in this build the code that would has not been compiled
/// in at all.
#[derive(Clone)]
pub struct Shielded {
    /// The viewing material this was built from, kept as it arrived.
    ///
    /// `dfvk` below is the same key in the form the scan functions want. Both
    /// are held rather than one being recomputed, because a caller that wants
    /// to start a **fresh** scan of the same account needs the bytes, and
    /// reconstructing them from the parsed key each time would be work to
    /// undo work.
    view: ShieldedView,
    dfvk: DiversifiableFullViewingKey,
    scanned: Option<ScanResult>,

    /// Nullifiers of notes this wallet has spent, but has not yet seen spent.
    ///
    /// # Why this is not redundant with the scan
    ///
    /// A note is spent when its nullifier appears in a block. Between
    /// broadcasting a spend and that block arriving — a minute on Verus, longer
    /// if the fee was thin — the chain still shows the note as unspent, because
    /// as far as the chain is concerned it is.
    ///
    /// Without this, a second spend in that window selects the same note again
    /// and the daemon refuses the whole transaction with
    /// `bad-txns-sapling-nullifier-exists`, after the prover has been paid for.
    /// That is not a hypothetical: it is what `live_shielded_spend.rs` did on
    /// its first real run, and it is the shielded half of exactly what the
    /// pending ledger does for transparent outputs.
    ///
    /// Pruned by [`Self::sync`] once the scan has seen them, so this cannot
    /// grow without bound.
    spent_locally: Vec<[u8; 32]>,
}

/// A scan, in a form that can be written down and read back.
///
/// # What is in here, and why all of it
///
/// The notes **and** every nullifier the scan saw. Keeping only the notes looks
/// like the smaller thing to store and is the dangerous one: a note spent
/// inside the range comes back as spendable, and the wallet offers money it no
/// longer has. The SDK says the same at [`ScanResult`] and this type exists so
/// that the pair cannot be separated by accident here either.
///
/// `spent_locally` too, which is not from the chain at all — it is what this
/// wallet has spent and not yet seen spent. Dropping it across a restart
/// reopens the window where a second spend picks the same note and the daemon
/// refuses the whole transaction after the prover has been paid for.
///
/// # Why the account is named inside
///
/// So the thing that reads it back can tell whether it is the right one. This
/// is a serialised blob in a file, and the file cannot say which key it belongs
/// to without saying so somewhere; putting it in a column would tell anybody
/// who opens the database which shielded account this wallet holds. Inside the
/// ciphertext it tells only the wallet.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Kept {
    /// The 128 viewing-key bytes, hex, of the account this describes.
    account: String,
    scanned: ScanResult,
    /// Nullifiers, hex. Hex rather than `[u8; 32]` because serde writes a byte
    /// array as thirty-two separate numbers, which is neither smaller nor
    /// readable when somebody is working out what a file contains.
    spent_locally: Vec<String>,
}

impl Shielded {
    /// Start watching an account, having scanned nothing yet.
    pub fn watching(view: &ShieldedView) -> Result<Self, ShieldedError> {
        let dfvk =
            dfvk_from_bytes(&view.dfvk).map_err(|e| ShieldedError::BadViewingKey(e.to_string()))?;
        Ok(Self {
            view: view.clone(),
            dfvk,
            scanned: None,
            spent_locally: Vec::new(),
        })
    }

    /// Start watching an account, with what a previous run already scanned.
    ///
    /// # Errors
    ///
    /// [`ShieldedError::WrongAccount`] when `kept` was made for a different
    /// viewing key — which is a switch of keys inside one wallet, not a fault,
    /// and the caller's answer is to scan from the beginning rather than to
    /// show anybody a message. [`ShieldedError::BadViewingKey`] if the view
    /// itself does not reconstruct.
    ///
    /// A malformed nullifier is **skipped, not fatal**, and the skip is logged.
    /// The consequence of dropping one is that a note this wallet spent very
    /// recently may be offered again before the chain confirms it — recoverable,
    /// and one bad byte must not cost an hour of scanning.
    pub fn restore(view: &ShieldedView, kept: Kept) -> Result<Self, ShieldedError> {
        if kept.account != hex::encode(view.dfvk) {
            return Err(ShieldedError::WrongAccount);
        }

        let mut watching = Self::watching(view)?;
        watching.spent_locally = kept
            .spent_locally
            .iter()
            .filter_map(|hexed| {
                let bytes: Option<[u8; 32]> = hex::decode(hexed)
                    .ok()
                    .and_then(|bytes| bytes.try_into().ok());
                if bytes.is_none() {
                    tracing::warn!("a kept nullifier was not 32 hex bytes and was skipped");
                }
                bytes
            })
            .collect();
        watching.scanned = Some(kept.scanned);
        Ok(watching)
    }

    /// This scan, in a form that can be written down.
    ///
    /// `None` before anything has been scanned: there is nothing to keep, and
    /// writing an empty result would make the next launch believe it had
    /// already looked.
    pub fn keep(&self) -> Option<Kept> {
        Some(Kept {
            account: hex::encode(self.view.dfvk),
            scanned: self.scanned.clone()?,
            spent_locally: self.spent_locally.iter().map(hex::encode).collect(),
        })
    }

    /// The address to receive at.
    pub fn address(&self) -> &str {
        &self.view.address
    }

    /// The viewing material, for starting a fresh scan of the same account.
    ///
    /// Viewing only — see [`ShieldedView`]. Handing this out grants nothing the
    /// caller did not already have: it is what they passed in.
    pub fn view(&self) -> &ShieldedView {
        &self.view
    }

    /// The last block this wallet has scanned, if any.
    pub fn scanned_to(&self) -> Option<u64> {
        self.scanned.as_ref().map(|result| result.to)
    }

    /// What is spendable, in satoshis.
    ///
    /// Zero before the first scan, which is the truthful answer: nothing has
    /// been looked for yet. A wallet that showed a blank here would be saying
    /// something different from a wallet that showed zero, and the interface
    /// has [`Self::scanned_to`] to tell those apart.
    ///
    /// This is [`ScanResult::balance`], which joins detection against the
    /// nullifiers seen in the same range. Reading `notes` instead would report
    /// money already spent.
    pub fn balance(&self) -> u64 {
        self.scanned
            .as_ref()
            .map_or(0, |result| result.balance(&self.spent_locally))
    }

    /// How many unspent notes back that balance.
    ///
    /// Not decoration: a spend can only use so many notes at once, so a balance
    /// spread across many small ones does not all move in one transaction. That
    /// matters at the point somebody tries to send, and the number is what
    /// makes it explainable.
    pub fn note_count(&self) -> usize {
        self.scanned
            .as_ref()
            .map_or(0, |result| result.unspent(&self.spent_locally).len())
    }

    /// Record that a spend of these notes has been broadcast.
    ///
    /// Called after the network accepts a shielded spend, not before: a
    /// transaction the daemon refused has spent nothing, and marking its notes
    /// would strand them until the next scan.
    pub fn note_spent(&mut self, planned: &PlannedSpend) {
        self.mark_spent(&planned.nullifiers());
    }

    /// The same, from nullifiers that have already been carried somewhere.
    ///
    /// What the actor uses: by the time a broadcast has been accepted, the plan
    /// is long gone and only the nullifiers came back with the answer.
    pub fn mark_spent(&mut self, nullifiers: &[[u8; 32]]) {
        self.spent_locally.extend_from_slice(nullifiers);
    }

    /// Scan whatever this wallet has not, up to `to`.
    ///
    /// `from` is used only for the very first scan — the wallet's birthday.
    /// Afterwards the range starts where the last one ended, and continuity is
    /// proven rather than assumed: `scan_after` refuses a range that does not
    /// descend from the block the last scan finished on.
    ///
    /// # Reorgs are handled here, once
    ///
    /// A refused continuation is not transient — retrying with the same state
    /// fails identically for as long as the fork stands — so this does not
    /// retry. It rolls back to the oldest block it can *verify* a rollback to
    /// and scans forward again from there. Rolling back too little would fail
    /// loudly on the next call rather than quietly mixing positions from two
    /// chains, which is why going straight to the oldest checkpoint is safe
    /// rather than merely convenient.
    pub fn sync<T: LightTransport>(
        &mut self,
        light: &LightClient<T>,
        from: u64,
        to: u64,
    ) -> Result<Progress, ShieldedError> {
        let Some(previous) = self.scanned.as_mut() else {
            let result = scan(light, &self.dfvk, from, to).map_err(|e| scan_error(&e))?;
            let progress = Progress {
                from: result.from,
                to: result.to,
                rewound: 0,
            };
            self.scanned = Some(result);
            return Ok(progress);
        };

        if to < previous.to {
            return Err(ShieldedError::ServerBehind {
                ours: previous.to,
                theirs: to,
            });
        }

        match scan_after(light, &self.dfvk, previous, to) {
            Ok(tail) => {
                let progress = Progress {
                    from: tail.from,
                    to: tail.to,
                    rewound: 0,
                };
                previous.absorb(tail).map_err(|e| scan_error(&e))?;
                // Anything the chain now agrees is spent no longer needs
                // remembering separately.
                let seen = previous;
                self.spent_locally
                    .retain(|nullifier| !seen.nullifiers.iter().any(|s| s.nullifier == *nullifier));
                Ok(progress)
            }
            Err(FlowError::Reorged(_)) => {
                let was = previous.to;
                let target = previous
                    .earliest_rewind()
                    .ok_or(ShieldedError::ReorgTooDeep)?;
                previous.rewind_to(target).map_err(|e| scan_error(&e))?;

                let tail = scan_after(light, &self.dfvk, previous, to).map_err(|e| match e {
                    // Still refused from as far back as this result can prove:
                    // the fork is deeper than the checkpoints reach, and only a
                    // fresh scan from further back can settle it.
                    FlowError::Reorged(_) => ShieldedError::ReorgTooDeep,
                    ref other => scan_error(other),
                })?;

                let progress = Progress {
                    from: tail.from,
                    to: tail.to,
                    rewound: was.saturating_sub(target),
                };
                previous.absorb(tail).map_err(|e| scan_error(&e))?;
                Ok(progress)
            }
            Err(FlowError::NotReady(_)) => Err(ShieldedError::ServerBehind {
                ours: previous.to,
                theirs: to,
            }),
            Err(ref other) => Err(scan_error(other)),
        }
    }

    /// Scan against a live server, up to the height it has actually synced to.
    ///
    /// The convenience the wallet actually calls. Kept separate from
    /// [`Self::sync`] so that everything above can be tested against a scripted
    /// transport with no network in the graph.
    pub fn sync_to_tip(
        &mut self,
        server: &LightServer,
        from: u64,
    ) -> Result<Progress, ShieldedError> {
        let to = server
            .synced_height()
            .map_err(|e| ShieldedError::Scan(e.to_string()))?;
        self.sync(server.client(), from, to)
    }
}

/// Flatten an SDK error into this layer's wording.
///
/// Deliberately loses no text: the SDK's messages say what continuity check
/// failed, and a wallet that replaced them with "scan failed" would be throwing
/// away the only description of what went wrong.
fn scan_error(error: &FlowError) -> ShieldedError {
    ShieldedError::Scan(error.to_string())
}

// ── Spending, which is z→z and z→t ──────────────────────────────────────────

/// Where a shielded spend is going.
///
/// One enum rather than two entry points, because a wallet should work this out
/// from what somebody pasted rather than asking them which kind of address they
/// hold. The difference is real — one output is a note, the other is a script —
/// and it is the wallet's job to know it, not the user's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// Another shielded address. Nothing about the payment is visible.
    Shielded(String),
    /// An `R` address or a VerusID. The amount and the recipient become public
    /// at the moment it lands, and the sender does not.
    Transparent(String),
}

impl Destination {
    /// Read an address and decide which kind it is.
    pub fn read(address: &str) -> Result<Self, ShieldedError> {
        let text = address.trim();
        if verus_sdk::light::zaddr::decode(text).is_ok() {
            return Ok(Self::Shielded(text.to_string()));
        }
        if text.parse::<verus_sdk::verus_keys::Address>().is_ok() {
            return Ok(Self::Transparent(text.to_string()));
        }
        Err(ShieldedError::BadAddress)
    }

    /// How many transparent outputs this destination costs.
    ///
    /// The fee the daemon enforces counts outputs, and a shielded destination
    /// adds none: the Sapling bundle is padded to two whatever it carries.
    fn transparent_outputs(&self) -> usize {
        match self {
            Self::Shielded(_) => 0,
            Self::Transparent(_) => 1,
        }
    }
}

/// A shielded spend that has been costed but not proven.
#[derive(Debug, Clone)]
pub struct PlannedSpend {
    pub to: Destination,
    pub amount: Amount,
    pub fee: Amount,
    /// The notes that will be spent. A note enters a spend whole or not at all.
    notes: Vec<DetectedNote>,
}

impl PlannedSpend {
    /// The nullifiers this spend will publish.
    ///
    /// What [`Shielded::note_spent`] records, so the same notes are not chosen
    /// again while the spend is still in the mempool.
    pub fn nullifiers(&self) -> Vec<[u8; 32]> {
        self.notes.iter().map(|note| note.nullifier).collect()
    }

    /// What the selected notes are worth in total.
    ///
    /// Worth showing on a review screen, because it is usually **more** than the
    /// amount: value cannot be split at the input, so a spend of 0.1 from a
    /// 0.5 note consumes the whole note and returns 0.3999 as a new one.
    pub fn notes_worth(&self) -> Amount {
        Amount::from_sat(self.notes.iter().map(|note| note.value).sum())
    }

    /// How many notes it takes.
    pub fn note_count(&self) -> usize {
        self.notes.len()
    }
}

impl Shielded {
    /// Choose notes and cost a spend, without the spending key and without
    /// proving.
    ///
    /// # Errors
    ///
    /// [`ShieldedError::NothingScanned`] before the first scan — a wallet that
    /// has not looked cannot know it has nothing, and refusing for "no funds"
    /// would be a different and wrong statement.
    pub fn plan_spend(&self, to: &str, amount: &str) -> Result<PlannedSpend, ShieldedError> {
        let to = Destination::read(to)?;

        let amount = Amount::from_coins_str(amount.trim()).map_err(|_| ShieldedError::BadAmount)?;
        if amount.is_zero() {
            return Err(ShieldedError::BadAmount);
        }

        let scanned = self.scanned.as_ref().ok_or(ShieldedError::NothingScanned)?;

        let fee = min_relay_fee(2, to.transparent_outputs());
        let needed = amount.checked_add(fee).ok_or(ShieldedError::BadAmount)?;

        // Largest first, and capped: a Sapling bundle carries at most
        // `MAX_SPEND_NOTES` spends, and each one is another Groth16 proof —
        // so this is not only a consensus limit, it is the difference between
        // half a minute and several.
        let mut unspent = scanned.unspent(&self.spent_locally);
        unspent.sort_by_key(|note| std::cmp::Reverse(note.value));

        let mut notes = Vec::new();
        let mut gathered = Amount::ZERO;
        for note in unspent.into_iter().take(MAX_SPEND_NOTES) {
            gathered = gathered
                .checked_add(Amount::from_sat(note.value))
                .ok_or(ShieldedError::BadAmount)?;
            notes.push(note);
            if gathered >= needed {
                break;
            }
        }

        if gathered < needed {
            return Err(ShieldedError::NotEnough {
                held: self.balance_amount().to_coins_string(),
                needed: needed.to_coins_string(),
                reachable: gathered.to_coins_string(),
                notes: notes.len(),
            });
        }

        Ok(PlannedSpend {
            to,
            amount,
            fee,
            notes,
        })
    }

    /// The balance as an `Amount`, for arithmetic rather than for display.
    fn balance_amount(&self) -> Amount {
        Amount::from_sat(self.balance())
    }
}

/// Prove and sign a shielded spend. **Tens of seconds of Groth16 per note.**
///
/// The extended spending key arrives as a borrowed slice from
/// `Vault::with_shielded_key`, and the proof therefore runs inside that closure.
/// See its documentation for what that costs and why the alternative is worse.
///
/// # Why this takes the light client
///
/// Every note has to be witnessed — a Merkle path to an anchor the chain agrees
/// with — and the path comes from the light server. That is what makes this the
/// half that cannot be exercised while `lightwalletd.verustest.net` is serving
/// an expired certificate, while `pecu_core::shield` (t→z) can: a shield spends
/// no notes, so there is nothing to witness.
pub fn prove_spend<T: LightTransport>(
    light: &LightClient<T>,
    reader: &impl verus_sdk::network::ChainReader,
    params: &SaplingParams,
    extsk: &[u8; 169],
    planned: &PlannedSpend,
) -> Result<Unsent<ShieldedSpent>, ShieldedError> {
    let shielded_to;
    let transparent_to;

    match &planned.to {
        Destination::Shielded(address) => {
            let recipient =
                verus_sdk::light::zaddr::decode(address).map_err(|_| ShieldedError::BadAddress)?;
            shielded_to = vec![ShieldedRecipient::new(recipient, planned.amount.to_sat())];
            transparent_to = Vec::new();
        }
        Destination::Transparent(address) => {
            let parsed = address
                .parse::<verus_sdk::verus_keys::Address>()
                .map_err(|_| ShieldedError::BadAddress)?;
            shielded_to = Vec::new();
            transparent_to = vec![TransparentRecipient {
                address: parsed,
                amount: planned.amount.to_sat(),
            }];
        }
    }

    let request = SpendRequest {
        extsk,
        notes: &planned.notes,
        shielded_to: &shielded_to,
        transparent_to: &transparent_to,
        fee: planned.fee.to_sat(),
        // `None`: change returns to the address the largest selected note was
        // paid to, which this spending key demonstrably controls. A fresh
        // diversified address would be better for privacy and is a separate
        // decision with its own screen — offering it silently would change
        // where somebody's change lives without telling them.
        change_address: None,
        anchor_height: None,
        expiry: None,
    };

    // Returned as `Unsent`, not unwrapped. That type is the SDK's own
    // guarantee that signed bytes reach the network through a `Broadcaster` and
    // nowhere else — and in this wallet a `Broadcaster` can only be got from a
    // `SpendPermit`. Reading `hex` and `txid` off it for a review screen costs
    // nothing; taking the bytes out of it would throw the guarantee away for
    // the convenience of one return type.
    verus_sdk::light::prepare_spend(light, reader, params, &request)
        .map_err(|e| ShieldedError::Scan(e.to_string()))
}

/// Send a proven shielded spend. The permit is the only route to a broadcaster.
pub fn broadcast_spend(
    chain: &pecu_chain::Chain,
    permit: &pecu_chain::SpendPermit,
    unsent: Unsent<ShieldedSpent>,
) -> Result<ShieldedSpent, FlowError> {
    unsent.broadcast(&chain.broadcaster(permit))
}
