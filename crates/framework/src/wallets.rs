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
    };
}

define_wallet_roster! {
    pub const EMPTY_WALLETS: EmptyWallets = {};
}
