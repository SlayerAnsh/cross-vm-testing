//! Tron ping-pong bindings: the same contract compiled by tronc (tronbox). Consumers take
//! `PingPong::BYTECODE` from here and reuse the EVM call/event types.
#![allow(missing_docs)] // sol!-generated items are undocumented by construction.

alloy::sol!(
    #[sol(abi)]
    PingPong,
    "../../contracts/tron/build/PingPong.json"
);
