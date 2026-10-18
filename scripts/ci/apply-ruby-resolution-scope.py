#!/usr/bin/env python3
"""Apply the reviewed caller-scope cache correction to pinned inputs."""
from pathlib import Path
import subprocess

expected = {
    "crates/tracedecay-code-index/src/production/helpers.rs": "429740cd258cb32bda58fe03a88b972308c832aa",
    "crates/tracedecay-code-index/src/languages.rs": "fbae1e5b75fd4ecf6003c1d285d94a23bcdc30cc",
    "crates/tracedecay-code-index/tests/code_index_suite/cross_file_import_calls.rs": "93d4330fc597710b050ab41e64ee6c9268b9c62b",
}
for name, sha in expected.items():
    actual = subprocess.check_output(["git", "hash-object", name], text=True).strip()
    if actual != sha:
        raise SystemExit(f"source changed: {name}: {actual} != {sha}")

def replace_once(source, before, after):
    if source.count(before) != 1:
        raise SystemExit(f"ambiguous edit anchor: {before!r}")
    return source.replace(before, after, 1)

path = Path("crates/tracedecay-code-index/src/production/helpers.rs")
source = path.read_text()
source = replace_once(source, '''/// The memo is file-local on purpose. `ResolvedReferenceCacheV1` is keyed by
/// source-file index, so a shared map could never serve another file's entry.
/// A per-file resolution therefore decides exactly what the whole-repository
/// serial loop decided.''', '''/// The memo is file-local and caller-sensitive. Ruby constant lookup reads
/// the caller's lexical nesting, so equal spellings in different callers must
/// not reuse a binding. Repeated references in one caller still share work.''')
source = replace_once(source, '''        let cache_key = (
            index,
            reference.reference_name.as_str(),''', '''        let cache_key = (
            &reference.from_occurrence,
            reference.reference_name.as_str(),''')
source = replace_once(source, '''    (usize, &'a str, RelationEdgeKindV1, Option<u32>, bool),''', '''    (
        &'a SymbolOccurrenceId,
        &'a str,
        RelationEdgeKindV1,
        Option<u32>,
        bool,
    ),''')
path.write_text(source)

path = Path("crates/tracedecay-code-index/src/languages.rs")
source = replace_once(path.read_text(), '''                "ruby" | "protobuf" | "python" => 7,''', '''                // Rebuild Ruby generations whose seal memo could reuse a
                // different caller's lexical constant binding.
                "ruby" => 8,
                "protobuf" | "python" => 7,''')
path.write_text(source)

path = Path("crates/tracedecay-code-index/tests/code_index_suite/cross_file_import_calls.rs")
source = path.read_text().rstrip() + r'''

#[test]
fn ruby_same_spelling_calls_keep_each_callers_lexical_module() {
    for modules in [["A", "B"], ["B", "A"]] {
        let mut entry = "require_relative 'a_util'\nrequire_relative 'b_util'\n".to_owned();
        for module in modules {
            entry.push_str(&format!(
                "module {module}\n  module Runner\n    def self.run\n      Util.call\n      Util.call\n    end\n  end\nend\n"
            ));
        }
        let files = vec![
            (
                "lib/a_util.rb".to_owned(),
                "module A\n  module Util\n    def self.call\n      1\n    end\n  end\nend\n".to_owned(),
            ),
            (
                "lib/b_util.rb".to_owned(),
                "module B\n  module Util\n    def self.call\n      2\n    end\n  end\nend\n".to_owned(),
            ),
            ("lib/entry.rb".to_owned(), entry),
        ];
        let changed = files.iter().map(|(path, _)| path.clone()).collect();
        let generation = cold_generation(
            &applying_owner()
                .build_and_publish(
                    fixture_tree_request("ruby-lexical-callers", 1, &files, &changed),
                    &ActiveControl,
                )
                .expect("Ruby source fixture publishes"),
        );
        let graph = CallGraphV1 {
            reader: reader(&generation),
            generation,
            roles: BTreeMap::from([
                ("caller_a", "lib/entry.rb::A::Runner::run"),
                ("caller_b", "lib/entry.rb::B::Runner::run"),
                ("target_a", "lib/a_util.rb::A::Util::call"),
                ("target_b", "lib/b_util.rb::B::Util::call"),
            ]),
        };
        assert_eq!(
            graph.calls(),
            BTreeSet::from([("caller_a", "target_a"), ("caller_b", "target_b")]),
            "bindings must not depend on caller order {modules:?}"
        );
        for (caller, target) in [("caller_a", "target_a"), ("caller_b", "target_b")] {
            let caller = graph.occurrence(caller);
            let target = graph.occurrence(target);
            let calls = graph.generation.edges().iter().filter(|edge| {
                edge.kind == RelationEdgeKindV1::Calls && edge.from_occurrence == caller
            }).collect::<Vec<_>>();
            assert_eq!(calls.len(), 2, "each source call retains its own evidence");
            assert!(calls.iter().all(|edge| edge.to_occurrence == target));
            assert_ne!(calls[0].evidence_span, calls[1].evidence_span);
        }
    }
}
'''
path.write_text(source)
