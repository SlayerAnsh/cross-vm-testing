//! Solana chain providers: the in-process mock and the live-RPC stub.

mod mock;
mod rpc;

use solana_address::Address;
use solana_sha256_hasher::hashv;
use solana_signature::Signature;

/// Test-visible so the budget arithmetic (`gas_adjustment`, the ceiling clamp) can be pinned
/// directly, rather than inferred from a transaction that merely executes.
#[cfg(test)]
pub(crate) use mock::adjusted;
pub use mock::{SvmMockProvider, DEFAULT_FUNDING_LAMPORTS};
pub use rpc::SvmRpcProvider;

/// The runtime's per-transaction compute-unit ceiling. Re-exported from the crate litesvm itself
/// derives its limits from, so the ceiling named here is the one actually enforced.
pub use solana_compute_budget::compute_budget_limits::MAX_COMPUTE_UNIT_LIMIT;

/// Domain tag for the mock's synthetic deploy signature, so its preimage can never collide with
/// bytes hashed for another purpose.
const DEPLOY_DOMAIN: &[u8] = b"cross-vm:svm:add_program";

/// The compute-unit ceiling a mutating Solana transaction runs under.
///
/// This is not a gas limit and it does not bound what the transaction costs. Solana's fee is per
/// signature (plus an opt-in priority fee, set by a separate `SetComputeUnitPrice` instruction);
/// it is not a function of compute units. A compute budget constrains *execution*: it is a
/// `ComputeBudgetInstruction::SetComputeUnitLimit` prepended to the instruction list, and a
/// transaction that runs past it aborts with "Computational budget exceeded" (having still paid
/// its fee). The budget is therefore about not letting a runaway program burn the block's compute,
/// not about not overpaying.
///
/// Two consequences the API cannot hide:
///
/// - The cap covers the whole transaction *including the `SetComputeUnitLimit` instruction itself*,
///   which invokes the compute-budget builtin and burns 150 CU of the very budget it sets. Every
///   number here (both what [`Exact`] must cover and what an estimate reports) is for the
///   transaction as sent, budget instruction included.
/// - Instructions that run no transaction take no budget. `add_program` / `add_program_at` write
///   the program account straight into the mock's account store, so there is nothing to cap.
///
/// [`Exact`]: SvmComputeBudget::Exact
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SvmComputeBudget {
    /// Cap the transaction at exactly this many compute units. Below what it actually consumes,
    /// it aborts. The runtime silently clamps anything above `MAX_COMPUTE_UNIT_LIMIT` (1_400_000).
    Exact(u32),
    /// Simulate the transaction and cap it at what it consumed, scaled by the chain's
    /// [`gas_adjustment`](crate::SolanaChainInfo::gas_adjustment) and clamped to the runtime's
    /// per-transaction ceiling. Costs one extra simulation.
    Estimated,
}

/// The outcome of loading a program: the id it is executable at, and the base58 signature of the
/// transaction that loaded it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SvmDeploy {
    /// The program id the bytecode now lives at.
    pub program_id: Address,
    /// Base58 transaction signature of the load.
    pub tx_hash: String,
}

impl SvmDeploy {
    /// Mint the record for a program load the mock backend performed.
    ///
    /// `litesvm::add_program` writes the program account straight into the SVM account store, so
    /// no transaction is built and no signature exists to report. Synthesize one by hashing what
    /// identifies the load (the blockhash it landed under, the program id, the bytecode) into the
    /// two halves of a 64-byte signature. The hash is therefore reproducible across runs, distinct
    /// per load (the mock expires the blockhash after each), and parses back as a `Signature`.
    pub(crate) fn minted(blockhash: &[u8], program_id: Address, bytecode: &[u8]) -> Self {
        let head = hashv(&[DEPLOY_DOMAIN, blockhash, program_id.as_ref(), bytecode]);
        let tail = hashv(&[DEPLOY_DOMAIN, head.as_ref()]);
        let mut sig = [0u8; 64];
        sig[..32].copy_from_slice(head.as_ref());
        sig[32..].copy_from_slice(tail.as_ref());
        Self {
            program_id,
            tx_hash: Signature::from(sig).to_string(),
        }
    }
}
