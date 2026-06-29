//! A minimal collateralized-debt vault for cross-VM harness testing (Solana / Anchor port).
//!
//! Mirrors the CosmWasm and EVM vaults: a pure accounting ledger (no token transfers) where a
//! user deposits collateral, borrows debt up to an LTV fraction of it, repays, and withdraws
//! collateral not locked by debt. Each user owns a `Vault` PDA at `["vault", user]`. The reverts
//! (via `require!`) are the rejection paths a property test exercises; identical LTV math across
//! all three VMs lets one shadow model validate every chain.

use anchor_lang::prelude::*;

declare_id!("GFNizKSbcjBH7aTwPyyA3vnqfksjWEfci6fgWeCJ34GB");

/// Loan-to-value, in basis points (5000 = 50%): max debt is `collateral * LTV_BPS / 10000`.
const LTV_BPS: u128 = 5000;

/// Maximum debt a given collateral can support. Widened to `u128` to avoid overflow, then back.
fn max_debt(collateral: u64) -> u64 {
    ((collateral as u128 * LTV_BPS) / 10000) as u64
}

/// Collateral that must remain locked to back `debt`. Inverse of [`max_debt`], rounded up so a
/// borrower can never withdraw into bad debt.
fn required_collateral(debt: u64) -> u64 {
    ((debt as u128 * 10000 + LTV_BPS - 1) / LTV_BPS) as u64
}

#[program]
pub mod vault {
    use super::*;

    /// Create the caller's vault PDA with zero collateral and debt.
    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        let v = &mut ctx.accounts.vault;
        v.collateral = 0;
        v.debt = 0;
        v.bump = ctx.bumps.vault;
        Ok(())
    }

    /// Credit `amount` of collateral to the caller.
    pub fn deposit(ctx: Context<Update>, amount: u64) -> Result<()> {
        let v = &mut ctx.accounts.vault;
        v.collateral = v.collateral.saturating_add(amount);
        Ok(())
    }

    /// Withdraw collateral not locked by outstanding debt.
    pub fn withdraw(ctx: Context<Update>, amount: u64) -> Result<()> {
        let v = &mut ctx.accounts.vault;
        require!(amount <= v.collateral, VaultError::AmountExceedsCollateral);
        require!(
            v.collateral - amount >= required_collateral(v.debt),
            VaultError::InsufficientFreeCollateral
        );
        v.collateral -= amount;
        Ok(())
    }

    /// Borrow against collateral, up to the LTV limit.
    pub fn borrow(ctx: Context<Update>, amount: u64) -> Result<()> {
        let v = &mut ctx.accounts.vault;
        let new_debt = v.debt.saturating_add(amount);
        require!(new_debt <= max_debt(v.collateral), VaultError::ExceedsMaxDebt);
        v.debt = new_debt;
        Ok(())
    }

    /// Repay outstanding debt.
    pub fn repay(ctx: Context<Update>, amount: u64) -> Result<()> {
        let v = &mut ctx.accounts.vault;
        require!(amount <= v.debt, VaultError::RepayExceedsDebt);
        v.debt -= amount;
        Ok(())
    }
}

/// Per-user vault state. Anchor lays this out as an 8-byte discriminator followed by the fields
/// in declaration order, so off-chain readers find `collateral` at bytes 8..16 and `debt` at
/// 16..24.
#[account]
#[derive(InitSpace)]
pub struct Vault {
    pub collateral: u64,
    pub debt: u64,
    pub bump: u8,
}

/// Accounts for [`vault::initialize`]: creates the per-user PDA, paid for by the user.
#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(
        init,
        payer = user,
        space = 8 + Vault::INIT_SPACE,
        seeds = [b"vault", user.key().as_ref()],
        bump
    )]
    pub vault: Account<'info, Vault>,
    #[account(mut)]
    pub user: Signer<'info>,
    pub system_program: Program<'info, System>,
}

/// Accounts for the mutating instructions: the caller's existing PDA plus the signing user.
#[derive(Accounts)]
pub struct Update<'info> {
    #[account(
        mut,
        seeds = [b"vault", user.key().as_ref()],
        bump = vault.bump,
    )]
    pub vault: Account<'info, Vault>,
    pub user: Signer<'info>,
}

/// Reverts that map to a failed transaction (a legitimate rejection in the harness).
#[error_code]
pub enum VaultError {
    #[msg("amount exceeds collateral")]
    AmountExceedsCollateral,
    #[msg("insufficient free collateral")]
    InsufficientFreeCollateral,
    #[msg("exceeds max debt")]
    ExceedsMaxDebt,
    #[msg("repay exceeds debt")]
    RepayExceedsDebt,
}
