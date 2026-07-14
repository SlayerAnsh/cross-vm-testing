//! CosmWasm typed message-handle derives: `CwExecuteFns` and `CwQueryFns`.
//!
//! Both run on an `ExecuteMsg` / `QueryMsg` enum in the contract crate and generate a local
//! `pub trait <Name>Fns` plus an `impl` of it for `::cross_vm_cosmwasm::CwContract<I>` where
//! `I: CwInterface<ExecuteMsg = ThisEnum>` (or `QueryMsg = ThisEnum`), one method
//! per variant. Generated code uses absolute paths (`::cross_vm_cosmwasm::*`,
//! `::cosmwasm_std::Coin`) so the contract crate needs no imports beyond the deps it gains under
//! its `cross-vm` feature.
//!
//! The generated trait is named `<EnumName>Fns` by default. An enum-level
//! `#[cross_vm(trait_name = "...")]` attribute overrides that, which avoids a name clash when the
//! same enum also derives cw-orch's `ExecuteFns` / `QueryFns` (both would otherwise emit an
//! `ExecuteMsgFns` / `QueryMsgFns` trait in the same module).

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DataEnum, DeriveInput, Fields, Ident, LitStr, Type, Variant};

/// Build the execute-side trait + impl from an `ExecuteMsg` enum.
pub fn expand_execute_fns(input: DeriveInput) -> syn::Result<TokenStream> {
    let enum_ident = &input.ident;
    let trait_ident = trait_ident(&input)?;

    let mut sigs = Vec::new();
    let mut methods = Vec::new();
    for v in &enum_data(&input)?.variants {
        let method = snake_ident(&v.ident);
        let (arg_decls, field_idents) = variant_fields(v)?;
        let ctor = variant_ctor(enum_ident, v, &field_idents);

        // Every generated method is a mutating op, so each takes a trailing, required
        // `CwGasLimit` (last, after `funds` on a `#[payable]` variant): the underlying
        // `CwContract::execute` / `execute_with_funds` have no default to fall back on.
        if is_payable(v) {
            sigs.push(quote! {
                async fn #method(&self, wallet: &str #(, #arg_decls)*, funds: &[::cosmwasm_std::Coin], gas: ::cross_vm_cosmwasm::CwGasLimit)
                    -> Result<::cross_vm_cosmwasm::CwExecution, ::cross_vm_cosmwasm::CwError>;
            });
            methods.push(quote! {
                async fn #method(&self, wallet: &str #(, #arg_decls)*, funds: &[::cosmwasm_std::Coin], gas: ::cross_vm_cosmwasm::CwGasLimit)
                    -> Result<::cross_vm_cosmwasm::CwExecution, ::cross_vm_cosmwasm::CwError> {
                    self.execute_with_funds(#ctor, wallet, funds, gas).await
                }
            });
        } else {
            sigs.push(quote! {
                async fn #method(&self, wallet: &str #(, #arg_decls)*, gas: ::cross_vm_cosmwasm::CwGasLimit)
                    -> Result<::cross_vm_cosmwasm::CwExecution, ::cross_vm_cosmwasm::CwError>;
            });
            methods.push(quote! {
                async fn #method(&self, wallet: &str #(, #arg_decls)*, gas: ::cross_vm_cosmwasm::CwGasLimit)
                    -> Result<::cross_vm_cosmwasm::CwExecution, ::cross_vm_cosmwasm::CwError> {
                    self.execute(#ctor, wallet, gas).await
                }
            });
        }
    }

    Ok(quote! {
        #[allow(async_fn_in_trait)]
        pub trait #trait_ident {
            #(#sigs)*
        }
        impl<I: ::cross_vm_cosmwasm::CwInterface<ExecuteMsg = #enum_ident>> #trait_ident
            for ::cross_vm_cosmwasm::CwContract<I> {
            #(#methods)*
        }
    })
}

/// Build the query-side trait + impl from a `QueryMsg` enum (each variant needs `#[returns(T)]`).
pub fn expand_query_fns(input: DeriveInput) -> syn::Result<TokenStream> {
    let enum_ident = &input.ident;
    let trait_ident = trait_ident(&input)?;

    let mut sigs = Vec::new();
    let mut methods = Vec::new();
    for v in &enum_data(&input)?.variants {
        let method = snake_ident(&v.ident);
        let ret = returns_type(v)?;
        let (arg_decls, field_idents) = variant_fields(v)?;
        let ctor = variant_ctor(enum_ident, v, &field_idents);

        sigs.push(quote! {
            async fn #method(&self #(, #arg_decls)*)
                -> Result<#ret, ::cross_vm_cosmwasm::CwError>;
        });
        methods.push(quote! {
            async fn #method(&self #(, #arg_decls)*)
                -> Result<#ret, ::cross_vm_cosmwasm::CwError> {
                self.query(#ctor).await
            }
        });
    }

    Ok(quote! {
        #[allow(async_fn_in_trait)]
        pub trait #trait_ident {
            #(#sigs)*
        }
        impl<I: ::cross_vm_cosmwasm::CwInterface<QueryMsg = #enum_ident>> #trait_ident
            for ::cross_vm_cosmwasm::CwContract<I> {
            #(#methods)*
        }
    })
}

