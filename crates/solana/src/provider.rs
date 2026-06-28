//! Solana chain providers.
//!
//! [`SvmMockProvider`] wraps a `litesvm` `LiteSVM` instance. [`SvmRpcProvider`] is a
//! phase-1 stub returning [`SvmError::Unimplemented`] for every operation.
//!
//! Solana has no notion of an "address without a key" for sending transactions, so the
//! mock keeps the [`Keypair`] generated for each account and looks it up by pubkey when
//! signing.

use std::collections::HashMap;

use cross_vm_core::{ChainKind, ChainProvider, CrossVmError};
use litesvm::types::TransactionMetadata;
use litesvm::LiteSVM;
use solana_account::Account;
use solana_address::Address;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_native_token::LAMPORTS_PER_SOL;
use solana_signer::Signer;
use solana_transaction::Transaction;
use thiserror::Error;

use crate::chains::SolanaChainInfo;

/// Default funding handed to accounts created via [`ChainProvider::new_account`]:
/// 100 SOL in lamports.
pub const DEFAULT_FUNDING_LAMPORTS: u64 = 100 * LAMPORTS_PER_SOL;

/// Errors surfaced by the Solana providers.
#[derive(Debug, Error)]
pub enum SvmError {
    /// Program deployment failed.
    #[error("deploy: {0}")]
    Deploy(String),
    /// Transaction execution failed.
    #[error("execute: {0}")]
    Execute(String),
    /// A query failed.
    #[error("query: {0}")]
    Query(String),
    /// A balance operation failed.
    #[error("balance: {0}")]
    Balance(String),
    /// Feature not implemented yet (live RPC in phase 1).
    #[error("unimplemented: {0}")]
    Unimplemented(String),
}

impl From<SvmError> for CrossVmError {
    fn from(e: SvmError) -> Self {
        let kind = ChainKind::Svm;
        match e {
            SvmError::Deploy(reason) => CrossVmError::Deploy { kind, reason },
            SvmError::Execute(reason) => CrossVmError::Execute { kind, reason },
            SvmError::Query(reason) => CrossVmError::Query { kind, reason },
            SvmError::Balance(reason) => CrossVmError::Balance { kind, reason },
            SvmError::Unimplemented(what) => CrossVmError::Unimplemented { kind, what },
        }
    }
}

/// In-process Solana provider backed by `litesvm`.
pub struct SvmMockProvider {
    svm: LiteSVM,
    info: SolanaChainInfo,
    keypairs: HashMap<Address, Keypair>,
    slot: u64,
}

impl SvmMockProvider {
    /// Build a fresh mock chain from a predefined [`SolanaChainInfo`].
    pub fn new(info: SolanaChainInfo) -> Self {
        Self {
            svm: LiteSVM::new(),
            info,
            keypairs: HashMap::new(),
            slot: 0,
        }
    }

    /// Borrow the keypair previously generated for an address, if any.
    pub fn keypair(&self, addr: &Address) -> Option<&Keypair> {
        self.keypairs.get(addr)
    }

    /// Borrow the underlying `LiteSVM` for advanced use.
    pub fn svm(&self) -> &LiteSVM {
        &self.svm
    }

    /// Mutably borrow the underlying `LiteSVM`.
    pub fn svm_mut(&mut self) -> &mut LiteSVM {
        &mut self.svm
    }
}

impl ChainProvider for SvmMockProvider {
    type Spec = SolanaChainInfo;
    type Address = Address;
    type Account = Keypair;
    type Code = Vec<u8>;
    type InitMsg = ();
    type ExecMsg = Vec<Instruction>;
    type QueryMsg = ();
    type ContractRef = Address;
    type Response = TransactionMetadata;
    type QueryResponse = Option<Account>;
    type Balance = u64;
    type Error = SvmError;

    fn chain_info(&self) -> &Self::Spec {
        &self.info
    }

    fn new_account(&mut self, _label: &str) -> Address {
        let kp = Keypair::new();
        let pubkey = kp.pubkey();
        let _ = self.svm.airdrop(&pubkey, DEFAULT_FUNDING_LAMPORTS);
        self.keypairs.insert(pubkey, kp);
        pubkey
    }

