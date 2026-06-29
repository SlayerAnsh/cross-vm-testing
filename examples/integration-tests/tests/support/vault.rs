//! Cross-VM `Vault` wrapper: a collateralized-debt ledger on CosmWasm, EVM, and Solana.
//!
//! deposit / withdraw / borrow / repay, identical on any supported VM. Ledger-only (no token
//! transfers), 50% LTV. The same logic and reverts on each VM let one harness drive all three
//! (`harness/vault.rs`).

use cross_vm_framework::prelude::*;

use cross_vm_solana::Address as SvmAddress;
use cross_vm_solidity::Bytes;
use solana_instruction::{AccountMeta, Instruction};

mod cw_vault {
    use cosmwasm_std::Empty;
    use cw_multi_test::{Contract, ContractWrapper};

    pub use vault::{AmountResponse, ExecuteMsg, InstantiateMsg, QueryMsg};

    pub fn contract() -> Box<dyn Contract<Empty, Empty>> {
        Box::new(ContractWrapper::new(
            vault::execute,
            vault::instantiate,
            vault::query,
        ))
    }
}

mod evm_vault {
    alloy::sol!(
        #[sol(abi)]
        Vault,
        "../solidity-contracts/out/Vault.sol/Vault.json"
    );
}

const VAULT_PROGRAM_ID: &str = "GFNizKSbcjBH7aTwPyyA3vnqfksjWEfci6fgWeCJ34GB";
const VDISC_INITIALIZE: [u8; 8] = [175, 175, 109, 31, 13, 152, 155, 237];
const VDISC_DEPOSIT: [u8; 8] = [242, 35, 198, 137, 82, 225, 242, 182];
const VDISC_WITHDRAW: [u8; 8] = [183, 18, 70, 156, 148, 109, 161, 34];
const VDISC_BORROW: [u8; 8] = [228, 253, 131, 202, 207, 116, 89, 18];
const VDISC_REPAY: [u8; 8] = [234, 103, 67, 82, 208, 234, 219, 166];
/// Built by `make compile-solana` (`cargo-build-sbf`).
const VAULT_SO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../solana-contracts/target/deploy/vault.so"
));

/// A cross-VM collateralized-debt vault: deposit / withdraw / borrow / repay, identical on any
/// supported VM. Ledger-only (no token transfers), 50% LTV. The same logic and reverts on each
/// VM let one harness drive all three.
pub struct Vault {
    base: ContractBase,
}

impl Vault {
    /// A vault not yet deployed on `chain`. Call [`Vault::setup`] to deploy.
    pub fn new(chain: AnyChain) -> Self {
        Self {
            base: ContractBase::new(chain),
        }
    }

    /// A vault attached to an already-deployed instance at `address` (a contract address, or the
    /// program id on Solana). Lets the harness rebuild a handle from a stored address.
    pub fn instance(chain: AnyChain, address: Account) -> Self {
        Self {
            base: ContractBase::with_address(chain, address),
        }
    }

    /// The deployed instance address, if [`Vault::setup`] (or [`Vault::instance`]) bound one.
    pub fn address(&self) -> Option<Account> {
        self.base.address()
    }

    /// Deploy the vault, signed by `wallet`.
    pub async fn setup(&self, wallet: &str) -> Result<(), CrossVmError> {
        match self.base.kind() {
            ChainKind::CosmWasm => {
                let chain = self.base.cosmwasm()?;
                let code_id = chain.store_code(cw_vault::contract()).await?;
                let addr = chain
                    .instantiate(
                        code_id,
                        cw_vault::InstantiateMsg {},
                        WalletLabel::wrap(wallet),
                        &[],
                        "vault",
                    )
                    .await?;
                self.base.set_address(Account::CosmWasm(addr));
                Ok(())
            }
            ChainKind::Evm => {
                let chain = self.base.evm()?;
                let addr = chain
                    .deploy_create(
                        evm_vault::Vault::BYTECODE.clone(),
                        Bytes::new(),
                        WalletLabel::wrap(wallet),
                    )
                    .await?;
                self.base.set_address(Account::Evm(addr));
                Ok(())
            }
            ChainKind::Svm => {
                let chain = self.base.solana()?;
                let program_id = SvmAddress::from_str_const(VAULT_PROGRAM_ID);
                chain.add_program_at(program_id, VAULT_SO.to_vec()).await?;
                // Per-user PDAs are created lazily on first deposit; store the program id.
                self.base.set_address(Account::Svm(program_id));
                Ok(())
            }
        }
    }

    /// Credit `amount` of collateral to `wallet`.
    pub async fn deposit(
        &self,
        wallet: &str,
        amount: u128,
    ) -> Result<AppResponse<()>, CrossVmError> {
        match self.base.kind() {
            ChainKind::CosmWasm => {
                self.cw_exec(
                    wallet,
                    cw_vault::ExecuteMsg::Deposit {
                        amount: amount.into(),
                    },
                )
                .await
            }
            ChainKind::Evm => self.evm_exec(wallet, evm_deposit(amount)).await,
            ChainKind::Svm => {
                self.svm_ensure_init(wallet).await?;
                self.svm_exec(wallet, &VDISC_DEPOSIT, amount).await
            }
        }
    }

