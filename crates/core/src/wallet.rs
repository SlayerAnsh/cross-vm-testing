//! VM-agnostic wallet roster, secret resolution, and the per-VM key-derivation trait.
//!
//! Each project defines its wallet *roster* once (typically via `define_wallet_roster!`) as a
//! compile-time [`WalletSpec`] table. Every row is self-describing: a label plus a
//! [`WalletSource`] that fully says how the wallet resolves, one of three ways:
//!
//! - [`WalletSource::EnvMnemonic`] — read a BIP-39 phrase from a process env var, then derive
//!   via the row's account index / HD path.
//! - [`WalletSource::Auto`] — generate a fresh random mnemonic at build time (mock chains; the
//!   address is random and must be funded in the setup `fund` phase).
//! - [`WalletSource::EnvPrivateKey`] — read a raw VM-native private key from a process env var
//!   (hex for EVM/Cosmos, base58 for Solana); no HD derivation.
//!
//! Secrets live only in the process environment (load a `.env` with `dotenvy` before building
//! if you keep them in a file). The [`WalletFactory`] resolves every roster row into a
//! [`WalletDef`] and hands out a per-wallet async lock so the same wallet never broadcasts two
//! transactions concurrently (which would collide on the EVM nonce / Cosmos account sequence).
//!
//! This module is deliberately VM-agnostic: it knows nothing about `Address`, `Keypair`, or
//! coin types. The actual mnemonic -> signer derivation lives in each VM crate via the
//! [`WalletDeriver`] trait, so the factory can sit in `core` (which the VM crates depend on)
//! without a dependency cycle.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::chain_provider::ChainProvider;
use crate::error::CrossVmError;

/// A compile-time wallet label. Use roster macro fields (e.g. `TEST_WALLETS.alice`) instead
/// of raw string literals at chain API boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WalletLabel<'a>(&'a str);

impl WalletLabel<'static> {
    /// Construct a label from a static string (typically only used by generated roster code).
    pub const fn new(label: &'static str) -> Self {
        Self(label)
    }
}

impl<'a> WalletLabel<'a> {
    /// Wrap a borrowed label (e.g. at a `cross_vm_contract` hook boundary).
    pub fn wrap(label: &'a str) -> Self {
        Self(label)
    }

    /// The underlying label string.
    pub fn as_str(self) -> &'a str {
        self.0
    }
}

impl fmt::Display for WalletLabel<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl AsRef<str> for WalletLabel<'_> {
    fn as_ref(&self) -> &str {
        self.0
    }
}

