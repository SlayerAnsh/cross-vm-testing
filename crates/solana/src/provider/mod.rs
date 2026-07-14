//! Solana chain providers: the in-process mock and the live-RPC stub.

mod mock;
mod rpc;

use solana_address::Address;
use solana_sha256_hasher::hashv;
use solana_signature::Signature;

pub use mock::{SvmMockProvider, DEFAULT_FUNDING_LAMPORTS};
pub use rpc::SvmRpcProvider;

/// Domain tag for the mock's synthetic deploy signature, so its preimage can never collide with
/// bytes hashed for another purpose.
const DEPLOY_DOMAIN: &[u8] = b"cross-vm:svm:add_program";

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
