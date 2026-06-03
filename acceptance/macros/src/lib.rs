//! Procedural macros for the World Chain acceptance test harness.
//!
//! `#[acceptance_test]` registers an async function in the code-derived
//! acceptance catalog. Metadata — including the hardfork and features the test
//! requires — lives next to the test, not in an external gate file. The runner
//! reads that metadata to select and gate tests per the committed manifest.

use proc_macro::TokenStream;
use quote::quote;
use syn::{
    Expr, ExprArray, ItemFn, Lit, Meta, Token, parse::Parser, punctuated::Punctuated,
    spanned::Spanned,
};

/// Register an `async fn(Arc<TestCtx>) -> eyre::Result<()>` as an acceptance test.
///
/// Arguments (all optional):
/// - `name = "..."` — stable test name (defaults to the function name).
/// - `requires_hardfork = "tropo"` — minimum [`WorldChainHardfork`]; the runner
///   skips the test when the committed manifest is on an earlier fork.
/// - `features = ["flashblocks", "pbh"]` — features the test needs; the runner
///   skips the test when the manifest does not commit to all of them.
/// - `serial` — opt the test out of intra-cell parallelism.
/// - `flaky = "reason"` — failures downgrade to skips unless
///   `ACCEPTANCE_FAIL_FLAKY_TESTS=true`.
///
/// The hardfork/feature strings are stored verbatim and resolved against the
/// typed enums at selection time (see `Requirements`), keeping this proc-macro
/// crate free of a dependency on the chainspec types.
#[proc_macro_attribute]
pub fn acceptance_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = syn::parse_macro_input!(item as ItemFn);

    let args = match Args::parse(attr) {
        Ok(args) => args,
        Err(err) => return err.to_compile_error().into(),
    };

    let fn_name = &func.sig.ident;
    let test_name = args.name.unwrap_or_else(|| fn_name.to_string());
    let flaky = match args.flaky {
        Some(reason) => quote! { ::std::option::Option::Some(#reason) },
        None => quote! { ::std::option::Option::None },
    };
    let min_hardfork = match args.requires_hardfork {
        Some(fork) => quote! { ::std::option::Option::Some(#fork) },
        None => quote! { ::std::option::Option::None },
    };
    let features = args.features;
    let serial = args.serial;

    let expanded = quote! {
        #func

        ::world_chain_acceptance::inventory::submit! {
            ::world_chain_acceptance::AcceptanceTest {
                name: #test_name,
                package: ::std::module_path!(),
                location: ::world_chain_acceptance::Location {
                    file: ::std::file!(),
                    line: ::std::line!(),
                },
                flaky: #flaky,
                requirements: ::world_chain_acceptance::Requirements {
                    min_hardfork: #min_hardfork,
                    features: &[#(#features),*],
                    serial: #serial,
                },
                run: |ctx| ::std::boxed::Box::pin(#fn_name(ctx)),
            }
        }
    };

    expanded.into()
}

#[derive(Default)]
struct Args {
    name: Option<String>,
    flaky: Option<String>,
    requires_hardfork: Option<String>,
    features: Vec<String>,
    serial: bool,
}

impl Args {
    fn parse(attr: TokenStream) -> syn::Result<Self> {
        let metas = Punctuated::<Meta, Token![,]>::parse_terminated.parse(attr)?;

        let mut args = Args::default();
        for meta in metas {
            match meta {
                Meta::NameValue(nv) if nv.path.is_ident("name") => {
                    args.name = Some(expr_str(&nv.value, "name")?);
                }
                Meta::NameValue(nv) if nv.path.is_ident("flaky") => {
                    args.flaky = Some(expr_str(&nv.value, "flaky")?);
                }
                Meta::NameValue(nv) if nv.path.is_ident("requires_hardfork") => {
                    args.requires_hardfork = Some(expr_str(&nv.value, "requires_hardfork")?);
                }
                Meta::NameValue(nv) if nv.path.is_ident("features") => {
                    args.features = expr_str_array(&nv.value, "features")?;
                }
                Meta::Path(path) if path.is_ident("serial") => {
                    args.serial = true;
                }
                other => {
                    return Err(syn::Error::new(
                        other.span(),
                        "expected one of `name = \"...\"`, `requires_hardfork = \"...\"`, \
                         `features = [\"...\"]`, `serial`, or `flaky = \"...\"`",
                    ));
                }
            }
        }

        Ok(args)
    }
}

/// Extract a string literal from `key = "value"`.
fn expr_str(expr: &Expr, key: &str) -> syn::Result<String> {
    if let Expr::Lit(lit) = expr
        && let Lit::Str(s) = &lit.lit
    {
        return Ok(s.value());
    }
    Err(syn::Error::new(
        expr.span(),
        format!("`{key}` must be a string literal"),
    ))
}

/// Extract a list of string literals from `key = ["a", "b"]`.
fn expr_str_array(expr: &Expr, key: &str) -> syn::Result<Vec<String>> {
    let Expr::Array(ExprArray { elems, .. }) = expr else {
        return Err(syn::Error::new(
            expr.span(),
            format!("`{key}` must be an array of string literals"),
        ));
    };
    elems.iter().map(|elem| expr_str(elem, key)).collect()
}
