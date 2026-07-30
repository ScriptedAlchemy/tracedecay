//! Consolidated extractor test suite.
//!
//! Each module was previously a standalone integration-test binary
//! (`tests/<lang>_extraction_test.rs`). They are merged into a single
//! binary to cut per-binary link time on Windows CI.

mod astro;
#[cfg(feature = "lang-bash")]
mod bash;
#[cfg(feature = "lang-batch")]
mod batch;
mod c;
#[cfg(feature = "lang-cobol")]
mod cobol;
mod cpp;
mod csharp;
#[cfg(feature = "lang-dart")]
mod dart;
#[cfg(feature = "lang-dockerfile")]
mod dockerfile;
mod fixture;
#[cfg(feature = "lang-fortran")]
mod fortran;
mod general;
#[cfg(feature = "lang-glsl")]
mod glsl;
mod go;
#[cfg(feature = "lang-gwbasic")]
mod gwbasic;
mod java;
mod kotlin;
#[cfg(feature = "lang-lean")]
mod lean;
#[cfg(feature = "lang-lua")]
mod lua;
#[cfg(feature = "lang-markdown")]
mod markdown;
#[cfg(feature = "lang-markdown")]
mod markdown_modern_grammar;
#[cfg(feature = "lang-msbasic2")]
mod msbasic2;
#[cfg(feature = "lang-nix")]
mod nix;
#[cfg(feature = "lang-objc")]
mod objc;
#[cfg(feature = "lang-pascal")]
mod pascal;
#[cfg(feature = "lang-perl")]
mod perl;
#[cfg(feature = "lang-php")]
mod php;
#[cfg(feature = "lang-powershell")]
mod powershell;
#[cfg(feature = "lang-protobuf")]
mod proto;
mod python;
#[cfg(feature = "lang-qbasic")]
mod qbasic;
#[cfg(feature = "lang-qbasic")]
mod quickbasic;
#[cfg(feature = "lang-quint")]
mod quint;
#[cfg(feature = "lang-ruby")]
mod ruby;
mod rust;
mod scala;
mod svelte;
mod swift;
#[cfg(feature = "lang-toml")]
mod toml;
mod typescript;
#[cfg(feature = "lang-vbnet")]
mod vbnet;
#[cfg(feature = "lang-zig")]
mod zig;
