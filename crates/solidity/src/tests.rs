//! Unit tests for the EVM provider.

use std::rc::Rc;

use crate::chains::{ETHEREUM, LOCAL};
use alloy_primitives::{Address, Bytes, B256, U256};
use cross_vm_core::{BlockTime, ChainProvider, ChainSpec, WalletFactory};
use cross_vm_macros::define_wallet_roster;

define_wallet_roster! {
    pub const TEST_WALLETS: TestWallets = {
        alice: auto @ 0,
        bob: auto @ 1,
    };
}

fn empty_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(&[]).unwrap())
}

fn test_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(TestWallets::SPECS).expect("resolve roster"))
}

#[test]
fn predefined_chain_metadata() {
    assert_eq!(ETHEREUM.chain_id(), "1");
    assert_eq!(ETHEREUM.numeric_id(), 1);
    assert_eq!(ETHEREUM.native_symbol(), "ETH");
}

#[tokio::test]
async fn new_account_is_funded() {
    let mut chain = ETHEREUM.mock(empty_wallets());
    let alice = chain.new_account("alice").await;
    assert_eq!(
        chain.balance(&alice).await.unwrap(),
        U256::from(crate::DEFAULT_FUNDING_WEI)
    );
}

#[tokio::test]
async fn set_and_read_balance() {
    let mut chain = LOCAL.mock(empty_wallets());
    let bob = chain.new_account("bob").await;
    chain
        .set_balance(&bob, "ETH", U256::from(42u64))
        .await
        .unwrap();
    assert_eq!(chain.balance(&bob).await.unwrap(), U256::from(42u64));
}

#[tokio::test]
async fn set_balance_validates_denom() {
    let mut chain = LOCAL.mock(empty_wallets());
    let bob = chain.new_account("bob").await;

    // Unknown denom is rejected.
    assert!(chain
        .set_balance(&bob, "BTC", U256::from(1u64))
        .await
        .is_err());

    // The native symbol is accepted case-insensitively.
    chain
        .set_balance(&bob, "eth", U256::from(7u64))
        .await
        .unwrap();
    assert_eq!(chain.balance(&bob).await.unwrap(), U256::from(7u64));
}

#[tokio::test]
async fn blocks_advance() {
    let mut chain = LOCAL.mock(empty_wallets());
    let h0 = chain.block_height().await;
    chain.advance_blocks(5, BlockTime::Increment(1)).await;
    assert_eq!(chain.block_height().await, h0 + 5);
}

#[tokio::test]
async fn reads_storage_slot_written_by_constructor() {
    // Initcode whose constructor writes 42 into storage slot 0, then returns an empty runtime:
    //   PUSH1 0x2a, PUSH1 0x00, SSTORE, PUSH1 0x00, PUSH1 0x00, RETURN.
    let initcode = Bytes::from(vec![
        0x60, 0x2a, 0x60, 0x00, 0x55, 0x60, 0x00, 0x60, 0x00, 0xf3,
    ]);
    let mut chain = LOCAL.mock(empty_wallets());
    let deployer = chain.new_account("deployer").await;
    let deploy = chain
        .deploy_create(initcode, [], &deployer)
        .await
        .expect("storage-writing deploy succeeds");
    // The constructor wrote 42 at slot 0; an untouched slot reads as zero.
    assert_eq!(
        chain
            .get_storage_at(&deploy.address, U256::ZERO)
            .await
            .unwrap(),
        U256::from(42u64)
    );
    assert_eq!(
        chain
            .get_storage_at(&deploy.address, U256::from(1u64))
            .await
            .unwrap(),
        U256::ZERO
    );
}

