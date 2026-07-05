//! `#[config_runner]`: bridges a `*.cross-vm.toml` config profile into `#[tokio::test]`s, reusing
//! the P1 loader + P2 registry at run time through
//! `cross_vm_framework::config::test_bridge::run_profile_for_test`.
//!
//! ```ignore
//! #[config_runner(config = "vault.cross-vm.toml", harness = VaultHarness, setup = vault_config_setup, profile = "smoke")]
//! async fn vault_smoke_config() {}
//! ```
//!
//! At **expansion** time this reads `<CARGO_MANIFEST_DIR>/<config>` (a proc-macro is a build-time
//! process with `CARGO_MANIFEST_DIR` set, same as any other build script) purely to learn how
//! many `#[tokio::test]` fns to fan out: a `mode = "fuzz"` profile emits one
//! `<name>_case_<i>` test per `cases` (mirroring `harness_core_macros::fuzz_runner`'s per-case
//! fan-out), any other mode emits a single `<name>` test. The annotated fn's own body is dropped
//! entirely; only its name and asyncness are used.
//!
//! The runtime bridge, not this macro, is the source of truth for the config's actual contents:
//! each generated fuzz-case test passes the `cases` count this macro saw at expansion time as
//! `expected_cases`, and `run_profile_for_test` panics with a "config changed since compile,
//! rebuild" message if a fresh load of the same file no longer agrees (spec section 13/P5).
//!
//! The emitted code names `cross_vm_framework::config::test_bridge::run_profile_for_test`
//! fully qualified (no scope requirement for the framework path itself), but `harness`/`setup`
//! are emitted unqualified — the call site must `use` them into scope, same convention as
//! `harness_core_macros::fuzz_runner` and `#[cross_vm_contract]`.

use std::path::Path;

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{Expr, Ident, ItemFn, LitStr, Token};

/// Parsed `key = value` attribute arguments.
struct Args {
    config: LitStr,
    harness: Expr,
    setup: Expr,
    profile: LitStr,
}

impl Parse for Args {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut config = None;
        let mut harness = None;
        let mut setup = None;
        let mut profile = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "config" => config = Some(input.parse()?),
                "harness" => harness = Some(input.parse()?),
                "setup" => setup = Some(input.parse()?),
                "profile" => profile = Some(input.parse()?),
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "config_runner: unknown key `{other}` (expected config / harness / \
                             setup / profile)"
                        ),
                    ))
                }
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(Args {
            config: config.ok_or_else(|| {
                syn::Error::new(Span::call_site(), "config_runner: missing `config`")
            })?,
            harness: harness.ok_or_else(|| {
                syn::Error::new(Span::call_site(), "config_runner: missing `harness`")
            })?,
            setup: setup.ok_or_else(|| {
                syn::Error::new(Span::call_site(), "config_runner: missing `setup`")
            })?,
            profile: profile.ok_or_else(|| {
                syn::Error::new(Span::call_site(), "config_runner: missing `profile`")
            })?,
        })
    }
}

/// What a `[profile.<name>]` table's `mode` implies for fan-out: a `fuzz` profile fans out one
/// test per `cases`, every other mode emits exactly one.
#[derive(Debug)]
enum ProfileMode {
    Fuzz(usize),
    Other,
}

/// Reads `path` as TOML and learns `profile_name`'s fan-out shape (spec section 13/P5): a
/// `mode = "fuzz"` profile's `cases` count (honoring a `[defaults].cases` fallback, mirroring the
/// loader's own defaults-merge for just this one key — see `harness-config`'s `merge` module for
/// the full version this deliberately does not replicate), or [`ProfileMode::Other`] for any
/// other mode. Every error path returns a plain `String`; the caller turns it into a
/// `compile_error!`.
fn read_profile_mode(path: &Path, profile_name: &str) -> Result<ProfileMode, String> {
    let contents = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "config_runner: failed to read config `{}`: {e}",
            path.display()
        )
    })?;
    let doc: toml::Value = contents.parse().map_err(|e| {
        format!(
            "config_runner: failed to parse config `{}` as TOML: {e}",
            path.display()
        )
    })?;

    let profile = doc
        .get("profile")
        .and_then(|v| v.get(profile_name))
        .and_then(|v| v.as_table())
        .ok_or_else(|| {
            format!(
                "config_runner: `{}` has no [profile.{profile_name}]",
                path.display()
            )
        })?;

    let defaults = doc.get("defaults").and_then(|v| v.as_table());

    let mode = profile
        .get("mode")
        .and_then(|v| v.as_str())
        .or_else(|| {
            defaults
                .and_then(|d| d.get("mode"))
                .and_then(|v| v.as_str())
        })
        .ok_or_else(|| {
            format!(
                "config_runner: profile `{profile_name}` has no `mode` (and no [defaults].mode)"
            )
        })?;

    if mode != "fuzz" {
        return Ok(ProfileMode::Other);
    }

    let cases = profile
        .get("cases")
        .and_then(|v| v.as_integer())
        .or_else(|| {
            defaults
                .and_then(|d| d.get("cases"))
                .and_then(|v| v.as_integer())
        })
        .ok_or_else(|| {
            format!(
                "config_runner: fuzz profile `{profile_name}` has no `cases` (set it on the \
                 profile itself, or in [defaults])"
            )
        })?;

    if cases <= 0 {
        return Err(format!(
            "config_runner: fuzz profile `{profile_name}`: `cases` must be greater than 0"
        ));
    }

    Ok(ProfileMode::Fuzz(cases as usize))
}