/// How a wallet resolves its signing material. Each roster row carries exactly one.
pub enum WalletSource {
    /// Read a BIP-39 mnemonic phrase from this process env var, then derive via the row's
    /// account index / HD path.
    EnvMnemonic(&'static str),
    /// Mint a fresh random BIP-39 mnemonic at factory construction. Useful for mock chains
    /// (the derived address is random and must be funded via the setup `fund` phase); on live
    /// RPC such a wallet is unfunded and cannot broadcast.
    Auto,
    /// Read a raw VM-native private key from this process env var (hex for EVM/Cosmos, base58
    /// for Solana). Derived directly with no HD path; the row's `index`/`hd_path` are ignored.
    EnvPrivateKey(&'static str),
}

/// A single compile-time wallet declaration. All fields are `&'static` so roster tables are
/// `const`.
pub struct WalletSpec {
    /// VM-agnostic label used by broadcast calls (e.g. `chain.call(.., TEST_WALLETS.alice)`).
    pub label: &'static str,
    /// Where the mnemonic comes from.
    pub source: WalletSource,
    /// BIP-44 account index; combined with each VM's coin type to build the HD path.
    pub index: u32,
    /// Explicit full HD path override. When `None`, the VM deriver's `default_hd_path(index)`
    /// is used.
    pub hd_path: Option<&'static str>,
}

/// A resolved wallet's signing material. Held in-process only.
#[derive(Clone, Debug)]
pub enum WalletDef {
    /// A BIP-39 mnemonic plus its derivation parameters (from `EnvMnemonic` or `Auto`).
    Mnemonic {
        /// The resolved BIP-39 phrase (from a process env var or freshly generated).
        phrase: String,
        /// BIP-44 account index.
        index: u32,
        /// Optional explicit HD path override; when `None`, the VM deriver's
        /// `default_hd_path(index)` is used.
        hd_path: Option<String>,
    },
    /// A raw VM-native private key (from `EnvPrivateKey`); derived directly, no HD path.
    PrivateKey(String),
}

/// Owns the resolved wallet roster and per-wallet broadcast locks.
///
/// VM-agnostic by design (see module docs). Cheap to share behind an `Rc`; each VM chain holds
/// a clone and derives its own signer type from the [`WalletDef`] it looks up here.
#[derive(Debug)]
pub struct WalletFactory {
    defs: HashMap<String, WalletDef>,
    locks: HashMap<String, Arc<Mutex<()>>>,
}

impl WalletFactory {
    /// Resolve every roster row into a [`WalletDef`]. The only way to construct a factory.
    ///
    /// `EnvMnemonic`/`EnvPrivateKey` rows read their value from the process environment (load a
    /// `.env` with `dotenvy` first if you keep secrets in a file); a missing variable is a
    /// [`CrossVmError::SecretVarMissing`] error. `Auto` rows generate a fresh mnemonic.
    pub fn from_roster(roster: &[WalletSpec]) -> Result<Self, CrossVmError> {
        let mut defs = HashMap::new();
        let mut locks = HashMap::new();
        for spec in roster {
            locks.insert(spec.label.to_string(), Arc::new(Mutex::new(())));
            let def = match &spec.source {
                WalletSource::Auto => WalletDef::Mnemonic {
                    phrase: generate_mnemonic()?,
                    index: spec.index,
                    hd_path: spec.hd_path.map(str::to_string),
                },
                WalletSource::EnvMnemonic(var) => WalletDef::Mnemonic {
                    phrase: read_env(var, spec.label)?,
                    index: spec.index,
                    hd_path: spec.hd_path.map(str::to_string),
                },
                WalletSource::EnvPrivateKey(var) => {
                    WalletDef::PrivateKey(read_env(var, spec.label)?)
                }
            };
            defs.insert(spec.label.to_string(), def);
        }
        Ok(Self { defs, locks })
    }

    /// Look up a resolved wallet by label.
    pub fn def<'a>(&self, label: WalletLabel<'a>) -> Result<&WalletDef, CrossVmError> {
        self.defs
            .get(label.as_str())
            .ok_or_else(|| CrossVmError::WalletNotFound {
                label: label.to_string(),
            })
    }

    /// Acquire the wallet's broadcast lock. The returned guard is `'static` (owned) so it can
    /// be held across `.await` points for the whole build -> sign -> broadcast sequence, then
    /// released on drop. Uses an async mutex: on the single-thread runtime a `std` mutex held
    /// across an await would deadlock.
    pub async fn lock<'a>(
        &self,
        label: WalletLabel<'a>,
    ) -> Result<OwnedMutexGuard<()>, CrossVmError> {
        let m = self
            .locks
            .get(label.as_str())
            .ok_or_else(|| CrossVmError::WalletNotFound {
                label: label.to_string(),
            })?;
        Ok(m.clone().lock_owned().await)
    }
}

/// Standard BIP-44 account path `m/44'/<coin>'/<index>'/0/0` (EVM and Cosmos shape). Solana
/// uses a shorter `m/44'/<coin>'/<index>'/0'` and builds its own.
pub fn bip44_account_path(coin_type: u32, index: u32) -> String {
    format!("m/44'/{coin_type}'/{index}'/0/0")
}

fn generate_mnemonic() -> Result<String, CrossVmError> {
    bip39::Mnemonic::generate(12)
        .map(|m| m.to_string())
        .map_err(|e| CrossVmError::wallet(format!("generating mnemonic: {e}")))
}

