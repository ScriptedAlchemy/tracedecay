//! Graph-analysis family: similarity, redundancy, topology, impact, and
//! source reads — all generation-bound queries against the mounted graph.

use serde_json::{Value, json};

use crate::queries::{
    PrimeStep, Query, QueryContext, ToolGroup, file_at, five, prime_function, symbol_name,
};

use super::rq;

const MATCH_CLASSES: [&str; 2] = ["conservative_exact", "rename_normalized_exact"];

fn repo_ids(ctx: &QueryContext) -> (Value, Value) {
    (
        json!(
            ctx.seeds
                .project_id
                .clone()
                .unwrap_or_else(|| "missing".into())
        ),
        json!(
            ctx.seeds
                .repository_id
                .clone()
                .unwrap_or_else(|| "missing".into())
        ),
    )
}

fn path_at(ctx: &QueryContext, i: usize) -> String {
    crate::queries::dir(ctx, i)
}

/// Verify only bounded reads that target the checked-in runtime fixture. The
/// other graph queries vary with the repository and remain dispatch evidence.
pub(crate) fn verify_fixture_read(
    tool: &str,
    label: &str,
    args: &Value,
    payload: &Value,
) -> Option<Result<(), String>> {
    let payload = payload.pointer("/outcome/value/payload").unwrap_or(payload);
    let expected_file = args.get("file").and_then(Value::as_str);
    let has_symbol = |name: &str| {
        payload
            .pointer("/symbols")
            .and_then(Value::as_array)
            .is_some_and(|symbols| {
                symbols
                    .iter()
                    .any(|symbol| symbol.get("name").and_then(Value::as_str) == Some(name))
            })
    };
    match tool {
        "tracedecay_test_results" if crate::repos::small_fixture_enabled() => Some(
            if payload.get("passed") == Some(&json!(1))
                && payload.get("failed") == Some(&json!(0))
                && payload.get("exit_code") == Some(&json!(0))
                && payload
                    .get("results")
                    .and_then(Value::as_array)
                    .is_some_and(|results| {
                        results.iter().any(|result| {
                            result.get("test").and_then(Value::as_str)
                                == Some("fixture_catalog_has_stable_total")
                                && result.get("passed") == Some(&json!(true))
                        })
                    })
            {
                Ok(())
            } else {
                Err("retained test results omitted the exact passed fixture test".to_owned())
            },
        ),
        "tracedecay_find_exact_symbol" | "tracedecay_code_symbol_search"
            if crate::repos::small_fixture_enabled() =>
        {
            let name = args
                .get("name")
                .or_else(|| args.get("query"))
                .and_then(Value::as_str);
            let expected = match name {
                Some("buildFixtureReport") => Some("src/report.ts::buildFixtureReport"),
                Some("fixture_catalog") => Some("src/catalog.py::fixture_catalog"),
                Some("total_quantity") => Some("src/catalog.py::total_quantity"),
                Some("render_summary") => Some("src/main.py::render_summary"),
                Some("fixture_catalog_total") => Some("src/lib.rs::fixture_catalog_total"),
                _ => None,
            }?;
            let items = payload.get("matches").or_else(|| payload.get("items"));
            Some(
                if items.and_then(Value::as_array).is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("qualified_name").and_then(Value::as_str) == Some(expected)
                    })
                }) {
                    Ok(())
                } else {
                    Err(format!(
                        "symbol search omitted exact fixture declaration {expected}"
                    ))
                },
            )
        }
        "tracedecay_run_affected_tests"
            if args.get("changed_paths") == Some(&json!(["src/lib.rs"])) =>
        {
            Some(
                if payload.get("passed") == Some(&json!(1))
                    && payload.get("failed") == Some(&json!(0))
                    && payload.get("exit_code") == Some(&json!(0))
                    && payload
                        .get("results")
                        .and_then(Value::as_array)
                        .is_some_and(|results| {
                            results.iter().any(|result| {
                                result.get("test").and_then(Value::as_str)
                                    == Some("fixture_catalog_has_stable_total")
                                    && result.get("passed") == Some(&json!(true))
                            })
                        })
                {
                    Ok(())
                } else {
                    Err("affected tests did not pass fixture_catalog_has_stable_total".to_owned())
                },
            )
        }
        "tracedecay_source_outline" if expected_file == Some("src/catalog.py") => {
            Some(if has_symbol("fixture_catalog") {
                Ok(())
            } else {
                Err("src/catalog.py outline omitted fixture_catalog".to_owned())
            })
        }
        "tracedecay_grep"
            if args.get("pattern").and_then(Value::as_str) == Some("fixture_catalog") =>
        {
            Some(
                if payload
                    .pointer("/results")
                    .and_then(Value::as_array)
                    .is_some_and(|results| {
                        results.iter().any(|result| {
                            result.get("file").and_then(Value::as_str) == Some("src/catalog.py")
                                && result
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .is_some_and(|text| text.contains("fixture_catalog"))
                        })
                    })
                {
                    Ok(())
                } else {
                    Err("grep omitted the fixture_catalog source match".to_owned())
                },
            )
        }
        "tracedecay_file_dependents" if expected_file == Some("src/catalog.py") => Some(
            if payload
                .get("dependent_files")
                .and_then(Value::as_array)
                .is_some_and(|files| files.iter().any(|file| file == "src/main.py"))
            {
                Ok(())
            } else {
                Err("file_dependents omitted src/main.py for src/catalog.py".to_owned())
            },
        ),
        "tracedecay_test_map" if expected_file == Some("src/lib.rs") => Some(
            if payload
                .get("coverage")
                .and_then(Value::as_array)
                .is_some_and(|coverage| {
                    coverage.iter().any(|row| {
                        row.get("tests")
                            .and_then(Value::as_array)
                            .is_some_and(|tests| {
                                tests.iter().any(|test| {
                                    test.get("test_name").and_then(Value::as_str)
                                        == Some("fixture_catalog_has_stable_total")
                                })
                            })
                    })
                })
            {
                Ok(())
            } else {
                Err("test_map omitted fixture_catalog_has_stable_total".to_owned())
            },
        ),
        "tracedecay_by_qualified_name"
            if args.get("qualified_name").and_then(Value::as_str)
                == Some("src/main.py::render_summary") =>
        {
            Some((|| {
                let result: tracedecay_contracts::retrieval::ByQualifiedNameResultV1 =
                    serde_json::from_value(payload.clone())
                        .map_err(|error| format!("invalid qualified-name result: {error}"))?;
                if result.0.iter().any(|symbol| {
                    symbol.qualified_name == "src/main.py::render_summary"
                        && symbol.name == "render_summary"
                        && symbol.kind == "function"
                        && symbol.file == "src/main.py"
                        && symbol.start_line == 8
                        && symbol.end_line == 11
                }) {
                    Ok(())
                } else {
                    Err("qualified-name lookup omitted the literal src/main.py::render_summary location".to_owned())
                }
            })())
        }
        "tracedecay_files" if args.get("pattern").and_then(Value::as_str) == Some("Cargo.toml") => {
            Some(
                if payload
                    .get("files")
                    .and_then(Value::as_array)
                    .is_some_and(|files| {
                        files.iter().any(|file| {
                            file.get("path").and_then(Value::as_str) == Some("Cargo.toml")
                        })
                    })
                {
                    Ok(())
                } else {
                    Err("files lookup omitted the fixture Cargo.toml path".to_owned())
                },
            )
        }
        "tracedecay_node"
            if crate::repos::small_fixture_enabled() && label == "fixture_render_summary_node" =>
        {
            let expected_id = args.get("node_id").and_then(Value::as_str);
            Some(
                if expected_id.is_some()
                    && payload.get("id").and_then(Value::as_str) == expected_id
                    && payload.get("name").and_then(Value::as_str) == Some("render_summary")
                    && payload.get("qualified_name").and_then(Value::as_str)
                        == Some("src/main.py::render_summary")
                    && payload.get("file").and_then(Value::as_str) == Some("src/main.py")
                {
                    Ok(())
                } else {
                    Err("node lookup did not return the requested render_summary fixture declaration".to_owned())
                },
            )
        }
        "tracedecay_callers"
            if crate::repos::small_fixture_enabled() && label == "fixture_catalog_callers" =>
        {
            let has_render_summary_caller = payload
                .get("items")
                .and_then(Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.pointer("/symbol/qualified_name")
                            .and_then(Value::as_str)
                            == Some("src/main.py::render_summary")
                    })
                });
            Some(if has_render_summary_caller {
                Ok(())
            } else {
                Err("callers omitted the src/main.py render_summary caller edge for fixture_catalog".to_owned())
            })
        }
        "tracedecay_complexity"
            if args.get("path").and_then(Value::as_str) == Some("src/report.ts") =>
        {
            Some(
                if payload
                    .get("ranking")
                    .and_then(Value::as_array)
                    .is_some_and(|ranking| {
                        ranking.iter().any(|entry| {
                            entry.get("name").and_then(Value::as_str) == Some("buildFixtureReport")
                                && entry.get("file").and_then(Value::as_str)
                                    == Some("src/report.ts")
                                && entry.get("cyclomatic_complexity") == Some(&json!(1))
                        })
                    })
                {
                    Ok(())
                } else {
                    Err(
                        "complexity omitted buildFixtureReport with cyclomatic complexity 1"
                            .to_owned(),
                    )
                },
            )
        }
        "tracedecay_doc_coverage"
            if args.get("path").and_then(Value::as_str) == Some("src/main.py") =>
        {
            Some(
                if payload
                    .get("files")
                    .and_then(Value::as_array)
                    .is_some_and(|files| {
                        files.iter().any(|file| {
                            file.get("file").and_then(Value::as_str) == Some("src/main.py")
                                && file.get("symbols").and_then(Value::as_array).is_some_and(
                                    |symbols| {
                                        symbols.iter().any(|symbol| {
                                            symbol.get("name").and_then(Value::as_str)
                                                == Some("render_summary")
                                        })
                                    },
                                )
                        })
                    })
                {
                    Ok(())
                } else {
                    Err("doc coverage omitted undocumented render_summary".to_owned())
                },
            )
        }
        "tracedecay_dsm" if crate::repos::small_fixture_enabled() && label == "dsm" => Some(
            if payload.get("shape").and_then(Value::as_str) == Some("stats")
                && payload.pointer("/stats/files") == Some(&json!(5))
                && payload.pointer("/stats/edges") == Some(&json!(2))
                && payload
                    .pointer("/clusters/0/directory")
                    .and_then(Value::as_str)
                    == Some("src")
            {
                Ok(())
            } else {
                Err("DSM omitted the fixture src cluster statistics".to_owned())
            },
        ),
        "tracedecay_gini" if crate::repos::small_fixture_enabled() && label == "gini" => Some(
            if payload.get("metric").and_then(Value::as_str) == Some("complexity")
                && payload.pointer("/outliers/0/name").and_then(Value::as_str)
                    == Some("src/graph.ts")
            {
                Ok(())
            } else {
                Err("Gini omitted the literal fixture complexity distribution".to_owned())
            },
        ),
        "tracedecay_distribution"
            if crate::repos::small_fixture_enabled() && label == "distribution" =>
        {
            Some(
                if payload.get("total_file_count") == Some(&json!(5))
                    && payload.get("file_count") == Some(&json!(5))
                    && payload
                        .pointer("/files")
                        .and_then(Value::as_array)
                        .is_some_and(|files| {
                            files.iter().any(|file| {
                                file.get("file").and_then(Value::as_str) == Some("src/catalog.py")
                                    && file.pointer("/kinds/1/kind").and_then(Value::as_str)
                                        == Some("function")
                                    && file.pointer("/kinds/1/count") == Some(&json!(3))
                            })
                        })
                {
                    Ok(())
                } else {
                    Err("distribution omitted the fixture catalog.py function counts".to_owned())
                },
            )
        }
        "tracedecay_dependency_depth"
            if crate::repos::small_fixture_enabled() && label == "dependency_depth" =>
        {
            Some(
                if payload
                    .pointer("/chains")
                    .and_then(Value::as_array)
                    .is_some_and(|chains| {
                        chains.iter().any(|chain| {
                            chain.get("file").and_then(Value::as_str) == Some("src/catalog.py")
                                && chain.get("depth") == Some(&json!(1))
                                && chain.pointer("/chain/0").and_then(Value::as_str)
                                    == Some("src/main.py")
                                && chain.pointer("/chain/1").and_then(Value::as_str)
                                    == Some("src/catalog.py")
                        })
                    })
                {
                    Ok(())
                } else {
                    Err("dependency depth omitted the main.py to catalog.py edge".to_owned())
                },
            )
        }
        "tracedecay_recursion" if crate::repos::small_fixture_enabled() && label == "recursion" => {
            Some(
                if payload
                    .get("cycles")
                    .and_then(Value::as_array)
                    .is_some_and(|cycles| {
                        cycles.iter().any(|cycle| {
                            cycle.get("length") == Some(&json!(1))
                                && cycle.pointer("/chain/0/name").and_then(Value::as_str)
                                    == Some("fixture_countdown")
                                && cycle.pointer("/chain/0/file").and_then(Value::as_str)
                                    == Some("src/catalog.py")
                        })
                    })
                {
                    Ok(())
                } else {
                    Err("recursion omitted the fixture_countdown cycle".to_owned())
                },
            )
        }
        "tracedecay_unsafe_patterns"
            if crate::repos::small_fixture_enabled() && label == "unsafe_patterns" =>
        {
            Some(
                if payload
                    .get("matches")
                    .and_then(Value::as_array)
                    .is_some_and(|matches| {
                        matches.iter().any(|item| {
                            item.get("kind").and_then(Value::as_str) == Some("unsafe_block")
                                && item.get("file").and_then(Value::as_str) == Some("src/lib.rs")
                                && item
                                    .get("enclosing")
                                    .and_then(Value::as_str)
                                    .is_some_and(|name| name.ends_with("fixture_unsafe_probe"))
                        })
                    })
                {
                    Ok(())
                } else {
                    Err("unsafe-patterns omitted the fixture_unsafe_probe unsafe block".to_owned())
                },
            )
        }
        "tracedecay_todos" if crate::repos::small_fixture_enabled() && label == "todos" => Some(
            if payload
                .get("markers")
                .and_then(Value::as_array)
                .is_some_and(|markers| {
                    markers.iter().any(|marker| {
                        marker.get("kind").and_then(Value::as_str) == Some("TODO")
                            && marker.get("file").and_then(Value::as_str) == Some("src/lib.rs")
                            && marker
                                .get("text")
                                .and_then(Value::as_str)
                                .is_some_and(|text| text.contains("replace this probe"))
                    })
                })
            {
                Ok(())
            } else {
                Err("TODOs omitted the fixture replacement marker".to_owned())
            },
        ),
        "tracedecay_inheritance_depth"
            if crate::repos::small_fixture_enabled() && label == "inheritance_depth" =>
        {
            Some(
                if payload
                    .get("ranking")
                    .and_then(Value::as_array)
                    .is_some_and(|ranking| {
                        ranking.iter().any(|item| {
                            item.get("name").and_then(Value::as_str) == Some("CatalogChild")
                                && item.get("depth").and_then(Value::as_u64) == Some(1)
                        })
                    })
                {
                    Ok(())
                } else {
                    Err("inheritance depth omitted CatalogChild at depth 1".to_owned())
                },
            )
        }
        "tracedecay_unmounted_files"
            if crate::repos::small_fixture_enabled() && label == "unmounted_files" =>
        {
            Some(
                if payload.get("complete") == Some(&json!(true))
                    && payload.get("unmounted_file_count") == Some(&json!(0))
                    && payload
                        .pointer("/ecosystems")
                        .and_then(Value::as_array)
                        .is_some_and(|ecosystems| {
                            ecosystems.iter().any(|ecosystem| {
                                ecosystem.get("ecosystem").and_then(Value::as_str) == Some("rust")
                                    && ecosystem.get("status").and_then(Value::as_str)
                                        == Some("audited")
                            })
                        })
                {
                    Ok(())
                } else {
                    Err("unmounted-files omitted the audited fixture Rust tree".to_owned())
                },
            )
        }
        "tracedecay_test_risk" if crate::repos::small_fixture_enabled() && label == "test_risk" => {
            Some(
                if payload
                    .pointer("/risks")
                    .and_then(Value::as_array)
                    .is_some_and(|risks| {
                        risks.iter().any(|risk| {
                            risk.get("file").and_then(Value::as_str) == Some("src/catalog.py")
                                && risk.get("name").and_then(Value::as_str)
                                    == Some("fixture_catalog")
                                && risk.get("risk") == Some(&json!(4.0))
                        })
                    })
                {
                    Ok(())
                } else {
                    Err("test-risk omitted the fixture_catalog risk record".to_owned())
                },
            )
        }
        "tracedecay_port_status"
            if crate::repos::small_fixture_enabled() && label == "port_status" =>
        {
            Some(
                if payload.get("source_dir").and_then(Value::as_str) == Some("src")
                    && payload.get("target_dir").and_then(Value::as_str) == Some("tests")
                    && payload
                        .get("target_only_symbols")
                        .and_then(Value::as_array)
                        .is_some_and(|symbols| {
                            symbols.iter().any(|symbol| {
                                symbol.get("file").and_then(Value::as_str)
                                    == Some("tests/catalog.rs")
                                    && symbol.get("name").and_then(Value::as_str)
                                        == Some("fixture_catalog_has_stable_total")
                            })
                        })
                {
                    Ok(())
                } else {
                    Err("port status omitted the fixture test-only catalog symbol".to_owned())
                },
            )
        }
        "tracedecay_port_order"
            if crate::repos::small_fixture_enabled() && label == "port_order" =>
        {
            Some(
                if payload
                    .pointer("/levels/2/symbols")
                    .and_then(Value::as_array)
                    .is_some_and(|symbols| {
                        symbols.iter().any(|symbol| {
                            symbol.get("file").and_then(Value::as_str) == Some("src/report.ts")
                                && symbol.get("name").and_then(Value::as_str)
                                    == Some("buildFixtureReport")
                        })
                    })
                {
                    Ok(())
                } else {
                    Err("port order omitted buildFixtureReport at dependency level 2".to_owned())
                },
            )
        }
        "tracedecay_feedback_advisory_cycle"
            if crate::repos::small_fixture_enabled() && label == "retained_diagnostic_cycle" =>
        {
            let published =
                payload.pointer("/cycle/published").and_then(Value::as_bool) == Some(true);
            let has_fixture_diagnostic = payload
                .pointer("/cycle/cycle/findings")
                .and_then(Value::as_array)
                .is_some_and(|findings| {
                    findings.iter().any(|finding| {
                        finding.get("safe_bounded_preview").and_then(Value::as_str)
                            == Some("unused variable: `feedback_bench_unused_probe`")
                    })
                });
            Some(if published && has_fixture_diagnostic {
                Ok(())
            } else {
                Err("feedback advisory cycle omitted the published fixture diagnostic".to_owned())
            })
        }
        "tracedecay_feedback_proximity" if label == "proximity" => {
            let observed_at_matches = args
                .get("observed_at")
                .and_then(Value::as_i64)
                .zip(payload.pointer("/page/observed_at").and_then(Value::as_i64))
                .is_some_and(|(requested, observed)| requested == observed);
            let complete = payload.get("state").and_then(Value::as_str) == Some("complete");
            Some(if observed_at_matches && complete {
                Ok(())
            } else {
                Err("feedback proximity did not return complete evidence for the requested observation".to_owned())
            })
        }
        "tracedecay_context" if crate::repos::small_fixture_enabled() && label == "task" => {
            if args.get("task").and_then(Value::as_str) != Some("How does buildFixtureReport work?")
            {
                return None;
            }
            let lexical_anchor = payload
                .pointer("/lexical_anchors")
                .and_then(Value::as_array)
                .is_some_and(|anchors| {
                    anchors.iter().any(|anchor| {
                        anchor.get("anchor").and_then(Value::as_str) == Some("buildFixtureReport")
                            && anchor.get("outcome").and_then(Value::as_str) == Some("matched")
                    })
                });
            let complete = payload.pointer("/coverage/exact").and_then(Value::as_str)
                == Some("complete")
                && payload.pointer("/coverage/graph").and_then(Value::as_str) == Some("complete")
                && payload.pointer("/coverage/lexical").and_then(Value::as_str) == Some("complete")
                && payload.pointer("/coverage/recall").and_then(Value::as_str) == Some("full");
            Some(if lexical_anchor && complete {
                Ok(())
            } else {
                Err(
                    "context omitted complete coverage for the buildFixtureReport anchor"
                        .to_owned(),
                )
            })
        }
        "tracedecay_coupling"
            if crate::repos::small_fixture_enabled()
                && label == "scoped"
                && args.get("path").and_then(Value::as_str) == Some("src") =>
        {
            let ranking = payload.get("ranking").and_then(Value::as_array);
            let has_file = |path: &str| {
                ranking.is_some_and(|rows| {
                    rows.iter()
                        .any(|row| row.get("file").and_then(Value::as_str) == Some(path))
                })
            };
            Some(
                if payload.get("direction").and_then(Value::as_str) == Some("fan_in")
                    && has_file("src/catalog.py")
                    && has_file("src/graph.ts")
                {
                    Ok(())
                } else {
                    Err("coupling omitted the fixture catalog and graph fan-in edges".to_owned())
                },
            )
        }
        "tracedecay_largest"
            if crate::repos::small_fixture_enabled()
                && label == "by_kind"
                && args.get("node_kind").and_then(Value::as_str) == Some("function") =>
        {
            let has_report = payload
                .get("ranking")
                .and_then(Value::as_array)
                .is_some_and(|rows| {
                    rows.iter().any(|row| {
                        row.get("name").and_then(Value::as_str) == Some("buildFixtureReport")
                            && row.get("file").and_then(Value::as_str) == Some("src/report.ts")
                            && row.get("kind").and_then(Value::as_str) == Some("function")
                            && row.get("start_line") == Some(&json!(8))
                            && row
                                .get("end_line")
                                .and_then(Value::as_u64)
                                .is_some_and(|line| line >= 14)
                    })
                });
            Some(if has_report {
                Ok(())
            } else {
                Err(
                    "largest-function ranking omitted buildFixtureReport at its fixture location"
                        .to_owned(),
                )
            })
        }
        "tracedecay_rank"
            if crate::repos::small_fixture_enabled()
                && label == "by_kind"
                && args.get("edge_kind").and_then(Value::as_str) == Some("calls") =>
        {
            let ranking = payload.get("ranking").and_then(Value::as_array);
            let has_name = |name: &str| {
                ranking.is_some_and(|rows| {
                    rows.iter()
                        .any(|row| row.get("name").and_then(Value::as_str) == Some(name))
                })
            };
            Some(
                if payload.get("direction").and_then(Value::as_str) == Some("incoming")
                    && has_name("fixture_catalog_total")
                    && has_name("dependencyCount")
                {
                    Ok(())
                } else {
                    Err("call ranking omitted known fixture call targets".to_owned())
                },
            )
        }
        "tracedecay_search"
            if crate::repos::small_fixture_enabled()
                && label == "term"
                && args.get("query").and_then(Value::as_str) == Some("buildFixtureReport") =>
        {
            let result = payload.pointer("/results/0/display");
            Some(
                if payload.pointer("/coverage/exact").and_then(Value::as_str) == Some("complete")
                    && payload.pointer("/coverage/graph").and_then(Value::as_str)
                        == Some("complete")
                    && result
                        .and_then(|v| v.get("qualified_name"))
                        .and_then(Value::as_str)
                        == Some("src/report.ts::buildFixtureReport")
                {
                    Ok(())
                } else {
                    Err("search omitted the exact buildFixtureReport result".to_owned())
                },
            )
        }
        "tracedecay_hotspots" if crate::repos::small_fixture_enabled() && label == "limit" => {
            let has_summary = payload
                .get("hotspots")
                .and_then(Value::as_array)
                .is_some_and(|rows| {
                    rows.iter().any(|row| {
                        row.get("file").and_then(Value::as_str) == Some("src/main.py")
                            && row.get("name").and_then(Value::as_str) == Some("render_summary")
                    })
                });
            Some(if has_summary {
                Ok(())
            } else {
                Err("hotspots omitted the known render_summary fixture symbol".to_owned())
            })
        }
        "tracedecay_diagnostics"
            if crate::repos::small_fixture_enabled()
                && label == "diagnostics"
                && args.get("path").and_then(Value::as_str) == Some("src/lib.rs") =>
        {
            let has_probe = payload
                .get("diagnostics")
                .and_then(Value::as_array)
                .is_some_and(|rows| {
                    rows.iter().any(|row| {
                        let diagnostic = row.get("diagnostic").unwrap_or(row);
                        diagnostic.get("code").and_then(Value::as_str) == Some("warning")
                            && diagnostic
                                .get("message")
                                .and_then(Value::as_str)
                                .is_some_and(|message| {
                                    message.contains("feedback_bench_unused_probe")
                                })
                    })
                });
            Some(
                if payload.get("clean_generation") == Some(&json!(true)) && has_probe {
                    Ok(())
                } else {
                    Err("diagnostics omitted the fixture warning evidence".to_owned())
                },
            )
        }
        _ => None,
    }
}

