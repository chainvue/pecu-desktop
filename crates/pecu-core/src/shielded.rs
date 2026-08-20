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
//! # Why nothing is written to disk
//!
//! A [`ScanResult`] is the wallet's shielded history: every note paid to it,
//! with amounts and heights. The wallet's databases are plain SQLite, encrypted
//! by nothing — only the vault is. Writing note data there would put a
//! shielded balance and its whole history in a file any other process on the
//! machine can read, which is precisely the property somebody chose a shielded
//! address to avoid. A wallet that leaks that at rest has not delivered privacy;
//! it has delivered the appearance of it.
//!
//! So this scans into memory and starts again next launch. That is slower, and
//! it is the honest default until there is a place to put this that is as
//! protected as the keys are. It is not free: a restored wallet has to walk
//! from its birthday, and `birthday` for a *new* wallet is the tip, so the
//! common case costs nothing and the recovery case costs a wait.
//!
//! # A balance here is a claim by a server
//!
//! Everything below rests on what one lightwalletd said. See
//! [`pecu_chain::light`] for what that is worth and what it is not.

use pecu_chain::LightServer;
use pecu_keystore::ShieldedView;
use verus_sdk::light::{
    dfvk_from_bytes, scan, scan_after, DiversifiableFullViewingKey, LightClient, LightTransport,
    ScanResult,
};
use verus_sdk::network::FlowError;

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
pub struct Shielded {
    address: String,
    dfvk: DiversifiableFullViewingKey,
    scanned: Option<ScanResult>,
}

impl Shielded {
    /// Start watching an account, having scanned nothing yet.
    pub fn watching(view: &ShieldedView) -> Result<Self, ShieldedError> {
        let dfvk =
            dfvk_from_bytes(&view.dfvk).map_err(|e| ShieldedError::BadViewingKey(e.to_string()))?;
        Ok(Self {
            address: view.address.clone(),
            dfvk,
            scanned: None,
        })
    }

    /// The address to receive at.
    pub fn address(&self) -> &str {
        &self.address
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
            .map_or(0, |result| result.balance(&[]))
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
            .map_or(0, |result| result.unspent(&[]).len())
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
