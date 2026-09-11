//! GPU execution-provider selection for the FastEmbed/ORT session builder
//! (CoreML on macOS, CUDA on Linux).
//!
//! A compiled `semantic-gpu-coreml` feature enables automatic CoreML probing
//! on Apple targets, while `semantic-gpu-cuda` enables automatic CUDA probing
//! on Linux. `TRACEDECAY_EMBED_EXECUTION_PROVIDER=cpu` opts out; `coreml` or
//! `cuda` explicitly requests that provider.
//!
//! An unavailable automatic provider is normal and quietly falls back to
//! ONNX Runtime's CPU provider. An unavailable explicitly requested provider
//! also falls back, but warns the operator. This module only narrows to CPU;
//! it never fails a session open.

use fastembed::ExecutionProviderDispatch;

/// `auto` (default), `coreml`, `cuda`, or `cpu`.
const EXECUTION_PROVIDER_ENV: &str = "TRACEDECAY_EMBED_EXECUTION_PROVIDER";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestedExecutionProviderV1 {
    Auto,
    Cpu,
    CoreMl,
    Cuda,
}

fn requested_execution_provider() -> RequestedExecutionProviderV1 {
    match std::env::var(EXECUTION_PROVIDER_ENV) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "" => RequestedExecutionProviderV1::Auto,
            "cpu" => RequestedExecutionProviderV1::Cpu,
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
        Err(_) => RequestedExecutionProviderV1::Auto,
    }
}

/// Execution providers to register on the FastEmbed session builder, most
/// preferred first. An empty vector means ONNX Runtime's own default CPU EP,
/// which is what a build without a usable automatic provider produces.
pub(crate) fn requested_execution_providers() -> Vec<ExecutionProviderDispatch> {
    match requested_execution_provider() {
        RequestedExecutionProviderV1::Auto => automatic_dispatch(),
        RequestedExecutionProviderV1::Cpu => Vec::new(),
        RequestedExecutionProviderV1::CoreMl => coreml_dispatch(true),
        RequestedExecutionProviderV1::Cuda => cuda_dispatch(true),
    }
}

fn automatic_dispatch() -> Vec<ExecutionProviderDispatch> {
    if cfg!(all(
        feature = "semantic-gpu-coreml",
        target_vendor = "apple"
    )) {
        coreml_dispatch(false)
    } else if cfg!(all(feature = "semantic-gpu-cuda", target_os = "linux")) {
        cuda_dispatch(false)
    } else {
        Vec::new()
    }
}

#[cfg(feature = "semantic-gpu-coreml")]
fn coreml_dispatch(explicit: bool) -> Vec<ExecutionProviderDispatch> {
    use ort::execution_providers::{CoreML, ExecutionProvider};

    let provider = CoreML::default();
    if !provider.supported_by_platform() {
        if explicit {
            tracing::warn!(
                "TRACEDECAY_EMBED_EXECUTION_PROVIDER=coreml requested on a non-Apple build; using cpu"
            );
        } else {
            tracing::debug!("CoreML execution provider is unsupported; using cpu");
        }
        return Vec::new();
    }
    match provider.is_available() {
        Ok(true) => {
            tracing::info!("using CoreML execution provider for embeddings");
            vec![provider.build()]
        }
        Ok(false) => {
            if explicit {
                tracing::warn!(
                    "the CoreML execution provider is unavailable in this ONNX Runtime build; using cpu"
                );
            } else {
                tracing::info!("CoreML execution provider is unavailable; using cpu");
            }
            Vec::new()
        }
        Err(error) => {
            if explicit {
                tracing::warn!(
                    %error,
                    "failed to probe CoreML execution provider availability; using cpu"
                );
            } else {
                tracing::info!(
                    %error,
                    "failed to probe CoreML execution provider availability; using cpu"
                );
            }
            Vec::new()
        }
    }
}

#[cfg(not(feature = "semantic-gpu-coreml"))]
fn coreml_dispatch(_explicit: bool) -> Vec<ExecutionProviderDispatch> {
    tracing::warn!(
        "TRACEDECAY_EMBED_EXECUTION_PROVIDER=coreml requested but this build was compiled without the semantic-gpu-coreml feature; using cpu"
    );
    Vec::new()
}

#[cfg(feature = "semantic-gpu-cuda")]
fn cuda_dispatch(explicit: bool) -> Vec<ExecutionProviderDispatch> {
    use ort::execution_providers::{CUDA, ExecutionProvider};

    let provider = CUDA::default();
    if !provider.supported_by_platform() {
        if explicit {
            tracing::warn!(
                "TRACEDECAY_EMBED_EXECUTION_PROVIDER=cuda requested on an unsupported platform; using cpu"
            );
        } else {
            tracing::debug!("CUDA execution provider is unsupported; using cpu");
        }
        return Vec::new();
    }
    match provider.is_available() {
        Ok(true) => {
            tracing::info!("using CUDA execution provider for embeddings");
            vec![provider.build()]
        }
        Ok(false) => {
            if explicit {
                tracing::warn!(
                    "the CUDA execution provider is unavailable in this ONNX Runtime build (no CUDA driver/toolkit found); using cpu"
                );
            } else {
                tracing::info!("CUDA execution provider is unavailable; using cpu");
            }
            Vec::new()
        }
        Err(error) => {
            if explicit {
                tracing::warn!(
                    %error,
                    "failed to probe CUDA execution provider availability; using cpu"
                );
            } else {
                tracing::info!(
                    %error,
                    "failed to probe CUDA execution provider availability; using cpu"
                );
            }
            Vec::new()
        }
    }
}

#[cfg(not(feature = "semantic-gpu-cuda"))]
fn cuda_dispatch(_explicit: bool) -> Vec<ExecutionProviderDispatch> {
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
    fn unset_or_empty_env_defaults_to_auto() {
        with_env(None, || {
            assert_eq!(
                requested_execution_provider(),
                RequestedExecutionProviderV1::Auto
            );
        });
        with_env(Some(" \n"), || {
            assert_eq!(
                requested_execution_provider(),
                RequestedExecutionProviderV1::Auto
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
    fn auto_without_a_platform_provider_uses_cpu() {
        with_env(None, || {
            if !cfg!(any(
                all(feature = "semantic-gpu-coreml", target_vendor = "apple"),
                all(feature = "semantic-gpu-cuda", target_os = "linux")
            )) {
                assert!(requested_execution_providers().is_empty());
            }
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
