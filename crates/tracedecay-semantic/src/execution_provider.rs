//! GPU execution-provider selection for the FastEmbed/ORT session builder
//! (CoreML on macOS, CUDA or WebGPU on Linux).
//!
//! A compiled `semantic-gpu-coreml` feature enables automatic CoreML on Apple
//! targets (MLProgram format, Metal GPU compute units, persistent compiled-model
//! cache — see `coreml_dispatch`), while `semantic-gpu-cuda` and
//! `semantic-gpu-webgpu` participate in the Linux auto ladder.
//! `TRACEDECAY_EMBED_EXECUTION_PROVIDER=cpu` opts out; `coreml`, `cuda`, or
//! `webgpu` explicitly requests that provider.
//!
//! An unavailable automatic provider is normal and quietly falls back to
//! ONNX Runtime's CPU provider. An unavailable explicitly requested provider
//! also falls back, but warns the operator. This module only narrows to CPU;
//! it never fails a session open.

use std::path::Path;

use fastembed::ExecutionProviderDispatch;
use tracedecay_domain::EmbeddingExecutionProviderV1;

/// `auto` (default), `coreml`, `cuda`, `webgpu`, or `cpu`.
const EXECUTION_PROVIDER_ENV: &str = "TRACEDECAY_EMBED_EXECUTION_PROVIDER";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestedExecutionProviderV1 {
    Auto,
    Cpu,
    CoreMl,
    Cuda,
    WebGpu,
}

fn requested_execution_provider() -> RequestedExecutionProviderV1 {
    match std::env::var(EXECUTION_PROVIDER_ENV) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => RequestedExecutionProviderV1::Auto,
            "cpu" => RequestedExecutionProviderV1::Cpu,
            "coreml" => RequestedExecutionProviderV1::CoreMl,
            "cuda" => RequestedExecutionProviderV1::Cuda,
            "webgpu" => RequestedExecutionProviderV1::WebGpu,
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
#[cfg(test)]
pub(crate) fn requested_execution_providers() -> Vec<ExecutionProviderDispatch> {
    execution_providers(resolved_execution_provider(), None)
}

pub(crate) fn resolved_execution_provider() -> EmbeddingExecutionProviderV1 {
    let provider = match requested_execution_provider() {
        RequestedExecutionProviderV1::Auto => automatic_provider(),
        RequestedExecutionProviderV1::Cpu => EmbeddingExecutionProviderV1::Cpu,
        RequestedExecutionProviderV1::CoreMl => {
            coreml_provider(true).unwrap_or(EmbeddingExecutionProviderV1::Cpu)
        }
        RequestedExecutionProviderV1::Cuda => {
            cuda_provider(true).unwrap_or(EmbeddingExecutionProviderV1::Cpu)
        }
        RequestedExecutionProviderV1::WebGpu => {
            webgpu_provider(true).unwrap_or(EmbeddingExecutionProviderV1::Cpu)
        }
    };
    crate::hotpath_observe::record_embed_execution_provider(match provider {
        EmbeddingExecutionProviderV1::Cpu => "cpu",
        EmbeddingExecutionProviderV1::CoreMl => "coreml",
        EmbeddingExecutionProviderV1::Cuda => "cuda",
        EmbeddingExecutionProviderV1::WebGpu => "webgpu",
    });
    provider
}

fn automatic_provider() -> EmbeddingExecutionProviderV1 {
    if cfg!(all(
        feature = "semantic-gpu-coreml",
        target_vendor = "apple"
    )) && let Some(provider) = coreml_provider(false)
    {
        return provider;
    }
    if cfg!(all(feature = "semantic-gpu-cuda", target_os = "linux"))
        && let Some(provider) = cuda_provider(false)
    {
        return provider;
    }
    if cfg!(feature = "semantic-gpu-webgpu")
        && let Some(provider) = webgpu_provider(false)
    {
        return provider;
    }
    EmbeddingExecutionProviderV1::Cpu
}

#[cfg(feature = "semantic-gpu-coreml")]
fn coreml_provider(explicit: bool) -> Option<EmbeddingExecutionProviderV1> {
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
        return None;
    }
    // `is_available` only reflects GetAvailableProviders, which is not a
    // usability signal for the statically linked CoreML in pyke's
    // aarch64-apple-darwin distribution (measured on a Mac: probe false, EP
    // registers and runs 796/1012 nodes). Register on every Apple build and
    // let ORT fall back to CPU itself if registration fails.
    match provider.is_available() {
        Ok(true) => tracing::info!("using CoreML execution provider for embeddings"),
        Ok(false) => {
            if explicit {
                tracing::warn!(
                    "CoreML GetAvailableProviders probe returned false; still registering CoreML (ORT falls back to CPU if registration fails)"
                );
            } else {
                tracing::info!(
                    "CoreML GetAvailableProviders probe returned false; still registering CoreML"
                );
            }
        }
        Err(error) => {
            if explicit {
                tracing::warn!(%error, "CoreML availability probe failed; still registering CoreML");
            } else {
                tracing::info!(%error, "CoreML availability probe failed; still registering CoreML");
            }
        }
    }
    Some(EmbeddingExecutionProviderV1::CoreMl)
}