    fn balance(&self, addr: &Address) -> Result<u64, SvmError> {
        Ok(self.svm.get_balance(addr).unwrap_or(0))
    }

    fn set_balance(&mut self, addr: &Address, amount: u64) -> Result<(), SvmError> {
        let account = Account {
            lamports: amount,
            data: Vec::new(),
            owner: solana_system_interface::program::ID,
            executable: false,
            rent_epoch: u64::MAX,
        };
        self.svm
            .set_account(*addr, account)
            .map_err(|e| SvmError::Balance(format!("{e:?}")))
    }

    fn block_height(&self) -> u64 {
        self.slot
    }

    fn advance_blocks(&mut self, n: u64) {
        self.slot += n;
        self.svm.warp_to_slot(self.slot);
    }

    fn deploy(&mut self, code: Vec<u8>, _init: (), _sender: &Address) -> Result<Address, SvmError> {
        let program = Keypair::new();
        let program_id = program.pubkey();
        self.svm
            .add_program(program_id, &code)
            .map_err(|e| SvmError::Deploy(format!("{e:?}")))?;
        Ok(program_id)
    }

    fn execute(
        &mut self,
        _contract: &Address,
        msg: Vec<Instruction>,
        sender: &Address,
    ) -> Result<TransactionMetadata, SvmError> {
        let blockhash = self.svm.latest_blockhash();
        let tx = {
            let kp = self
                .keypairs
                .get(sender)
                .ok_or_else(|| SvmError::Execute(format!("no keypair for sender {sender}")))?;
            let payer = kp.pubkey();
            Transaction::new_signed_with_payer(&msg, Some(&payer), &[kp], blockhash)
        };
        self.svm
            .send_transaction(tx)
            .map_err(|fail| SvmError::Execute(format!("{:?}", fail.err)))
    }

    fn query(&self, contract: &Address, _msg: ()) -> Result<Option<Account>, SvmError> {
        Ok(self.svm.get_account(contract))
    }
}

/// Phase-1 stub for a live-RPC Solana provider. Constructs fine; every operation returns
/// [`SvmError::Unimplemented`].
pub struct SvmRpcProvider {
    info: SolanaChainInfo,
}

impl SvmRpcProvider {
    /// Create an RPC provider bound to a cluster's metadata.
    pub fn new(info: SolanaChainInfo) -> Self {
        Self { info }
    }
}

impl ChainProvider for SvmRpcProvider {
    type Spec = SolanaChainInfo;
    type Address = Address;
    type Account = Keypair;
    type Code = Vec<u8>;
    type InitMsg = ();
    type ExecMsg = Vec<Instruction>;
    type QueryMsg = ();
    type ContractRef = Address;
    type Response = TransactionMetadata;
    type QueryResponse = Option<Account>;
    type Balance = u64;
    type Error = SvmError;

    fn chain_info(&self) -> &Self::Spec {
        &self.info
    }

    fn new_account(&mut self, _label: &str) -> Address {
        Keypair::new().pubkey()
    }

    fn balance(&self, _addr: &Address) -> Result<u64, SvmError> {
        Err(SvmError::Unimplemented("rpc balance".into()))
    }

    fn set_balance(&mut self, _addr: &Address, _amount: u64) -> Result<(), SvmError> {
        Err(SvmError::Unimplemented("rpc set_balance".into()))
    }

    fn block_height(&self) -> u64 {
        0
    }

    fn advance_blocks(&mut self, _n: u64) {}

    fn deploy(&mut self, _code: Vec<u8>, _init: (), _sender: &Address) -> Result<Address, SvmError> {
        Err(SvmError::Unimplemented("rpc deploy".into()))
    }

    fn execute(
        &mut self,
        _contract: &Address,
        _msg: Vec<Instruction>,
        _sender: &Address,
    ) -> Result<TransactionMetadata, SvmError> {
        Err(SvmError::Unimplemented("rpc execute".into()))
    }

    fn query(&self, _contract: &Address, _msg: ()) -> Result<Option<Account>, SvmError> {
        Err(SvmError::Unimplemented("rpc query".into()))
    }
}