/// Read a secret from the process environment, mapping absence to a labelled error.
fn read_env(var: &str, label: &str) -> Result<String, CrossVmError> {
    std::env::var(var).map_err(|_| CrossVmError::SecretVarMissing {
        label: label.to_string(),
        var: var.to_string(),
    })
}

/// Per-VM key derivation: turn a mnemonic + HD path into that ecosystem's signer.
///
/// A sibling of [`ChainProvider`] (not a method on it) so `ChainProvider`'s many impls need no
/// crypto and the new `Signer` associated type stays isolated. Each VM crate implements this on
/// its chain handle.
pub trait WalletDeriver: ChainProvider {
    /// Full signing identity (holds the private key). Distinct from [`ChainProvider::Address`].
    type Signer: Clone;

    /// BIP-44 coin type: 60 (EVM), 118 (Cosmos), 501 (Solana).
    const COIN_TYPE: u32;

    /// This ecosystem's standard derivation path for an account index.
    fn default_hd_path(index: u32) -> String;

    /// Derive a signer from a mnemonic phrase and a full BIP-44 path.
    fn derive_signer(&self, mnemonic: &str, hd_path: &str) -> Result<Self::Signer, Self::Error>;

    /// Derive a signer from a raw VM-native private key (hex for EVM/Cosmos, base58 for Solana).
    fn derive_from_key(&self, private_key: &str) -> Result<Self::Signer, Self::Error>;

    /// The on-chain address a signer controls.
    fn signer_address(&self, signer: &Self::Signer) -> Self::Address;

