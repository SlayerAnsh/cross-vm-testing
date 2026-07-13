//! End-to-end check of the `env_any(..)` seam: that the roster DSL really expands into
//! [`WalletSource::EnvAny`] and that the resulting chain resolves through the real
//! [`WalletFactory`].
//!
//! `cross-vm-macros` only ever asserts on its own token output and `cross-vm-core` only ever
//! builds `EnvCandidate` slices by hand, so nothing else exercises both halves at once: that
//! `env_any(private_key("A"), mnemonic("B") @ 1)` in a `define_wallet_roster!` body becomes an
//! ordered `&'static [EnvCandidate]` the factory then walks in declaration order.
//!
//! Every assertion lives in a single `#[test]` fn deliberately. `std::env::set_var` is
//! process-global and cargo runs a test binary's tests across many threads; the repo has no
//! env-var test guard, so this file serializes its own writes and reads by having exactly one
//! test. The var names are unique to this file (and each `tests/*.rs` is its own process), so
//! nothing outside this fn writes them.

use cross_vm_core::{
    CrossVmError, EnvCandidate, WalletDef, WalletFactory, WalletSource, WalletSpec,
};
use cross_vm_macros::define_wallet_roster;

/// Two distinct BIP-39 phrases, so an assertion can say *which* candidate won rather than merely
/// that some mnemonic did.
const PHRASE_A: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const PHRASE_B: &str =
    "legal winner thank year wave sausage worth useful legal winner thank yellow";

/// A raw VM-native key; `resolve` returns it verbatim (no derivation), so any hex will do.
const PRIVATE_KEY: &str = "0xfeedface";

define_wallet_roster! {
    pub const ENV_ANY_WALLETS: EnvAnyWallets = {
        // A mixed chain: a raw key first, then a mnemonic pinned to its own account index. Both
        // vars are set below, so only declaration order can decide the winner.
        mixed: env_any(
            private_key("XVM_ENV_ANY_MIXED_PK"),
            mnemonic("XVM_ENV_ANY_MIXED_PHRASE") @ 1,
        ),
        // The leading private key is left unset, so resolution falls through to the mnemonic,
        // which must derive at its *own* `@ 4` and not at this row's `@ 9`.
        own_index: env_any(
            private_key("XVM_ENV_ANY_OWN_PK"),
            mnemonic("XVM_ENV_ANY_OWN_PHRASE") @ 4,
        ) @ 9,
        // Neither candidate carries an index, so both inherit the row-level `@ 3`. The primary
        // var is set but blank, which counts as missing.
        inherited_index: env_any(
            mnemonic("XVM_ENV_ANY_INHERIT_PRIMARY"),
            mnemonic("XVM_ENV_ANY_INHERIT_BACKUP"),
        ) @ 3,
        // Every var stays missing (one unset, one empty): resolving this row must name both.
        orphan: env_any(
            private_key("XVM_ENV_ANY_ORPHAN_PK"),
            mnemonic("XVM_ENV_ANY_ORPHAN_PHRASE") @ 2,
        ),
    };
}

/// The generated roster row for `label`.
fn spec(label: &str) -> &'static WalletSpec {
    EnvAnyWallets::SPECS
        .iter()
        .find(|spec| spec.label == label)
        .unwrap_or_else(|| panic!("the generated roster has no `{label}` row"))
}

/// The `EnvAny` chain `label` expanded to, panicking if the DSL produced any other source.
fn candidates(label: &str) -> &'static [EnvCandidate] {
    match spec(label).source {
        WalletSource::EnvAny(chain) => chain,
        other => panic!("`{label}` expanded to {other:?}, not WalletSource::EnvAny"),
    }
}

/// What `mixed` must expand to: the two candidates, in declaration order, the mnemonic keeping
/// its own `@ 1`.
const MIXED_CHAIN: &[EnvCandidate] = &[
    EnvCandidate::PrivateKey {
        var: "XVM_ENV_ANY_MIXED_PK",
    },
    EnvCandidate::Mnemonic {
        var: "XVM_ENV_ANY_MIXED_PHRASE",
        index: 1,
    },
];

/// What `inherited_index` must expand to: index-less candidates take the row-level `@ 3`.
const INHERITED_CHAIN: &[EnvCandidate] = &[
    EnvCandidate::Mnemonic {
        var: "XVM_ENV_ANY_INHERIT_PRIMARY",
        index: 3,
    },
    EnvCandidate::Mnemonic {
        var: "XVM_ENV_ANY_INHERIT_BACKUP",
        index: 3,
    },
];

