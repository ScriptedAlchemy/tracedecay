//! Transport-neutral surface request contracts used by the daemon invocation
//! wire and by HTTP/MCP adapters.
//!
//! These types are data only: no catalog composition, no daemon internals, and
//! no socket I/O. Conversion helpers that need `tracedecay-usecases` stay with
//! the adapter that already depends on that crate.

pub mod callable_code;
pub mod native_integration;

pub use callable_code::{
    CallableCodeSurfaceMeta, CallableCodeSurfaceRequest, CodeCalleesSurfaceRequest,
    CodeCallersSurfaceRequest, CodeExactOccurrenceSurfaceRequest, CodeFacetSurfaceRequest,
    CodeImplementationsSurfaceRequest, CodeNavigationSurfaceRequest,
    CodePhraseSearchSurfaceRequest, CodeSignatureSearchSurfaceRequest,
    CodeSymbolSearchSurfaceRequest, CodeTimelineSurfaceRequest, CodeTypeHierarchySurfaceRequest,
    PrimitiveCodeSurfaceRequest, primitive_code_into_primitive,
};
pub use native_integration::NativeIntegrationSurfaceRequest;
