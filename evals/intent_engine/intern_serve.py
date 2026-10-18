"""Launch Intern-Decision's own FastAPI service (/v1/decisions) from the untrusted clone, CPU, file-backed weights.
usage: python -I intern_serve.py PORT     env: INFERENCE_CONFIG (json), CALIBRATION_PATH (optional preset binding)"""
import os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.join(HERE, "ext", "intern_src", "Intern-Decision")
sys.path[:0] = [HERE, REPO]; sys.path.append(os.path.join(HERE, "ext", "intern_deps"))
import torch
import mmap_loader
import src.inference.hf_backend as hb

REAL = hb.Qwen3_5ForConditionalGeneration

class MmapQwen:
    """Stands in for Qwen3_5ForConditionalGeneration inside hf_backend: same weights, mmap-backed."""
    @staticmethod
    def from_pretrained(checkpoint, dtype=torch.bfloat16, local_files_only=True, attn_implementation=None, **kw):
        m, unused = mmap_loader.load_full_mmap(REAL, checkpoint, dtype, attn_implementation,
                                                  allow_missing=tuple(filter(None, os.environ.get("MMAP_ALLOW_MISSING", "").split(","))))
        print("mmap load: %d checkpoint tensors unused: %s" % (len(unused), unused[:5]), flush=True)
        return m

hb.Qwen3_5ForConditionalGeneration = MmapQwen

class TextOnlyProcessor:
    """The image/video processor needs torchvision, which this CPU venv lacks; text-only requests never use it
    (HFBackend.encode uses the tokenizer's chat template when a row has no images)."""
    @staticmethod
    def from_pretrained(*a, **kw):
        import types
        return types.SimpleNamespace()

hb.AutoProcessor = TextOnlyProcessor

if os.environ.get("DELTANET_FTZ", "1") == "1":
    # CPU-only speed fix, not a model change: this checkpoint's Gated DeltaNet decays drive the torch fallback's fp32
    # intermediates into denormals, which x86 computes ~30x slower. torch.set_flush_denormal only reaches the calling
    # thread, so the fallback runs single-threaded with flush-to-zero (3.4 s -> 0.09 s per layer call at 300 tokens).
    import transformers.models.qwen3_5.modeling_qwen3_5 as mq
    _orig = mq.torch_chunk_gated_delta_rule
    def _ftz(*a, **k):
        n = torch.get_num_threads(); torch.set_num_threads(1); torch.set_flush_denormal(True)
        try: return _orig(*a, **k)
        finally: torch.set_num_threads(n)
    mq.torch_chunk_gated_delta_rule = _ftz
import uvicorn
from src.service.app import app
uvicorn.run(app, host="127.0.0.1", port=int(sys.argv[1]))
