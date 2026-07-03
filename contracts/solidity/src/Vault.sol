// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// A minimal collateralized-debt vault for cross-VM harness testing.
///
/// A pure accounting ledger (no token transfers, so it fits the framework's value-less call
/// API): a user deposits collateral, may borrow debt up to an LTV fraction of that collateral,
/// repays debt, and withdraws collateral that is not locked by outstanding debt. The reverts
/// make the rejection paths a property test exercises explicit.
contract Vault {
    /// Loan-to-value, in basis points (5000 = 50%): max debt is `collateral * LTV_BPS / 10000`.
    uint256 public constant LTV_BPS = 5000;

    mapping(address => uint256) private collateral;
    mapping(address => uint256) private debt;

    uint256 public totalCollateral;
    uint256 public totalDebt;

    /// Credit `amount` of collateral to the caller.
    function deposit(uint256 amount) external {
        collateral[msg.sender] += amount;
        totalCollateral += amount;
    }

    /// Withdraw collateral not locked by outstanding debt.
    function withdraw(uint256 amount) external {
        uint256 c = collateral[msg.sender];
        require(amount <= c, "amount exceeds collateral");
        require(c - amount >= requiredCollateral(debt[msg.sender]), "insufficient free collateral");
        collateral[msg.sender] = c - amount;
        totalCollateral -= amount;
    }

    /// Borrow against collateral, up to the LTV limit.
    function borrow(uint256 amount) external {
        uint256 newDebt = debt[msg.sender] + amount;
        require(newDebt <= maxDebt(collateral[msg.sender]), "exceeds max debt");
        debt[msg.sender] = newDebt;
        totalDebt += amount;
    }

    /// Repay outstanding debt.
    function repay(uint256 amount) external {
        uint256 d = debt[msg.sender];
        require(amount <= d, "repay exceeds debt");
        debt[msg.sender] = d - amount;
        totalDebt -= amount;
    }

    /// Collateral held by `who`.
    function collateralOf(address who) external view returns (uint256) {
        return collateral[who];
    }

    /// Debt owed by `who`.
    function debtOf(address who) external view returns (uint256) {
        return debt[who];
    }

    /// Maximum debt a given collateral can support.
    function maxDebt(uint256 c) public pure returns (uint256) {
        return (c * LTV_BPS) / 10000;
    }

    /// Collateral that must remain locked to back `d` of debt.
    function requiredCollateral(uint256 d) public pure returns (uint256) {
        // Inverse of maxDebt, rounded up so a borrower can never withdraw into bad debt.
        return (d * 10000 + LTV_BPS - 1) / LTV_BPS;
    }
}
