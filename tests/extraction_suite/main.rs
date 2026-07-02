//! Consolidated extractor test suite.
//!
//! Each module was previously a standalone integration-test binary
//! (`tests/<lang>_extraction_test.rs`). They are merged into a single
//! binary to cut per-binary link time on Windows CI.

mod astro;
mod bash;
mod batch;
mod c;
mod cobol;
mod cpp;
mod csharp;
mod dart;
mod dockerfile;
mod fixture;
mod fortran;
mod general;
mod glsl;
mod go;
mod gwbasic;
mod java;
mod kotlin;
mod lean;
mod lua;
mod markdown;
mod markdown_modern_grammar;
mod msbasic2;
mod nix;
mod objc;
mod pascal;
mod perl;
mod php;
mod powershell;
mod proto;
mod python;
mod qbasic;
mod quickbasic;
mod quint;
mod ruby;
mod rust;
mod scala;
mod svelte;
mod swift;
mod toml;
mod typescript;
mod vbnet;
mod zig;
