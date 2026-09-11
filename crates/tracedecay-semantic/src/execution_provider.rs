//! GPU execution-provider selection for the FastEmbed/ORT session builder
//! (CoreML on macOS, CUDA on Linux).
//!
//! A compiled `semantic-gpu-coreml` feature enables automatic CoreML probing
//! on Apple targets, while `semantic-gpu-cuda` enables automatic CUDA probing
//! on Linux. `TRACEDECAY_EMBED_EXECUTION_PROVIDER=cpu` opts out; `coreml` or
//! `cuda` explicitly requests that provider.
//!
//! On a supported platform the compiled provider is always offered to ORT.
//! ORT itself falls back to CPU if registration fails. A failed
//! `GetAvailableProviders` probe is logged but no longer blocks registration
//! (that probe is not a reliable usability signal for statically linked
//! CoreML). This module only narrows to CPU; it never fails a session open.

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
    let providers = match requested_execution_provider() {
        RequestedExecutionProviderV1::Auto => automatic_dispatch(),
        RequestedExecutionProviderV1::Cpu => Vec::new(),
        RequestedExecutionProviderV1::CoreMl => coreml_dispatch(true),
        RequestedExecutionProviderV1::Cuda => cuda_dispatch(true),
    };
    if providers.is_empty() {
        crate::hotpath_observe::record_embed_execution_provider("cpu");
    }
    providers
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
    // `is_available` only checks GetAvailableProviders. ort's own docs say that is
    // not the right gate for usability — register the EP and let ORT fall back if
    // registration fails. Pyke's aarch64-apple-darwin "none" dfbin ships CoreML
    // inside libonnxruntime.a; gating on the probe previously skipped a working EP
    // whenever the probe disagreed with the linked provider table.
    match provider.is_available() {
        Ok(true) => tracing::info!("using CoreML execution provider for embeddings"),
        Ok(false) => {
            if explicit {
                tracing::warn!(
                    "CoreML GetAvailableProviders probe returned false; still registering CoreML (ORT will fall back to CPU if registration fails)"
                );
            } else {
                tracing::info!(
                    "CoreML GetAvailableProviders probe returned false; still registering CoreML"
                );
            }
        }
        Err(error) => {
            if explicit {
                tracing::warn!(
                    %error,
                    "CoreML availability probe failed; still registering CoreML"
                );
            } else {
                tracing::info!(
                    %error,
                    "CoreML availability probe failed; still registering CoreML"
                );
            }
        }
    }
    crate::hotpath_observe::record_embed_execution_provider("coreml");
    vec![provider.build()]
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
            crate::hotpath_observe::record_embed_execution_provider("cuda");
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
    fn auto_on_apple_with_coreml_feature_registers_coreml() {
        with_env(None, || {
            if cfg!(all(
                feature = "semantic-gpu-coreml",
                target_vendor = "apple"
            )) {
                assert_eq!(
                    requested_execution_providers().len(),
                    1,
                    "Apple + semantic-gpu-coreml must offer CoreML to ORT"
                );
            }
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
