//! Opt-in GPU execution-provider selection for the FastEmbed/ORT session
//! builder (owner request: CoreML on macOS, CUDA on Linux — both strictly
//! opt-in over the default CPU-only build).
//!
//! Two independent switches must both agree before anything other than ONNX
//! Runtime's own default CPU execution provider is even attempted:
//!
//! 1. **Compile-time**: the `semantic-gpu-coreml` / `semantic-gpu-cuda`
//!    cargo feature (off in `default`; each forwards to the matching `ort`
//!    execution-provider feature so the *same* compiled `ort` instance
//!    FastEmbed's session builder uses gets it too — see the crate's
//!    `Cargo.toml`).
//! 2. **Run-time**: `TRACEDECAY_EMBED_EXECUTION_PROVIDER=coreml|cuda`. Unset,
//!    empty, `cpu`, or any unrecognized value all mean CPU.
//!
//! With either switch off, or the platform/host/runtime unable to serve the
//! requested provider, [`requested_execution_providers`] returns an empty
//! list — FastEmbed/ORT's own default CPU EP — after one `tracing::warn!`.
//! This module can only narrow the requested provider back to CPU; it never
//! produces an error, so a GPU-provider mismatch can never fail a session
//! open. ONNX Runtime itself layers its own CPU fallback beneath this: even
//! a provider this module *did* admit (host reported it available, driver
//! then failed to initialize at session-build time) still degrades to CPU
//! rather than failing the session.

use fastembed::ExecutionProviderDispatch;

/// `coreml`, `cuda`, or `cpu` (default, and every unrecognized value).
const EXECUTION_PROVIDER_ENV: &str = "TRACEDECAY_EMBED_EXECUTION_PROVIDER";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestedExecutionProviderV1 {
    Cpu,
    CoreMl,
    Cuda,
}

fn requested_execution_provider() -> RequestedExecutionProviderV1 {
    match std::env::var(EXECUTION_PROVIDER_ENV) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "" | "cpu" => RequestedExecutionProviderV1::Cpu,
            "coreml" => RequestedExecutionProviderV1::CoreMl,
            "cuda" => RequestedExecutionProviderV1::Cuda,
            _ => {
                tracing::warn!(
                    env = EXECUTION_PROVIDER_ENV,
                    value = %value,
                    "unrecognized embedding execution provider requested; using cpu"
                );
                RequestedExecutionProviderV1::Cpu
            }
        },
        Err(_) => RequestedExecutionProviderV1::Cpu,
    }
}

/// Execution providers to register on the FastEmbed session builder, most
/// preferred first. An empty vector means ONNX Runtime's own default CPU EP,
/// which is always what a default (no env, no GPU feature) build produces.
pub(crate) fn requested_execution_providers() -> Vec<ExecutionProviderDispatch> {
    match requested_execution_provider() {
        RequestedExecutionProviderV1::Cpu => Vec::new(),
        RequestedExecutionProviderV1::CoreMl => coreml_dispatch(),
        RequestedExecutionProviderV1::Cuda => cuda_dispatch(),
    }
}

#[cfg(feature = "semantic-gpu-coreml")]
fn coreml_dispatch() -> Vec<ExecutionProviderDispatch> {
    use ort::execution_providers::{CoreML, ExecutionProvider};

    // `CoreML::supported_by_platform()` is `cfg!(target_vendor = "apple")` in
    // `ort` itself; checking it here (rather than only inside `register`)
    // keeps the WARN in our own log shape instead of ORT's silent
    // `RegisterError::MissingFeature` path.
    let provider = CoreML::default();
    if !provider.supported_by_platform() {
        tracing::warn!(
            "TRACEDECAY_EMBED_EXECUTION_PROVIDER=coreml requested on a non-Apple build; using cpu"
        );
        return Vec::new();
    }
    match provider.is_available() {
        Ok(true) => vec![provider.build()],
        Ok(false) => {
            tracing::warn!(
                "the CoreML execution provider is unavailable in this ONNX Runtime build; using cpu"
            );
            Vec::new()
        }
        Err(error) => {
            tracing::warn!(
                %error,
                "failed to probe CoreML execution provider availability; using cpu"
            );
            Vec::new()
        }
    }
}

