//! Compatibility façade for language extraction.

pub mod complexity {
    pub use tracedecay_code_extraction::complexity::*;
}

pub mod ts_provider {
    pub use tracedecay_code_extraction::ts_provider::*;
}

pub(crate) use tracedecay_code_extraction::source_mask;

pub use tracedecay_code_extraction::{
    AstroExtractor, CExtractor, CSharpExtractor, CppExtractor, GoExtractor, JavaExtractor,
    KotlinExtractor, LanguageExtractor, LanguageRegistry, PythonExtractor, RustExtractor,
    ScalaExtractor, SvelteExtractor, SwiftExtractor, TypeScriptExtractor,
};

#[cfg(feature = "lang-bash")]
pub use tracedecay_code_extraction::BashExtractor;
#[cfg(feature = "lang-batch")]
pub use tracedecay_code_extraction::BatchExtractor;
#[cfg(feature = "lang-clojure")]
pub use tracedecay_code_extraction::ClojureExtractor;
#[cfg(feature = "lang-cobol")]
pub use tracedecay_code_extraction::CobolExtractor;
#[cfg(feature = "lang-dart")]
pub use tracedecay_code_extraction::DartExtractor;
#[cfg(feature = "lang-dockerfile")]
pub use tracedecay_code_extraction::DockerfileExtractor;
#[cfg(feature = "lang-elixir")]
pub use tracedecay_code_extraction::ElixirExtractor;
#[cfg(feature = "lang-erlang")]
pub use tracedecay_code_extraction::ErlangExtractor;
#[cfg(feature = "lang-fsharp")]
pub use tracedecay_code_extraction::FSharpExtractor;
#[cfg(feature = "lang-fortran")]
pub use tracedecay_code_extraction::FortranExtractor;
#[cfg(feature = "lang-glsl")]
pub use tracedecay_code_extraction::GlslExtractor;
#[cfg(feature = "lang-gwbasic")]
pub use tracedecay_code_extraction::GwBasicExtractor;
#[cfg(feature = "lang-haskell")]
pub use tracedecay_code_extraction::HaskellExtractor;
#[cfg(feature = "lang-hlsl")]
pub use tracedecay_code_extraction::HlslExtractor;
#[cfg(feature = "lang-julia")]
pub use tracedecay_code_extraction::JuliaExtractor;
#[cfg(feature = "lang-lean")]
pub use tracedecay_code_extraction::LeanExtractor;
#[cfg(feature = "lang-lua")]
pub use tracedecay_code_extraction::LuaExtractor;
#[cfg(feature = "lang-markdown")]
pub use tracedecay_code_extraction::MarkdownExtractor;
#[cfg(feature = "lang-metal")]
pub use tracedecay_code_extraction::MetalExtractor;
#[cfg(feature = "lang-msbasic2")]
pub use tracedecay_code_extraction::MsBasic2Extractor;
#[cfg(feature = "lang-nix")]
pub use tracedecay_code_extraction::NixExtractor;
#[cfg(feature = "lang-objc")]
pub use tracedecay_code_extraction::ObjcExtractor;
#[cfg(feature = "lang-ocaml")]
pub use tracedecay_code_extraction::OcamlExtractor;
#[cfg(feature = "lang-pascal")]
pub use tracedecay_code_extraction::PascalExtractor;
#[cfg(feature = "lang-perl")]
pub use tracedecay_code_extraction::PerlExtractor;
#[cfg(feature = "lang-php")]
pub use tracedecay_code_extraction::PhpExtractor;
#[cfg(feature = "lang-powershell")]
pub use tracedecay_code_extraction::PowerShellExtractor;
#[cfg(feature = "lang-protobuf")]
pub use tracedecay_code_extraction::ProtoExtractor;
#[cfg(feature = "lang-quint")]
pub use tracedecay_code_extraction::QuintExtractor;
#[cfg(feature = "lang-r")]
pub use tracedecay_code_extraction::RExtractor;
#[cfg(feature = "lang-ruby")]
pub use tracedecay_code_extraction::RubyExtractor;
#[cfg(feature = "lang-sql")]
pub use tracedecay_code_extraction::SqlExtractor;
#[cfg(feature = "lang-toml")]
pub use tracedecay_code_extraction::TomlExtractor;
#[cfg(feature = "lang-vbnet")]
pub use tracedecay_code_extraction::VbNetExtractor;
#[cfg(feature = "lang-wgsl")]
pub use tracedecay_code_extraction::WgslExtractor;
#[cfg(feature = "lang-zig")]
pub use tracedecay_code_extraction::ZigExtractor;
#[cfg(feature = "lang-qbasic")]
pub use tracedecay_code_extraction::{QBasicExtractor, QuickBasicExtractor};