#[cfg(not(feature = "semantic-gpu-coreml"))]
fn coreml_provider(_explicit: bool) -> Option<EmbeddingExecutionProviderV1> {
    tracing::warn!(
        "TRACEDECAY_EMBED_EXECUTION_PROVIDER=coreml requested but this build was compiled without the semantic-gpu-coreml feature; using cpu"
    );
    None
}

#[cfg(feature = "semantic-gpu-cuda")]
fn cuda_provider(explicit: bool) -> Option<EmbeddingExecutionProviderV1> {
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
        return None;
    }
    match provider.is_available() {
        Ok(true) => {
            tracing::info!("using CUDA execution provider for embeddings");
            Some(EmbeddingExecutionProviderV1::Cuda)
        }
        Ok(false) => {
            if explicit {
                tracing::warn!(
                    "the CUDA execution provider is unavailable in this ONNX Runtime build (no CUDA driver/toolkit found); using cpu"
                );
            } else {
                tracing::info!("CUDA execution provider is unavailable; using cpu");
            }
            None
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
            None
        }
    }
}

#[cfg(not(feature = "semantic-gpu-cuda"))]
fn cuda_provider(_explicit: bool) -> Option<EmbeddingExecutionProviderV1> {
    tracing::warn!(
        "TRACEDECAY_EMBED_EXECUTION_PROVIDER=cuda requested but this build was compiled without the semantic-gpu-cuda feature; using cpu"
    );
    None
}

#[cfg(feature = "semantic-gpu-webgpu")]
fn webgpu_provider(explicit: bool) -> Option<EmbeddingExecutionProviderV1> {
    use ort::execution_providers::{ExecutionProvider, WebGPU};

    let provider = WebGPU::default();
    if !provider.supported_by_platform() {
        if explicit {
            tracing::warn!(
                "TRACEDECAY_EMBED_EXECUTION_PROVIDER=webgpu requested on an unsupported platform; using cpu"
            );
        } else {
            tracing::debug!("WebGPU execution provider is unsupported; using cpu");
        }
        return None;
    }
    match provider.is_available() {
        Ok(true) => {
            tracing::info!("using WebGPU execution provider for embeddings");
            Some(EmbeddingExecutionProviderV1::WebGpu)
        }
        Ok(false) => {
            if explicit {
                tracing::warn!(
                    "the WebGPU execution provider is unavailable in this ONNX Runtime build; using cpu"
                );
            } else {
                tracing::info!("WebGPU execution provider is unavailable; using cpu");
            }
            None
        }
        Err(error) => {
            if explicit {
                tracing::warn!(
                    %error,
                    "failed to probe WebGPU execution provider availability; using cpu"
                );
            } else {
                tracing::info!(
                    %error,
                    "failed to probe WebGPU execution provider availability; using cpu"
                );
            }
            None
        }
    }
}

#[cfg(not(feature = "semantic-gpu-webgpu"))]
fn webgpu_provider(_explicit: bool) -> Option<EmbeddingExecutionProviderV1> {
    tracing::warn!(
        "TRACEDECAY_EMBED_EXECUTION_PROVIDER=webgpu requested but this build was compiled without the semantic-gpu-webgpu feature; using cpu"
    );
    None
}

/// Execution providers to register for `provider`. `coreml_cache_dir` is
/// where the CoreML EP persists its compiled model; without it ORT recompiles
/// the captured subgraphs on every session open, which the ORT docs put at
/// "even minutes" for a model this size.
pub(crate) fn execution_providers(
    provider: EmbeddingExecutionProviderV1,
    coreml_cache_dir: Option<&Path>,
) -> Vec<ExecutionProviderDispatch> {
    match provider {
        EmbeddingExecutionProviderV1::Cpu => Vec::new(),
        EmbeddingExecutionProviderV1::CoreMl => coreml_dispatch(coreml_cache_dir),
        EmbeddingExecutionProviderV1::Cuda => cuda_dispatch(),
        EmbeddingExecutionProviderV1::WebGpu => webgpu_dispatch(),
    }
}

