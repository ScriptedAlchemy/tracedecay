"""File-backed (mmap) loading of a bf16 Qwen3.5 base for Kev on a RAM-contended CPU box.

kev.model calls AutoModelForCausalLM.from_pretrained(name, revision=..., dtype=..., attn_implementation=...).model.
This shim builds the text model without initialising weights and points every parameter at the safetensors file
through a copy-on-write mmap, so the 8 GB of base weights are page cache (reclaimable) instead of anonymous memory.
Requires the adapter to stay unmerged (KEV_MERGE=0): merging would write to, and so copy, every adapted weight."""
import json, mmap, os, struct
import torch, torch.nn as nn
from huggingface_hub import snapshot_download
from transformers import AutoConfig, AutoModelForCausalLM
from transformers.initialization import no_init_weights

DT = {"BF16": torch.bfloat16, "F16": torch.float16, "F32": torch.float32}
_KEEP = []   # mmaps must outlive the tensors

def _tensors(path):
    out = {}
    for fn in sorted(os.listdir(path)):
        if not fn.endswith(".safetensors"): continue
        f = open(os.path.join(path, fn), "rb")
        n = struct.unpack("<Q", f.read(8))[0]; hdr = json.loads(f.read(n))
        mm = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_COPY); _KEEP.append((f, mm))
        base = 8 + n
        for k, v in hdr.items():
            if k == "__metadata__": continue
            a, b = v["data_offsets"]; dt = DT[v["dtype"]]
            t = torch.frombuffer(mm, dtype=dt, count=(b - a) // dt.itemsize, offset=base + a).view(v["shape"])
            out[k] = t
    return out

class Shim:
    @staticmethod
    def from_pretrained(name, revision=None, dtype=torch.bfloat16, attn_implementation=None, **kw):
        assert dtype == torch.bfloat16, "mmap path is bf16 only"
        from huggingface_hub.constants import HF_HUB_CACHE
        snaps = os.path.join(HF_HUB_CACHE, "models--" + name.replace("/", "--"), "snapshots")
        path = next(os.path.join(snaps, d) for d in os.listdir(snaps) if revision is None or d.startswith(revision))
        cfg = AutoConfig.from_pretrained(path)
        tcfg = getattr(cfg, "text_config", cfg)
        with no_init_weights():
            m = AutoModelForCausalLM.from_config(tcfg, dtype=dtype, attn_implementation=attn_implementation)
        src = _tensors(path)
        missing = []
        for full, p in list(m.model.named_parameters()):
            key = None
            for cand in ("model.language_model." + full, "model." + full, full):
                if cand in src: key = cand; break
            if key is None: missing.append(full); continue
            t = src[key]
            assert tuple(t.shape) == tuple(p.shape), (full, t.shape, p.shape)
            if t.dtype != p.dtype: t = t.to(p.dtype)
            mod = m.model.get_submodule(full.rsplit(".", 1)[0]) if "." in full else m.model
            mod._parameters[full.rsplit(".", 1)[-1]] = nn.Parameter(t, requires_grad=False)
        if missing: raise RuntimeError("mmap loader: no tensor for %s" % missing[:5])
        m.lm_head = None   # tied to embed_tokens; kev reads .model only
        m.eval()
        return m

def install():
    import kev.model
    kev.model.AutoModelForCausalLM = Shim


def load_full_mmap(cls, path, dtype=torch.bfloat16, attn_implementation=None, allow_missing=()):
    """cls (e.g. Qwen3_5ForConditionalGeneration) built from path's config without init, every parameter pointed at
    the safetensors files through a copy-on-write mmap. Tied lm_head is re-tied to the embedding."""
    cfg = AutoConfig.from_pretrained(path)
    with no_init_weights():
        m = cls._from_config(cfg, dtype=dtype, attn_implementation=attn_implementation)
    src = _tensors(path)
    missing, used = [], set()
    for full, p in list(m.named_parameters(remove_duplicate=False)):
        if full not in src:
            missing.append(full); continue
        t = src[full]; used.add(full)
        assert tuple(t.shape) == tuple(p.shape), (full, t.shape, p.shape)
        if t.dtype != p.dtype: t = t.to(p.dtype)
        mod = m.get_submodule(full.rsplit(".", 1)[0])
        mod._parameters[full.rsplit(".", 1)[-1]] = nn.Parameter(t, requires_grad=False)
    tie = getattr(getattr(cfg, "text_config", cfg), "tie_word_embeddings", False) or getattr(cfg, "tie_word_embeddings", False)
    if "lm_head.weight" in missing and tie:
        m.lm_head.weight = m.get_input_embeddings().weight; missing.remove("lm_head.weight")
    skipped = [k for k in missing if k.startswith(tuple(allow_missing))]
    missing = [k for k in missing if k not in skipped]
    if skipped: print("mmap loader: %d params left uninitialised (allowed, unused for text-only input): %s" % (len(skipped), skipped[:3]), flush=True)
    if missing: raise RuntimeError("mmap loader: no tensor for %d params, e.g. %s" % (len(missing), missing[:5]))
    unused = sorted(set(src) - used)
    return m.eval(), unused
