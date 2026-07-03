//! Tron vault bindings: the same contract compiled by tronc (tronbox). Consumers take
//! `Vault::BYTECODE` from here and reuse the EVM call types.
#![allow(missing_docs)] // sol!-generated items are undocumented by construction.

alloy::sol!(
    #[sol(abi)]
    Vault,
    "../../contracts/tron/build/Vault.json"
);