#[tokio::test]
async fn mutating_ops_carry_a_transaction_hash() {
    // Initcode returning an empty runtime: enough to exercise deploy -> call on the mock.
    let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);
    let chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));

    let deploy = chain
        .deploy_create(initcode, [], TEST_WALLETS.alice)
        .await
        .expect("deploy");
    assert_ne!(deploy.address, Address::ZERO);
    assert_ne!(deploy.tx_hash, B256::ZERO);

    let exec = chain
        .call(&deploy.address, [], TEST_WALLETS.alice)
        .await
        .expect("call");
    assert_ne!(exec.tx_hash, B256::ZERO);
    // Deploys and calls draw from one hash sequence, so the two never collide.
    assert_ne!(exec.tx_hash, deploy.tx_hash);

    let exec_value = chain
        .call_value(&deploy.address, [], TEST_WALLETS.alice, U256::from(1u64))
        .await
        .expect("payable call");
    assert_ne!(exec_value.tx_hash, B256::ZERO);
    assert_ne!(exec_value.tx_hash, exec.tx_hash);
}

#[tokio::test]
async fn mutating_ops_report_the_gas_they_burned() {
    // Initcode whose constructor writes 42 into slot 0 before returning an empty runtime: a
    // deploy that pays for an SSTORE plus the create intrinsic, against a call into the empty
    // runtime that pays for little beyond the call intrinsic.
    let initcode = Bytes::from(vec![
        0x60, 0x2a, 0x60, 0x00, 0x55, 0x60, 0x00, 0x60, 0x00, 0xf3,
    ]);
    let chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));

    let deploy = chain
        .deploy_create(initcode, [], TEST_WALLETS.alice)
        .await
        .expect("deploy");
    assert!(deploy.gas.used > 0, "deploy reported no gas");

    let exec = chain
        .call(&deploy.address, [], TEST_WALLETS.alice)
        .await
        .expect("call");
    assert!(exec.gas.used > 0, "call reported no gas");
    assert!(
        deploy.gas.used > exec.gas.used,
        "deploy ({}) must cost more gas than a trivial call ({})",
        deploy.gas.used,
        exec.gas.used
    );

    // The mock has no gas price to multiply by, so it reports no fee rather than a fake zero.
    assert_eq!(deploy.gas.fee, None);
    assert_eq!(exec.gas.fee, None);
}

#[tokio::test]
async fn get_storage_at_plumbs_through_chain() {
    // Exercise the `EvmChain` enum dispatch: an unset slot reads as zero.
    let mut chain = crate::EvmChain::from(LOCAL.mock(empty_wallets()));
    let alice = chain.new_account("alice").await;
    assert_eq!(
        chain.get_storage_at(&alice, U256::ZERO).await.unwrap(),
        U256::ZERO
    );
}

#[tokio::test]
async fn transfer_funds_moves_the_balance() {
    let mut chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));
    let alice = chain.wallet_address(TEST_WALLETS.alice).await.unwrap();
    let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();
    chain
        .set_balance(&alice, "ETH", U256::from(1_000u64))
        .await
        .unwrap();

    let hash = chain
        .transfer_funds(&bob, "ETH", U256::from(400u64), TEST_WALLETS.alice)
        .await
        .expect("transfer");

    assert_eq!(chain.balance(&bob).await.unwrap(), U256::from(400u64));
    assert_eq!(chain.balance(&alice).await.unwrap(), U256::from(600u64));
    // The mock's synthetic hash is rendered like the live one: 0x + 32 bytes of lowercase hex.
    assert!(hash.starts_with("0x"), "hash `{hash}` is not 0x-prefixed");
    assert_eq!(hash.len(), 66, "hash `{hash}` is not 32 bytes of hex");
    assert!(hash[2..]
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}

#[tokio::test]
async fn transfer_funds_rejects_unknown_denom() {
    let mut chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));
    let alice = chain.wallet_address(TEST_WALLETS.alice).await.unwrap();
    let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();
    chain
        .set_balance(&alice, "ETH", U256::from(1_000u64))
        .await
        .unwrap();

    let err = chain
        .transfer_funds(&bob, "BTC", U256::from(1u64), TEST_WALLETS.alice)
        .await
        .expect_err("unknown denom is rejected");
    assert!(
        err.to_string().contains("unknown denom 'BTC'"),
        "unexpected error: {err}"
    );
    // The rejected transfer moved nothing.
    assert_eq!(chain.balance(&bob).await.unwrap(), U256::ZERO);
}

