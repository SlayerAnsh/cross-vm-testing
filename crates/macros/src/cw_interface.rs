//! `cross_vm_cw_interface!`: declare a zero-sized marker + `CwInterface` impl for one contract.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{parse::Parse, parse::ParseStream, Ident, Token};

struct InterfaceInput {
    vis: syn::Visibility,
    name: Ident,
    init: Ident,
    exec: Ident,
    query: Ident,
}

impl Parse for InterfaceInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let vis: syn::Visibility = input.parse()?;
        let name: Ident = input.parse()?;
        input.parse::<Token![,]>()?;
        let init: Ident = input.parse()?;
        input.parse::<Token![,]>()?;
        let exec: Ident = input.parse()?;
        input.parse::<Token![,]>()?;
        let query: Ident = input.parse()?;
        Ok(Self {
            vis,
            name,
            init,
            exec,
            query,
        })
    }
}

/// Expand `cross_vm_cw_interface!(pub CounterContract, InstantiateMsg, ExecuteMsg, QueryMsg)`.
pub fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    let InterfaceInput {
        vis,
        name,
        init,
        exec,
        query,
    } = syn::parse2(input)?;

    Ok(quote! {
        #[doc = concat!("Zero-sized marker for typed `CwContract<", stringify!(#name), ">` handles.")]
        #vis struct #name;

        impl ::cross_vm_cosmwasm::CwInterface for #name {
            type InstantiateMsg = #init;
            type ExecuteMsg = #exec;
            type QueryMsg = #query;
        }
    })
}
