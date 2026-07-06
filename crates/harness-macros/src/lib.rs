//! Attribute macros for `harness-core`. The emitted code names `Runner`, `sub_seed`, and
//! `random_seed` unqualified, so bring them into scope (e.g. `use harness_core::*` or the
//! consuming framework's prelude) at the call site.

use proc_macro::TokenStream;

mod runner_macros;

/// Fan a fuzz test out into one `#[tokio::test]` per case, injecting a seeded `FuzzRunner` shell.
///
/// ```ignore
/// #[fuzz_runner(harness = CounterHarness, seed = 7, cases = 64)]
/// async fn counter_fuzz(#[runner] mut r: FuzzRunner<CounterHarness>) {
///     let (ctx, world) = counter_setup(r.seed()).await.expect("setup");
///     r.setup(ctx, world);
///     let report = r.run(25, None, 1).await;
///     assert!(report.passed(), "{:?}", report.failure);
/// }
/// ```
///
/// Emits `counter_fuzz_case_0` .. `counter_fuzz_case_63`, each seeded by `sub_seed(7, i)`. The
/// developer writes setup in the body; see the `runner_macros` module for the attribute keys and the scope
/// requirement (`use cross_vm_framework::prelude::*`).
#[proc_macro_attribute]
pub fn fuzz_runner(attr: TokenStream, item: TokenStream) -> TokenStream {
    match runner_macros::expand(runner_macros::Mode::Fuzz, attr.into(), item.into()) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Emit one `#[tokio::test]` that injects a seeded `InvariantRunner` shell.
///
/// ```ignore
/// #[invariant_runner(harness = vault_harness(), seed = 42)]
/// async fn vault_invariant(#[runner] mut r: InvariantRunner<OpSetHarness<Ctx, VaultWorld>>) {
///     let (ctx, world) = vault_setup(r.seed()).await.expect("setup");
///     r.setup(ctx, world);
///     let report = r.run(120, None, 1).await;
///     assert!(report.passed(), "{:?}", report.failure);
/// }
/// ```
#[proc_macro_attribute]
pub fn invariant_runner(attr: TokenStream, item: TokenStream) -> TokenStream {
    match runner_macros::expand(runner_macros::Mode::Invariant, attr.into(), item.into()) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Emit one `#[tokio::test]` that injects a seeded `EnduranceRunner` shell.
///
/// ```ignore
/// #[endurance_runner(harness = CounterHarness, seed = 1)]
/// async fn counter_endurance(#[runner] mut r: EnduranceRunner<CounterHarness>) {
///     let (ctx, world) = counter_setup(r.seed()).await.expect("setup");
///     r.setup(ctx, world);
///     let report = r.run(EnduranceConfig::new(Duration::from_millis(50)).check_every(5)).await;
///     assert!(report.passed(), "{:?}", report.failure);
/// }
/// ```
#[proc_macro_attribute]
pub fn endurance_runner(attr: TokenStream, item: TokenStream) -> TokenStream {
    match runner_macros::expand(runner_macros::Mode::Endurance, attr.into(), item.into()) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

#[cfg(test)]
mod runner_macro_tests {
    use super::runner_macros::{self, Mode};
    use quote::quote;

    #[test]
    fn invariant_runner_expands_single_test() {
        let attr = quote! { harness = CounterHarness, seed = 7 };
        let item = quote! {
            async fn counter_invariant_mode(#[runner] mut r: InvariantRunner<CounterHarness>) {
                let _ = r.seed();
            }
        };
        let out = runner_macros::expand(Mode::Invariant, attr, item).expect("expand");
        let s = out.to_string();
        assert!(s.contains("tokio :: test"), "{s}");
        assert!(s.contains("Runner :: invariant"), "{s}");
        assert!(s.contains("counter_invariant_mode"), "{s}");
        assert!(!s.contains("#[runner]"), "{s}");
    }

    #[test]
    fn fuzz_runner_fans_out_and_strips_runner_attr() {
        let attr = quote! { harness = CounterHarness, seed = 7, cases = 2 };
        let item = quote! {
            async fn counter_fuzz(#[runner] mut r: FuzzRunner<CounterHarness>) {
                let _ = r.seed();
            }
        };
        let out = runner_macros::expand(Mode::Fuzz, attr, item).expect("expand");
        let s = out.to_string();
        assert!(s.contains("counter_fuzz_case_0"), "{s}");
        assert!(s.contains("counter_fuzz_case_1"), "{s}");
        assert!(s.contains("sub_seed"), "{s}");
    }

    #[test]
    fn random_seed_emits_once_lock_for_fuzz_fan_out() {
        let attr = quote! { harness = CounterHarness, seed = -1, cases = 2 };
        let item = quote! {
            async fn counter_fuzz(#[runner] mut r: FuzzRunner<CounterHarness>) {}
        };
        let out = runner_macros::expand(Mode::Fuzz, attr, item).expect("expand");
        let s = out.to_string();
        assert!(s.contains("OnceLock"), "{s}");
        assert!(s.contains("random_seed"), "{s}");
    }
}
