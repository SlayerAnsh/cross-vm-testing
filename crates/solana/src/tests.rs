//! Unit tests for the Solana provider.

use std::rc::Rc;

use crate::chains::{SOLANA_DEVNET, SOLANA_LOCALNET};
use cross_vm_core::{BlockTime, ChainProvider, ChainSpec, WalletFactory};

fn empty_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(&[]).unwrap())
}

#[test]
fn predefined_chain_metadata() {
    assert_eq!(SOLANA_DEVNET.chain_id(), "devnet");
    assert_eq!(SOLANA_DEVNET.native_symbol(), "SOL");
}

#[tokio::test]
async fn new_account_is_funded() {
    let mut chain = SOLANA_LOCALNET.mock(empty_wallets());
    let alice = chain.new_account("alice").await;
    assert_eq!(
        chain.balance(&alice).await.unwrap(),
        crate::DEFAULT_FUNDING_LAMPORTS
    );
}

#[tokio::test]
async fn set_and_read_balance() {
    let mut chain = SOLANA_LOCALNET.mock(empty_wallets());
    let bob = chain.new_account("bob").await;
    chain.set_balance(&bob, "SOL", 12_345).await.unwrap();
    assert_eq!(chain.balance(&bob).await.unwrap(), 12_345);
}

#[tokio::test]
async fn set_balance_validates_denom() {
    let mut chain = SOLANA_LOCALNET.mock(empty_wallets());
    let bob = chain.new_account("bob").await;

    assert!(chain.set_balance(&bob, "BTC", 1).await.is_err());

    chain.set_balance(&bob, "sol", 7_777).await.unwrap();
    assert_eq!(chain.balance(&bob).await.unwrap(), 7_777);
}

#[tokio::test]
async fn get_account_data_matches_account_bytes() {
    let mut chain = SOLANA_LOCALNET.mock(empty_wallets());
    let carol = chain.new_account("carol").await;

    let account = chain
        .get_account(&carol)
        .await
        .expect("funded account exists");
    let data = chain
        .get_account_data(&carol)
        .await
        .expect("funded account exists");
    assert_eq!(data, account.data);

    // A never-seen pubkey has no account, hence no data.
    let missing = solana_address::Address::new_unique();
    assert!(chain.get_account_data(&missing).await.is_none());
}

#[tokio::test]
async fn get_account_data_slice_matches_prefix() {
    let mut chain = SOLANA_LOCALNET.mock(empty_wallets());
    let carol = chain.new_account("carol").await;

    let data = chain
        .get_account_data(&carol)
        .await
        .expect("funded account exists");
    let n = data.len().min(8);

    let slice = chain
        .get_account_data_slice(&carol, 0, n)
        .await
        .expect("slice within data");
    assert_eq!(slice, data[..n]);

    // An offset past the end of the data yields no window (all-or-nothing).
    assert!(chain
        .get_account_data_slice(&carol, data.len() + 1, 1)
        .await
        .is_none());

    // A never-seen pubkey has no account, hence no slice.
    let missing = solana_address::Address::new_unique();
    assert!(chain.get_account_data_slice(&missing, 0, 1).await.is_none());
}

#[test]
fn find_program_account_is_deterministic() {
    use crate::chain::SvmChain;

    let program_id = solana_address::Address::new_unique();
    let seeds: &[&[u8]] = &[b"counter", b"alice"];

    let a = SvmChain::find_program_account(&program_id, seeds);
    let b = SvmChain::find_program_account(&program_id, seeds);
    assert_eq!(a, b, "same seeds must derive the same PDA");

    let (direct, _bump) = solana_address::Address::find_program_address(seeds, &program_id);
    assert_eq!(a, direct, "helper must match Address::find_program_address");

    // Different seeds derive a different cell.
    let other = SvmChain::find_program_account(&program_id, &[b"counter", b"bob"]);
    assert_ne!(a, other);
}

#[tokio::test]
async fn get_program_state_none_when_pda_unfunded() {
    use crate::chain::SvmChain;

    let chain: SvmChain = SOLANA_LOCALNET.mock(empty_wallets()).into();
    let program_id = solana_address::Address::new_unique();

    // The derived PDA has no account yet, so a point-read reports Ok(None).
    let state = chain
        .get_program_state(&program_id, &[b"state"], 0, 8)
        .await
        .expect("query succeeds");
    assert!(state.is_none());
}

#[tokio::test]
async fn add_program_rejects_invalid_bytecode() {
    let chain: crate::SvmChain = SOLANA_LOCALNET.mock(empty_wallets()).into();

    let err = chain
        .add_program(b"not an sbf elf".to_vec())
        .await
        .expect_err("bytecode is not a loadable program");
    assert!(matches!(err, crate::SvmError::Deploy(_)), "got: {err}");
}