#[cfg(feature = "semantic-gpu-coreml")]
fn coreml_dispatch(cache_dir: Option<&Path>) -> Vec<ExecutionProviderDispatch> {
    use ort::ep::coreml::{ComputeUnits, ModelFormat};
    use ort::execution_providers::CoreML;

    // `CoreML::default()` is the wrong shape for a BERT-style encoder:
    // - the default `NeuralNetwork` format has no LayerNormalization, Gelu,
    //   Erf or ReduceMean and only accepts MatMul with a constant right-hand
    //   side, so every attention matmul, layer norm and GELU fell back to CPU
    //   (216 of 1012 nodes on the Mac dogfood) with a tensor copy at each of
    //   the resulting partition boundaries; `MLProgram` supports all of them;
    // - `ALL` compute units let CoreML route to the Neural Engine, which is
    //   poor for long-sequence attention; `CPUAndGPU` pins it to Metal.
    let mut provider = CoreML::default()
        .with_model_format(ModelFormat::MLProgram)
        .with_compute_units(ComputeUnits::CPUAndGPU);
    match cache_dir {
        Some(dir) => match std::fs::create_dir_all(dir) {
            Ok(()) => provider = provider.with_model_cache_dir(dir.display()),
            Err(error) => tracing::warn!(
                %error,
                dir = %dir.display(),
                "CoreML model cache directory is unavailable; every session open recompiles the model"
            ),
        },
        None => tracing::warn!(
            "no CoreML model cache directory for this artifact source; every session open recompiles the model"
        ),
    }
    vec![provider.build()]
}

#[cfg(not(feature = "semantic-gpu-coreml"))]
fn coreml_dispatch(_cache_dir: Option<&Path>) -> Vec<ExecutionProviderDispatch> {
    Vec::new()
}

#[cfg(feature = "semantic-gpu-cuda")]
fn cuda_dispatch() -> Vec<ExecutionProviderDispatch> {
    use ort::execution_providers::CUDA;
    vec![CUDA::default().build()]
}

#[cfg(not(feature = "semantic-gpu-cuda"))]
fn cuda_dispatch() -> Vec<ExecutionProviderDispatch> {
    Vec::new()
}

#[cfg(feature = "semantic-gpu-webgpu")]
fn webgpu_dispatch() -> Vec<ExecutionProviderDispatch> {
    use ort::execution_providers::WebGPU;
    vec![WebGPU::default().build()]
}

#[cfg(not(feature = "semantic-gpu-webgpu"))]
fn webgpu_dispatch() -> Vec<ExecutionProviderDispatch> {
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
    fn explicit_auto_matches_unset() {
        with_env(Some("auto"), || {
            assert_eq!(
                requested_execution_provider(),
                RequestedExecutionProviderV1::Auto
            );
        });
    }

    #[test]
    fn webgpu_is_recognized() {
        with_env(Some(" WebGPU "), || {
            assert_eq!(
                requested_execution_provider(),
                RequestedExecutionProviderV1::WebGpu
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
    fn auto_without_a_compiled_provider_uses_cpu() {
        with_env(None, || {
            if !cfg!(any(
                all(feature = "semantic-gpu-coreml", target_vendor = "apple"),
                all(feature = "semantic-gpu-cuda", target_os = "linux"),
                feature = "semantic-gpu-webgpu"
            )) {
                assert_eq!(
                    resolved_execution_provider(),
                    EmbeddingExecutionProviderV1::Cpu
                );
                assert!(requested_execution_providers().is_empty());
            }
        });
    }

    // CoreML is Apple-only: `Auto` must never resolve to it off Apple.
    #[test]
    fn auto_selects_coreml_only_on_apple() {
        with_env(None, || {
            if !cfg!(target_vendor = "apple") {
                assert_ne!(
                    resolved_execution_provider(),
                    EmbeddingExecutionProviderV1::CoreMl
                );
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

    #[test]
    fn requesting_webgpu_without_the_feature_falls_back_to_cpu() {
        with_env(Some("webgpu"), || {
            let providers = requested_execution_providers();
            if !cfg!(feature = "semantic-gpu-webgpu") {
                assert!(providers.is_empty());
            }
        });
    }
}
