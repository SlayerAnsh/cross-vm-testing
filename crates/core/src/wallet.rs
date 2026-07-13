//! VM-agnostic wallet roster, secret resolution, and the per-VM key-derivation trait.
//!
//! Each project defines its wallet *roster* once (typically via `define_wallet_roster!`) as a
//! compile-time [`WalletSpec`] table. Every row is self-describing: a label plus a
//! [`WalletSource`] that fully says how the wallet resolves, one of four ways:
//!
//! - [`WalletSource::EnvMnemonic`] — read a BIP-39 phrase from a process env var, then derive
//!   via the row's account index / HD path.
//! - [`WalletSource::Auto`] — generate a fresh random mnemonic at factory construction (mock
//!   chains; the address is random and must be funded in the setup `fund` phase).
//! - [`WalletSource::EnvPrivateKey`] — read a raw VM-native private key from a process env var
//!   (hex for EVM/Cosmos, base58 for Solana); no HD derivation.
//! - [`WalletSource::EnvAny`] — try a chain of [`EnvCandidate`]s in declaration order; the
//!   first whose env var is set and non-blank wins and dictates the resolved kind (mnemonic
//!   or private key).
//!
//! An env var that is unset, empty, or whitespace-only is uniformly treated as missing, and
//! set values are trimmed before use, so a `FOO=` line in a `.env` behaves exactly like an
//! absent variable.
//!
//! Secrets live only in the process environment (load a `.env` with `dotenvy` before the wallet is
//! used if you keep them in a file). The [`WalletFactory`] resolves every roster row into a
//! [`WalletDef`] on demand. Serializing concurrent broadcasts of one live account (which would
//! collide on the EVM nonce / Cosmos account sequence) is *not* the factory's job: that is the
//! process-global [`crate::wallet_lock`], keyed by `(chain, address)`, so the same account
//! serializes across tests where a per-factory lock could not.
//!
//! This module is deliberately VM-agnostic: it knows nothing about `Address`, `Keypair`, or
//! coin types. The actual mnemonic -> signer derivation lives in each VM crate via the
//! [`WalletDeriver`] trait, so the factory can sit in `core` (which the VM crates depend on)
//! without a dependency cycle.

use std::collections::HashMap;
use std::fmt;

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

/// One candidate in a [`WalletSource::EnvAny`] fallback chain: a process env var plus how its
/// value is interpreted. A candidate whose var is unset or blank (empty/whitespace-only) is
/// skipped in favor of the next one. All fields are `&'static`/`Copy` so chains are `const`
/// slice literals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvCandidate {
    /// The var holds a BIP-39 mnemonic phrase, derived with this candidate's own BIP-44
    /// account index (the roster row's `index` is not consulted) and the row's `hd_path`.
    Mnemonic {
        /// Process env var holding the phrase.
        var: &'static str,
        /// BIP-44 account index used when this candidate wins.
        index: u32,
    },
    /// The var holds a raw VM-native private key (hex for EVM/Cosmos, base58 for Solana).
    /// Derived directly with no HD path; the row's `index`/`hd_path` are ignored.
    PrivateKey {
        /// Process env var holding the key.
        var: &'static str,
    },
}

