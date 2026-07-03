//! EVM counter ABI bindings and creation bytecode, generated from the forge artifact.
//!
//! Access `Counter::BYTECODE`, the call types (`incrementCall`, `countCall`), etc.
#![allow(missing_docs)] // sol!-generated items are undocumented by construction.

alloy::sol!(
    #[sol(abi)]
    Counter,
    "../../contracts/solidity/out/Counter.sol/Counter.json"
);