/// The name of the generated `Fns` trait: `<EnumName>Fns` by default, or the value of an
/// enum-level `#[cross_vm(trait_name = "...")]` when present. Renaming lets the enum coexist with
/// cw-orch's `ExecuteFns` / `QueryFns` derives, which claim the default `<EnumName>Fns` name.
fn trait_ident(input: &DeriveInput) -> syn::Result<Ident> {
    let mut name: Option<Ident> = None;
    for attr in &input.attrs {
        if !attr.path().is_ident("cross_vm") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("trait_name") {
                let lit: LitStr = meta.value()?.parse()?;
                name = Some(Ident::new(&lit.value(), lit.span()));
                Ok(())
            } else {
                Err(meta.error("unknown `cross_vm` option; expected `trait_name = \"...\"`"))
            }
        })?;
    }
    Ok(name.unwrap_or_else(|| format_ident!("{}Fns", input.ident)))
}

fn enum_data(input: &DeriveInput) -> syn::Result<&DataEnum> {
    match &input.data {
        Data::Enum(e) => Ok(e),
        _ => Err(syn::Error::new_spanned(
            &input.ident,
            "CwExecuteFns / CwQueryFns can only be derived on an enum",
        )),
    }
}

/// `(method-arg declarations, field idents)` for a variant. Named fields become args; tuple
/// fields become positional `arg0`, `arg1`, ... args (cw-orch style); a unit variant has none.
fn variant_fields(v: &Variant) -> syn::Result<(Vec<TokenStream>, Vec<Ident>)> {
    match &v.fields {
        Fields::Named(named) => {
            let mut decls = Vec::new();
            let mut idents = Vec::new();
            for f in &named.named {
                let id = f.ident.as_ref().expect("named field has an ident").clone();
                let ty = &f.ty;
                decls.push(quote! { #id: #ty });
                idents.push(id);
            }
            Ok((decls, idents))
        }
        Fields::Unnamed(unnamed) => {
            let mut decls = Vec::new();
            let mut idents = Vec::new();
            for (i, f) in unnamed.unnamed.iter().enumerate() {
                let id = format_ident!("arg{}", i);
                let ty = &f.ty;
                decls.push(quote! { #id: #ty });
                idents.push(id);
            }
            Ok((decls, idents))
        }
        Fields::Unit => Ok((Vec::new(), Vec::new())),
    }
}

/// The expression that constructs this variant, e.g. `ExecuteMsg::Deposit { amount }` or
/// `ExecuteMsg::ManageFactoryState(arg0)`.
fn variant_ctor(enum_ident: &Ident, v: &Variant, field_idents: &[Ident]) -> TokenStream {
    let var = &v.ident;
    match &v.fields {
        Fields::Named(_) => quote! { #enum_ident::#var { #(#field_idents),* } },
        Fields::Unnamed(_) => quote! { #enum_ident::#var(#(#field_idents),*) },
        Fields::Unit => quote! { #enum_ident::#var },
    }
}

fn is_payable(v: &Variant) -> bool {
    v.attrs.iter().any(|a| a.path().is_ident("payable"))
}

/// The `T` from a variant's `#[returns(T)]`, or an error if it is missing.
fn returns_type(v: &Variant) -> syn::Result<Type> {
    for a in &v.attrs {
        if a.path().is_ident("returns") {
            return a.parse_args::<Type>();
        }
    }
    Err(syn::Error::new_spanned(
        v,
        format!("query variant `{}` is missing #[returns(T)]", v.ident),
    ))
}

fn snake_ident(id: &Ident) -> Ident {
    format_ident!("{}", to_snake(&id.to_string()), span = id.span())
}

fn to_snake(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.char_indices() {
        if ch.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec(src: &str) -> syn::Result<String> {
        expand_execute_fns(syn::parse_str(src)?).map(|t| t.to_string())
    }
    fn query(src: &str) -> syn::Result<String> {
        expand_query_fns(syn::parse_str(src)?).map(|t| t.to_string())
    }

    #[test]
    fn execute_one_method_per_variant_snake_cased() {
        let out = exec("enum ExecuteMsg { Increment {}, Reset {} }").unwrap();
        assert!(out.contains("trait ExecuteMsgFns"));
        assert!(out.contains("for :: cross_vm_cosmwasm :: CwContract < I >"));
        assert!(out.contains("CwInterface < ExecuteMsg = ExecuteMsg >"));
        assert!(out.contains("fn increment"));
        assert!(out.contains("fn reset"));
        // Non-payable: no funds and the plain execute path.
        assert!(!out.contains("funds"));
        assert!(!out.contains("execute_with_funds"));
        // Mutating: the gas limit is required, never defaulted.
        assert!(out.contains("gas : :: cross_vm_cosmwasm :: CwGasLimit"));
        assert!(out.contains("self . execute (ExecuteMsg :: Increment { } , wallet , gas)"));
    }

    #[test]
    fn execute_named_fields_become_args() {
        let out = exec("enum ExecuteMsg { Deposit { amount: Uint128 } }").unwrap();
        assert!(out.contains("fn deposit"));
        assert!(out.contains("amount"));
        assert!(out.contains("Uint128"));
    }

    #[test]
    fn payable_variant_takes_funds() {
        let out = exec("enum ExecuteMsg { #[payable] Deposit { amount: Uint128 } }").unwrap();
        assert!(out.contains("funds"));
        assert!(out.contains("execute_with_funds"));
        assert!(out.contains("Coin"));
        // The gas limit trails `funds`, and both reach the funded execute path.
        assert!(out.contains(
            "self . execute_with_funds (ExecuteMsg :: Deposit { amount } , wallet , funds , gas)"
        ));
    }

    #[test]
    fn query_uses_returns_type_and_no_wallet() {
        let out = query("enum QueryMsg { #[returns(CountResponse)] GetCount {} }").unwrap();
        assert!(out.contains("trait QueryMsgFns"));
        assert!(out.contains("for :: cross_vm_cosmwasm :: CwContract < I >"));
        assert!(out.contains("CwInterface < QueryMsg = QueryMsg >"));
        assert!(out.contains("fn get_count"));
        assert!(out.contains("CountResponse"));
        // Queries are unsigned: no wallet arg.
        assert!(!out.contains("wallet"));
    }

    #[test]
    fn query_missing_returns_is_an_error() {
        let err = query("enum QueryMsg { GetCount {} }").unwrap_err();
        assert!(err.to_string().contains("returns"), "{err}");
    }

    #[test]
    fn execute_single_field_tuple_variant() {
        let out = exec("enum ExecuteMsg { ManageFactoryState(ManageFactoryState) }").unwrap();
        assert!(out.contains("fn manage_factory_state"));
        assert!(out.contains("arg0"));
        assert!(out.contains("ManageFactoryState"));
        assert!(out.contains("ManageFactoryState (arg0)"));
    }

    #[test]
    fn execute_multi_field_tuple_variant() {
        let out = exec("enum ExecuteMsg { Pair(u64, String) }").unwrap();
        assert!(out.contains("fn pair"));
        assert!(out.contains("arg0"));
        assert!(out.contains("arg1"));
        assert!(out.contains("Pair (arg0 , arg1)"));
    }

    #[test]
    fn query_tuple_variant() {
        let out =
            query("enum QueryMsg { #[returns(CountResponse)] GetCount(CountRequest) }").unwrap();
        assert!(out.contains("fn get_count"));
        assert!(out.contains("arg0"));
        assert!(out.contains("CountRequest"));
        assert!(out.contains("CountResponse"));
        assert!(!out.contains("wallet"));
    }

    #[test]
    fn execute_custom_trait_name() {
        let out = exec(
            r#"#[cross_vm(trait_name = "CrossVmExecuteFns")] enum ExecuteMsg { Increment {} }"#,
        )
        .unwrap();
        assert!(out.contains("trait CrossVmExecuteFns"));
        assert!(out.contains("CwInterface < ExecuteMsg = ExecuteMsg >"));
        assert!(out.contains("CrossVmExecuteFns for :: cross_vm_cosmwasm :: CwContract < I >"));
        // The default name must not leak so it can coexist with cw-orch's `ExecuteMsgFns`.
        assert!(!out.contains("trait ExecuteMsgFns"));
    }

    #[test]
    fn query_custom_trait_name() {
        let out = query(
            r#"#[cross_vm(trait_name = "CrossVmQueryFns")] enum QueryMsg { #[returns(CountResponse)] GetCount {} }"#,
        )
        .unwrap();
        assert!(out.contains("trait CrossVmQueryFns"));
        assert!(!out.contains("trait QueryMsgFns"));
    }

    #[test]
    fn unknown_cross_vm_option_is_an_error() {
        let err = exec(r#"#[cross_vm(bogus = "x")] enum ExecuteMsg { Increment {} }"#).unwrap_err();
        assert!(err.to_string().contains("cross_vm"), "{err}");
    }

    #[test]
    fn non_enum_is_rejected() {
        let err = exec("struct ExecuteMsg { x: u64 }").unwrap_err();
        assert!(err.to_string().contains("enum"), "{err}");
    }
}
