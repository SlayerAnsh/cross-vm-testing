//! `define_wallet_roster!` — compile-time wallet roster + typed `WalletLabel` fields.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    parse::{Parse, ParseStream},
    Attribute, Ident, LitInt, LitStr, Token, Visibility,
};

struct RosterInput {
    vis: Visibility,
    const_name: Ident,
    struct_name: Ident,
    entries: Vec<RosterEntry>,
}

struct RosterEntry {
    attrs: Vec<Attribute>,
    field_name: Ident,
    source: SourceKind,
    index: u32,
}

enum SourceKind {
    EnvMnemonic(LitStr),
    Auto,
    EnvPrivateKey(LitStr),
}

impl Parse for RosterInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let vis: Visibility = input.parse()?;
        let _: Token![const] = input.parse()?;
        let const_name: Ident = input.parse()?;
        let _: Token![:] = input.parse()?;
        let struct_name: Ident = input.parse()?;
        let _: Token![=] = input.parse()?;

        let content;
        syn::braced!(content in input);

        let mut entries = Vec::new();
        while !content.is_empty() {
            let attrs = Attribute::parse_outer(&content)?;
            let field_name: Ident = content.parse()?;
            let _: Token![:] = content.parse()?;

            let source = if content.peek(Ident) {
                let kind: Ident = content.parse()?;
                if kind == "env_mnemonic" {
                    let env_args;
                    syn::parenthesized!(env_args in content);
                    let var: LitStr = env_args.parse()?;
                    SourceKind::EnvMnemonic(var)
                } else if kind == "env_private_key" {
                    let env_args;
                    syn::parenthesized!(env_args in content);
                    let var: LitStr = env_args.parse()?;
                    SourceKind::EnvPrivateKey(var)
                } else if kind == "auto" {
                    SourceKind::Auto
                } else {
                    return Err(syn::Error::new(
                        kind.span(),
                        "define_wallet_roster: expected `env_mnemonic(\"VAR\")`, `auto`, or `env_private_key(\"VAR\")`",
                    ));
                }
            } else {
                return Err(syn::Error::new(
                    content.span(),
                    "define_wallet_roster: expected `env_mnemonic(\"VAR\")`, `auto`, or `env_private_key(\"VAR\")`",
                ));
            };

            // `@ index` is optional (default 0). Private-key rows have no derivation, so they
            // typically omit it.
            let index = if content.peek(Token![@]) {
                let _: Token![@] = content.parse()?;
                let index_lit: LitInt = content.parse()?;
                index_lit.base10_parse()?
            } else {
                0u32
            };

            let _: Option<Token![,]> = content.parse()?;

            entries.push(RosterEntry {
                attrs,
                field_name,
                source,
                index,
            });
        }

        let _: Token![;] = input.parse()?;

        Ok(RosterInput {
            vis,
            const_name,
            struct_name,
            entries,
        })
    }
}

fn label_for(entry: &RosterEntry) -> syn::Result<String> {
    for attr in &entry.attrs {
        if attr.path().is_ident("label") {
            let lit: LitStr = match &attr.meta {
                syn::Meta::List(list) => syn::parse2(list.tokens.clone())?,
                syn::Meta::NameValue(nv) => {
                    if let syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    }) = &nv.value
                    {
                        s.clone()
                    } else {
                        return Err(syn::Error::new_spanned(
                            &nv.value,
                            "define_wallet_roster: #[label] expects a string literal",
                        ));
                    }
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        other,
                        "define_wallet_roster: #[label] expects label(\"...\") or label = \"...\"",
                    ));
                }
            };
            return Ok(lit.value());
        }
    }
    Ok(entry.field_name.to_string())
}

pub fn expand(input: TokenStream2) -> TokenStream2 {
    let parsed = match syn::parse2::<RosterInput>(input) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error(),
    };
    expand_parsed(parsed)
}

