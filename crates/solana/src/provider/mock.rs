//! In-process Solana provider backed by `litesvm`.
//!
//! Solana has no notion of an "address without a key" for sending transactions, so
//! [`SvmMockProvider`] keeps the [`Keypair`] generated for each account and looks it up
//! by pubkey when signing.

use std::cell::{Cell, Ref, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use cross_vm_core::{BlockTime, ChainProvider, WalletFactory};
use litesvm::types::TransactionMetadata;
use litesvm::LiteSVM;
use solana_account::Account;
use solana_address::Address;
use solana_clock::Clock;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_native_token::LAMPORTS_PER_SOL;
use solana_signer::Signer;
use solana_system_interface::instruction::transfer;
use solana_transaction::Transaction;

use crate::chains::SolanaChainInfo;
use crate::error::SvmError;
use crate::provider::{SvmComputeBudget, SvmDeploy, MAX_COMPUTE_UNIT_LIMIT};

/// Default funding handed to accounts created via [`ChainProvider::new_account`]:
/// 100 SOL in lamports.
pub const DEFAULT_FUNDING_LAMPORTS: u64 = 100 * LAMPORTS_PER_SOL;

/// Discriminator byte of `ComputeBudgetInstruction::SetComputeUnitLimit` in its encoded data.
const SET_COMPUTE_UNIT_LIMIT_TAG: u8 = 2;

/// Whether `ix` is a `SetComputeUnitLimit`. The runtime rejects a transaction carrying two of
/// them, so a caller-supplied one has to be caught before it collides with the one prepended here.
fn is_set_compute_unit_limit(ix: &Instruction) -> bool {
    ix.program_id == solana_compute_budget_interface::ID
        && ix.data.first() == Some(&SET_COMPUTE_UNIT_LIMIT_TAG)
}

/// The instruction list a transaction actually carries: `instructions` under a `SetComputeUnitLimit`
/// of `units`.
fn under_cap(instructions: &[Instruction], units: u32) -> Result<Vec<Instruction>, SvmError> {
    if instructions.iter().any(is_set_compute_unit_limit) {
        return Err(SvmError::Execute(
            "instructions already set a compute unit limit: the budget is the `budget` argument, \
             and a second SetComputeUnitLimit is rejected by the runtime as a duplicate"
                .into(),
        ));
    }
    let mut capped = Vec::with_capacity(instructions.len() + 1);
    capped.push(ComputeBudgetInstruction::set_compute_unit_limit(units));
    capped.extend_from_slice(instructions);
    Ok(capped)
}

/// `consumed` compute units scaled by `adjustment` and rounded up, clamped to the runtime's
/// per-transaction ceiling (which a `SetComputeUnitLimit` above is silently clamped to anyway).
pub(crate) fn adjusted(consumed: u64, adjustment: f64) -> u32 {
    let scaled = (consumed as f64) * adjustment;
    if scaled.ceil() >= f64::from(MAX_COMPUTE_UNIT_LIMIT) {
        return MAX_COMPUTE_UNIT_LIMIT;
    }
    scaled.ceil() as u32
}

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
        let mut svm = LiteSVM::new();
        // Seed the shared mock clock so Solana agrees with the EVM and cosmos chains on time, so a
        // cross-VM packet's timeout (stamped on one VM, checked on another) compares correctly.
        let mut clock = svm.get_sysvar::<Clock>();
        clock.unix_timestamp = cross_vm_core::MOCK_BLOCK_TIMESTAMP as i64;
        svm.set_sysvar(&clock);
        Self {
            svm: Rc::new(RefCell::new(svm)),
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

    /// Load a program into the chain at a fresh program id.
    ///
    /// Takes no compute budget: see [`add_program_at`](Self::add_program_at).
    pub async fn add_program(&self, bytecode: Vec<u8>) -> Result<SvmDeploy, SvmError> {
        let program_id = Keypair::new().pubkey();
        self.add_program_at(program_id, bytecode).await
    }

    /// Load a program at a specific program id.
    ///
    /// Required for frameworks like Anchor whose `declare_id!` makes the program reject
    /// execution unless it is deployed at its declared address.
    ///
    /// Takes no compute budget, unlike the other mutating operations. `litesvm::add_program`
    /// writes the program account straight into the account store: no transaction is built, no
    /// instruction is executed, and no compute units are consumed (which is also why the reported
    /// hash has to be synthesized). A cap on the compute an execution may burn has nothing to
    /// constrain here, so the parameter is not taken rather than taken and ignored.
    pub async fn add_program_at(
        &self,
        program_id: Address,
        bytecode: Vec<u8>,
    ) -> Result<SvmDeploy, SvmError> {
        let blockhash = self.svm.borrow().latest_blockhash();
        self.svm
            .borrow_mut()
            .add_program(program_id, &bytecode)
            .map_err(|e| SvmError::Deploy(format!("{e:?}")))?;
        // litesvm loads the program by writing the account store directly, so there is no signed
        // transaction to report: mint the hash from the blockhash the load landed under, then
        // expire that blockhash (as `send_transaction` does) so a repeat load of the same bytecode
        // at the same id still reports a distinct hash.
        let deploy = SvmDeploy::minted(blockhash.as_ref(), program_id, &bytecode);
        self.svm.borrow_mut().expire_blockhash();
        Ok(deploy)
    }

    /// Build the transaction `instructions` become when `signer` pays for and signs them.
    fn signed_tx(&self, instructions: &[Instruction], signer: &Keypair) -> Transaction {
        let blockhash = self.svm.borrow().latest_blockhash();
        let payer = signer.pubkey();
        Transaction::new_signed_with_payer(instructions, Some(&payer), &[signer], blockhash)
    }

    /// Simulate `instructions` under a `SetComputeUnitLimit` of `units`, without committing.
    fn simulate(
        &self,
        instructions: &[Instruction],
        units: u32,
        signer: &Keypair,
    ) -> Result<TransactionMetadata, SvmError> {
        let tx = self.signed_tx(&under_cap(instructions, units)?, signer);
        let simulated = self
            .svm
            .borrow()
            .simulate_transaction(tx)
            .map_err(|fail| SvmError::Execute(format!("{:?}", fail.err)))?;
        Ok(simulated.meta)
    }

    /// The compute-unit cap `budget` names for `instructions`, resolving [`SvmComputeBudget::Estimated`]
    /// by simulating the transaction at the runtime ceiling and scaling what it consumed.
    fn resolve(
        &self,
        instructions: &[Instruction],
        budget: SvmComputeBudget,
        signer: &Keypair,
    ) -> Result<u32, SvmError> {
        match budget {
            SvmComputeBudget::Exact(units) => Ok(units),
            SvmComputeBudget::Estimated => {
                let simulated = self.simulate(instructions, MAX_COMPUTE_UNIT_LIMIT, signer)?;
                Ok(adjusted(
                    simulated.compute_units_consumed,
                    self.info.gas_adjustment,
                ))
            }
        }
    }

    /// Sign and send a transaction built from `instructions`, capped at `budget` compute units,
    /// paid and signed by `signer`.
    ///
    /// The cap is a `SetComputeUnitLimit` instruction prepended to `instructions`, so it is part
    /// of the transaction that gets signed and it counts against itself (see [`SvmComputeBudget`]).
    /// A transaction that runs past its cap aborts with [`SvmError::Execute`] having still paid its
    /// fee, exactly as on a real cluster.
    pub async fn send_transaction(
        &self,
        instructions: impl AsRef<[Instruction]>,
        signer: &Keypair,
        budget: SvmComputeBudget,
    ) -> Result<TransactionMetadata, SvmError> {
        let instructions = instructions.as_ref();
        let units = self.resolve(instructions, budget, signer)?;
        let tx = self.signed_tx(&under_cap(instructions, units)?, signer);
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

    /// Report what the transaction built from `instructions` would consume and pay if `signer`
    /// sent it, without sending it.
    ///
    /// The transaction is run through `LiteSVM::simulate_transaction`, which executes against a
    /// `&LiteSVM`: it neither writes back the post-execution accounts nor records the signature in
    /// the transaction history, so the chain is left exactly as it was and the very same
    /// transaction can still be sent afterwards. The blockhash is deliberately not expired here,
    /// for the same reason.
    ///
    /// Returns the same [`TransactionMetadata`] a successful [`send_transaction`] reports, so a
    /// forecast and a receipt are directly comparable: the simulated transaction carries the same
    /// `SetComputeUnitLimit` instruction a sent one does (at the runtime ceiling, so the cap cannot
    /// be what aborts it), and that instruction's own 150 compute units are therefore in the
    /// reported figure. `Exact(estimate.compute_units_consumed)` is consequently the tightest
    /// budget that still executes.
    ///
    /// A transaction that would fail is an [`SvmError::Execute`], exactly as when sent: a failing
    /// execution has no meaningful cost to forecast, and reporting the compute units it burned
    /// before aborting as if it were an estimate would read as a cheap success.
    ///
    /// [`send_transaction`]: Self::send_transaction
    pub async fn estimate_transaction(
        &self,
        instructions: impl AsRef<[Instruction]>,
        signer: &Keypair,
    ) -> Result<TransactionMetadata, SvmError> {
        self.simulate(instructions.as_ref(), MAX_COMPUTE_UNIT_LIMIT, signer)
    }

    /// Transfer `amount` base units (lamports) of `denom` from `signer` to `to`, capped at `budget`
    /// compute units, returning the base58 transaction signature.
    ///
    /// `denom` must name this chain's native token (see [`ChainProvider::set_balance`]). Runs a
    /// real System Program transfer through litesvm, so an underfunded sender surfaces as
    /// [`SvmError::Execute`] and the returned signature is the one litesvm signed.
    pub async fn transfer_funds(
        &self,
        to: &Address,
        denom: &str,
        amount: u64,
        signer: &Keypair,
        budget: SvmComputeBudget,
    ) -> Result<String, SvmError> {
        if !denom.eq_ignore_ascii_case(self.info.native_symbol) {
            return Err(SvmError::Balance(format!(
                "unknown denom '{denom}': this chain's native token is '{}'",
                self.info.native_symbol
            )));
        }
        let ix = transfer(&signer.pubkey(), to, amount);
        let meta = self.send_transaction([ix], signer, budget).await?;
        Ok(meta.signature.to_string())
    }

    /// Read on-chain account data for `pubkey`.
    pub async fn get_account(&self, pubkey: &Address) -> Option<Account> {
        self.svm.borrow().get_account(pubkey)
    }

    /// Read the raw account data bytes for `pubkey` (SVM equivalent of raw storage).
    pub async fn get_account_data(&self, pubkey: &Address) -> Option<Vec<u8>> {
        self.get_account(pubkey).await.map(|a| a.data)
    }

    /// Read a fixed-width window `[offset, offset + len)` of `pubkey`'s account data.
    ///
    /// Returns `None` when the account is missing or the requested range is not fully within
    /// the account data (partial/out-of-range slices are never truncated), so a `Some` result
    /// always carries exactly `len` bytes.
    pub async fn get_account_data_slice(
        &self,
        pubkey: &Address,
        offset: usize,
        len: usize,
    ) -> Option<Vec<u8>> {
        self.get_account(pubkey)
            .await
            .and_then(|a| a.data.get(offset..offset + len).map(<[u8]>::to_vec))
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

    async fn set_balance(
        &mut self,
        addr: &Address,
        denom: &str,
        amount: u64,
    ) -> Result<(), SvmError> {
        if !denom.eq_ignore_ascii_case(self.info.native_symbol) {
            return Err(SvmError::Balance(format!(
                "unknown denom '{denom}': this chain's native token is '{}'",
                self.info.native_symbol
            )));
        }
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

    async fn advance_blocks(&mut self, n: u64, time: BlockTime) {
        let new_slot = self.slot.get() + n;
        self.slot.set(new_slot);
        let mut svm = self.svm.borrow_mut();
        // `warp_to_slot` rewrites the `Clock`, so advance the slot first, then override the
        // timestamp per `time`.
        svm.warp_to_slot(new_slot);
        let mut clock = svm.get_sysvar::<Clock>();
        let current = clock.unix_timestamp as u64;
        clock.unix_timestamp = time.apply(current) as i64;
        svm.set_sysvar(&clock);
    }
}