    /// Withdraw `amount` of free collateral for `wallet`.
    pub async fn withdraw(
        &self,
        wallet: &str,
        amount: u128,
    ) -> Result<AppResponse<()>, CrossVmError> {
        match self.base.kind() {
            ChainKind::CosmWasm => {
                self.cw_exec(
                    wallet,
                    cw_vault::ExecuteMsg::Withdraw {
                        amount: amount.into(),
                    },
                )
                .await
            }
            ChainKind::Evm => self.evm_exec(wallet, evm_withdraw(amount)).await,
            ChainKind::Svm => self.svm_exec(wallet, &VDISC_WITHDRAW, amount).await,
        }
    }

    /// Borrow `amount` of debt against `wallet`'s collateral.
    pub async fn borrow(
        &self,
        wallet: &str,
        amount: u128,
    ) -> Result<AppResponse<()>, CrossVmError> {
        match self.base.kind() {
            ChainKind::CosmWasm => {
                self.cw_exec(
                    wallet,
                    cw_vault::ExecuteMsg::Borrow {
                        amount: amount.into(),
                    },
                )
                .await
            }
            ChainKind::Evm => self.evm_exec(wallet, evm_borrow(amount)).await,
            ChainKind::Svm => self.svm_exec(wallet, &VDISC_BORROW, amount).await,
        }
    }

    /// Repay `amount` of `wallet`'s debt.
    pub async fn repay(&self, wallet: &str, amount: u128) -> Result<AppResponse<()>, CrossVmError> {
        match self.base.kind() {
            ChainKind::CosmWasm => {
                self.cw_exec(
                    wallet,
                    cw_vault::ExecuteMsg::Repay {
                        amount: amount.into(),
                    },
                )
                .await
            }
            ChainKind::Evm => self.evm_exec(wallet, evm_repay(amount)).await,
            ChainKind::Svm => self.svm_exec(wallet, &VDISC_REPAY, amount).await,
        }
    }

    /// Collateral held by `wallet`.
    pub async fn collateral_of(&self, wallet: &str) -> Result<u128, CrossVmError> {
        match self.base.kind() {
            ChainKind::CosmWasm => self.cw_query_amount(wallet, true).await,
            ChainKind::Evm => self.evm_view_user(wallet, true).await,
            ChainKind::Svm => Ok(self.svm_read(wallet).await?.0),
        }
    }

    /// Debt owed by `wallet`.
    pub async fn debt_of(&self, wallet: &str) -> Result<u128, CrossVmError> {
        match self.base.kind() {
            ChainKind::CosmWasm => self.cw_query_amount(wallet, false).await,
            ChainKind::Evm => self.evm_view_user(wallet, false).await,
            ChainKind::Svm => Ok(self.svm_read(wallet).await?.1),
        }
    }

    // ----- CosmWasm -----
    async fn cw_exec(
        &self,
        wallet: &str,
        msg: cw_vault::ExecuteMsg,
    ) -> Result<AppResponse<()>, CrossVmError> {
        let chain = self.base.cosmwasm()?;
        let addr = self.base.cw_addr()?;
        let raw = chain
            .execute_contract(&addr, msg, WalletLabel::wrap(wallet), &[])
            .await?;
        Ok(AppResponse::cosmwasm((), raw))
    }

    async fn cw_query_amount(&self, wallet: &str, collateral: bool) -> Result<u128, CrossVmError> {
        let chain = self.base.cosmwasm()?;
        let addr = self.base.cw_addr()?;
        let who = chain
            .wallet_address(WalletLabel::wrap(wallet))
            .await?
            .to_string();
        let msg = if collateral {
            cw_vault::QueryMsg::Collateral { who }
        } else {
            cw_vault::QueryMsg::Debt { who }
        };
        let resp: cw_vault::AmountResponse = chain.query_wasm_smart(&addr, msg).await?;
        Ok(resp.amount.u128())
    }

    // ----- EVM -----
    async fn evm_exec(
        &self,
        wallet: &str,
        calldata: Bytes,
    ) -> Result<AppResponse<()>, CrossVmError> {
        let chain = self.base.evm()?;
        let addr = self.base.evm_addr()?;
        let exec = chain
            .call(&addr, calldata, WalletLabel::wrap(wallet))
            .await?;
        Ok(AppResponse::evm((), exec.output, exec.logs))
    }

