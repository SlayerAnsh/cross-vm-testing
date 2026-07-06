//! Per-mode runner attribute macros: `#[fuzz_runner]`, `#[invariant_runner]`,
//! `#[endurance_runner]`.
//!
//! Each is an attribute on an `async fn` whose single `#[runner]`-marked argument receives a seeded,
//! mode-typed runner *shell* (`Runner::fuzz` / `::invariant` / `::endurance`). The developer writes
//! the whole body: their own setup (deploy, prime the model, op preconditions), `r.setup(ctx,
//! world)`, the `run(..)` call, and the asserts. The macro only injects the shell and, for fuzz,
//! fans the test out into one `#[tokio::test]` per case (seeded by `sub_seed(seed, i)`).
//!
//! ```ignore
//! #[fuzz_runner(harness = vault_harness(), seed = 11, cases = 50)]
//! async fn vault_deposit(#[runner] mut r: FuzzRunner<OpSetHarness<Ctx, VaultWorld>>) {
//!     let (ctx, world) = vault_deposit_setup(r.seed()).await.expect("setup");
//!     r.setup(ctx, world);
//!     let report = r.run(1, Some(&["deposit"]), 1).await;
//!     assert!(report.passed(), "{:?}", report.failure);
//! }
//! // -> tests vault_deposit_case_0 .. vault_deposit_case_49
//! ```
//!
//! `seed` defaults to 0. A negative seed (`seed = -1`) picks a fresh random seed per run via
//! `random_seed()` and prints it (for fuzz, one shared base across all cases), so a failure stays
//! reproducible: copy the printed value back as a fixed `seed`.
//!
//! The emitted code names `Runner`, `sub_seed`, and `random_seed` unqualified, so the call site
//! needs `use cross_vm_framework::prelude::*;` in scope (the same requirement the contract macro
//! has).

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{Expr, FnArg, Ident, ItemFn, LitInt, Token};

/// Which run mode an invocation targets. Selects the constructor and whether `cases` fan-out applies.
#[derive(Clone, Copy)]
pub enum Mode {
    Fuzz,
    Invariant,
    Endurance,
}

impl Mode {
    /// The `Runner::<ctor>` shell constructor for this mode.
    fn ctor(self) -> &'static str {
        match self {
            Mode::Fuzz => "fuzz",
            Mode::Invariant => "invariant",
            Mode::Endurance => "endurance",
        }
    }

    /// Whether this mode fans out into one test per case (only fuzz does).
    fn fans_out(self) -> bool {
        matches!(self, Mode::Fuzz)
    }

    fn name(self) -> &'static str {
        match self {
            Mode::Fuzz => "fuzz_runner",
            Mode::Invariant => "invariant_runner",
            Mode::Endurance => "endurance_runner",
        }
    }
}

/// How the base seed is chosen.
#[derive(Clone, Copy)]
enum SeedSpec {
    /// A fixed seed literal.
    Fixed(u64),
    /// A negative `seed` (e.g. `seed = -1`): pick a fresh seed per run via `random_seed()`.
    Random,
}

/// Parsed `key = value` attribute arguments.
struct Args {
    harness: Expr,
    seed: SeedSpec,
    cases: Option<usize>,
}

impl Parse for Args {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut harness = None;
        let mut seed = SeedSpec::Fixed(0);
        let mut cases = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "harness" => harness = Some(input.parse()?),
                // A leading `-` (e.g. `seed = -1`) requests a random per-run seed; LitInt itself is
                // unsigned, so the sign is a separate token.
                "seed" => {
                    if input.peek(Token![-]) {
                        input.parse::<Token![-]>()?;
                        input.parse::<LitInt>()?; // consume the magnitude (any negative => random)
                        seed = SeedSpec::Random;
                    } else {
                        seed = SeedSpec::Fixed(input.parse::<LitInt>()?.base10_parse()?);
                    }
                }
                "cases" => cases = Some(input.parse::<LitInt>()?.base10_parse()?),
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "runner macro: unknown key `{other}` (expected harness / seed / cases)"
                        ),
                    ))
                }
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(Args {
            harness: harness.ok_or_else(|| {
                syn::Error::new(Span::call_site(), "runner macro: missing `harness`")
            })?,
            seed,
            cases,
        })
    }
}