    /// Resolve a [`WalletDef`] into a signer: mnemonic rows derive via index/HD path, private-key
    /// rows derive directly. Used by every VM's `acquire`/`wallet_address` path.
    fn signer_for(&self, def: &WalletDef) -> Result<Self::Signer, Self::Error> {
        match def {
            WalletDef::Mnemonic {
                phrase,
                index,
                hd_path,
            } => {
                let path = hd_path
                    .clone()
                    .unwrap_or_else(|| Self::default_hd_path(*index));
                self.derive_signer(phrase, &path)
            }
            WalletDef::PrivateKey(key) => self.derive_from_key(key),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHRASE: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    const ALICE: WalletLabel<'static> = WalletLabel::new("alice");
    const BOB: WalletLabel<'static> = WalletLabel::new("bob");
    const EPHEMERAL: WalletLabel<'static> = WalletLabel::new("ephemeral");
    const NOBODY: WalletLabel<'static> = WalletLabel::new("nobody");

    /// Roster of two `Auto` rows, used by the lock tests (no env needed).
    const AUTO_ROSTER: &[WalletSpec] = &[
        WalletSpec {
            label: "alice",
            source: WalletSource::Auto,
            index: 0,
            hd_path: None,
        },
        WalletSpec {
            label: "bob",
            source: WalletSource::Auto,
            index: 1,
            hd_path: None,
        },
    ];

    /// Extract a mnemonic def's phrase/index, panicking if it is a private-key def.
    fn mnemonic(def: &WalletDef) -> (&str, u32) {
        match def {
            WalletDef::Mnemonic { phrase, index, .. } => (phrase, *index),
            WalletDef::PrivateKey(_) => panic!("expected a mnemonic def"),
        }
    }

    #[test]
    fn resolves_env_mnemonic_and_auto_rows() {
        // Use a test-unique env var to avoid colliding with other tests' process env.
        std::env::set_var("CORE_TEST_MNEMONIC_MAIN", PHRASE);
        let roster: &[WalletSpec] = &[
            WalletSpec {
                label: "alice",
                source: WalletSource::EnvMnemonic("CORE_TEST_MNEMONIC_MAIN"),
                index: 0,
                hd_path: None,
            },
            WalletSpec {
                label: "bob",
                source: WalletSource::EnvMnemonic("CORE_TEST_MNEMONIC_MAIN"),
                index: 1,
                hd_path: None,
            },
            WalletSpec {
                label: "ephemeral",
                source: WalletSource::Auto,
                index: 0,
                hd_path: None,
            },
        ];
        let f = WalletFactory::from_roster(roster).unwrap();
        assert_eq!(mnemonic(f.def(ALICE).unwrap()), (PHRASE, 0));
        assert_eq!(mnemonic(f.def(BOB).unwrap()).1, 1);
        let (gen, _) = mnemonic(f.def(EPHEMERAL).unwrap());
        assert_eq!(gen.split_whitespace().count(), 12);
        assert_ne!(gen, PHRASE);
        assert!(bip39::Mnemonic::parse_normalized(gen).is_ok());
    }

    #[test]
    fn resolves_env_private_key_row() {
        std::env::set_var("CORE_TEST_PRIVKEY", "0xdeadbeef");
        let roster: &[WalletSpec] = &[WalletSpec {
            label: "alice",
            source: WalletSource::EnvPrivateKey("CORE_TEST_PRIVKEY"),
            index: 0,
            hd_path: None,
        }];
        let f = WalletFactory::from_roster(roster).unwrap();
        assert!(matches!(
            f.def(ALICE).unwrap(),
            WalletDef::PrivateKey(k) if k == "0xdeadbeef"
        ));
    }

    #[test]
    fn missing_env_var_is_error() {
        let roster: &[WalletSpec] = &[WalletSpec {
            label: "alice",
            source: WalletSource::EnvMnemonic("CORE_TEST_DEFINITELY_UNSET_VAR"),
            index: 0,
            hd_path: None,
        }];
        let err = WalletFactory::from_roster(roster).unwrap_err();
        assert!(matches!(
            err,
            CrossVmError::SecretVarMissing { ref var, .. } if var == "CORE_TEST_DEFINITELY_UNSET_VAR"
        ));
    }

    #[test]
    fn unknown_label_is_not_found() {
        let f = WalletFactory::from_roster(AUTO_ROSTER).unwrap();
        assert!(matches!(
            f.def(NOBODY).unwrap_err(),
            CrossVmError::WalletNotFound { .. }
        ));
    }

    #[test]
    fn two_auto_runs_differ() {
        const GEN_ONLY: &[WalletSpec] = &[WalletSpec {
            label: "ephemeral",
            source: WalletSource::Auto,
            index: 0,
            hd_path: None,
        }];
        let a = WalletFactory::from_roster(GEN_ONLY).unwrap();
        let b = WalletFactory::from_roster(GEN_ONLY).unwrap();
        assert_ne!(
            mnemonic(a.def(EPHEMERAL).unwrap()).0,
            mnemonic(b.def(EPHEMERAL).unwrap()).0
        );
    }

    #[tokio::test]
    async fn same_wallet_lock_serializes() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let f = Rc::new(WalletFactory::from_roster(AUTO_ROSTER).unwrap());
        let inflight = Rc::new(RefCell::new(0u32));
        let max_seen = Rc::new(RefCell::new(0u32));

        let task = |f: Rc<WalletFactory>, inflight: Rc<RefCell<u32>>, max: Rc<RefCell<u32>>| async move {
            let _guard = f.lock(ALICE).await.unwrap();
            {
                let mut n = inflight.borrow_mut();
                *n += 1;
                let mut m = max.borrow_mut();
                if *n > *m {
                    *m = *n;
                }
            }
            tokio::task::yield_now().await;
            *inflight.borrow_mut() -= 1;
        };

        tokio::join!(
            task(f.clone(), inflight.clone(), max_seen.clone()),
            task(f.clone(), inflight.clone(), max_seen.clone()),
        );
        assert_eq!(
            *max_seen.borrow(),
            1,
            "same-wallet broadcasts must not overlap"
        );
    }

    #[tokio::test]
    async fn different_wallets_do_not_contend() {
        use std::rc::Rc;
        let f = Rc::new(WalletFactory::from_roster(AUTO_ROSTER).unwrap());
        let _alice = f.lock(ALICE).await.unwrap();
        let _bob = f.lock(BOB).await.unwrap();
    }
}