    async fn evm_view_user(&self, wallet: &str, collateral: bool) -> Result<u128, CrossVmError> {
        use alloy::sol_types::SolCall;
        let chain = self.base.evm()?;
        let addr = self.base.evm_addr()?;
        let who = chain.wallet_address(WalletLabel::wrap(wallet)).await?;
        if collateral {
            let cd = Bytes::from(evm_vault::Vault::collateralOfCall { who }.abi_encode());
            let out = chain.static_call(&addr, cd).await?;
            Ok(evm_vault::Vault::collateralOfCall::abi_decode_returns(&out)
                .map_err(decode_err)?
                .saturating_to::<u128>())
        } else {
            let cd = Bytes::from(evm_vault::Vault::debtOfCall { who }.abi_encode());
            let out = chain.static_call(&addr, cd).await?;
            Ok(evm_vault::Vault::debtOfCall::abi_decode_returns(&out)
                .map_err(decode_err)?
                .saturating_to::<u128>())
        }
    }

    // ----- Solana -----
    fn svm_pda(&self, user: &SvmAddress) -> SvmAddress {
        let program_id = SvmAddress::from_str_const(VAULT_PROGRAM_ID);
        SvmAddress::find_program_address(&[b"vault", user.as_ref()], &program_id).0
    }

    async fn svm_ensure_init(&self, wallet: &str) -> Result<(), CrossVmError> {
        let chain = self.base.solana()?;
        let program_id = SvmAddress::from_str_const(VAULT_PROGRAM_ID);
        let user = chain.wallet_address(WalletLabel::wrap(wallet)).await?;
        let pda = self.svm_pda(&user);
        if chain.get_account(&pda).await?.is_none() {
            let ix = Instruction::new_with_bytes(
                program_id,
                &VDISC_INITIALIZE,
                vec![
                    AccountMeta::new(pda, false),
                    AccountMeta::new(user, true),
                    AccountMeta::new_readonly(solana_system_interface::program::ID, false),
                ],
            );
            chain
                .send_transaction(vec![ix], WalletLabel::wrap(wallet))
                .await?;
        }
        Ok(())
    }

    async fn svm_exec(
        &self,
        wallet: &str,
        disc: &[u8; 8],
        amount: u128,
    ) -> Result<AppResponse<()>, CrossVmError> {
        let chain = self.base.solana()?;
        let program_id = SvmAddress::from_str_const(VAULT_PROGRAM_ID);
        let user = chain.wallet_address(WalletLabel::wrap(wallet)).await?;
        let pda = self.svm_pda(&user);
        let mut data = disc.to_vec();
        data.extend_from_slice(&(amount as u64).to_le_bytes());
        let ix = Instruction::new_with_bytes(
            program_id,
            &data,
            vec![
                AccountMeta::new(pda, false),
                AccountMeta::new_readonly(user, true),
            ],
        );
        let meta = chain
            .send_transaction(vec![ix], WalletLabel::wrap(wallet))
            .await?;
        Ok(AppResponse::solana((), meta))
    }

    /// Read `(collateral, debt)` from a user's PDA. A missing PDA reads as `(0, 0)`.
    async fn svm_read(&self, wallet: &str) -> Result<(u128, u128), CrossVmError> {
        let chain = self.base.solana()?;
        let user = chain.wallet_address(WalletLabel::wrap(wallet)).await?;
        let pda = self.svm_pda(&user);
        match chain.get_account(&pda).await? {
            None => Ok((0, 0)),
            Some(acct) => {
                // Anchor layout: 8-byte discriminator, collateral u64 (LE), debt u64 (LE).
                let c = le_u64(&acct.data, 8)?;
                let d = le_u64(&acct.data, 16)?;
                Ok((c as u128, d as u128))
            }
        }
    }
}

fn evm_deposit(amount: u128) -> Bytes {
    use alloy::sol_types::SolCall;
    Bytes::from(
        evm_vault::Vault::depositCall {
            amount: u256(amount),
        }
        .abi_encode(),
    )
}
fn evm_withdraw(amount: u128) -> Bytes {
    use alloy::sol_types::SolCall;
    Bytes::from(
        evm_vault::Vault::withdrawCall {
            amount: u256(amount),
        }
        .abi_encode(),
    )
}
fn evm_borrow(amount: u128) -> Bytes {
    use alloy::sol_types::SolCall;
    Bytes::from(
        evm_vault::Vault::borrowCall {
            amount: u256(amount),
        }
        .abi_encode(),
    )
}
fn evm_repay(amount: u128) -> Bytes {
    use alloy::sol_types::SolCall;
    Bytes::from(
        evm_vault::Vault::repayCall {
            amount: u256(amount),
        }
        .abi_encode(),
    )
}

fn u256(amount: u128) -> cross_vm_solidity::U256 {
    cross_vm_solidity::U256::from(amount)
}

fn le_u64(data: &[u8], offset: usize) -> Result<u64, CrossVmError> {
    let bytes = data
        .get(offset..offset + 8)
        .ok_or_else(|| CrossVmError::Query {
            kind: ChainKind::Svm,
            reason: "vault account too small".into(),
        })?;
    let arr: [u8; 8] = bytes.try_into().expect("8 bytes");
    Ok(u64::from_le_bytes(arr))
}

fn decode_err(e: impl core::fmt::Display) -> CrossVmError {
    CrossVmError::Query {
        kind: ChainKind::Evm,
        reason: e.to_string(),
    }
}
