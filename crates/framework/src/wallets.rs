//! Shared wallet roster for framework examples and integration tests.

use cross_vm_macros::define_wallet_roster;

define_wallet_roster! {
    pub const TEST_WALLETS: TestWallets = {
        alice: auto @ 0,
        bob: auto @ 1,
        #[label("test-admin")]
        test_admin: auto @ 0,
        test: auto @ 0,
        ephemeral: auto @ 0,
        // A real funded key for live on-chain runs (e.g. the `rpc-endurance` Base Sepolia chain).
        // `EnvMnemonic` is resolved lazily, so `ON_CHAIN_WALLET` need only be set when a test
        // actually signs with this wallet; mock-only runs ignore it.
        on_chain: env_mnemonic("ON_CHAIN_WALLET") @ 0,
    };
}

define_wallet_roster! {
    pub const EMPTY_WALLETS: EmptyWallets = {};
}
