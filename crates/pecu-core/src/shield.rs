//! Moving your own money from the transparent side into the shielded pool.
//!
//! # Why this is not part of `send`
//!
//! A `t→z` looks like a payment and is built nothing like one. The transparent
//! send path hands the whole job to `verus_flows::prepare_send`; this one has to
//! assemble the transaction itself, because the shielded half is proven by
//! `verus-sapling` and the transparent half is signed afterwards by
//! `verus-tx-transparent`, and no single SDK call spans both.
//!
//! # The order, and why either order would be safe
//!
//! Prove and binding-sign first with empty `scriptSig`s, then sign the
//! transparent inputs. The ZIP-243 *shielded* sighash has no transparent-input
//! section, and `scriptSig` bytes never reach `hashPrevouts`, `hashSequence` or
//! `hashOutputs` — so neither signature invalidates the other. The SDK states
//! this at both ends, and it is the reason a shield can be proven on one host
//! and signed on another.
//!
//! # This does not need the light server
//!
//! Nothing here is witnessed. A shield creates notes and spends none, so the
//! anchor is the empty tree and there is no commitment path to fetch. That is
//! what makes this the one shielded direction that can be exercised against a
//! real chain while `lightwalletd.verustest.net` is serving an expired
//! certificate — see `pecu_chain::light`.
//!
//! # It spends a transparent key
//!
//! The extended spending key does not appear here at all. A shield spends
//! ordinary P2PKH outputs, so it takes the same `Vault::with_key` closure every
//! transparent payment takes, and the shielded side needs no secret — an address
//! is enough to pay one.

use pecu_chain::{Chain, SpendPermit};
use pecu_keystore::{Vault, VaultError};
use verus_sdk::light::min_relay_fee;
use verus_sdk::light::zaddr;
use verus_sdk::money::Amount;
use verus_sdk::network::{self, FlowError};
use verus_sdk::verus_sapling::build::{build_shield, ShieldSpec, ShieldedOutput};
use verus_sdk::verus_sapling::params::SaplingParams;
use verus_sdk::verus_sapling::VERUS_ZIP212;
use verus_sdk::verus_tx::{sign_p2pkh_inputs, Expiry, Utxo};
use verus_sdk::verus_wire::consensus::VERUS_BRANCH_ID;
use verus_sdk::verus_wire::{TxIn, TxOut, TxV4};

/// How far ahead a shield stops being minable.
///
/// The same policy the transparent send applies: a payment that does not
/// confirm should die rather than land months later, against outputs the wallet
/// has since spent elsewhere.
const EXPIRY_BLOCKS: u32 = 20;

/// Why a shield could not be built.
#[derive(Debug, thiserror::Error)]
pub enum ShieldError {
    /// The destination is not a Sapling payment address.
    #[error("that is not a shielded address")]
    BadAddress,

    /// The amount could not be read, or is nothing.
    #[error("that is not an amount to shield")]
    BadAmount,

    /// Not enough transparent coin to cover the amount and the fee.
    #[error("this key holds {held}, and shielding {wanted} costs {needed} with the fee")]
    NotEnough {
        held: String,
        wanted: String,
        needed: String,
    },

