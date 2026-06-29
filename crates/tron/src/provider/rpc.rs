//! Live java-tron RPC provider (stub parity).
//!
//! [`TronRpcProvider`] mirrors the shape of the EVM [`EvmRpcProvider`] so a Tron chain can sit
//! behind the shared `ChainProvider` trait with the same call surface. Unlike the EVM backend,
//! v1 ships no live java-tron client: there is no alloy-equivalent in-process client for Tron, so
//! every write/network path returns [`TronError::Unimplemented`] rather than performing real I/O.
//! Chain reads return inert defaults (`balance` → `0`, `block_height` → `0`) and `new_account`
//! returns a deterministic placeholder address, exactly as the EVM RPC backend does during its
//! read-only phase.
//!
//! [`EvmRpcProvider`]: https://docs.rs/cross-vm-solidity
//!
//! # DEFERRED: real java-tron read/write paths
//!
//! When the live backend lands, the read and write paths map onto java-tron / TronGrid HTTP and
//! gRPC as follows (recorded here so the future design is anchored to authoritative endpoints):
//!
//! - Per-transaction events: `GET /v1/transactions/{txid}/events` (TronGrid) to resolve the logs
//!   emitted by a single broadcast transaction.
//!   Source: <https://developers.tron.network/reference/get-events-by-transaction-id>
//! - Range / topic log search: `eth_getLogs` (java-tron's EVM-compatible JSON-RPC) for block-range
//!   and topic filters, with the TronGrid `GET /v1/contracts/{addr}/events` endpoint as the
//!   contract-scoped alternative.
//!   Source: <https://developers.tron.network/reference/eth_getlogs>
//! - Transaction broadcast (writes): gRPC `TriggerSmartContract` for contract deploys/calls and
//!   `TransferContract` for native transfers, signed with the wallet's secp256k1 key.
//!
//! Until that backend exists, the methods below stand in as `Unimplemented` stubs.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use alloy_primitives::Bytes;
use alloy_signer_local::PrivateKeySigner;
use cross_vm_core::{ChainProvider, WalletFactory};

use crate::chains::TronChainInfo;
use crate::error::TronError;
use crate::provider::address::{address_from_label, TronAddress};
use crate::provider::execution::TronExecution;

/// A stub-parity live-RPC Tron provider.
///
/// Carries the same fields as the EVM RPC provider (chain metadata, endpoint, shared wallet
/// roster, per-label signer cache) so the live backend can be filled in later without changing
/// the surface. In v1 every write path returns [`TronError::Unimplemented`].
//
// `rpc_url`/`wallets`/`signers` are part of the stub-parity surface but are not read until the
// live backend and the crate-integration layer (`TronChain`, chain sugar) land; allow dead_code
// until then rather than dropping fields the future write path needs.
#[derive(Clone)]
#[allow(dead_code)]
pub struct TronRpcProvider {
    info: TronChainInfo,
    rpc_url: String,
    /// Shared wallet roster; empty until the testing env attaches one at setup.
    pub(crate) wallets: Rc<WalletFactory>,
    /// Per-label derived-signer cache (derive once, reuse).
    pub(crate) signers: Rc<RefCell<HashMap<String, PrivateKeySigner>>>,
}

