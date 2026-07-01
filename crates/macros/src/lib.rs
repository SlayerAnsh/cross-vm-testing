//! Proc-macros for the cross-vm testing framework.
//!
//! [`cross_vm_contract`] turns a trait of logical-method signatures into a cross-VM contract
//! wrapper: a struct holding a `ContractBase`, constructors, hook forwarders, and a dispatcher
//! per method that matches the chain's VM and calls the matching `cw_*` / `evm_*` / `svm_*` hook
//! the developer writes.
//!
//! The generated code names framework types (`ContractBase`, `AnyChain`, `Account`,
//! `ChainKind`, `AppResponse`, `BeforeContext`, `HookContext`, `CrossVmError`) unqualified, so
//! the invocation site must have `use cross_vm_framework::prelude::*;` in scope (the same
//! requirement the hand-written wrappers already had).

use proc_macro::TokenStream;
use quote::quote;
use syn::{
    parse_macro_input, DeriveInput, FnArg, Ident, ItemTrait, Pat, ReturnType, TraitItem, Type,
};

mod cw_fns;
mod runner_macros;
mod wallet_roster;

/// Generate a cross-VM contract wrapper from a trait of logical-method signatures.
///
/// `#[cross_vm_contract(Name)]` on `trait Spec { .. }` emits:
/// - `trait Spec` with each method's return type rewritten to `Result<Ok, CrossVmError>`;
/// - `struct Name { base: ContractBase }` with `new` / `instance` constructors and
///   `on_before` / `on_after` forwarders;
/// - `impl Spec for Name` whose methods dispatch on the chain's VM to `cw_*` / `evm_*` / `svm_*`.
///
/// A method whose declared Ok type is `AppResponse<_>` is wrapped in `run_before`/`run_after`;
/// any other return type is a plain dispatch. The developer writes the per-VM `cw_*`/`evm_*`/
/// `svm_*` hooks in a sibling `impl Name` block.
#[proc_macro_attribute]
pub fn cross_vm_contract(args: TokenStream, item: TokenStream) -> TokenStream {
    let struct_name = parse_macro_input!(args as Ident);
    let input = parse_macro_input!(item as ItemTrait);
    match expand(struct_name, input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Generate typed, per-variant execute methods from a CosmWasm `ExecuteMsg` enum.
///
/// `#[derive(CwExecuteFns)]` emits a `pub trait <Name>Fns` and `impl <Name>Fns for CwContract`,
/// one `async fn` per variant (snake_cased; named fields become args). A variant marked
/// `#[payable]` gains a trailing `funds: &[Coin]` arg routed through `execute_with_funds`.
#[proc_macro_derive(CwExecuteFns, attributes(payable))]
pub fn derive_cw_execute_fns(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    match cw_fns::expand_execute_fns(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Define a compile-time wallet roster and typed `WalletLabel` fields.
///
/// See the `wallet_roster` module for the input DSL.
#[proc_macro]
pub fn define_wallet_roster(input: TokenStream) -> TokenStream {
    wallet_roster::expand(input.into()).into()
}

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
/// #[invariant_runner(harness = VaultHarness, seed = 42)]
/// async fn vault_invariant(#[runner] mut r: InvariantRunner<VaultHarness>) {
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

/// Generate typed, per-variant query methods from a CosmWasm `QueryMsg` enum.
///
/// `#[derive(CwQueryFns)]` emits a `pub trait <Name>Fns` and `impl <Name>Fns for CwContract`,
/// one `async fn` per variant returning the variant's `#[returns(T)]` type. Every variant must
/// carry `#[returns(T)]`.
#[proc_macro_derive(CwQueryFns, attributes(returns))]
pub fn derive_cw_query_fns(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    match cw_fns::expand_query_fns(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Whether `attrs` already contain a doc comment (so the macro only fills gaps and never
/// duplicates developer-written docs).
fn has_doc(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("doc"))
}

fn expand(struct_name: Ident, input: ItemTrait) -> syn::Result<proc_macro2::TokenStream> {
    let vis = &input.vis;
    let trait_name = &input.ident;
    let trait_attrs = &input.attrs;
    // Generated public items must satisfy `missing_docs` in the caller's crate: keep the
    // developer's docs when present, fill in a generated line when absent.
    let trait_doc_fallback = if has_doc(trait_attrs) {
        quote! {}
    } else {
        let doc = format!(
            "Cross-VM spec: each method dispatches to the active VM's `{struct_name}` hook."
        );
        quote! { #[doc = #doc] }
    };

    let mut trait_methods = Vec::new();
    let mut dispatchers = Vec::new();
    let mut hook_methods = Vec::new();

    for it in &input.items {
        let m = match it {
            TraitItem::Fn(f) => f,
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "cross_vm_contract: the spec trait may only contain methods",
                ))
            }
        };
        let sig = &m.sig;

        if !sig.generics.params.is_empty() {
            return Err(syn::Error::new_spanned(
                &sig.generics,
                "cross_vm_contract: logical methods may not declare generic parameters (v1)",
            ));
        }

        let name = &sig.ident;
        let attrs = &m.attrs;

        let mut typed_args = Vec::new();
        let mut arg_names = Vec::new();
        let mut has_receiver = false;
        for inp in &sig.inputs {
            match inp {
                FnArg::Receiver(_) => has_receiver = true,
                FnArg::Typed(pt) => {
                    typed_args.push(quote! { #pt });
                    match &*pt.pat {
                        Pat::Ident(pi) => {
                            let id = &pi.ident;
                            arg_names.push(quote! { #id });
                        }
                        other => {
                            return Err(syn::Error::new_spanned(
                                other,
                                "cross_vm_contract: method arguments must be simple identifiers",
                            ))
                        }
                    }
                }
            }
        }
        if !has_receiver {
            return Err(syn::Error::new_spanned(
                sig,
                "cross_vm_contract: methods must take `&self`",
            ));
        }

        let ok_ty: Type = match &sig.output {
            ReturnType::Default => syn::parse_quote! { () },
            ReturnType::Type(_, t) => (**t).clone(),
        };
        let hooked = is_app_response(&ok_ty);

        let cw = Ident::new(&format!("cw_{name}"), name.span());
        let evm = Ident::new(&format!("evm_{name}"), name.span());
        let svm = Ident::new(&format!("svm_{name}"), name.span());
        let tron = Ident::new(&format!("tron_{name}"), name.span());
        let label = name.to_string();

        let method_doc_fallback = if has_doc(attrs) {
            quote! {}
        } else {
            let doc = format!("Dispatches `{name}` to the active VM's hook.");
            quote! { #[doc = #doc] }
        };
        trait_methods.push(quote! {
            #method_doc_fallback
            #(#attrs)*
            async fn #name(&self #(, #typed_args)*) -> Result<#ok_ty, CrossVmError>;
        });

        // Each per-VM hook gets a default body that panics if the VM is actually dispatched.
        // A hand-written inherent `cw_*`/`evm_*`/`svm_*`/`tron_*` shadows the default (inherent
        // methods win over trait methods), so a contract only needs to implement the VMs it runs.
        for hook in [&cw, &evm, &svm, &tron] {
            let hook_doc = format!("Per-VM hook for `{name}`; the default panics if dispatched.");
            hook_methods.push(quote! {
                #[doc = #hook_doc]
                async fn #hook(&self #(, #typed_args)*) -> Result<#ok_ty, CrossVmError> {
                    unimplemented!(concat!(stringify!(#hook), " is not implemented for this contract"))
                }
            });
        }

        let body = if hooked {
            quote! {
                self.base.run_before(#label)?;
                let resp = match self.base.kind() {
                    ChainKind::CosmWasm => self.#cw(#(#arg_names),*).await?,
                    ChainKind::Evm => self.#evm(#(#arg_names),*).await?,
                    ChainKind::Svm => self.#svm(#(#arg_names),*).await?,
                    ChainKind::Tron => self.#tron(#(#arg_names),*).await?,
                };
                self.base.run_after(#label, resp)
            }
        } else {
            quote! {
                match self.base.kind() {
                    ChainKind::CosmWasm => self.#cw(#(#arg_names),*).await,
                    ChainKind::Evm => self.#evm(#(#arg_names),*).await,
                    ChainKind::Svm => self.#svm(#(#arg_names),*).await,
                    ChainKind::Tron => self.#tron(#(#arg_names),*).await,
                }
            }
        };

        dispatchers.push(quote! {
            async fn #name(&self #(, #typed_args)*) -> Result<#ok_ty, CrossVmError> {
                #body
            }
        });
    }

    let hooks_name = Ident::new(&format!("{struct_name}Hooks"), struct_name.span());

    let struct_doc = format!(
        "Cross-VM `{trait_name}` wrapper generated by `#[cross_vm_contract]`; holds the chain \
         handle and deployed address, and dispatches each spec method to the active VM's hook."
    );

    Ok(quote! {
        #trait_doc_fallback
        #(#trait_attrs)*
        #[allow(async_fn_in_trait)]
        #vis trait #trait_name {
            #(#trait_methods)*
        }

        /// Per-VM hooks for the wrapper, each defaulting to `unimplemented!()`.
        ///
        /// The developer overrides the VMs they target with inherent `cw_*`/`evm_*`/`svm_*`/
        /// `tron_*` methods (inherent methods shadow these defaults); any VM left unimplemented
        /// panics only if that chain is actually dispatched.
        #[allow(async_fn_in_trait, unused_variables)]
        #vis trait #hooks_name {
            #(#hook_methods)*
        }

        impl #hooks_name for #struct_name {}

        #[doc = #struct_doc]
        #vis struct #struct_name {
            base: ContractBase,
        }

        impl #struct_name {
            /// A contract not yet deployed on `chain`. Call the spec's `setup` to deploy.
            pub fn new(chain: AnyChain) -> Self {
                Self { base: ContractBase::new(chain) }
            }
            /// A contract attached to an already-deployed instance at `address`.
            pub fn instance(chain: AnyChain, address: Account) -> Self {
                Self { base: ContractBase::with_address(chain, address) }
            }
            /// The deployed instance address, if `setup` (or `instance`) has bound one.
            ///
            /// Lets a caller capture where a wrapper deployed and later rebuild a fresh handle
            /// for the same instance with [`Self::instance`].
            pub fn address(&self) -> Option<Account> {
                self.base.address()
            }
            /// Register a before-hook (forwards to the shared `ContractBase`).
            pub fn on_before(
                &self,
                f: impl FnMut(&BeforeContext) -> Result<(), CrossVmError> + 'static,
            ) {
                self.base.on_before(f);
            }
            /// Register an after-hook (forwards to the shared `ContractBase`).
            pub fn on_after(
                &self,
                f: impl FnMut(&HookContext) -> Result<(), CrossVmError> + 'static,
            ) {
                self.base.on_after(f);
            }
        }

        impl #trait_name for #struct_name {
            #(#dispatchers)*
        }
    })
}

/// Whether `ty` is an `AppResponse<_>` (matched by the last path segment's identifier).
fn is_app_response(ty: &Type) -> bool {
    if let Type::Path(tp) = ty {
        if let Some(seg) = tp.path.segments.last() {
            return seg.ident == "AppResponse";
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::format_ident;

    fn expand_str(struct_name: &str, trait_src: &str) -> syn::Result<String> {
        let id = format_ident!("{}", struct_name);
        let it: ItemTrait = syn::parse_str(trait_src)?;
        expand(id, it).map(|ts| ts.to_string())
    }

    #[test]
    fn emits_struct_trait_constructors_and_dispatchers() {
        let out = expand_str(
            "Counter",
            r#"
            pub trait CounterSpec {
                async fn setup(&self, wallet: &str);
                async fn increment(&self, wallet: &str) -> AppResponse<()>;
                async fn count(&self) -> u64;
            }
            "#,
        )
        .expect("expand");

        assert!(out.contains("struct Counter"));
        assert!(out.contains("impl CounterSpec for Counter"));
        assert!(out.contains("fn new"));
        assert!(out.contains("fn instance"));
        assert!(out.contains("fn on_before"));
        assert!(out.contains("fn on_after"));
        // Per-VM hook calls for the count query trio.
        assert!(out.contains("cw_count"));
        assert!(out.contains("evm_count"));
        assert!(out.contains("svm_count"));
        assert!(out.contains("tron_count"));
    }

    #[test]
    fn emits_hooks_trait_with_unimplemented_defaults() {
        let out = expand_str(
            "Counter",
            r#"
            pub trait CounterSpec {
                async fn increment(&self, wallet: &str) -> AppResponse<()>;
                async fn count(&self) -> u64;
            }
            "#,
        )
        .expect("expand");

        // A default-hook trait plus a blanket impl are emitted next to the wrapper.
        assert!(out.contains("trait CounterHooks"));
        assert!(out.contains("impl CounterHooks for Counter"));
        // Every hook defaults to `unimplemented!()` so partial impls still compile.
        assert!(out.contains("unimplemented"));
        // Four VMs x two methods = eight default hook bodies, each with the panic message.
        assert_eq!(
            out.matches("is not implemented for this contract").count(),
            8
        );
    }

    #[test]
    fn app_response_method_is_hooked_query_is_not() {
        // A trait with exactly one hooked method and one plain method, so the count of
        // run_before/run_after occurrences pins which got hooks.
        let hooked = expand_str(
            "C",
            r#"trait S { async fn increment(&self, w: &str) -> AppResponse<()>; }"#,
        )
        .unwrap();
        assert!(hooked.contains("run_before"));
        assert!(hooked.contains("run_after"));

        let plain = expand_str("C", r#"trait S { async fn count(&self) -> u64; }"#).unwrap();
        assert!(!plain.contains("run_before"));
        assert!(!plain.contains("run_after"));
    }

    #[test]
    fn rejects_method_generics() {
        let err = expand_str("C", r#"trait S { async fn foo<T>(&self, x: T); }"#).unwrap_err();
        assert!(err.to_string().contains("generic"), "{err}");
    }

    #[test]
    fn rejects_missing_receiver() {
        let err = expand_str("C", r#"trait S { async fn foo(wallet: &str); }"#).unwrap_err();
        assert!(err.to_string().contains("&self"), "{err}");
    }

    #[test]
    fn is_app_response_matches_only_app_response() {
        let yes: Type = syn::parse_str("AppResponse<()>").unwrap();
        let also: Type = syn::parse_str("framework::AppResponse<u64>").unwrap();
        let no: Type = syn::parse_str("u64").unwrap();
        assert!(is_app_response(&yes));
        assert!(is_app_response(&also));
        assert!(!is_app_response(&no));
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
