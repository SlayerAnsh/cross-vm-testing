//! EVM ping-pong ABI bindings and creation bytecode from the forge artifact. Public so callers can
//! reuse the generated `SolEvent` types (packet relaying parses emitted events).
#![allow(missing_docs)] // sol!-generated items are undocumented by construction.

alloy::sol!(
    #[sol(abi)]
    PingPong,
    "../../contracts/solidity/out/PingPong.sol/PingPong.json"
);
