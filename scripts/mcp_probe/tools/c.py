"""C probe inputs for scripts/mcp_probe/probe.py.

Mirrors the shape of `tools/rust.py`: returns `{tool_name: [args_dict, ...]}`
with exactly 5 query variants per tool. Tools that don't apply to C (impls,
derives, inheritance — C has no method tables or trait edges) still get five
plausible inputs so the matrix surfaces them as EMPTY rather than skipping
them silently.
"""

DEFAULT_NAMES = ["main", "init", "free", "malloc", "printf"]
DEFAULT_TYPE_NAMES = ["size_t", "FILE", "int", "char", "void"]
DEFAULT_TASKS = [
    "error handling",
    "memory allocation",
    "string parsing",
    "I/O buffering",
    "thread synchronization",
]
DEFAULT_PATHS = ["src", "include", "lib", "tests", "test"]


def probes_for(d):
    ids = d["ids"]
    qns = d["qnames"]
    names = d["names"] or DEFAULT_NAMES
    files = d["files"]
    return {
        # core
        "tracedecay_search":            [{"query": q} for q in ["main", "init", "free", "malloc", "printf"]],
        "tracedecay_context":           [{"task": t} for t in DEFAULT_TASKS],
        "tracedecay_node":              [{"node_id": i} for i in ids],
        "tracedecay_by_qualified_name": [{"qualified_name": q} for q in qns],
        "tracedecay_signature":         [{"node_id": i} for i in ids],
        "tracedecay_body":              [{"symbol": n} for n in names],
        # traversal
        "tracedecay_callers":           [{"node_id": i} for i in ids],
        "tracedecay_callees":           [{"node_id": i} for i in ids],
        "tracedecay_callers_for":       [{"node_ids": [i]} for i in ids],
        "tracedecay_impls":             [{"name": n} for n in DEFAULT_TYPE_NAMES],
        "tracedecay_derives":           [{"qualified_name": q} for q in qns],
        "tracedecay_type_hierarchy":    [{"node_id": i} for i in ids],
        "tracedecay_similar":           [{"symbol": n} for n in names],
        "tracedecay_rank":              [{"edge_kind": k} for k in
                                        ["calls", "uses", "contains", "type_of", "returns"]],
        "tracedecay_impact":            [{"node_id": i} for i in ids],
        "tracedecay_rename_preview":    [{"node_id": i, "new_name": "renamed"} for i in ids],
        # analysis (whole-DB sweeps)
        "tracedecay_hotspots":          [{}, {"limit": 10}, {"path": "src"}, {"path": "include"}, {"path": "lib"}],
        "tracedecay_complexity":        [{}, {"limit": 10}, {"path": "src"}, {"path": "lib"}, {"path": "tests"}],
        "tracedecay_dead_code":         [{}, {"limit": 10}, {"include_public": False},
                                        {"path": "src"}, {"path": "lib"}],
        "tracedecay_circular":          [{}, {"limit": 5}, {"path": "src"}, {"path": "lib"}, {"path": "include"}],
        "tracedecay_doc_coverage":      [{}, {"limit": 10}, {"path": "src"}, {"path": "include"}, {"path": files[0]}],
        "tracedecay_god_class":         [{}, {"limit": 10}, {"path": "src"}, {"path": "lib"}, {"path": "include"}],
        "tracedecay_dependency_depth":  [{}, {"limit": 10}, {"path": "src"}, {"path": "lib"}, {"path": "include"}],
        "tracedecay_inheritance_depth": [{}, {"limit": 10}, {"path": "src"}, {"path": "lib"}, {"path": "include"}],
        "tracedecay_distribution":      [{}, {"path": "src"}, {"path": "include"}, {"path": "lib"}, {"path": "tests"}],
        "tracedecay_gini":              [{}, {"path": "src"}, {"path": "include"}, {"path": "lib"}, {"path": "tests"}],
        "tracedecay_largest":           [{}, {"limit": 10}, {"path": "src"}, {"path": "include"}, {"path": "lib"}],
        "tracedecay_recursion":         [{}, {"limit": 10}, {"path": "src"}, {"path": "lib"}, {"path": "tests"}],
        "tracedecay_coupling":          [{}, {"path": "src"}, {"path": "include"}, {"path": "lib"}, {"path": files[0]}],
        "tracedecay_dsm":               [{}, {"path": "src"}, {"path": "include"}, {"path": "lib"}, {"path": "tests"}],
        "tracedecay_module_api":        [{"path": f} for f in files],
        "tracedecay_simplify_scan":     [{"files": [f]} for f in files],
        "tracedecay_unused_imports":    [{}, {"limit": 10}, {"path": "src"}, {"path": "include"}, {"path": "lib"}],
        "tracedecay_test_map":          [{"file": f} for f in files],
        "tracedecay_test_risk":         [{}, {"limit": 10}, {"path": "src"}, {"path": "lib"}, {"path": "tests"}],
        "tracedecay_todos":             [{}, {"limit": 10}, {"path": "src"}, {"path": "lib"}, {"path": "include"}],
        "tracedecay_files":             [{}, {"path": "src"}, {"path": "include"}, {"path": "lib"}, {"path": "tests"}],
        "tracedecay_status":            [{}] * 5,
        "tracedecay_health":            [{}] * 5,
        "tracedecay_diagnose":          [{"cargo_output": "warning: implicit declaration of function `foo` in src/lib.c:1:5"}] * 5,
        # port
        "tracedecay_port_status":       [
            {"source_dir": "src", "target_dir": "tests"},
            {"source_dir": "lib", "target_dir": "tests"},
            {"source_dir": "src", "target_dir": "include"},
            {"source_dir": "include", "target_dir": "src"},
            {"source_dir": "tests", "target_dir": "src"},
        ],
        "tracedecay_port_order":        [{"source_dir": d} for d in DEFAULT_PATHS],
        # git/branch
        "tracedecay_branch_list":       [{}] * 5,
        "tracedecay_branch_diff":       [
            {"base": "master", "head": "master"},
            {"base": "main", "head": "main"},
            {},
            {"base": "HEAD", "head": "HEAD"},
            {"base": "HEAD~1", "head": "HEAD"},
        ],
        "tracedecay_branch_search":     [
            {"branch": "master", "query": "main"},
            {"branch": "main", "query": "main"},
            {"branch": "master", "query": "init"},
            {"branch": "master", "query": "free"},
            {"branch": "main", "query": "malloc"},
        ],
        "tracedecay_changelog":         [
            {"from_ref": "HEAD~10", "to_ref": "HEAD"},
            {"from_ref": "HEAD~5", "to_ref": "HEAD"},
            {"from_ref": "HEAD~1", "to_ref": "HEAD"},
            {"from_ref": "HEAD~20", "to_ref": "HEAD~5"},
            {"from_ref": "HEAD~3", "to_ref": "HEAD"},
        ],
        "tracedecay_pr_context":        [{}, {"base_ref": "main"}, {"base_ref": "master"},
                                        {"base_ref": "HEAD~5"}, {"base_ref": "HEAD~1"}],
        "tracedecay_diff_context":      [{"files": [f]} for f in files],
        "tracedecay_commit_context":    [{}, {"commit": "HEAD"}, {"commit": "HEAD~1"},
                                        {"commit": "HEAD~5"}, {"commit": "HEAD~10"}],
        "tracedecay_affected":          [{"files": [f]} for f in files],
    }