/// Expands `#[config_runner(..)]` over `item`: reads `CARGO_MANIFEST_DIR` (always set by cargo
/// during a real build) and delegates to [`expand_with_manifest_dir`], the testable body.
pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").map_err(|_| {
        syn::Error::new(
            Span::call_site(),
            "config_runner: CARGO_MANIFEST_DIR is not set (expected in a cargo build)",
        )
    })?;
    expand_with_manifest_dir(&manifest_dir, attr, item)
}

/// The body of [`expand`], parameterized over the manifest directory so unit tests can point it
/// at a fixture directory instead of mutating the real process environment (`CARGO_MANIFEST_DIR`
/// is process-global; this crate's `#[deny(unsafe_code)]`-adjacent lint config also forbids the
/// `unsafe` `std::env::set_var` a real env-var-swap test would need).
fn expand_with_manifest_dir(
    manifest_dir: &str,
    attr: TokenStream,
    item: TokenStream,
) -> syn::Result<TokenStream> {
    let args: Args = syn::parse2(attr)?;
    let func: ItemFn = syn::parse2(item)?;

    if func.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &func.sig,
            "config_runner: the fn must be `async`",
        ));
    }

    let name = &func.sig.ident;
    let config_lit = &args.config;
    let harness = &args.harness;
    let setup = &args.setup;
    let profile_lit = &args.profile;

    let full_path = Path::new(manifest_dir).join(args.config.value());

    let mode = read_profile_mode(&full_path, &args.profile.value())
        .map_err(|e| syn::Error::new(Span::call_site(), e))?;

    match mode {
        ProfileMode::Fuzz(cases) => {
            let tests = (0..cases).map(|i| {
                let fn_name = format_ident!("{}_case_{}", name, i);
                quote! {
                    #[tokio::test]
                    async fn #fn_name() {
                        cross_vm_framework::config::test_bridge::run_profile_for_test(
                            concat!(env!("CARGO_MANIFEST_DIR"), "/", #config_lit),
                            || #harness,
                            #setup,
                            #profile_lit,
                            Some(#i),
                            Some(#cases),
                        )
                        .await;
                    }
                }
            });
            Ok(quote! { #(#tests)* })
        }
        ProfileMode::Other => Ok(quote! {
            #[tokio::test]
            async fn #name() {
                cross_vm_framework::config::test_bridge::run_profile_for_test(
                    concat!(env!("CARGO_MANIFEST_DIR"), "/", #config_lit),
                    || #harness,
                    #setup,
                    #profile_lit,
                    None,
                    None,
                )
                .await;
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;
    use std::io::Write;

    /// A temp TOML file with `contents`, on a unique path per test (process id plus a monotonic
    /// counter avoids collisions across parallel test runs); returns its path.
    fn temp_toml(label: &str, contents: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "cross-vm-config-runner-macro-test-{}-{}-{label}.toml",
            std::process::id(),
            n
        ));
        let mut f = std::fs::File::create(&path).expect("create temp toml");
        f.write_all(contents.as_bytes()).expect("write temp toml");
        path
    }

    #[test]
    fn read_profile_mode_fuzz_reads_cases_directly() {
        let path = temp_toml(
            "fuzz-direct",
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 8
ops = 20
"#,
        );
        match read_profile_mode(&path, "smoke").expect("mode") {
            ProfileMode::Fuzz(n) => assert_eq!(n, 8),
            ProfileMode::Other => panic!("expected Fuzz"),
        }
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_profile_mode_fuzz_falls_back_to_defaults_cases() {
        let path = temp_toml(
            "fuzz-defaults",
            r#"
[harness]
name = "vault"

[defaults]
cases = 5

[profile.smoke]
mode = "fuzz"
ops = 20
"#,
        );
        match read_profile_mode(&path, "smoke").expect("mode") {
            ProfileMode::Fuzz(n) => assert_eq!(n, 5),
            ProfileMode::Other => panic!("expected Fuzz"),
        }
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_profile_mode_non_fuzz_is_other() {
        let path = temp_toml(
            "invariant",
            r#"
[harness]
name = "vault"

[profile.inv]
mode = "invariant"
ops = 100
"#,
        );
        match read_profile_mode(&path, "inv").expect("mode") {
            ProfileMode::Other => {}
            ProfileMode::Fuzz(_) => panic!("expected Other"),
        }
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_profile_mode_missing_profile_errors() {
        let path = temp_toml(
            "missing-profile",
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 1
ops = 1
"#,
        );
        let err = read_profile_mode(&path, "nope").unwrap_err();
        assert!(err.contains("profile.nope"), "{err}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_profile_mode_fuzz_missing_cases_errors() {
        let path = temp_toml(
            "fuzz-missing-cases",
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
ops = 1
"#,
        );
        let err = read_profile_mode(&path, "smoke").unwrap_err();
        assert!(err.contains("`cases`"), "{err}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_profile_mode_missing_file_errors() {
        let err = read_profile_mode(Path::new("/nonexistent/path.toml"), "smoke").unwrap_err();
        assert!(err.contains("failed to read"), "{err}");
    }

    #[test]
    fn expand_fuzz_profile_fans_out_one_test_per_case() {
        let dir = std::env::temp_dir().join(format!(
            "cross-vm-config-runner-macro-dir-{}-fanout",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("vault.cross-vm.toml"),
            r#"
[harness]
name = "vault"

[profile.smoke]
mode = "fuzz"
cases = 3
ops = 20
"#,
        )
        .expect("write config");

        let attr = quote! {
            config = "vault.cross-vm.toml",
            harness = VaultHarness,
            setup = vault_config_setup,
            profile = "smoke"
        };
        let item = quote! {
            async fn vault_smoke_config() {}
        };

        let out = expand_with_manifest_dir(dir.to_str().unwrap(), attr, item).expect("expand");
        let s = out.to_string();
        assert!(s.contains("vault_smoke_config_case_0"), "{s}");
        assert!(s.contains("vault_smoke_config_case_1"), "{s}");
        assert!(s.contains("vault_smoke_config_case_2"), "{s}");
        assert!(!s.contains("vault_smoke_config_case_3"), "{s}");
        assert!(s.contains("run_profile_for_test"), "{s}");
        assert!(s.contains("VaultHarness"), "{s}");
        assert!(s.contains("vault_config_setup"), "{s}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn expand_non_fuzz_profile_emits_a_single_test() {
        let dir = std::env::temp_dir().join(format!(
            "cross-vm-config-runner-macro-dir-{}-single",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("vault.cross-vm.toml"),
            r#"
[harness]
name = "vault"

[profile.inv]
mode = "invariant"
ops = 100
"#,
        )
        .expect("write config");

        let attr = quote! {
            config = "vault.cross-vm.toml",
            harness = VaultHarness,
            setup = vault_config_setup,
            profile = "inv"
        };
        let item = quote! {
            async fn vault_inv_config() {}
        };

        let out = expand_with_manifest_dir(dir.to_str().unwrap(), attr, item).expect("expand");
        let s = out.to_string();
        assert!(s.contains("fn vault_inv_config"), "{s}");
        assert!(!s.contains("vault_inv_config_case_0"), "{s}");
        assert!(s.contains("None"), "{s}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn expand_missing_config_file_is_a_clear_error() {
        let dir = std::env::temp_dir().join(format!(
            "cross-vm-config-runner-macro-dir-{}-missing",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");

        let attr = quote! {
            config = "does-not-exist.cross-vm.toml",
            harness = VaultHarness,
            setup = vault_config_setup,
            profile = "smoke"
        };
        let item = quote! {
            async fn vault_smoke_config() {}
        };

        let err = expand_with_manifest_dir(dir.to_str().unwrap(), attr, item).unwrap_err();
        assert!(err.to_string().contains("failed to read"), "{err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn expand_rejects_non_async_fn() {
        let attr = quote! {
            config = "vault.cross-vm.toml",
            harness = VaultHarness,
            setup = vault_config_setup,
            profile = "smoke"
        };
        let item = quote! {
            fn vault_smoke_config() {}
        };
        let err = expand(attr, item).unwrap_err();
        assert!(err.to_string().contains("async"), "{err}");
    }
}
