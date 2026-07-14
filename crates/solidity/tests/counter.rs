//! Integration test: deploy_create -> call -> static_call a Solidity `Counter` through the EVM
//! provider, using the canonical contract from `contracts/solidity`. The `sol!` JSON
//! form parses `out/Counter.sol/Counter.json` at compile time, generating the call bindings
//! (`Counter::incrementCall`, ...) and the `Counter::BYTECODE` creation bytecode. Run
//! `make compile-solidity` to (re)produce the artifact.

use std::rc::Rc;

use alloy::sol_types::SolCall;
use cross_vm_core::{ChainProvider, WalletFactory};
use cross_vm_solidity::chains::LOCAL;
use cross_vm_solidity::{Address, Bytes, EvmGasLimit, EvmMockProvider};

alloy::sol!(
    #[sol(abi)]
    Counter,
    "../../contracts/solidity/out/Counter.sol/Counter.json"
);

fn empty_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(&[]).unwrap())
}

async fn read_count(chain: &EvmMockProvider, contract: &Address) -> u64 {
    let out = chain
        .static_call(contract, Bytes::from(Counter::countCall {}.abi_encode()))
        .await
        .expect("static_call count");
    Counter::countCall::abi_decode_returns(&out)
        .expect("decode count")
        .saturating_to::<u64>()
}

#[tokio::test]
async fn deploy_increment_reset_query() {
    let mut chain = LOCAL.mock(empty_wallets());
    let deployer = chain.new_account("deployer").await;

    let contract = chain
        .deploy_create(
            Counter::BYTECODE.clone(),
            Bytes::new(),
            &deployer,
            EvmGasLimit::Estimated,
        )
        .await
        .expect("deploy_create")
        .address;
    assert_eq!(read_count(&chain, &contract).await, 0);

    chain
        .call(
            &contract,
            Bytes::from(Counter::incrementCall {}.abi_encode()),
            &deployer,
            EvmGasLimit::Estimated,
        )
        .await
        .expect("increment");
    assert_eq!(read_count(&chain, &contract).await, 1);

    chain
        .call(
            &contract,
            Bytes::from(Counter::resetCall {}.abi_encode()),
            &deployer,
            EvmGasLimit::Exact(100_000),
        )
        .await
        .expect("reset");
    assert_eq!(read_count(&chain, &contract).await, 0);
}
