"""Launch kev.serve from the untrusted clone without running Python from inside it.
usage: python -I kev_serve.py --run jaredpalmer/kev-4b --port 8009 --device cpu
env: KEV_DTYPE=bf16 (CPU RAM), KEV_MMAP=1 (file-backed base weights via mmap_loader; implies KEV_MERGE=0)"""
import os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path[:0] = [HERE, os.path.join(HERE, "ext", "kev_src", "kev"), os.path.join(HERE, "ext", "kev_deps")]
if os.environ.get("KEV_MMAP") == "1":
    os.environ["KEV_MERGE"] = "0"
    import mmap_loader; mmap_loader.install()
from kev import serve
sys.argv = ["kev.serve"] + sys.argv[1:]
serve.main()
