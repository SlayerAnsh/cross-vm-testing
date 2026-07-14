//! Cross-VM contract example: one `Counter` wrapper (in `support`), one test that runs
//! identically on CosmWasm, EVM, and Solana, asserting per-VM hook observations.

use std::cell::RefCell;
use std::rc::Rc;

use cross_vm_framework::prelude::*;

use crate::support::{fund_alice, test_wallets, Counter, CounterSpec};

fn chain_for(kind: ChainKind, wallets: Rc<WalletFactory>) -> AnyChain {
    match kind {
        ChainKind::CosmWasm => OSMOSIS.mock(wallets.clone()).into(),
        ChainKind::Evm => ETHEREUM.mock(wallets.clone()).into(),
        ChainKind::Svm => SOLANA_DEVNET.mock(wallets).into(),
        ChainKind::Tron => TRON_LOCAL.mock(wallets).into(),
    }
}

#[rstest::rstest]
#[tokio::test]
async fn counter_increments_across_vms(
    #[values(ChainKind::CosmWasm, ChainKind::Evm, ChainKind::Svm, ChainKind::Tron)] kind: ChainKind,
) {
    let wallets = test_wallets();
    let mut chain = chain_for(kind, wallets);
    fund_alice(&mut chain).await;
    let counter = Counter::new(chain.clone());

    let seen: Rc<RefCell<Vec<ChainKind>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    counter.on_after(move |ctx| {
        assert_eq!(ctx.label(), "increment");
        // Every VM reports a hash, on either backend: the real broadcast hash on live RPC, a
        // synthetic deterministic one on the in-process mocks this test runs against.
        assert!(!ctx.transaction_hash().is_empty());
        match ctx.kind() {
            ChainKind::CosmWasm => {
                assert!(!ctx.cosmwasm_events().expect("cw events").is_empty());
                assert!(ctx.evm_logs().is_err());
                assert!(ctx.solana_logs().is_err());
                // `cw-multi-test` has no gas meter, so the mock has nothing to report: absence,
                // never a fabricated zero, which would read as "metered, and it was free".
                assert_eq!(ctx.cost(), None);
            }
            ChainKind::Evm => {
                ctx.evm_logs().expect("evm logs");
                assert!(ctx.cosmwasm_events().is_err());
                let cost = ctx.cost().expect("evm mock meters gas");
                assert_eq!(cost.unit, CostUnit::Gas);
                assert!(cost.units > 0);
                assert_eq!(cost.bandwidth, None);
                // No gas price on the mock, so no fee can be derived without inventing one.
                assert_eq!(cost.fee, None);
            }
            ChainKind::Svm => {
                assert!(!ctx.solana_logs().expect("svm logs").is_empty());
                assert!(ctx.evm_logs().is_err());
                let cost = ctx.cost().expect("svm mock meters compute units");
                assert_eq!(cost.unit, CostUnit::ComputeUnits);
                assert!(cost.units > 0);
                assert_eq!(cost.bandwidth, None);
                // litesvm prices the signature, so unlike the EVM mock it does report a fee.
                assert!(cost.fee.expect("litesvm charges a lamport fee") > 0);
            }
            ChainKind::Tron => {
                // Tron logs are EVM-shaped but carried on the Tron response variant.
                ctx.tron_logs().expect("tron logs");
                assert!(ctx.evm_logs().is_err());
                assert!(ctx.cosmwasm_events().is_err());
                let cost = ctx.cost().expect("tron mock meters its revm gas");
                // The Tron mock *is* revm: it meters EVM gas, not energy. Relabelling that as
                // `Energy` would report a quantity the mock never measures.
                assert_eq!(cost.unit, CostUnit::Gas);
                assert!(cost.units > 0);
                // Bandwidth is the one resource the mock's shim genuinely charges.
                assert!(cost.bandwidth.expect("tron bills bandwidth") > 0);
                assert_eq!(cost.fee, None);
            }
        }
        sink.borrow_mut().push(ctx.kind());
        Ok(())
    });

    counter.setup("alice").await.expect("setup");
    assert_eq!(counter.count().await.expect("count after setup"), 0);

    counter.increment("alice").await.expect("increment 1");
    counter.increment("alice").await.expect("increment 2");

    assert_eq!(counter.count().await.expect("count after increments"), 2);

    assert_eq!(*seen.borrow(), vec![kind, kind]);
}