#[cfg(not(feature = "semantic-gpu-coreml"))]
fn coreml_dispatch() -> Vec<ExecutionProviderDispatch> {
    tracing::warn!(
        "TRACEDECAY_EMBED_EXECUTION_PROVIDER=coreml requested but this build was compiled without the semantic-gpu-coreml feature; using cpu"
    );
    Vec::new()
}

#[cfg(feature = "semantic-gpu-cuda")]
fn cuda_dispatch() -> Vec<ExecutionProviderDispatch> {
    use ort::execution_providers::{CUDA, ExecutionProvider};

    let provider = CUDA::default();
    if !provider.supported_by_platform() {
        tracing::warn!(
            "TRACEDECAY_EMBED_EXECUTION_PROVIDER=cuda requested on an unsupported platform; using cpu"
        );
        return Vec::new();
    }
    match provider.is_available() {
        Ok(true) => vec![provider.build()],
        Ok(false) => {
            tracing::warn!(
                "the CUDA execution provider is unavailable in this ONNX Runtime build (no CUDA driver/toolkit found); using cpu"
            );
            Vec::new()
        }
        Err(error) => {
            tracing::warn!(
                %error,
                "failed to probe CUDA execution provider availability; using cpu"
            );
            Vec::new()
        }
    }
}

#[cfg(not(feature = "semantic-gpu-cuda"))]
fn cuda_dispatch() -> Vec<ExecutionProviderDispatch> {
    tracing::warn!(
        "TRACEDECAY_EMBED_EXECUTION_PROVIDER=cuda requested but this build was compiled without the semantic-gpu-cuda feature; using cpu"
    );
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `std::env::var` is process-global state; serialize the tests that
    // touch `EXECUTION_PROVIDER_ENV` so they cannot interleave.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env<R>(value: Option<&str>, body: impl FnOnce() -> R) -> R {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let previous = std::env::var(EXECUTION_PROVIDER_ENV).ok();
        match value {
            Some(value) => unsafe { std::env::set_var(EXECUTION_PROVIDER_ENV, value) },
            None => unsafe { std::env::remove_var(EXECUTION_PROVIDER_ENV) },
        }
        let result = body();
        match previous {
            Some(previous) => unsafe { std::env::set_var(EXECUTION_PROVIDER_ENV, previous) },
            None => unsafe { std::env::remove_var(EXECUTION_PROVIDER_ENV) },
        }
        result
    }

    #[test]
    fn unset_env_defaults_to_cpu() {
        with_env(None, || {
            assert_eq!(
                requested_execution_provider(),
                RequestedExecutionProviderV1::Cpu
            );
        });
    }

    #[test]
    fn explicit_cpu_is_cpu() {
        with_env(Some("cpu"), || {
            assert_eq!(
                requested_execution_provider(),
                RequestedExecutionProviderV1::Cpu
            );
        });
    }

    #[test]
    fn coreml_is_case_and_whitespace_insensitive() {
        with_env(Some(" CoreML \n"), || {
            assert_eq!(
                requested_execution_provider(),
                RequestedExecutionProviderV1::CoreMl
            );
        });
    }

    #[test]
    fn cuda_is_recognized() {
        with_env(Some("cuda"), || {
            assert_eq!(
                requested_execution_provider(),
                RequestedExecutionProviderV1::Cuda
            );
        });
    }

    #[test]
    fn unrecognized_value_falls_back_to_cpu() {
        with_env(Some("rocm"), || {
            assert_eq!(
                requested_execution_provider(),
                RequestedExecutionProviderV1::Cpu
            );
        });
    }

    #[test]
    fn default_env_produces_no_execution_providers() {
        with_env(None, || {
            assert!(requested_execution_providers().is_empty());
        });
    }

    // Whichever GPU feature is or isn't compiled in, requesting the *other*
    // provider must still resolve without panicking and must never register
    // an execution provider it does not have compiled support for.
    #[test]
    fn requesting_coreml_without_the_feature_or_platform_falls_back_to_cpu() {
        with_env(Some("coreml"), || {
            let providers = requested_execution_providers();
            if !cfg!(all(
                feature = "semantic-gpu-coreml",
                target_vendor = "apple"
            )) {
                assert!(providers.is_empty());
            }
        });
    }

    #[test]
    fn requesting_cuda_without_the_feature_falls_back_to_cpu() {
        with_env(Some("cuda"), || {
            let providers = requested_execution_providers();
            if !cfg!(feature = "semantic-gpu-cuda") {
                assert!(providers.is_empty());
            }
        });
    }
}
