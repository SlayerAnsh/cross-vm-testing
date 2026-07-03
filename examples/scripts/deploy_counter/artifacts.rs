//! Compiled contract artifacts embedded at build time.
//!
//! Paths resolve against `CARGO_MANIFEST_DIR` (the `examples/scripts` crate root), so they are
//! independent of which module references them. Build them first with `make compile-solidity`
//! and `make compile-cosmwasm`.

/// Optimizer-built, chain-deployable counter wasm (run `make compile-cosmwasm` first).
pub const COUNTER_WASM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/cosmwasm/counter/artifacts/counter.wasm"
));

/// EVM ABI + creation bytecode from the forge artifact. Its own module so the generated `Counter`
/// type does not clash with the `#[cross_vm_contract(Counter)]` wrapper.
pub mod evm_counter {
    alloy::sol!(
        #[sol(abi)]
        Counter,
        "../../contracts/solidity/out/Counter.sol/Counter.json"
    );
}