pub(crate) fn groups(ctx: &QueryContext, out: &mut Vec<ToolGroup>) {
    let (pid, rid) = repo_ids(ctx);

    out.push(ToolGroup {
        tool: "tracedecay_similar",
        queries: five(|i| {
            Query::prepared_read(
                "similar",
                "tracedecay_similar",
                json!({
                    "project_id": pid,
                    "repository_id": rid,
                    "target": {
                        "kind": "symbol_occurrence",
                        "symbol_occurrence_id": "{{live_node}}",
                    },
                    "match_classes": [MATCH_CLASSES[i % 2]],
                    "result_limit": 20,
                    "work_limit": 4,
                }),
                i,
                prime_function,
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_redundancy",
        queries: five(|i| {
            rq(
                "tracedecay_redundancy",
                "redundancy",
                json!({
                    "project_id": pid,
                    "repository_id": rid,
                    "scope": {"kind": "path", "path": path_at(ctx, i)},
                    "match_classes": [MATCH_CLASSES[i % 2]],
                    "family_limit": 20,
                    "member_limit": 10,
                    "work_limit": 4,
                    "include_generated_paths": false,
                }),
            )
        }),
    });
    for (tool, label) in [
        ("tracedecay_recursion", "recursion"),
        ("tracedecay_inheritance_depth", "inheritance_depth"),
        ("tracedecay_dependency_depth", "dependency_depth"),
        ("tracedecay_distribution", "distribution"),
        ("tracedecay_unsafe_patterns", "unsafe_patterns"),
        ("tracedecay_unmounted_files", "unmounted_files"),
        ("tracedecay_test_risk", "test_risk"),
        ("tracedecay_todos", "todos"),
    ] {
        out.push(ToolGroup {
            tool,
            queries: five(|i| rq(tool, label, json!({"path": path_at(ctx, i), "limit": 25}))),
        });
    }
    out.push(ToolGroup {
        tool: "tracedecay_dsm",
        queries: five(|i| {
            rq(
                "tracedecay_dsm",
                "dsm",
                json!({"path": path_at(ctx, i), "max_files": 50, "shape": null}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_gini",
        queries: five(|i| {
            rq(
                "tracedecay_gini",
                "gini",
                json!({"path": path_at(ctx, i), "limit": 50, "metric": null, "scope": null}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_port_status",
        queries: five(|i| {
            rq(
                "tracedecay_port_status",
                "port_status",
                json!({
                    "source_dir": path_at(ctx, i),
                    "target_dir": path_at(ctx, i + 1),
                    "kinds": null,
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_port_order",
        queries: five(|i| {
            rq(
                "tracedecay_port_order",
                "port_order",
                json!({"source_dir": path_at(ctx, i), "kinds": null, "limit": 20}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_file_dependents",
        queries: five(|i| {
            rq(
                "tracedecay_file_dependents",
                "file_dependents",
                json!({"file": file_at(ctx, i)["path"]}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_test_map",
        queries: five(|i| {
            rq(
                "tracedecay_test_map",
                "test_map",
                json!({"file": file_at(ctx, i)["path"]}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_affected",
        queries: five(|i| {
            rq(
                "tracedecay_affected",
                "affected",
                json!({"files": [file_at(ctx, i)["path"]], "depth": 2}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_diff_context",
        queries: five(|i| {
            rq(
                "tracedecay_diff_context",
                "diff_context",
                json!({"files": [file_at(ctx, i)["path"]], "depth": 2}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_commit_context",
        queries: five(|i| {
            rq(
                "tracedecay_commit_context",
                "commit_context",
                json!({"staged_only": i % 2 == 0}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_pr_context",
        queries: five(|_i| {
            rq(
                "tracedecay_pr_context",
                "pr_context",
                json!({
                    "base_ref": "HEAD~1",
                    "head_ref": "HEAD",
                    "maximum_symbols": 64,
                }),
            )
        }),
    });
    if let Some(parent_commit) = &ctx.seeds.parent_commit {
        out.push(ToolGroup {
            tool: "tracedecay_changelog",
            queries: five(|_i| {
                rq(
                    "tracedecay_changelog",
                    "changelog",
                    json!({"from_ref": parent_commit, "to_ref": "HEAD"}),
                )
            }),
        });
    }
    if let Some(diagnostic) = &ctx.seeds.compiler_diagnostic {
        out.push(ToolGroup {
            tool: "tracedecay_diagnose",
            queries: five(|i| {
                rq(
                    "tracedecay_diagnose",
                    "diagnose_fixture_warning",
                    json!({
                        "cargo_output": diagnostic,
                        "include_callers": i % 2 == 0,
                        "max_diagnostics": 8,
                    }),
                )
            }),
        });
    }
    out.push(ToolGroup {
        tool: "tracedecay_diagnostics",
        queries: five(|i| {
            rq(
                "tracedecay_diagnostics",
                "diagnostics",
                json!({"path": ctx.seeds.compiler_diagnostic_path.clone().unwrap_or_else(|| path_at(ctx, i)), "maximum_diagnostics": 25}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_run_affected_tests",
        queries: five(|i| {
            rq(
                "tracedecay_run_affected_tests",
                "run_affected",
                json!({
                    "changed_paths": [file_at(ctx, i)["path"]],
                    "max_tests": 5,
                    "timeout_secs": 30,
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_source_outline",
        queries: five(|i| {
            rq(
                "tracedecay_source_outline",
                "source_outline",
                json!({"file": file_at(ctx, i)["path"]}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_source_body",
        queries: five(|i| {
            Query::prepared_read(
                "source_body",
                "tracedecay_source_body",
                json!({"node_id": "{{live_node}}"}),
                i,
                prime_function,
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_source_lines",
        queries: five(|i| {
            Query::prepared_read(
                "source_lines",
                "tracedecay_source_lines",
                json!({
                    "file": "{{live_file_occurrence}}",
                    "span": "{{live_source_span}}",
                    "meta": {
                        "projection": "evidence",
                        "order": "source_position",
                        "page": {"page_size": 20, "cursor": null},
                        "temporal": {"kind": "current"},
                    },
                }),
                i,
                |ctx, selection| {
                    let index = selection as usize;
                    let qname = QueryContext::pick(&ctx.function_qnames, index);
                    let path = qname.split_once("::").map(|(path, _)| path);
                    let mut steps = prime_function(ctx, selection);
                    steps.push(PrimeStep {
                        inject: vec![],
                        tool: "tracedecay_code_exact_occurrence",
                        args: json!({
                            "literal": symbol_name(ctx, index),
                            "kind": "whole_symbol",
                            "scope": {"generation": "{{live_generation}}", "path_prefix": path},
                            "meta": {"projection": "evidence", "order": "source_position", "cursor": null},
                            "format": "json",
                        }),
                        capture: &[
                            ("outcome.value.payload.items.0.occurrence.file", "live_file_occurrence"),
                            ("outcome.value.payload.items.0.occurrence.span", "live_source_span"),
                        ],
                    });
                    steps
                },
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_grep",
        queries: five(|i| {
            rq(
                "tracedecay_grep",
                "grep",
                json!({
                    "pattern": symbol_name(ctx, i),
                    "path_glob": "**/*",
                    "max_results": 25,
                    "fixed_strings": true,
                }),
            )
        }),
    });
    if crate::repos::small_fixture_enabled() {
        out.push(ToolGroup {
            tool: "tracedecay_by_qualified_name",
            queries: five(|_| {
                rq(
                    "tracedecay_by_qualified_name",
                    "fixture_render_summary",
                    json!({"qualified_name": "src/main.py::render_summary"}),
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_code_symbol_search",
            queries: five(|_| {
                rq(
                    "tracedecay_code_symbol_search",
                    "fixture_render_summary_search",
                    json!({
                        "query": "render_summary",
                        "lazy_index_ignored_dependencies": false,
                        "scope": {"path_prefix": "src/main.py"},
                        "meta": {"projection": "summary", "order": "relevance", "cursor": null},
                    }),
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_files",
            queries: five(|_| {
                rq(
                    "tracedecay_files",
                    "fixture_cargo_manifest",
                    json!({"pattern": "Cargo.toml"}),
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_node",
            queries: five(|_| Query::prepared_read(
                "fixture_render_summary_node",
                "tracedecay_node",
                json!({"node_id": "{{live_node}}"}),
                0,
                |_, _| vec![PrimeStep {
                    inject: Vec::new(),
                    tool: "tracedecay_code_symbol_search",
                    args: json!({
                        "query": "render_summary",
                        "lazy_index_ignored_dependencies": false,
                        "scope": {"path_prefix": "src/main.py"},
                        "meta": {"projection": "summary", "order": "relevance", "cursor": null},
                        "format": "json",
                    }),
                    capture: &[("outcome.value.payload.items.0.node_id", "live_node")],
                }],
            )),
        });
        out.push(ToolGroup {
            tool: "tracedecay_callers",
            queries: five(|_| Query::prepared_read(
                "fixture_catalog_callers",
                "tracedecay_callers",
                json!({"node_id": "{{live_node}}", "maximum_depth": 3}),
                0,
                |_, _| vec![PrimeStep {
                    inject: Vec::new(),
                    tool: "tracedecay_code_symbol_search",
                    args: json!({
                        "query": "fixture_catalog",
                        "lazy_index_ignored_dependencies": false,
                        "scope": {"path_prefix": "src/catalog.py"},
                        "meta": {"projection": "summary", "order": "relevance", "cursor": null},
                        "format": "json",
                    }),
                    capture: &[("outcome.value.payload.items.0.node_id", "live_node")],
                }],
            )),
        });
        out.push(ToolGroup {
            tool: "tracedecay_complexity",
            queries: five(|_| {
                rq(
                    "tracedecay_complexity",
                    "fixture_report_complexity",
                    json!({"path": "src/report.ts", "limit": 20}),
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_doc_coverage",
            queries: five(|_| {
                rq(
                    "tracedecay_doc_coverage",
                    "fixture_main_doc_coverage",
                    json!({"path": "src/main.py"}),
                )
            }),
        });
    }
}
