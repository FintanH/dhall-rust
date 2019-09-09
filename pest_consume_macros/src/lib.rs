#![feature(drain_filter)]
//! This crate contains the code-generation primitives for the [dhall-rust][dhall-rust] crate.
//! This is highly unstable and breaks regularly; use at your own risk.
//!
//! [dhall-rust]: https://github.com/Nadrieril/dhall-rust

extern crate proc_macro;

mod make_parser;
mod parse_children;

use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn make_parser(attrs: TokenStream, input: TokenStream) -> TokenStream {
    TokenStream::from(match make_parser::make_parser(attrs, input) {
        Ok(tokens) => tokens,
        Err(err) => err.to_compile_error(),
    })
}

#[proc_macro]
pub fn parse_children(input: TokenStream) -> TokenStream {
    TokenStream::from(match parse_children::parse_children(input) {
        Ok(tokens) => tokens,
        Err(err) => err.to_compile_error(),
    })
}