impl TronRpcProvider {
    /// Create an RPC provider bound to a chain's metadata.
    ///
    /// Stays infallible so `NILE.rpc(wallets)` sugar keeps working; a missing or empty `rpc_url`
    /// would surface as an error at the first network call instead (once the live backend lands).
    pub fn new(info: TronChainInfo, wallets: Rc<WalletFactory>) -> Self {
        let rpc_url = info.rpc_url.unwrap_or("").to_string();
        Self {
            info,
            rpc_url,
            wallets,
            signers: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    // ----- Write paths: unimplemented in v1 (no live java-tron client). -----

    /// Deploy bytecode via a create transaction signed by `signer`.
    ///
    /// DEFERRED: the live path signs and broadcasts via gRPC `TriggerSmartContract`, then resolves
    /// the new contract address from the transaction receipt. Unimplemented in v1.
    pub async fn deploy_create(
        &self,
        _bytecode: Bytes,
        _constructor_args: impl AsRef<[u8]>,
        _signer: &PrivateKeySigner,
    ) -> Result<TronAddress, TronError> {
        Err(TronError::Unimplemented("tron rpc deploy_create".into()))
    }

    /// Execute a state-mutating call against `to`, signed by `signer`.
    ///
    /// DEFERRED: the live path signs and broadcasts via gRPC `TriggerSmartContract` and reads the
    /// emitted logs back via `GET /v1/transactions/{txid}/events`. Unimplemented in v1.
    pub async fn call(
        &self,
        _to: &TronAddress,
        _calldata: impl AsRef<[u8]>,
        _signer: &PrivateKeySigner,
    ) -> Result<TronExecution, TronError> {
        Err(TronError::Unimplemented("tron rpc call".into()))
    }

    /// Run a read-only static call against `to`.
    ///
    /// DEFERRED: the live path issues an `eth_call` against java-tron's EVM-compatible JSON-RPC.
    /// Unimplemented in v1.
    pub async fn static_call(
        &self,
        _to: &TronAddress,
        _calldata: impl AsRef<[u8]>,
    ) -> Result<Bytes, TronError> {
        Err(TronError::Unimplemented("tron rpc static_call".into()))
    }
}

impl ChainProvider for TronRpcProvider {
    type Spec = TronChainInfo;
    type Address = TronAddress;
    type Account = TronAddress;
    type Balance = u64;
    type Error = TronError;

    fn chain_info(&self) -> &Self::Spec {
        &self.info
    }

    async fn new_account(&mut self, label: &str) -> TronAddress {
        // No signing backend in the read-only phase; return a deterministic placeholder
        // address. Real key derivation arrives with the write (sign + broadcast) pass.
        address_from_label(label)
    }

    async fn balance(&self, _addr: &TronAddress) -> Result<u64, TronError> {
        // v1 has no live read path. The real backend would query the node for the account's
        // native (TRX, in sun) balance; until then report zero.
        Ok(0)
    }

    async fn set_balance(&mut self, _addr: &TronAddress, _amount: u64) -> Result<(), TronError> {
        // Cannot mint on a real chain. Use a faucet; declared funding is validated, not minted.
        Err(TronError::Unimplemented("tron rpc set_balance".into()))
    }

    async fn block_height(&self) -> u64 {
        // v1 has no live read path; the real backend would query the latest block number.
        0
    }

    async fn advance_blocks(&mut self, _n: u64) {
        // No-op: a real chain advances on its own; tests poll instead of forcing blocks.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chains::NILE;
    use cross_vm_core::{ChainProvider, WalletFactory};
    use std::rc::Rc;

    #[tokio::test]
    async fn set_balance_unimplemented() {
        let mut c = TronRpcProvider::new(NILE, Rc::new(WalletFactory::from_roster(&[]).unwrap()));
        let a = c.new_account("x").await;
        assert!(c.set_balance(&a, 1).await.is_err());
    }

    #[tokio::test]
    async fn new_account_is_tron_shaped() {
        let mut c = TronRpcProvider::new(NILE, Rc::new(WalletFactory::from_roster(&[]).unwrap()));
        let a = c.new_account("x").await;
        assert!(a.to_base58().starts_with('T'));
    }

    #[tokio::test]
    async fn deploy_unimplemented() {
        let c = TronRpcProvider::new(NILE, Rc::new(WalletFactory::from_roster(&[]).unwrap()));
        let signer = PrivateKeySigner::random();
        let res = c
            .deploy_create(Bytes::new(), Vec::<u8>::new(), &signer)
            .await;
        assert!(matches!(res, Err(TronError::Unimplemented(_))));
    }
}
