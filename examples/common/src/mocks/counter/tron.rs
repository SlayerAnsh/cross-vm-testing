//! Tron counter bindings: the same contract compiled by tronc (tronbox). The mock TVM runs this
//! TVM-native creation bytecode; the ABI matches the EVM build, so consumers typically take only
//! `Counter::BYTECODE` from here and reuse the EVM call types.
#![allow(missing_docs)] // sol!-generated items are undocumented by construction.

alloy::sol!(
    #[sol(abi)]
    Counter,
    "../../contracts/tron/build/Counter.json"
);
