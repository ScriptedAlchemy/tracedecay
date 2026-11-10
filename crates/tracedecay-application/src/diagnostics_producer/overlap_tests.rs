//! A real compiler run may finish after its replacement has published.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_domain::UtcMicros;
use tracedecay_domain::test_fixtures::id;
use tracedecay_lsp::{TypeScriptCompilerAdmission, typescript_projects};
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_runtime_core::db::engine::TestConnection;
use tracedecay_runtime_core::test_executable::write_executable_script;

use super::{
    CompilerProducerRunV1, TypeScriptDiagnosticsAvailabilityV1, TypeScriptProducerStateV1,
    TypeScriptProducerStatusHandleV1, TypeScriptProjectCheckV1, publish_typescript_discovery_v1,
    run_typescript_producer_v1, typescript_diagnostics_availability,
};
use crate::diagnostics_publication::CodeIndexPublicationIdentityV1;
use crate::diagnostics_store::DiagnosticsStore;

#[tokio::test]
async fn an_overlapping_old_compiler_cannot_replace_new_records_or_status_for_the_same_generation()
{
    let compiler = PathBuf::from(
        std::env::var_os("TRACEDECAY_TEST_TYPESCRIPT_COMPILER")
            .expect("set TRACEDECAY_TEST_TYPESCRIPT_COMPILER to the installed pinned tsc"),
    )
    .canonicalize()
    .expect("installed pinned tsc");
    let temp = tempfile::tempdir().expect("isolated compiler project");
    let root = temp.path().join("project");
    std::fs::create_dir_all(root.join("src")).unwrap();
    let root = root.canonicalize().unwrap();
    let source =
        b"export function echo(value) { return value; }\nexport const label: string = 1;\n";
    std::fs::write(root.join("src/index.ts"), source).unwrap();
    std::fs::write(
        root.join("tsconfig.json"),
        r#"{"extends":"./node_modules/check.json","include":["src/**/*.ts"]}"#,
    )
    .unwrap();
    let bin = root.join("node_modules/.bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(compiler, bin.join("real-tsc")).unwrap();
    std::fs::write(
        root.join("node_modules/check.json"),
        r#"{"compilerOptions":{"noImplicitAny":true}}"#,
    )
    .unwrap();
    std::fs::write(root.join("node_modules/hold-compiler"), "").unwrap();
    // Delay delivery of the installed compiler's actual check. The wrapper
    // synthesizes no diagnostics: tsc has already read the old config before
    // the replacement is admitted, but its producer is still awaiting exit.
    write_executable_script(
        &bin.join("tsc"),
        b"#!/bin/sh\n\
          case \" $* \" in *\" --noEmit \"*) checking=1 ;; esac\n\
          if [ -n \"$checking\" ] && [ -f node_modules/hold-compiler ]; then\n\
            ./node_modules/.bin/real-tsc \"$@\" > node_modules/old-output 2>&1\n\
            result=$?\n\
            : > node_modules/old-compiled\n\
            while [ ! -f node_modules/release-old ]; do sleep 0.01; done\n\
            cat node_modules/old-output\n\
            exit \"$result\"\n\
          fi\n\
          exec ./node_modules/.bin/real-tsc \"$@\"\n",
    )
    .unwrap();
    let connection = TestConnection::open(&temp.path().join("diagnostics.db"));
    let store = DiagnosticsStore::new_runtime(&connection);
    let identity = CodeIndexPublicationIdentityV1::new(
        id("generation.compiler-overlap"),
        id("repository.compiler-overlap"),
        None,
        None,
        None,
        [(
            "src/index.ts".to_owned(),
            id("file.compiler-overlap"),
            tracedecay_code_index::intake::content_digest(source),
        )],
    );
    let projects = typescript_projects(&root);
    assert_eq!(projects.len(), 1);
    let build_info = temp.path().join("typescript-build-info");
    let status = TypeScriptProducerStatusHandleV1::default();
    let background_cpu = Arc::new(ProcessBackgroundCpuV1::new(NonZeroUsize::new(2).unwrap()));
    let admission = TypeScriptCompilerAdmission::new(Arc::clone(&background_cpu));
    let mut predecessor = TypeScriptProducerStateV1::new(status.clone(), admission.clone());
    assert_eq!(
        publish_typescript_discovery_v1(&root, &projects, &mut predecessor).await,
        None,
        "a project with a compiler is about to be checked"
    );
    let old_run = run_typescript_producer_v1(
        &root,
        &projects,
        &identity,
        &store,
        &build_info,
        &mut predecessor,
        UtcMicros(1_700_000_000_000_000),
    );
    let replacement_run = async {
        while !root.join("node_modules/old-compiled").try_exists().unwrap() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let old_output = std::fs::read_to_string(root.join("node_modules/old-output")).unwrap();
        assert!(old_output.contains("TS7006"), "{old_output}");
        assert!(old_output.contains("TS2322"), "{old_output}");
        assert_eq!(
            status.current().and_then(|status| status.run().cloned()),
            None,
            "the old producer is still running"
        );
        assert_eq!(
            typescript_diagnostics_availability(
                &root,
                Some("src/index.ts".as_ref()),
                &status,
                None
            ),
            TypeScriptDiagnosticsAvailabilityV1::Configured {
                tsconfig: root.join("tsconfig.json"),
                compiler: root.join("node_modules/.bin/tsc"),
                check: None,
                last_run: None,
            },
            "a read names the compiler the first run is checking with"
        );
        let mut replacement = TypeScriptProducerStateV1::new(status.clone(), admission.clone());
        std::fs::write(
            root.join("node_modules/check.json"),
            r#"{"compilerOptions":{"noImplicitAny":false}}"#,
        )
        .unwrap();
        std::fs::remove_file(root.join("node_modules/hold-compiler")).unwrap();
        let run = run_typescript_producer_v1(
            &root,
            &projects,
            &identity,
            &store,
            &build_info,
            &mut replacement,
            UtcMicros(1_700_000_000_000_001),
        )
        .await;
        assert_eq!(
            run,
            CompilerProducerRunV1::Published {
                generation: identity.generation_id().clone(),
                inserted: 1,
                unresolved: 0,
                checked: 1,
                reused: 0,
            }
        );
        let records = store
            .current_records(identity.generation_id())
            .await
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].code, "TS2322");
        let published = status.current().expect("replacement status");
        assert_eq!(published.run(), Some(&run));
        std::fs::write(root.join("node_modules/release-old"), "").unwrap();
        (published, records)
    };
    let (old_run, (replacement_status, replacement_records)) =
        tokio::time::timeout(Duration::from_mins(1), async {
            tokio::join!(old_run, replacement_run)
        })
        .await
        .expect("both real compiler processes must complete");
    assert_eq!(old_run, CompilerProducerRunV1::Superseded);
    assert_eq!(
        background_cpu.active_units(),
        0,
        "both compiler children were reaped"
    );
    assert_eq!(
        store
            .current_records(identity.generation_id())
            .await
            .unwrap(),
        replacement_records,
        "late old findings must not create a newer publication of the same generation"
    );
    assert!(
        Arc::ptr_eq(&replacement_status, &status.current().unwrap()),
        "the late old result must leave the replacement's snapshot intact"
    );
    let TypeScriptDiagnosticsAvailabilityV1::Configured {
        check: Some(TypeScriptProjectCheckV1::Checked),
        last_run: Some(last_run),
        ..
    } = typescript_diagnostics_availability(
        &root,
        Some("src/index.ts".as_ref()),
        &status,
        Some(identity.generation_id()),
    )
    else {
        panic!("the file must report its completed replacement check");
    };
    assert_eq!(Some(&last_run), replacement_status.run());
}
