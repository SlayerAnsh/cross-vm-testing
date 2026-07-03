//! EVM vault ABI bindings and creation bytecode from the forge artifact.
#![allow(missing_docs)] // sol!-generated items are undocumented by construction.

alloy::sol!(
    #[sol(abi)]
    Vault,
    "../../contracts/solidity/out/Vault.sol/Vault.json"
);