#[test]
fn env_any_roster_expands_to_env_any_and_resolves_in_order() {
    // --- Expansion: the DSL lands on `WalletSource::EnvAny` with the candidates it was written
    // with, in declaration order, and index inheritance is settled at codegen time.
    assert_eq!(
        candidates("mixed"),
        MIXED_CHAIN,
        "`env_any(private_key(..), mnemonic(..) @ 1)` must expand in declaration order"
    );
    assert_eq!(
        candidates("inherited_index"),
        INHERITED_CHAIN,
        "index-less `env_any` candidates must inherit the row-level `@ 3`"
    );
    assert_eq!(
        spec("own_index").index,
        9,
        "the row keeps its own `@ 9` even when a candidate overrides it"
    );

    // --- Resolution. Every var this file reads is written here, on one thread, before the factory
    // resolves anything (see the module docs on why this is one test).
    std::env::set_var("XVM_ENV_ANY_MIXED_PK", PRIVATE_KEY);
    std::env::set_var("XVM_ENV_ANY_MIXED_PHRASE", PHRASE_A);
    std::env::remove_var("XVM_ENV_ANY_OWN_PK");
    std::env::set_var("XVM_ENV_ANY_OWN_PHRASE", PHRASE_B);
    std::env::set_var("XVM_ENV_ANY_INHERIT_PRIMARY", "   ");
    std::env::set_var("XVM_ENV_ANY_INHERIT_BACKUP", format!(" {PHRASE_A} "));
    std::env::remove_var("XVM_ENV_ANY_ORPHAN_PK");
    std::env::set_var("XVM_ENV_ANY_ORPHAN_PHRASE", "");

    let factory = WalletFactory::from_roster(EnvAnyWallets::SPECS)
        .expect("an env_any roster reads no env at construction, so it cannot fail here");

    // The first *set* candidate wins, and its kind decides the def: the leading private key beats
    // the mnemonic behind it even though that var is set too. Reversing the fallback order would
    // resolve this row to a mnemonic instead.
    match factory.resolve(ENV_ANY_WALLETS.mixed) {
        Ok(WalletDef::PrivateKey(key)) => assert_eq!(
            key, PRIVATE_KEY,
            "`mixed` must resolve to the leading private key's value"
        ),
        Ok(other) => panic!(
            "`mixed` must resolve to the leading private key (both vars are set), got {other:?}"
        ),
        Err(e) => panic!("resolving `mixed` failed: {e}"),
    }

    // An unset first candidate falls through to the second, and a winning mnemonic derives at its
    // *own* index (`@ 4`), not the row's (`@ 9`).
    match factory.resolve(ENV_ANY_WALLETS.own_index) {
        Ok(WalletDef::Mnemonic {
            phrase,
            index,
            hd_path,
        }) => {
            assert_eq!(
                phrase, PHRASE_B,
                "an unset leading candidate must fall through to the mnemonic"
            );
            assert_eq!(
                index, 4,
                "a winning mnemonic candidate derives at its own `@ 4`, not the row's `@ 9`"
            );
            assert_eq!(hd_path, None, "the row declared no explicit HD path");
        }
        Ok(other) => panic!("`own_index` must fall through to its mnemonic, got {other:?}"),
        Err(e) => panic!("resolving `own_index` failed: {e}"),
    }

    // A blank (whitespace-only) first candidate is missing too, so it falls through; the winner
    // inherits the row-level `@ 3` and its value is trimmed.
    match factory.resolve(ENV_ANY_WALLETS.inherited_index) {
        Ok(WalletDef::Mnemonic { phrase, index, .. }) => {
            assert_eq!(
                phrase, PHRASE_A,
                "a blank leading var counts as missing and its value is trimmed on the winner"
            );
            assert_eq!(index, 3, "the winning candidate inherited the row's `@ 3`");
        }
        Ok(other) => panic!("`inherited_index` must resolve to a mnemonic, got {other:?}"),
        Err(e) => panic!("resolving `inherited_index` failed: {e}"),
    }

    // Every candidate missing (one unset, one empty) names each var tried, in declaration order.
    match factory.resolve(ENV_ANY_WALLETS.orphan) {
        Err(CrossVmError::SecretVarsAllMissing { label, vars }) => {
            assert_eq!(
                label, "orphan",
                "the error carries the failing wallet's label"
            );
            assert_eq!(
                vars,
                ["XVM_ENV_ANY_ORPHAN_PK", "XVM_ENV_ANY_ORPHAN_PHRASE"],
                "the error must name every candidate var, in declaration order"
            );
        }
        Err(other) => panic!("expected SecretVarsAllMissing, got {other:?}"),
        Ok(def) => panic!("`orphan` has no var set, yet resolved to {def:?}"),
    }
}