/// How a wallet resolves its signing material. Each roster row carries exactly one. Stored by the
/// [`WalletFactory`] and resolved dynamically (env vars are read at wallet-use time).
#[derive(Clone, Copy, Debug)]
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
    /// Try each [`EnvCandidate`] in declaration order; the first whose env var is set and
    /// non-blank wins and dictates the resolved kind (mnemonic or private key). When every
    /// candidate is missing, [`WalletFactory::resolve`] fails with
    /// [`CrossVmError::SecretVarsAllMissing`], naming each var tried.
    EnvAny(&'static [EnvCandidate]),
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
    /// A BIP-39 mnemonic plus its derivation parameters (from `Auto`, or a resolved `EnvMnemonic`).
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

/// One roster row as kept by the factory: its [`WalletSource`] plus derivation params, resolved to
/// a [`WalletDef`] on demand. `Auto` rows carry their generated phrase so their derived address is
/// stable within a run; env rows read their secret at use time.
#[derive(Debug)]
struct Row {
    source: WalletSource,
    index: u32,
    hd_path: Option<String>,
    /// Pre-generated mnemonic for an `Auto` row; `None` for env-sourced rows.
    auto_phrase: Option<String>,
}

/// Owns the wallet roster as [`WalletSource`] rows, resolved to signing material on demand.
///
/// VM-agnostic by design (see module docs). Cheap to share behind an `Rc`; each VM chain holds
/// a clone and derives its own signer type from the [`WalletDef`] it resolves here. Broadcast
/// serialization is **not** here: it lives in the process-global [`crate::wallet_lock`], keyed by
/// `(chain, address)`, so the same live account serializes across tests (a per-factory lock could
/// not, since each test builds its own factory).
#[derive(Debug)]
pub struct WalletFactory {
    rows: HashMap<String, Row>,
}

impl WalletFactory {
    /// Store each roster row by label. The only way to construct a factory.
    ///
    /// `Auto` rows generate their fresh mnemonic now (so the derived address is stable within the
    /// run); env-sourced rows keep their [`WalletSource`] and are read lazily by [`resolve`]. The
    /// env var is therefore read only when the wallet is actually used, so a roster can carry an
    /// on-chain wallet whose secret is absent for runs that never sign with it. Load a `.env` with
    /// `dotenvy` before the wallet is used if you keep secrets in a file.
    ///
    /// [`resolve`]: Self::resolve
    pub fn from_roster(roster: &[WalletSpec]) -> Result<Self, CrossVmError> {
        let mut rows = HashMap::new();
        for spec in roster {
            // `Auto` is the only source resolved eagerly: generate once so it stays stable.
            let auto_phrase = match spec.source {
                WalletSource::Auto => Some(generate_mnemonic()?),
                _ => None,
            };
            rows.insert(
                spec.label.to_string(),
                Row {
                    source: spec.source,
                    index: spec.index,
                    hd_path: spec.hd_path.map(str::to_string),
                    auto_phrase,
                },
            );
        }
        Ok(Self { rows })
    }

    /// Resolve a wallet's [`WalletSource`] into a [`WalletDef`], reading env-sourced secrets now.
    ///
    /// `Auto` returns its pre-generated mnemonic; `EnvMnemonic`/`EnvPrivateKey` read their process
    /// env var (a missing variable is a [`CrossVmError::SecretVarMissing`] error raised here, at the
    /// signing call, not at [`from_roster`](Self::from_roster)). `EnvAny` walks its candidates in
    /// declaration order and resolves as the first one whose var is set and non-blank; when none
    /// is, the error is [`CrossVmError::SecretVarsAllMissing`], listing every var tried.
    pub fn resolve<'a>(&self, label: WalletLabel<'a>) -> Result<WalletDef, CrossVmError> {
        let row = self
            .rows
            .get(label.as_str())
            .ok_or_else(|| CrossVmError::WalletNotFound {
                label: label.to_string(),
            })?;
        Ok(match row.source {
            WalletSource::Auto => WalletDef::Mnemonic {
                phrase: row
                    .auto_phrase
                    .clone()
                    .expect("auto row carries a generated phrase"),
                index: row.index,
                hd_path: row.hd_path.clone(),
            },
            WalletSource::EnvMnemonic(var) => WalletDef::Mnemonic {
                phrase: read_env(var, label.as_str())?,
                index: row.index,
                hd_path: row.hd_path.clone(),
            },
            WalletSource::EnvPrivateKey(var) => {
                WalletDef::PrivateKey(read_env(var, label.as_str())?)
            }
            WalletSource::EnvAny(candidates) => {
                resolve_any(candidates, &row.hd_path, label.as_str())?
            }
        })
    }
}

