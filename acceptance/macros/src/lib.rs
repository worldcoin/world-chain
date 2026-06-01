//! Procedural macros for the World Chain acceptance test harness.
//!
//! The [`macro@acceptance_test`] attribute turns a plain `async fn` into a
//! self-registering acceptance check. Every annotated function is collected at
//! link time via [`inventory`], so adding a new check is a single attribute on
//! a new function — no central registry to edit.
//!
//! ```ignore
//! use std::sync::Arc;
//! use world_chain_acceptance::{TestCtx, acceptance_test};
//!
//! #[acceptance_test(category = Health)]
//! async fn chain_id_matches(ctx: Arc<TestCtx>) -> eyre::Result<()> {
//!     // ...
//!     Ok(())
//! }
//!
//! // With explicit name and capability requirements:
//! #[acceptance_test(category = SpecCompatibility, name = "flashblocks_capability", requires(Flashblocks))]
//! async fn supports_flashblocks(ctx: Arc<TestCtx>) -> eyre::Result<()> {
//!     Ok(())
//! }
//! ```

use proc_macro::TokenStream;
use quote::quote;
use syn::{
    Expr, ExprPath, ItemFn, Meta, Token, parse::Parser, punctuated::Punctuated, spanned::Spanned,
};

/// Register an `async fn(Arc<TestCtx>) -> eyre::Result<()>` as an acceptance check.
///
/// Arguments:
/// - `category = <Health|SpecCompatibility|Performance>` (required)
/// - `name = "..."` (optional; defaults to the function name)
/// - `requires(<Capability>, ...)` (optional; the check is skipped when the
///   environment does not advertise every listed capability)
#[proc_macro_attribute]
pub fn acceptance_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = syn::parse_macro_input!(item as ItemFn);

    let args = match Args::parse(attr) {
        Ok(args) => args,
        Err(err) => return err.to_compile_error().into(),
    };

    let category = match args.category {
        Some(category) => category,
        None => {
            return syn::Error::new(
                proc_macro2::Span::call_site(),
                "`#[acceptance_test]` requires a `category = <Health|SpecCompatibility|Performance>` argument",
            )
            .to_compile_error()
            .into();
        }
    };

    let fn_name = &func.sig.ident;
    let test_name = args
        .name
        .unwrap_or_else(|| fn_name.to_string().replace('_', " "));

    // Each requirement is emitted as a typed `Requirement` (a `Hardfork` or
    // `Feature`) resolved against the network manifest at run time.
    let requires = args.requires.iter().map(|(kind, name)| {
        let name = name.to_string().to_ascii_lowercase();
        match kind.to_string().as_str() {
            "hardfork" => quote! { ::world_chain_acceptance::Requirement::Hardfork(
                ::world_chain_acceptance::Hardfork(#name)
            ) },
            // already validated to be `hardfork` or `feature`
            _ => quote! { ::world_chain_acceptance::Requirement::Feature(
                ::world_chain_acceptance::Feature(#name)
            ) },
        }
    });

    let expanded = quote! {
        #func

        ::world_chain_acceptance::inventory::submit! {
            ::world_chain_acceptance::AcceptanceTest {
                name: #test_name,
                category: ::world_chain_acceptance::Category::#category,
                requires: &[ #( #requires ),* ],
                run: |ctx| ::std::boxed::Box::pin(#fn_name(ctx)),
            }
        }
    };

    expanded.into()
}

#[derive(Default)]
struct Args {
    category: Option<proc_macro2::Ident>,
    name: Option<String>,
    /// Requirement entries as `(kind, name)` where kind is `hardfork`/`feature`.
    requires: Vec<(proc_macro2::Ident, proc_macro2::Ident)>,
}

impl Args {
    fn parse(attr: TokenStream) -> syn::Result<Self> {
        let metas = Punctuated::<Meta, Token![,]>::parse_terminated.parse(attr)?;

        let mut args = Args::default();
        for meta in metas {
            match meta {
                Meta::NameValue(nv) if nv.path.is_ident("category") => {
                    args.category = Some(expr_ident(&nv.value, "category")?);
                }
                Meta::NameValue(nv) if nv.path.is_ident("name") => {
                    args.name = Some(expr_str(&nv.value, "name")?);
                }
                Meta::List(list) if list.path.is_ident("requires") => {
                    let entries = list.parse_args_with(
                        Punctuated::<syn::MetaNameValue, Token![,]>::parse_terminated,
                    )?;
                    for entry in entries {
                        let kind = entry.path.get_ident().cloned().ok_or_else(|| {
                            syn::Error::new(entry.path.span(), "expected `hardfork` or `feature`")
                        })?;
                        if kind != "hardfork" && kind != "feature" {
                            return Err(syn::Error::new(
                                kind.span(),
                                "requirement kind must be `hardfork` or `feature`",
                            ));
                        }
                        let name = expr_ident(&entry.value, "requirement")?;
                        args.requires.push((kind, name));
                    }
                }
                other => {
                    return Err(syn::Error::new(
                        other.span(),
                        "expected `category = ...`, `name = \"...\"`, or `requires(...)`",
                    ));
                }
            }
        }

        Ok(args)
    }
}

/// Extract a bare identifier from `key = Ident`.
fn expr_ident(expr: &Expr, key: &str) -> syn::Result<proc_macro2::Ident> {
    if let Expr::Path(ExprPath { path, .. }) = expr
        && let Some(ident) = path.get_ident()
    {
        return Ok(ident.clone());
    }
    Err(syn::Error::new(
        expr.span(),
        format!("`{key}` must be a single identifier"),
    ))
}

/// Extract a string literal from `key = "value"`.
fn expr_str(expr: &Expr, key: &str) -> syn::Result<String> {
    if let Expr::Lit(lit) = expr
        && let syn::Lit::Str(s) = &lit.lit
    {
        return Ok(s.value());
    }
    Err(syn::Error::new(
        expr.span(),
        format!("`{key}` must be a string literal"),
    ))
}
