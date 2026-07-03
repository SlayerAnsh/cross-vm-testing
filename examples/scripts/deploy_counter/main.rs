//! Deploy the `Counter` contract and call `increment` on Base Sepolia (EVM) and Osmosis testnet
//! (CosmWasm), driving both through the chain-AGNOSTIC layer.
//!
//! `main` wraps each target chain in an `AnyChain` and runs one identical `setup -> increment ->
//! count` flow ([`run`]); the `#[cross_vm_contract]` macro dispatches each call to the matching VM
//! hook at runtime. The logical contract and its per-VM hooks live in [`contract`]; the contract
//! bindings come from `cross_vm_common::mocks`.
//!
//! Prerequisites (artifacts embedded at compile time; wallet funded at run time):
//!   make compile-solidity   # contracts/solidity/out/Counter.sol/Counter.json
//!   make compile-cosmwasm   # contracts/cosmwasm/counter/artifacts/counter.wasm
//! A workspace `.env` must define `MNEMONIC_TEST`, funded on BOTH the EVM address (BIP-44 coin 60)
//! and the Osmosis address (coin 118) derived from it. If a wallet is unfunded, `setup` fails and
//! the per-chain error is reported (the run continues to the next chain and exits non-zero).
//!
//! Run:
//!   cargo run --manifest-path examples/scripts/Cargo.toml --bin deploy_counter

mod contract;

use std::rc::Rc;

use cross_vm_cosmwasm::chains::OSMOSIS_TESTNET;
use cross_vm_framework::prelude::*;
use cross_vm_solidity::chains::BASE_SEPOLIA;

use contract::{Counter, CounterSpec};

define_wallet_roster! {
    pub const ONCHAIN_WALLETS: OnchainWallets = {
        test: env_mnemonic("MNEMONIC_TEST") @ 0,
    };
}

/// `.env` lives at the workspace root; the crate manifest is `examples/scripts`.
const ENV_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env");

/// Run the identical chain-agnostic flow against one chain.
async fn run(chain: AnyChain) -> Result<u64, CrossVmError> {
    let wallet = ONCHAIN_WALLETS.test.as_str();
    let counter = Counter::new(chain);
    counter.setup(wallet).await?;
    counter.increment(wallet).await?;
    counter.count().await
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    dotenvy::from_path(ENV_PATH).unwrap_or_else(|e| panic!("load {ENV_PATH}: {e}"));
    let wallets = Rc::new(
        WalletFactory::from_roster(OnchainWallets::SPECS)
            .unwrap_or_else(|e| panic!("resolve roster: {e}")),
    );

    // Each target wrapped as an `AnyChain`; the loop below treats them identically.
    let evm: EvmChain = BASE_SEPOLIA.rpc(wallets.clone()).into();
    let cosmos: CwChain = OSMOSIS_TESTNET.rpc(wallets).into();
    let targets: Vec<(&str, AnyChain)> = vec![
        ("base sepolia", evm.into()),
        ("osmosis testnet", cosmos.into()),
    ];

    // Sequential: each chain runs to completion before the next. One failing does not stop the
    // rest; the run exits non-zero if any failed.
    let mut failed = false;
    for (name, chain) in targets {
        match run(chain).await {
            Ok(count) => println!("{name}: ok, count = {count}"),
            Err(e) => {
                eprintln!("{name}: FAILED: {e}");
                failed = true;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}