fn expand_parsed(
    RosterInput {
        vis,
        const_name,
        struct_name,
        entries,
    }: RosterInput,
) -> TokenStream2 {
    let mut field_names = Vec::new();
    let mut labels = Vec::new();
    let mut sources = Vec::new();
    let mut indices = Vec::new();

    for entry in &entries {
        field_names.push(&entry.field_name);
        let label = match label_for(entry) {
            Ok(l) => l,
            Err(e) => return e.to_compile_error(),
        };
        labels.push(LitStr::new(&label, entry.field_name.span()));
        indices.push(entry.index);
        sources.push(match &entry.source {
            SourceKind::EnvMnemonic(var) => {
                quote! { cross_vm_core::WalletSource::EnvMnemonic(#var) }
            }
            SourceKind::Auto => quote! { cross_vm_core::WalletSource::Auto },
            SourceKind::EnvPrivateKey(var) => {
                quote! { cross_vm_core::WalletSource::EnvPrivateKey(#var) }
            }
        });
    }

    quote! {
        #vis struct #struct_name {
            #( pub #field_names: cross_vm_core::WalletLabel<'static>, )*
        }

        impl #struct_name {
            pub const SPECS: &'static [cross_vm_core::WalletSpec] = &[
                #(
                    cross_vm_core::WalletSpec {
                        label: #labels,
                        source: #sources,
                        index: #indices,
                        hd_path: None,
                    },
                )*
            ];
        }

        #vis const #const_name: #struct_name = #struct_name {
            #( #field_names: cross_vm_core::WalletLabel::new(#labels), )*
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    fn expand_str(src: TokenStream2) -> String {
        expand(src).to_string()
    }

    #[test]
    fn emits_struct_fields_and_specs() {
        let input = quote! {
            pub const TEST_WALLETS: TestWallets = {
                alice: env_mnemonic("MNEMONIC_MAIN") @ 0,
                bob: env_mnemonic("MNEMONIC_MAIN") @ 1,
            };
        };
        let out = expand_str(input);
        assert!(out.contains("struct TestWallets"));
        assert!(out.contains("pub alice : cross_vm_core :: WalletLabel"));
        assert!(out.contains("pub bob : cross_vm_core :: WalletLabel"));
        assert!(out.contains("label : \"alice\""));
        assert!(out.contains("label : \"bob\""));
        assert!(out.contains("WalletSource :: EnvMnemonic"));
        assert!(out.contains("MNEMONIC_MAIN"));
        assert!(out.contains("const TEST_WALLETS : TestWallets"));
    }

    #[test]
    fn label_attr_overrides_field_name() {
        let input = quote! {
            pub const TEST_WALLETS: TestWallets = {
                #[label("test-admin")]
                test_admin: env_mnemonic("MNEMONIC_ADMIN") @ 0,
            };
        };
        let out = expand_str(input);
        assert!(out.contains("label : \"test-admin\""));
        assert!(out.contains("pub test_admin : cross_vm_core :: WalletLabel"));
    }

    #[test]
    fn auto_source_and_empty_roster() {
        let input = quote! {
            pub const EMPTY: EmptyWallets = {};
        };
        let out = expand_str(input);
        assert!(out.contains("struct EmptyWallets"));
        assert!(out.contains("SPECS") && out.contains("WalletSpec") && out.contains("& []"));

        let gen = quote! {
            pub const W: Wallets = {
                ephemeral: auto @ 0,
            };
        };
        let out = expand_str(gen);
        assert!(out.contains("WalletSource :: Auto"));
    }

    #[test]
    fn private_key_source_omits_index() {
        let input = quote! {
            pub const W: Wallets = {
                signer: env_private_key("PRIVKEY_SIGNER"),
            };
        };
        let out = expand_str(input);
        assert!(out.contains("WalletSource :: EnvPrivateKey"));
        assert!(out.contains("PRIVKEY_SIGNER"));
        assert!(out.contains("index : 0u32"));
    }

    #[test]
    fn rejects_unknown_source() {
        match syn::parse2::<RosterInput>(quote! {
            pub const W: Wallets = {
                alice: bogus("X") @ 0,
            };
        }) {
            Ok(_) => panic!("bogus source should fail to parse"),
            Err(e) => assert!(e.to_string().contains("env_mnemonic"), "{e}"),
        }
    }
}
