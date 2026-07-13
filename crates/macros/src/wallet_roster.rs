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
    /// An ordered fallback chain of typed env-var candidates; the first one set at resolve time
    /// wins. Parsed from `env_any(private_key("A"), mnemonic("B") @ 1)`.
    EnvAny(Vec<CandidateKind>),
}

/// One `env_any(..)` candidate. A `mnemonic` candidate's index is `None` until codegen, where it
/// falls back to the row-level `@ N` (itself defaulting to `0`). `private_key` carries no index.
enum CandidateKind {
    Mnemonic { var: LitStr, index: Option<u32> },
    PrivateKey { var: LitStr },
}

/// Parse the inside of `env_any( .. )`: a non-empty, comma-separated list of
/// `mnemonic("VAR") @ N` / `private_key("VAR")` candidates, trailing comma allowed.
fn parse_env_any(env_any_kw: &Ident, args: ParseStream) -> syn::Result<Vec<CandidateKind>> {
    let mut candidates = Vec::new();
    while !args.is_empty() {
        let kind: Ident = args.parse()?;
        let var_args;
        syn::parenthesized!(var_args in args);
        let var: LitStr = var_args.parse()?;

        // A candidate's own `@ N` overrides the row-level default.
        let index = if args.peek(Token![@]) {
            let _: Token![@] = args.parse()?;
            let index_lit: LitInt = args.parse()?;
            Some((index_lit.base10_parse::<u32>()?, index_lit))
        } else {
            None
        };

        if kind == "mnemonic" {
            candidates.push(CandidateKind::Mnemonic {
                var,
                index: index.map(|(n, _)| n),
            });
        } else if kind == "private_key" {
            if let Some((_, lit)) = index {
                return Err(syn::Error::new(
                    lit.span(),
                    "define_wallet_roster: a `private_key` candidate has no derivation index; drop the `@ N`",
                ));
            }
            candidates.push(CandidateKind::PrivateKey { var });
        } else {
            return Err(syn::Error::new(
                kind.span(),
                "define_wallet_roster: expected `mnemonic(\"VAR\")` or `private_key(\"VAR\")` inside `env_any(..)`",
            ));
        }

        let comma: Option<Token![,]> = args.parse()?;
        if comma.is_none() {
            break;
        }
    }

    if candidates.is_empty() {
        return Err(syn::Error::new(
            env_any_kw.span(),
            "define_wallet_roster: `env_any(..)` needs at least one candidate, e.g. `env_any(private_key(\"VAR\"))`",
        ));
    }

    Ok(candidates)
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
                } else if kind == "env_any" {
                    let env_args;
                    syn::parenthesized!(env_args in content);
                    SourceKind::EnvAny(parse_env_any(&kind, &env_args)?)
                } else {
                    return Err(syn::Error::new(
                        kind.span(),
                        "define_wallet_roster: expected `env_mnemonic(\"VAR\")`, `auto`, `env_private_key(\"VAR\")`, or `env_any(..)`",
                    ));
                }
            } else {
                return Err(syn::Error::new(
                    content.span(),
                    "define_wallet_roster: expected `env_mnemonic(\"VAR\")`, `auto`, `env_private_key(\"VAR\")`, or `env_any(..)`",
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
            SourceKind::EnvAny(candidates) => {
                // Declaration order is the fallback order. A `mnemonic` candidate without its own
                // `@ N` inherits the row-level index (which itself defaults to 0).
                let candidates = candidates.iter().map(|c| match c {
                    CandidateKind::PrivateKey { var } => {
                        quote! { cross_vm_core::EnvCandidate::PrivateKey { var: #var } }
                    }
                    CandidateKind::Mnemonic { var, index } => {
                        let index = index.unwrap_or(entry.index);
                        quote! { cross_vm_core::EnvCandidate::Mnemonic { var: #var, index: #index } }
                    }
                });
                quote! { cross_vm_core::WalletSource::EnvAny(&[ #(#candidates),* ]) }
            }
        });
    }

    let struct_doc = "Wallet roster generated by `define_wallet_roster!`; one typed \
         [`WalletLabel`](cross_vm_core::WalletLabel) field per row.";
    let const_doc =
        format!("The `{struct_name}` roster instance; pass its labels to signing calls.");

    quote! {
        #[doc = #struct_doc]
        #vis struct #struct_name {
            #(
                #[doc = concat!("The `", #labels, "` wallet label.")]
                pub #field_names: cross_vm_core::WalletLabel<'static>,
            )*
        }

        impl #struct_name {
            /// Every row of this roster as a [`WalletSpec`](cross_vm_core::WalletSpec) slice, the
            /// input to [`WalletFactory::from_roster`](cross_vm_core::WalletFactory::from_roster).
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

        #[doc = #const_doc]
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
    fn env_any_emits_ordered_candidate_chain() {
        let input = quote! {
            pub const W: Wallets = {
                bob: env_any(
                    private_key("BOB_PRIVATE_KEY"),
                    mnemonic("FIXTURE_MNEMONIC") @ 1,
                ),
            };
        };
        let out = expand_str(input);
        assert!(out.contains(
            "source : cross_vm_core :: WalletSource :: EnvAny \
             (& [cross_vm_core :: EnvCandidate :: PrivateKey { var : \"BOB_PRIVATE_KEY\" } , \
             cross_vm_core :: EnvCandidate :: Mnemonic { var : \"FIXTURE_MNEMONIC\" , index : 1u32 }])"
        ));
        // The row itself keeps the default index, and `hd_path` still emits `None`.
        assert!(out.contains("index : 0u32"));
        assert!(out.contains("hd_path : None"));
    }

    #[test]
    fn env_any_candidates_inherit_row_index() {
        let input = quote! {
            pub const W: Wallets = {
                carol: env_any(mnemonic("A"), mnemonic("B")) @ 2,
            };
        };
        let out = expand_str(input);
        assert!(out.contains(
            "EnvAny (& [cross_vm_core :: EnvCandidate :: Mnemonic { var : \"A\" , index : 2u32 } , \
             cross_vm_core :: EnvCandidate :: Mnemonic { var : \"B\" , index : 2u32 }])"
        ));
        assert!(out.contains("index : 2u32 , hd_path : None"));
    }

    #[test]
    fn env_any_candidate_index_overrides_row_index() {
        let input = quote! {
            pub const W: Wallets = {
                dave: env_any(mnemonic("A") @ 7, mnemonic("B")) @ 2,
            };
        };
        let out = expand_str(input);
        assert!(out.contains(
            "EnvAny (& [cross_vm_core :: EnvCandidate :: Mnemonic { var : \"A\" , index : 7u32 } , \
             cross_vm_core :: EnvCandidate :: Mnemonic { var : \"B\" , index : 2u32 }])"
        ));
    }

    #[test]
    fn rejects_empty_env_any() {
        match syn::parse2::<RosterInput>(quote! {
            pub const W: Wallets = {
                bob: env_any(),
            };
        }) {
            Ok(_) => panic!("empty env_any should fail to parse"),
            Err(e) => assert!(e.to_string().contains("at least one candidate"), "{e}"),
        }
    }

    #[test]
    fn rejects_indexed_private_key_candidate() {
        match syn::parse2::<RosterInput>(quote! {
            pub const W: Wallets = {
                bob: env_any(private_key("X") @ 1),
            };
        }) {
            Ok(_) => panic!("an indexed private_key candidate should fail to parse"),
            Err(e) => assert!(e.to_string().contains("no derivation index"), "{e}"),
        }
    }

    #[test]
    fn rejects_unknown_env_any_candidate() {
        match syn::parse2::<RosterInput>(quote! {
            pub const W: Wallets = {
                bob: env_any(keyring("X")),
            };
        }) {
            Ok(_) => panic!("an unknown candidate kind should fail to parse"),
            Err(e) => assert!(e.to_string().contains("private_key"), "{e}"),
        }
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