    /// The proving parameters were not available.
    #[error(transparent)]
    Params(#[from] crate::params::ParamsError),

    /// The vault refused.
    #[error(transparent)]
    Vault(#[from] VaultError),

    /// The chain, or a flow over it, refused.
    #[error(transparent)]
    Flow(#[from] FlowError),

    /// Proving or signing failed.
    #[error("the shielded transaction could not be built: {0}")]
    Build(String),
}

/// A shield that has been costed but not built.
///
/// Made without the key and without the proving parameters, so the review
/// screen can show what it will cost before anything expensive begins.
#[derive(Debug, Clone)]
pub struct Planned {
    /// The `zs…` address, as typed.
    pub to: String,
    /// What arrives in the shielded pool.
    pub amount: Amount,
    /// What the miner takes.
    pub fee: Amount,
    /// What comes back to the transparent address.
    pub change: Amount,
    /// Which outputs are being spent, in the order they will appear.
    inputs: Vec<Utxo>,
    /// The address change returns to — the one being spent from.
    change_to: String,
    /// The tip at planning time, for the expiry.
    tip: u32,
}

/// A proven, fully signed shield that has not been broadcast.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub hex: String,
    pub txid: String,
    pub to: String,
    pub amount: Amount,
    pub fee: Amount,
    pub change: Amount,
}

/// Cost a shield against the chain, without the key and without proving.
///
/// Takes a `ChainReader` rather than a [`Chain`], and the difference is the
/// point: a reader cannot broadcast. This function looks at the chain, decides
/// what a shield would cost, and is structurally incapable of sending
/// anything — the same shape `plan_conversion` has, and the reason a test can
/// hand it a scripted chain and then assert that nothing was sent.
///
/// # Why the fee is counted in outputs
///
/// `min_relay_fee` mirrors the daemon's own check, which counts outputs rather
/// than bytes. Two shielded outputs, always: a Sapling bundle is **padded to
/// two** whatever it carries, because concealing which output is the real
/// recipient is the point of it. So a shield of one note still declares two,
/// and a fee computed for one would be below the floor the daemon enforces.
pub fn plan(
    reader: &impl verus_sdk::network::ChainReader,
    from_address: &str,
    to: &str,
    amount: &str,
) -> Result<Planned, ShieldError> {
    let to = to.trim();
    zaddr::decode(to).map_err(|_| ShieldError::BadAddress)?;

    let amount = Amount::from_coins_str(amount.trim()).map_err(|_| ShieldError::BadAmount)?;
    if amount.is_zero() {
        return Err(ShieldError::BadAmount);
    }

    let funding = network::spendable(reader, from_address)?;

    // Selection is done here rather than by `select_utxos`, and that is not a
    // reimplementation for its own sake. `select_utxos` prices a transaction by
    // its transparent size, which for a shield is the wrong rule twice over: it
    // cannot see the ~2.5 KB of proofs and note ciphertexts, and the daemon does
    // not price a shielded transaction by size at all. It counts outputs. So
    // the fee is `min_relay_fee`, which mirrors that check branch for branch,
    // and the inputs are gathered to cover it.
    let fee_with_change = min_relay_fee(2, 1);
    let needed = amount
        .checked_add(fee_with_change)
        .ok_or(ShieldError::BadAmount)?;

    // Largest first, so the fewest inputs cover it. Fewer inputs is a smaller
    // transaction and one less signature, and it leaves the small outputs for
    // the payments that need them.
    let mut candidates = funding.utxos.clone();
    candidates.sort_by_key(|utxo| std::cmp::Reverse(utxo.satoshis.to_sat()));

    let mut selected = Vec::new();
    let mut gathered = Amount::ZERO;
    for utxo in candidates {
        gathered = gathered
            .checked_add(utxo.satoshis)
            .ok_or(ShieldError::BadAmount)?;
        selected.push(utxo);
        if gathered >= needed {
            break;
        }
    }
    if gathered < needed {
        return Err(ShieldError::NotEnough {
            held: funding.total.to_coins_string(),
            wanted: amount.to_coins_string(),
            needed: needed.to_coins_string(),
        });
    }

    // `checked_sub`, though `gathered >= needed` was just established: an
    // `Amount` has no `-`, and reaching for one would mean an `as` cast or an
    // unwrap where the type is deliberately refusing to be either.
    let remainder = gathered.checked_sub(needed).ok_or(ShieldError::BadAmount)?;

    // Change worth less than it costs to spend is not change. Folding it into
    // the fee removes an output, which lowers the fee to the no-change floor —
    // so the miner gets the remainder and the wallet is not left holding an
    // output it would lose money moving.
    let (fee, change) = if remainder.to_sat() < min_relay_fee(2, 1).to_sat() {
        (
            fee_with_change
                .checked_add(remainder)
                .ok_or(ShieldError::BadAmount)?,
            Amount::ZERO,
        )
    } else {
        (fee_with_change, remainder)
    };

    let tip = funding.tip;

    Ok(Planned {
        to: to.to_string(),
        amount,
        fee,
        change,
        inputs: selected,
        change_to: from_address.to_string(),
        tip,
    })
}

/// Prove and sign. **This is the expensive call** — tens of seconds of Groth16.
///
/// Runs off the actor, like every other signing path here. The proving
/// parameters are loaded by the caller rather than here, because finding or
/// downloading them is its own step with its own progress.
pub fn prepare(
    vault: &Vault,
    label: &str,
    params: &SaplingParams,
    planned: &Planned,
) -> Result<Prepared, ShieldError> {
    // The key exists for the length of this closure and no longer, which is the
    // rule every signing path here follows. Note that the proving happens
    // *inside* it: a shield's transparent signature commits to the shielded
    // sighash's outputs, so the two cannot be separated without carrying the
    // key past the point it is needed.
    vault.with_key(label, |key| prepare_with_key(key, params, planned))?
}

/// The same, given the key directly.
///
/// Exists so a test can build a real shield without standing up a vault —
/// `convert_build.rs` takes the same shape for the same reason. **Not a way
/// around the keystore:** the only caller in the application is [`prepare`],
/// inside its `with_key` closure.
pub fn prepare_with_key(
    key: &verus_sdk::verus_keys::PrivateKey,
    params: &SaplingParams,
    planned: &Planned,
) -> Result<Prepared, ShieldError> {
    let recipient = zaddr::decode(&planned.to).map_err(|_| ShieldError::BadAddress)?;

    let inputs: Vec<TxIn> = planned
        .inputs
        .iter()
        // `0xffffffff` — what every Verus wallet writes, and what disables
        // both nLockTime and the relative-timelock reading of the field.
        .map(|utxo| TxIn::unsigned(utxo.txid.to_internal(), utxo.vout, 0xffff_ffff))
        .collect();

    let mut outputs = Vec::new();
    if !planned.change.is_zero() {
        let address: verus_sdk::verus_keys::Address = planned
            .change_to
            .parse()
            .map_err(|_| ShieldError::BadAddress)?;
        outputs.push(TxOut {
            value: planned.change.to_sat(),
            script_pubkey: address
                .p2pkh_script_pubkey()
                .map_err(|e| ShieldError::Build(e.to_string()))?,
        });
    }

    let shielded = [ShieldedOutput::new(recipient, planned.amount.to_sat())];

    let spec = ShieldSpec {
        transparent_inputs: &inputs,
        transparent_outputs: &outputs,
        shielded_outputs: &shielded,
        lock_time: 0,
        expiry_height: Expiry::within(planned.tip, EXPIRY_BLOCKS).to_height(),
        branch_id: VERUS_BRANCH_ID,
        zip212: VERUS_ZIP212,
    };

    // Proven with empty scriptSigs, then signed. See the module docs on why the
    // order does not matter.
    let mut tx: TxV4 =
        build_shield(params, &spec).map_err(|e| ShieldError::Build(e.to_string()))?;

    // `TxError` is not one of this module's own kinds and does not get a `From`
    // that would let it in anywhere else.
    sign_p2pkh_inputs(&mut tx, key, &planned.inputs)
        .map_err(|e| ShieldError::Build(e.to_string()))?;

    let bytes = tx
        .serialize()
        .map_err(|e| ShieldError::Build(e.to_string()))?;
    // Internal order out of `txid()`, reversed for display — the same
    // convention every explorer and every RPC reply uses.
    let mut txid = tx.txid().map_err(|e| ShieldError::Build(e.to_string()))?;
    txid.reverse();

    Ok(Prepared {
        hex: hex::encode(bytes),
        txid: hex::encode(txid),
        to: planned.to.clone(),
        amount: planned.amount,
        fee: planned.fee,
        change: planned.change,
    })
}

/// Send it. The permit is the only route to a broadcaster.
pub fn broadcast(
    chain: &Chain,
    permit: &SpendPermit,
    prepared: &Prepared,
) -> Result<String, FlowError> {
    network::broadcast(&chain.broadcaster(permit), &prepared.hex, &prepared.txid)
}
