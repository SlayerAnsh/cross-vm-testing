//! Process-global broadcast locker, keyed by a live account's nonce/sequence domain.
//!
//! Real on-chain broadcasts that share one account on one chain must not run concurrently, or they
//! collide on the EVM nonce / Cosmos account sequence. The per-[`WalletFactory`](crate::WalletFactory)
//! lock cannot enforce this across tests: each test builds its own factory, so two tests signing
//! with the same key would hold different mutexes. This locker lives in a process-global keyed by
//! `(chain, address)`, so the same account on the same chain serializes everywhere (including across
//! the separate per-test Tokio runtimes), while different chains and the in-process mock backends
//! stay fully parallel.
//!
//! Only the RPC broadcast path acquires it (mock backends have no shared nonce). Hold the returned
//! guard across the whole send -> confirm so the nonce cannot be reused mid-flight.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::chain_kind::ChainKind;

/// Per-key async mutexes. The outer `std` mutex guards the registry and is held only long enough to
/// clone an `Arc`, never across an `.await`.
static LOCKER: LazyLock<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Build the broadcast key for a live account: its `(chain kind, chain id, address)` triple, which
/// is the domain a nonce / account sequence is unique within.
pub fn lock_key(kind: ChainKind, chain_id: &str, address: &str) -> String {
    format!("{kind:?}:{chain_id}:{address}")
}

/// Acquire the broadcast lock for `key`, serializing every broadcast that shares it process-wide.
///
/// The guard is owned (`'static`) so it can be held across the full build -> sign -> broadcast ->
/// confirm sequence and released on drop. Callers build `key` with [`lock_key`].
pub async fn lock_broadcast(key: &str) -> OwnedMutexGuard<()> {
    let m = {
        let mut map = LOCKER.lock().expect("wallet locker registry poisoned");
        map.entry(key.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    };
    m.lock_owned().await
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use super::*;

    /// Two broadcasts on the same key never overlap: the second starts only after the first drops
    /// its guard. Tracked by a max-concurrency counter that must never exceed 1.
    #[tokio::test]
    async fn same_key_serializes() {
        let key = lock_key(ChainKind::Evm, "test-chain", "0xsame");
        let inflight = Rc::new(RefCell::new(0u32));
        let max_seen = Rc::new(RefCell::new(0u32));

        let task = |inflight: Rc<RefCell<u32>>, max: Rc<RefCell<u32>>, key: String| async move {
            let _g = lock_broadcast(&key).await;
            {
                let mut n = inflight.borrow_mut();
                *n += 1;
                let mut m = max.borrow_mut();
                *m = (*m).max(*n);
            }
            // Yield so a non-serialized impl would let the other task observe inflight == 2.
            tokio::time::sleep(Duration::from_millis(20)).await;
            *inflight.borrow_mut() -= 1;
        };

        let a = task(inflight.clone(), max_seen.clone(), key.clone());
        let b = task(inflight.clone(), max_seen.clone(), key.clone());
        tokio::join!(a, b);

        assert_eq!(
            *max_seen.borrow(),
            1,
            "broadcasts on one key must serialize"
        );
    }

    /// Distinct keys do not serialize: both tasks are inflight at once (max-concurrency reaches 2).
    /// Uses the *same address on two different chain ids* to also prove per-chain independence (a
    /// shared account's nonce domains are separate per chain). If every key shared one mutex, the
    /// second task would block on the first and the max would stay 1.
    #[tokio::test]
    async fn different_keys_run_concurrently() {
        let addr = "0xsame";
        let key_a = lock_key(ChainKind::Evm, "chain-a", addr);
        let key_b = lock_key(ChainKind::Evm, "chain-b", addr);
        let inflight = Rc::new(RefCell::new(0u32));
        let max_seen = Rc::new(RefCell::new(0u32));

        let task = |inflight: Rc<RefCell<u32>>, max: Rc<RefCell<u32>>, key: String| async move {
            let _g = lock_broadcast(&key).await;
            {
                let mut n = inflight.borrow_mut();
                *n += 1;
                let mut m = max.borrow_mut();
                *m = (*m).max(*n);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            *inflight.borrow_mut() -= 1;
        };

        tokio::join!(
            task(inflight.clone(), max_seen.clone(), key_a),
            task(inflight.clone(), max_seen.clone(), key_b),
        );

        assert_eq!(
            *max_seen.borrow(),
            2,
            "distinct keys (different chains) must not serialize"
        );
    }

    /// The headline guarantee: one key serializes broadcasts *across separate Tokio runtimes*, the
    /// way two `#[tokio::test]`s on different OS threads contend. A per-factory lock could not do
    /// this (each test builds its own factory); the process-global registry can. Two threads, each
    /// with its own current-thread runtime, lock the same key; the shared max-concurrency counter
    /// must never exceed 1.
    #[test]
    fn serializes_across_runtimes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::thread;

        let key = lock_key(ChainKind::CosmWasm, "rt-test", "addr-xrt");
        let inflight = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));

        let spawn = |key: String, inflight: Arc<AtomicUsize>, max: Arc<AtomicUsize>| {
            thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .unwrap();
                rt.block_on(async move {
                    let _g = lock_broadcast(&key).await;
                    let n = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                    max.fetch_max(n, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    inflight.fetch_sub(1, Ordering::SeqCst);
                });
            })
        };

        let t1 = spawn(key.clone(), inflight.clone(), max_seen.clone());
        let t2 = spawn(key.clone(), inflight.clone(), max_seen.clone());
        t1.join().unwrap();
        t2.join().unwrap();

        assert_eq!(
            max_seen.load(Ordering::SeqCst),
            1,
            "one account+chain must serialize across separate runtimes"
        );
    }
}