#[test]
fn deploy_hash_is_a_signature_and_pins_the_load() {
    use std::str::FromStr;

    use crate::provider::SvmDeploy;

    let blockhash = [7u8; 32];
    let program_id = solana_address::Address::new_unique();
    let bytecode = b"program".as_slice();

    let deploy = SvmDeploy::minted(&blockhash, program_id, bytecode);
    assert_eq!(deploy.program_id, program_id);
    // The synthetic hash must still be a real base58 signature, so callers can parse it back.
    solana_signature::Signature::from_str(&deploy.tx_hash).expect("base58 signature");

    // Same load, same hash: a mock run is reproducible.
    assert_eq!(
        deploy.tx_hash,
        SvmDeploy::minted(&blockhash, program_id, bytecode).tx_hash
    );

    // Each of the three inputs the hash commits to changes it.
    let other_id = solana_address::Address::new_unique();
    assert_ne!(
        deploy.tx_hash,
        SvmDeploy::minted(&blockhash, other_id, bytecode).tx_hash
    );
    assert_ne!(
        deploy.tx_hash,
        SvmDeploy::minted(&[8u8; 32], program_id, bytecode).tx_hash
    );
    assert_ne!(
        deploy.tx_hash,
        SvmDeploy::minted(&blockhash, program_id, b"other program").tx_hash
    );
}

#[tokio::test]
async fn estimate_transaction_reports_cost_without_committing() {
    use solana_keypair::Keypair;
    use solana_signer::Signer;
    use solana_system_interface::instruction::transfer;

    let mut chain = SOLANA_LOCALNET.mock(empty_wallets());
    let alice = Keypair::new();
    let bob = solana_address::Address::new_unique();
    chain
        .set_balance(&alice.pubkey(), "SOL", 10_000_000_000)
        .await
        .unwrap();

    let amount = 1_000_000_000;
    let ix = transfer(&alice.pubkey(), &bob, amount);

    let alice_before = chain.balance(&alice.pubkey()).await.unwrap();
    let bob_before = chain.balance(&bob).await.unwrap();
    let account_before = chain.get_account(&alice.pubkey()).await;

    let estimate = chain
        .estimate_transaction([ix.clone()], &alice)
        .await
        .expect("estimate");
    assert!(estimate.compute_units_consumed > 0, "no compute units");
    assert!(estimate.fee > 0, "no fee");

    // Nothing was committed: no lamports moved (not even the fee) and the payer's account is
    // byte-for-byte what it was.
    assert_eq!(chain.balance(&alice.pubkey()).await.unwrap(), alice_before);
    assert_eq!(chain.balance(&bob).await.unwrap(), bob_before);
    assert_eq!(chain.get_account(&alice.pubkey()).await, account_before);

    // The very same transaction still sends afterwards (the estimate did not consume its
    // blockhash, nor record its signature as already processed) and it costs what was forecast.
    // `Exact(MAX_COMPUTE_UNIT_LIMIT)` reproduces it byte for byte: the estimate simulates the
    // instructions under a `SetComputeUnitLimit` at the ceiling, which is why the signatures match.
    let receipt = chain
        .send_transaction(
            [ix],
            &alice,
            crate::SvmComputeBudget::Exact(crate::MAX_COMPUTE_UNIT_LIMIT),
        )
        .await
        .expect("send");
    assert_eq!(receipt.signature, estimate.signature, "not the same tx");
    assert_eq!(
        receipt.compute_units_consumed,
        estimate.compute_units_consumed
    );
    assert_eq!(receipt.fee, estimate.fee);

    assert_eq!(chain.balance(&bob).await.unwrap(), bob_before + amount);
    assert_eq!(
        chain.balance(&alice.pubkey()).await.unwrap(),
        alice_before - amount - receipt.fee
    );
}

#[tokio::test]
async fn estimate_transaction_surfaces_a_failing_simulation() {
    use solana_keypair::Keypair;
    use solana_signer::Signer;
    use solana_system_interface::instruction::transfer;

    let mut chain = SOLANA_LOCALNET.mock(empty_wallets());
    let alice = Keypair::new();
    let bob = solana_address::Address::new_unique();
    chain
        .set_balance(&alice.pubkey(), "SOL", 1_000_000)
        .await
        .unwrap(); // 0.001 SOL

    // Transfers more than it holds: the estimate must fail, not report a plausible cheap success.
    let ix = transfer(&alice.pubkey(), &bob, 10_000_000_000);
    let err = chain
        .estimate_transaction([ix], &alice)
        .await
        .expect_err("insufficient funds");
    assert!(matches!(err, crate::SvmError::Execute(_)), "got: {err}");
    assert_eq!(chain.balance(&bob).await.unwrap(), 0);
}

