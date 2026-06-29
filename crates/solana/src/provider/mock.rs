//! In-process Solana provider backed by `litesvm`.
//!
//! Solana has no notion of an "address without a key" for sending transactions, so
//! [`SvmMockProvider`] keeps the [`Keypair`] generated for each account and looks it up
//! by pubkey when signing.

use std::cell::{Cell, Ref, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use cross_vm_core::{ChainProvider, WalletFactory};
use litesvm::types::TransactionMetadata;
use litesvm::LiteSVM;
use solana_account::Account;
use solana_address::Address;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_native_token::LAMPORTS_PER_SOL;
use solana_signer::Signer;
use solana_transaction::Transaction;

use crate::chains::SolanaChainInfo;
use crate::error::SvmError;

/// Default funding handed to accounts created via [`ChainProvider::new_account`]:
/// 100 SOL in lamports.
pub const DEFAULT_FUNDING_LAMPORTS: u64 = 100 * LAMPORTS_PER_SOL;

/// In-process Solana provider backed by `litesvm`.
///
/// The `LiteSVM`, the keypair map, and the slot counter live behind `Rc<RefCell<_>>` /
/// `Rc<Cell<_>>` so the handle is cheap to `clone` and every clone shares one chain state.
/// This lets a contract own its own handle (`Contract::new(chain)`) while the test still
/// drives the same chain, and lets the program operations run behind `&self`.
#[derive(Clone)]
pub struct SvmMockProvider {
    svm: Rc<RefCell<LiteSVM>>,
    info: SolanaChainInfo,
    slot: Rc<Cell<u64>>,
    /// Shared wallet roster; empty until the testing env attaches one at setup.
    pub(crate) wallets: Rc<WalletFactory>,
    /// Per-label derived-signer cache (derive once, reuse).
    pub(crate) signers: Rc<RefCell<HashMap<String, crate::wallet::SvmSigner>>>,
}

impl SvmMockProvider {
    /// Build a fresh mock chain from a predefined [`SolanaChainInfo`].
    pub fn new(info: SolanaChainInfo, wallets: Rc<WalletFactory>) -> Self {
        Self {
            svm: Rc::new(RefCell::new(LiteSVM::new())),
            info,
            slot: Rc::new(Cell::new(0)),
            wallets,
            signers: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// Borrow the underlying `LiteSVM` for advanced use.
    pub fn svm(&self) -> Ref<'_, LiteSVM> {
        self.svm.borrow()
    }

    /// Load a program into the chain and return its program id.
    pub async fn add_program(&self, bytecode: Vec<u8>) -> Result<Address, SvmError> {
        let program = Keypair::new();
        let program_id = program.pubkey();
        self.add_program_at(program_id, bytecode).await
    }

    /// Load a program at a specific program id and return it.
    ///
    /// Required for frameworks like Anchor whose `declare_id!` makes the program reject
    /// execution unless it is deployed at its declared address.
    pub async fn add_program_at(
        &self,
        program_id: Address,
        bytecode: Vec<u8>,
    ) -> Result<Address, SvmError> {
        self.svm
            .borrow_mut()
            .add_program(program_id, &bytecode)
            .map_err(|e| SvmError::Deploy(format!("{e:?}")))?;
        Ok(program_id)
    }

    /// Sign and send a transaction built from `instructions`, paid and signed by `signer`.
    pub async fn send_transaction(
        &self,
        instructions: impl AsRef<[Instruction]>,
        signer: &Keypair,
    ) -> Result<TransactionMetadata, SvmError> {
        let blockhash = self.svm.borrow().latest_blockhash();
        let payer = signer.pubkey();
        let tx = Transaction::new_signed_with_payer(
            instructions.as_ref(),
            Some(&payer),
            &[signer],
            blockhash,
        );
        let meta = self
            .svm
            .borrow_mut()
            .send_transaction(tx)
            .map_err(|fail| SvmError::Execute(format!("{:?}", fail.err)))?;
        // Advance the blockhash so a subsequent identical transaction (e.g. two increments in
        // a row) produces a distinct signature instead of being rejected as `AlreadyProcessed`.
        self.svm.borrow_mut().expire_blockhash();
        Ok(meta)
    }

    /// Read on-chain account data for `pubkey`.
    pub async fn get_account(&self, pubkey: &Address) -> Option<Account> {
        self.svm.borrow().get_account(pubkey)
    }
}

impl ChainProvider for SvmMockProvider {
    type Spec = SolanaChainInfo;
    type Address = Address;
    type Account = Keypair;
    type Balance = u64;
    type Error = SvmError;

    fn chain_info(&self) -> &Self::Spec {
        &self.info
    }

    async fn new_account(&mut self, _label: &str) -> Address {
        // A throwaway funded account for balance/read scenarios. Sending transactions goes
        // through wallet labels (the factory owns the keys), so the keypair is not retained.
        let pubkey = Keypair::new().pubkey();
        let _ = self
            .svm
            .borrow_mut()
            .airdrop(&pubkey, DEFAULT_FUNDING_LAMPORTS);
        pubkey
    }

    async fn balance(&self, addr: &Address) -> Result<u64, SvmError> {
        Ok(self.svm.borrow().get_balance(addr).unwrap_or(0))
    }

    async fn set_balance(&mut self, addr: &Address, amount: u64) -> Result<(), SvmError> {
        let account = Account {
            lamports: amount,
            data: Vec::new(),
            owner: solana_system_interface::program::ID,
            executable: false,
            rent_epoch: u64::MAX,
        };
        self.svm
            .borrow_mut()
            .set_account(*addr, account)
            .map_err(|e| SvmError::Balance(format!("{e:?}")))
    }

    async fn block_height(&self) -> u64 {
        self.slot.get()
    }

    async fn advance_blocks(&mut self, n: u64) {
        let new_slot = self.slot.get() + n;
        self.slot.set(new_slot);
        self.svm.borrow_mut().warp_to_slot(new_slot);
    }
}