#[tokio::test]
async fn transfer_funds_rejects_insufficient_balance() {
    // The mock mints on a payable `call_value`; a transfer must not, so a short sender errors.
    let chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));
    let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();

    let err = chain
        .transfer_funds(&bob, "ETH", U256::from(1u64), TEST_WALLETS.alice)
        .await
        .expect_err("an unfunded sender cannot transfer");
    assert!(
        err.to_string().contains("insufficient funds"),
        "unexpected error: {err}"
    );
    assert_eq!(chain.balance(&bob).await.unwrap(), U256::ZERO);
}

/// The EIP-55 spec's own published test vectors: two all-caps, two all-lower, four normal.
/// Each is already in its checksummed form, so re-checksumming a lowercased copy must reproduce it.
const EIP55_VECTORS: [&str; 8] = [
    // All caps
    "0x52908400098527886E0F7030069857D2E4169EE7",
    "0x8617E340B3D01FA5F11F306F4090FD50E238070D",
    // All lower
    "0xde709f2102306220921060314715629080e2fb77",
    "0x27b1fdb04752bbc536007a920d24acb045561c26",
    // Normal
    "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
    "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359",
    "0xdbF03B407c01E7cD3CBea99509d93f8DDDC8C6FB",
    "0xD1220A0cf47c7B9Be7A2E6BA89F429762e7b9aDb",
];

#[test]
fn address_rendering_matches_eip55_reference_vectors() {
    for vector in EIP55_VECTORS {
        // Parse the *lowercased* form, so the rendered case can only come from a checksum
        // computation and not from echoing back the input's case.
        let addr: Address = vector
            .to_lowercase()
            .parse()
            .expect("EIP-55 vector is valid hex");
        assert_eq!(
            addr.to_string(),
            vector,
            "rendering `{vector}` lost its EIP-55 checksum"
        );
    }
}

#[tokio::test]
async fn chain_ops_render_eip55_checksummed_addresses() {
    // Addresses out of the mock are deterministic: an account is keccak(label)[12..] and a
    // deploy is CREATE(deployer, nonce 0). The literals below were checksummed with an
    // implementation independent of alloy, so matching them pins the rendered case, not just
    // the bytes. Both are mixed-case, so any lowercasing regression fails this test.
    const ALICE: &str = "0x5dad7600C5D89fE3824fFa99ec1c3eB8BF3b0501";
    const DEPLOYER: &str = "0x1b5CEb79b60DC455aD691D856E6E4025Cf542CAA";
    const CONTRACT: &str = "0x25A25a4Cd120784f7428d26001d9E34FFb90FAFe";
    for pinned in [ALICE, DEPLOYER, CONTRACT] {
        assert_ne!(
            pinned,
            pinned.to_lowercase(),
            "`{pinned}` must be mixed-case or it cannot detect a lowercasing regression"
        );
    }

    let mut chain = LOCAL.mock(empty_wallets());
    let alice = chain.new_account("alice").await;
    assert_eq!(alice.to_string(), ALICE);

    let deployer = chain.new_account("deployer").await;
    assert_eq!(deployer.to_string(), DEPLOYER);

    // Initcode returning an empty runtime.
    let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);
    let deploy = chain
        .deploy_create(initcode, [], &deployer)
        .await
        .expect("deploy");
    assert_eq!(deploy.address.to_string(), CONTRACT);
}

#[tokio::test]
async fn rpc_write_paths_unimplemented() {
    let mut chain = ETHEREUM.rpc(empty_wallets());
    assert!(chain
        .set_balance(&alloy_primitives::Address::ZERO, "ETH", U256::from(1u64))
        .await
        .is_err());
}