#[test]
fn an_estimated_budget_scales_by_gas_adjustment_and_clamps_to_the_ceiling() {
    use crate::provider::adjusted;
    use crate::MAX_COMPUTE_UNIT_LIMIT;

    // The chain's headroom, rounded up: a fractional compute unit cannot be requested.
    assert_eq!(adjusted(300, 1.3), 390);
    assert_eq!(adjusted(301, 1.3), 392); // 391.3 -> 392
    assert_eq!(
        adjusted(300, 1.0),
        300,
        "no headroom is still the full cost"
    );

    // The runtime silently clamps a `SetComputeUnitLimit` above its per-transaction ceiling, so
    // the number requested here is the number that will be enforced.
    assert_eq!(
        adjusted(u64::from(MAX_COMPUTE_UNIT_LIMIT), 1.3),
        MAX_COMPUTE_UNIT_LIMIT
    );
    assert_eq!(adjusted(u64::MAX, 1.3), MAX_COMPUTE_UNIT_LIMIT);
}

#[tokio::test]
async fn an_estimated_budget_leaves_headroom_over_the_true_cost() {
    use solana_keypair::Keypair;
    use solana_signer::Signer;
    use solana_system_interface::instruction::transfer;

    use crate::provider::adjusted;
    use crate::SvmComputeBudget;

    let mut chain = SOLANA_LOCALNET.mock(empty_wallets());
    let alice = Keypair::new();
    let bob = solana_address::Address::new_unique();
    chain
        .set_balance(&alice.pubkey(), "SOL", 10_000_000_000)
        .await
        .unwrap();

    let ix = transfer(&alice.pubkey(), &bob, 1_000_000_000);
    let cost = chain
        .estimate_transaction([ix.clone()], &alice)
        .await
        .expect("estimate")
        .compute_units_consumed;

    // What `Estimated` resolves to, spelled out: strictly above the cost at the preset's 1.3, and
    // the transaction that runs under it executes.
    let budget = adjusted(cost, SOLANA_LOCALNET.gas_adjustment);
    assert!(u64::from(budget) > cost, "{budget} leaves no headroom");

    let receipt = chain
        .send_transaction([ix], &alice, SvmComputeBudget::Estimated)
        .await
        .expect("send under an estimated budget");
    assert_eq!(receipt.compute_units_consumed, cost);
}

#[tokio::test]
async fn send_transaction_rejects_a_caller_supplied_compute_budget() {
    use solana_compute_budget_interface::ComputeBudgetInstruction;
    use solana_keypair::Keypair;
    use solana_signer::Signer;
    use solana_system_interface::instruction::transfer;

    use crate::SvmComputeBudget;

    let mut chain = SOLANA_LOCALNET.mock(empty_wallets());
    let alice = Keypair::new();
    let bob = solana_address::Address::new_unique();
    chain
        .set_balance(&alice.pubkey(), "SOL", 10_000_000_000)
        .await
        .unwrap();

    // The budget is the `budget` argument. A hand-rolled `SetComputeUnitLimit` on top of the one
    // this provider prepends is a duplicate instruction, which the runtime rejects outright: say
    // so, rather than letting it surface as an opaque `DuplicateInstruction`.
    let err = chain
        .send_transaction(
            [
                ComputeBudgetInstruction::set_compute_unit_limit(50_000),
                transfer(&alice.pubkey(), &bob, 1_000_000_000),
            ],
            &alice,
            SvmComputeBudget::Exact(50_000),
        )
        .await
        .expect_err("two compute unit limits");
    assert!(
        matches!(&err, crate::SvmError::Execute(msg) if msg.contains("already set a compute unit limit")),
        "got: {err}"
    );
    assert_eq!(chain.balance(&bob).await.unwrap(), 0);
}

#[tokio::test]
async fn blocks_advance() {
    let mut chain = SOLANA_LOCALNET.mock(empty_wallets());
    assert_eq!(chain.block_height().await, 0);
    chain.advance_blocks(4, BlockTime::Increment(1)).await;
    assert_eq!(chain.block_height().await, 4);
}

#[tokio::test]
async fn rpc_write_paths_unimplemented() {
    let mut chain = SOLANA_DEVNET.rpc(empty_wallets());
    let addr = solana_address::Address::new_unique();
    assert!(chain.set_balance(&addr, "SOL", 1).await.is_err());

    let err = chain
        .add_program(b"program".to_vec())
        .await
        .expect_err("rpc program load is a deliberate gap");
    assert!(
        matches!(&err, crate::SvmError::Unimplemented(what) if what == "rpc add_program"),
        "got: {err}"
    );
    let err = chain
        .add_program_at(addr, b"program".to_vec())
        .await
        .expect_err("rpc program load is a deliberate gap");
    assert!(
        matches!(&err, crate::SvmError::Unimplemented(what) if what == "rpc add_program_at"),
        "got: {err}"
    );
}
