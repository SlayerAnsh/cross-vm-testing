//! Attribute macros for `harness-core`. The emitted code names `Runner`, `sub_seed`, and
//! `random_seed` unqualified, so bring them into scope (e.g. `use harness_core::*` or the
//! consuming framework's prelude) at the call site.

use proc_macro::TokenStream;

mod op_doc;
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

/// Harvest doc comments off an op's param struct into runtime-introspectable help strings.
///
/// Generates an inherent impl with `field_docs() -> Vec<(&'static str, &'static str)>` (one
/// `(field_name, doc)` pair per documented named field; undocumented fields are omitted) and
/// `doc() -> Option<&'static str>` (the struct-level doc comment, joined, or `None`). This lets a
/// harness feed `OpDef::with_help(description, field_docs)` from the struct itself instead of
/// hand-typed strings.
///
/// ```ignore
/// #[derive(OpParamsDoc)]
/// /// Move funds between two accounts.
/// struct TransferParams {
///     /// Source account id.
///     from: String,
///     /// Destination account id.
///     to: String,
///     amount: u128, // undocumented -> not in field_docs()
/// }
/// // TransferParams::doc()        == Some("Move funds between two accounts.")
/// // TransferParams::field_docs() == vec![("from", "Source account id."),
/// //                                       ("to",   "Destination account id.")]
/// ```
#[proc_macro_derive(OpParamsDoc)]
pub fn op_doc(input: TokenStream) -> TokenStream {
    match op_doc::expand(input.into()) {
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

#[cfg(test)]
mod op_doc_tests {
    use super::op_doc;
    use quote::quote;

    #[test]
    fn field_and_struct_docs_are_harvested() {
        // Multi-line field doc exercises the join-and-trim path (`///` -> several `#[doc]` attrs).
        let input = quote! {
            /// Move funds between two accounts.
            struct TransferParams {
                /// Source account id.
                from: String,
                /// Destination account id.
                /// Second line of the same field.
                to: String,
                amount: u128,
            }
        };
        let out = op_doc::expand(input).expect("expand");
        let s = out.to_string();

        // Both associated fns are emitted on the right type.
        assert!(s.contains("impl TransferParams"), "{s}");
        assert!(s.contains("fn field_docs"), "{s}");
        assert!(s.contains("fn doc"), "{s}");

        // Struct-level doc harvested.
        assert!(
            s.contains(r#"Some ("Move funds between two accounts.")"#),
            "{s}"
        );

        // Field names appear as string literals, paired with their trimmed docs.
        assert!(s.contains(r#"("from" , "Source account id.")"#), "{s}");
        // Multi-line field doc joined with a single space.
        assert!(
            s.contains(r#"("to" , "Destination account id. Second line of the same field.")"#),
            "{s}"
        );

        // Undocumented field is omitted entirely.
        assert!(!s.contains("\"amount\""), "{s}");
    }

    #[test]
    fn undocumented_struct_emits_empty_table_and_none() {
        let input = quote! {
            struct Bare {
                a: u32,
                b: u32,
            }
        };
        let out = op_doc::expand(input).expect("expand");
        let s = out.to_string();
        assert!(s.contains("impl Bare"), "{s}");
        // No field docs -> empty vec literal; struct doc -> None.
        assert!(s.contains("None"), "{s}");
        assert!(!s.contains("\"a\""), "{s}");
        assert!(!s.contains("\"b\""), "{s}");
    }

    #[test]
    fn enum_input_is_an_error_not_a_panic() {
        let input = quote! {
            enum NotAStruct { A, B }
        };
        assert!(op_doc::expand(input).is_err());
    }

    #[test]
    fn tuple_struct_is_an_error_not_a_panic() {
        let input = quote! {
            struct Tuple(u32, u32);
        };
        assert!(op_doc::expand(input).is_err());
    }
}