/// Walk an `EnvAny` fallback chain in declaration order; the first candidate whose env var is set
/// and non-blank wins. A winning `Mnemonic` candidate carries its own account index (paired with
/// the row's `hd_path`); a winning `PrivateKey` ignores both. When every candidate is missing, the
/// error names each var tried, never any value.
fn resolve_any(
    candidates: &[EnvCandidate],
    hd_path: &Option<String>,
    label: &str,
) -> Result<WalletDef, CrossVmError> {
    for candidate in candidates {
        match *candidate {
            EnvCandidate::Mnemonic { var, index } => {
                if let Some(phrase) = read_env_opt(var) {
                    return Ok(WalletDef::Mnemonic {
                        phrase,
                        index,
                        hd_path: hd_path.clone(),
                    });
                }
            }
            EnvCandidate::PrivateKey { var } => {
                if let Some(key) = read_env_opt(var) {
                    return Ok(WalletDef::PrivateKey(key));
                }
            }
        }
    }
    Err(CrossVmError::SecretVarsAllMissing {
        label: label.to_string(),
        vars: candidates
            .iter()
            .map(|c| match c {
                EnvCandidate::Mnemonic { var, .. } | EnvCandidate::PrivateKey { var } => {
                    var.to_string()
                }
            })
            .collect(),
    })
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

/// Read a secret from the process environment, mapping absence (unset or blank) to a labelled
/// error. The error carries only the var and label, never the value.
fn read_env(var: &str, label: &str) -> Result<String, CrossVmError> {
    read_env_opt(var).ok_or_else(|| CrossVmError::SecretVarMissing {
        label: label.to_string(),
        var: var.to_string(),
    })
}

/// `Some(trimmed value)` when `var` is set and non-blank; `None` when it is unset, empty, or
/// whitespace-only, so a `FOO=` line behaves exactly like an absent variable.
fn read_env_opt(var: &str) -> Option<String> {
    let value = std::env::var(var).ok()?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
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
    /// rows derive directly. The `WalletDef` is already fully resolved by
    /// [`WalletFactory::resolve`]. Used by every VM's `acquire`/`wallet_address` path.
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

    /// Extract a resolved mnemonic def's phrase/index, panicking on any other variant.
    fn mnemonic(def: &WalletDef) -> (&str, u32) {
        match def {
            WalletDef::Mnemonic { phrase, index, .. } => (phrase, *index),
            other => panic!("expected a resolved mnemonic def, got {other:?}"),
        }
    }

    #[test]
    fn resolves_env_mnemonic_and_auto_rows() {
        // Construction reads no env; `resolve` reads the var dynamically. `Auto` resolves to its
        // pre-generated mnemonic.
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
        assert_eq!(mnemonic(&f.resolve(ALICE).unwrap()), (PHRASE, 0));
        assert_eq!(mnemonic(&f.resolve(BOB).unwrap()).1, 1);
        let def = f.resolve(EPHEMERAL).unwrap();
        let (gen, _) = mnemonic(&def);
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
            f.resolve(ALICE).unwrap(),
            WalletDef::PrivateKey(k) if k == "0xdeadbeef"
        ));
    }

    #[test]
    fn missing_env_var_is_deferred_to_resolve() {
        // A missing env var no longer fails at construction; it errors only when the wallet is
        // resolved (i.e. at the signing call).
        let roster: &[WalletSpec] = &[WalletSpec {
            label: "alice",
            source: WalletSource::EnvMnemonic("CORE_TEST_DEFINITELY_UNSET_VAR"),
            index: 0,
            hd_path: None,
        }];
        let f = WalletFactory::from_roster(roster).expect("construction defers env resolution");
        assert!(matches!(
            f.resolve(ALICE).unwrap_err(),
            CrossVmError::SecretVarMissing { ref var, .. } if var == "CORE_TEST_DEFINITELY_UNSET_VAR"
        ));
    }

    #[test]
    fn auto_resolves_stably() {
        // Two resolves of the same `Auto` wallet return the same generated phrase (its derived
        // address must be stable within a run).
        let f = WalletFactory::from_roster(AUTO_ROSTER).unwrap();
        let a = f.resolve(ALICE).unwrap();
        let b = f.resolve(ALICE).unwrap();
        assert_eq!(mnemonic(&a).0, mnemonic(&b).0);
    }

    #[test]
    fn unknown_label_is_not_found() {
        let f = WalletFactory::from_roster(AUTO_ROSTER).unwrap();
        assert!(matches!(
            f.resolve(NOBODY).unwrap_err(),
            CrossVmError::WalletNotFound { .. }
        ));
    }

    /// Build a one-row roster whose `alice` resolves via the given `EnvAny` chain.
    fn any_factory(chain: &'static [EnvCandidate]) -> WalletFactory {
        any_factory_with_path(chain, None)
    }

    fn any_factory_with_path(
        chain: &'static [EnvCandidate],
        hd_path: Option<&'static str>,
    ) -> WalletFactory {
        let roster: &[WalletSpec] = &[WalletSpec {
            label: "alice",
            source: WalletSource::EnvAny(chain),
            index: 0,
            hd_path,
        }];
        WalletFactory::from_roster(roster).unwrap()
    }

    #[test]
    fn blank_env_var_is_missing_for_single_var_sources() {
        // `FOO=` (or whitespace-only) counts as unset on the single-var paths too, erroring at
        // resolve rather than in downstream BIP-39 parsing. Set values are trimmed.
        std::env::set_var("CORE_TEST_BLANK_SINGLE", "   ");
        let roster: &[WalletSpec] = &[WalletSpec {
            label: "alice",
            source: WalletSource::EnvMnemonic("CORE_TEST_BLANK_SINGLE"),
            index: 0,
            hd_path: None,
        }];
        let f = WalletFactory::from_roster(roster).unwrap();
        assert!(matches!(
            f.resolve(ALICE).unwrap_err(),
            CrossVmError::SecretVarMissing { ref var, .. } if var == "CORE_TEST_BLANK_SINGLE"
        ));

        std::env::set_var("CORE_TEST_PADDED_SINGLE", format!("  {PHRASE}  "));
        let roster: &[WalletSpec] = &[WalletSpec {
            label: "alice",
            source: WalletSource::EnvMnemonic("CORE_TEST_PADDED_SINGLE"),
            index: 0,
            hd_path: None,
        }];
        let f = WalletFactory::from_roster(roster).unwrap();
        assert_eq!(mnemonic(&f.resolve(ALICE).unwrap()).0, PHRASE);
    }

    #[test]
    fn env_any_first_candidate_wins() {
        std::env::set_var("CORE_TEST_ANY_FIRST_A", PHRASE);
        std::env::set_var("CORE_TEST_ANY_FIRST_B", "0xsecond");
        const CHAIN: &[EnvCandidate] = &[
            EnvCandidate::Mnemonic {
                var: "CORE_TEST_ANY_FIRST_A",
                index: 0,
            },
            EnvCandidate::PrivateKey {
                var: "CORE_TEST_ANY_FIRST_B",
            },
        ];
        let f = any_factory(CHAIN);
        assert_eq!(mnemonic(&f.resolve(ALICE).unwrap()), (PHRASE, 0));
    }

    #[test]
    fn env_any_blank_candidate_falls_through() {
        // A blank (whitespace-only) first var is skipped, and the winning value is trimmed.
        std::env::set_var("CORE_TEST_ANY_BLANK_A", "   ");
        std::env::set_var("CORE_TEST_ANY_BLANK_B", format!(" {PHRASE} "));
        const CHAIN: &[EnvCandidate] = &[
            EnvCandidate::Mnemonic {
                var: "CORE_TEST_ANY_BLANK_A",
                index: 0,
            },
            EnvCandidate::Mnemonic {
                var: "CORE_TEST_ANY_BLANK_B",
                index: 2,
            },
        ];
        let f = any_factory(CHAIN);
        assert_eq!(mnemonic(&f.resolve(ALICE).unwrap()), (PHRASE, 2));
    }

    #[test]
    fn env_any_private_key_wins_over_later_mnemonic() {
        // The winning candidate's kind decides the def: a private key resolves directly even
        // when a later mnemonic candidate is also set.
        std::env::set_var("CORE_TEST_ANY_PK_A", "0xdeadbeef");
        std::env::set_var("CORE_TEST_ANY_PK_B", PHRASE);
        const CHAIN: &[EnvCandidate] = &[
            EnvCandidate::PrivateKey {
                var: "CORE_TEST_ANY_PK_A",
            },
            EnvCandidate::Mnemonic {
                var: "CORE_TEST_ANY_PK_B",
                index: 1,
            },
        ];
        let f = any_factory(CHAIN);
        assert!(matches!(
            f.resolve(ALICE).unwrap(),
            WalletDef::PrivateKey(k) if k == "0xdeadbeef"
        ));
    }

    #[test]
    fn env_any_mnemonic_candidate_honors_own_index() {
        // A winning mnemonic candidate derives with its own index (not the row's, which is 0
        // here) and the row's hd_path.
        std::env::set_var("CORE_TEST_ANY_INDEX", PHRASE);
        const CHAIN: &[EnvCandidate] = &[EnvCandidate::Mnemonic {
            var: "CORE_TEST_ANY_INDEX",
            index: 7,
        }];
        let f = any_factory_with_path(CHAIN, Some("m/44'/118'/7'/0/0"));
        match f.resolve(ALICE).unwrap() {
            WalletDef::Mnemonic {
                phrase,
                index,
                hd_path,
            } => {
                assert_eq!(phrase, PHRASE);
                assert_eq!(index, 7);
                assert_eq!(hd_path.as_deref(), Some("m/44'/118'/7'/0/0"));
            }
            other => panic!("expected a resolved mnemonic def, got {other:?}"),
        }
    }

    #[test]
    fn env_any_all_missing_names_every_var_in_order() {
        std::env::remove_var("CORE_TEST_ANY_MISSING_A");
        std::env::set_var("CORE_TEST_ANY_MISSING_B", "");
        const CHAIN: &[EnvCandidate] = &[
            EnvCandidate::Mnemonic {
                var: "CORE_TEST_ANY_MISSING_A",
                index: 0,
            },
            EnvCandidate::PrivateKey {
                var: "CORE_TEST_ANY_MISSING_B",
            },
        ];
        let f = any_factory(CHAIN);
        match f.resolve(ALICE).unwrap_err() {
            CrossVmError::SecretVarsAllMissing { label, vars } => {
                assert_eq!(label, "alice");
                assert_eq!(vars, ["CORE_TEST_ANY_MISSING_A", "CORE_TEST_ANY_MISSING_B"]);
            }
            other => panic!("expected SecretVarsAllMissing, got {other:?}"),
        }
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
            mnemonic(&a.resolve(EPHEMERAL).unwrap()).0,
            mnemonic(&b.resolve(EPHEMERAL).unwrap()).0
        );
    }
}