/// Expand one of the runner attribute macros over `item` in `mode`.
pub fn expand(mode: Mode, attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let args: Args = syn::parse2(attr)?;
    let mut func: ItemFn = syn::parse2(item)?;

    if func.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &func.sig,
            format!("{}: the test fn must be `async`", mode.name()),
        ));
    }

    // The `cases` key is required for fuzz and rejected otherwise.
    let cases = match (mode.fans_out(), args.cases) {
        (true, Some(c)) if c > 0 => c,
        (true, Some(_)) => {
            return Err(syn::Error::new(
                Span::call_site(),
                "fuzz_runner: `cases` must be greater than 0",
            ))
        }
        (true, None) => {
            return Err(syn::Error::new(
                Span::call_site(),
                "fuzz_runner: missing `cases`",
            ))
        }
        (false, Some(_)) => {
            return Err(syn::Error::new(
                Span::call_site(),
                format!("{}: `cases` is only valid on fuzz_runner", mode.name()),
            ))
        }
        (false, None) => 0,
    };

    // Find and strip the single `#[runner]` argument; verify exactly one.
    let mut found = false;
    for arg in func.sig.inputs.iter_mut() {
        if let FnArg::Typed(pt) = arg {
            let before = pt.attrs.len();
            pt.attrs.retain(|a| !a.path().is_ident("runner"));
            if pt.attrs.len() != before {
                if found {
                    return Err(syn::Error::new_spanned(
                        pt,
                        format!("{}: only one `#[runner]` argument is allowed", mode.name()),
                    ));
                }
                found = true;
            }
        }
    }
    if !found {
        return Err(syn::Error::new_spanned(
            &func.sig,
            format!(
                "{}: exactly one argument must be marked `#[runner]` (e.g. \
                 `#[runner] mut r: FuzzRunner<MyHarness>`)",
                mode.name()
            ),
        ));
    }

    let name = func.sig.ident.clone();
    let helper = format_ident!("__{}_body", name);
    func.sig.ident = helper.clone();
    // The helper is a plain private async fn; drop any test attributes the user may have added.
    func.attrs
        .retain(|a| a.path().segments.last().is_none_or(|s| s.ident != "test"));
    func.vis = syn::Visibility::Inherited;

    let harness = &args.harness;
    let ctor = format_ident!("{}", mode.ctor());
    let label = name.to_string();

    if mode.fans_out() {
        // For a random base seed, all cases must share ONE value so the run is reproducible as a
        // set: a per-invocation `OnceLock` holds it, the first case to run picks and prints it.
        let base = match args.seed {
            SeedSpec::Fixed(s) => quote! { #s },
            SeedSpec::Random => {
                let cell = format_ident!("__{}_base_seed", name);
                quote! {
                    *#cell.get_or_init(|| {
                        let s = random_seed();
                        eprintln!("[{}] random base seed = {} (set `seed = {}` to reproduce)",
                            #label, s, s);
                        s
                    })
                }
            }
        };
        let cell_def = match args.seed {
            SeedSpec::Fixed(_) => quote! {},
            SeedSpec::Random => {
                let cell = format_ident!("__{}_base_seed", name);
                quote! {
                    #[allow(non_upper_case_globals)]
                    static #cell: ::std::sync::OnceLock<u64> = ::std::sync::OnceLock::new();
                }
            }
        };
        let tests = (0..cases).map(|i| {
            let fn_name = format_ident!("{}_case_{}", name, i);
            quote! {
                #[tokio::test]
                async fn #fn_name() {
                    let __seed = sub_seed(#base, #i);
                    #helper(Runner::#ctor(#harness, __seed)).await;
                }
            }
        });
        Ok(quote! {
            #func
            #cell_def
            #(#tests)*
        })
    } else {
        let seed = match args.seed {
            SeedSpec::Fixed(s) => quote! { #s },
            SeedSpec::Random => quote! {{
                let s = random_seed();
                eprintln!("[{}] random seed = {} (set `seed = {}` to reproduce)", #label, s, s);
                s
            }},
        };
        Ok(quote! {
            #func
            #[tokio::test]
            async fn #name() {
                #helper(Runner::#ctor(#harness, #seed)).await;
            }
        })
    }
}
