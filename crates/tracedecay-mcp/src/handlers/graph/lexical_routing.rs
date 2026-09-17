use crate::tools::render::Md;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_query::retrieval::lexical::{
    LexicalAliasV1, LexicalFieldFilterV1, LexicalProximityV1, LexicalRouteKindV1,
    LexicalRouteReceiptV1, LexicalRoutingV1,
};

pub(super) fn routing_from_args(args: &Value) -> Result<LexicalRoutingV1> {
    let anchors = match args.get("lexical_anchors") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| TraceDecayError::Config {
                        message: format!("lexical_anchors[{index}] must be a string"),
                    })
            })
            .collect::<Result<Vec<_>>>()?,
        Some(_) => {
            return Err(TraceDecayError::Config {
                message: "lexical_anchors must be an array of strings".to_owned(),
            });
        }
    };
    let prefer_symbol = match args.get("prefer_symbol") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => {
            return Err(TraceDecayError::Config {
                message: "prefer_symbol must be a boolean".to_owned(),
            });
        }
    };
    let aliases: Vec<LexicalAliasV1> = decode_array(args, "lexical_aliases")?;
    let phrases: Vec<String> = decode_array(args, "lexical_phrases")?;
    let proximities: Vec<LexicalProximityV1> = decode_array(args, "lexical_proximities")?;
    let field_filters: Vec<LexicalFieldFilterV1> = decode_array(args, "lexical_field_filters")?;
    let mut routing = routing_from_parts(anchors, prefer_symbol)?
        .with_aliases(aliases)
        .map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?;
    routing.phrases = phrases;
    routing.proximities = proximities;
    routing.field_filters = field_filters;
    Ok(routing)
}

fn decode_array<T: DeserializeOwned>(args: &Value, field: &str) -> Result<Vec<T>> {
    match args.get(field) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .cloned()
            .map(|value| {
                serde_json::from_value(value).map_err(|error| TraceDecayError::Config {
                    message: format!("{field} is invalid: {error}"),
                })
            })
            .collect(),
        Some(_) => Err(TraceDecayError::Config {
            message: format!("{field} must be an array"),
        }),
    }
}

pub(super) fn routing_from_parts(
    anchors: Vec<String>,
    prefer_symbol: bool,
) -> Result<LexicalRoutingV1> {
    LexicalRoutingV1::new(anchors, prefer_symbol).map_err(|error| TraceDecayError::Config {
        message: error.to_string(),
    })
}

pub(super) fn route_label(route: &LexicalRouteKindV1) -> String {
    match route {
        LexicalRouteKindV1::Query => "query".to_owned(),
        LexicalRouteKindV1::Anchor { anchor } => format!("anchor:{}", anchor.as_str()),
        LexicalRouteKindV1::PreferredSymbol { tokens } => {
            format!("symbol:{}", tokens.join("|"))
        }
        LexicalRouteKindV1::IdentifierSplit { terms, .. } => {
            format!("split:{}", terms.join("|"))
        }
        LexicalRouteKindV1::Alias { alternative, .. } => format!("alias:{alternative}"),
    }
}

pub(super) fn attach_route_evidence(
    output: &mut Value,
    results: &mut [Value],
    receipt: &LexicalRouteReceiptV1,
) -> Result<()> {
    if !receipt.has_disclosure() {
        return Ok(());
    }
    let mut routes = Vec::with_capacity(receipt.routes.len());
    for route in &receipt.routes {
        let mut value = serde_json::to_value(route)?;
        value["label"] = json!(route_label(route));
        routes.push(value);
    }
    output["lexical_routes"] = Value::Array(routes);
    for result in results.iter_mut() {
        let Some(anchor) = result
            .get("candidate")
            .and_then(|candidate| candidate.get("anchor_id"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(matches) = receipt
            .matches_by_anchor
            .iter()
            .find(|(candidate_anchor, _)| candidate_anchor.as_str() == anchor)
            .map(|(_, matches)| matches)
        else {
            continue;
        };
        result["lexical_routes"] = json!(
            matches
                .iter()
                .map(|route_match| {
                    let mut value = json!({
                        "route": route_label(&route_match.route),
                        "score_micros": route_match.score_micros,
                        "matched_terms": route_match.matched_terms,
                    });
                    if !route_match.spelling_variants.is_empty() {
                        value["spelling_variants"] = json!(route_match.spelling_variants);
                    }
                    value
                })
                .collect::<Vec<_>>()
        );
    }
    Ok(())
}

pub(super) fn result_route_suffix(result: &Value) -> String {
    let Some(routes) = result.get("lexical_routes").and_then(Value::as_array) else {
        return String::new();
    };
    let labels: Vec<&str> = routes
        .iter()
        .filter_map(|route| route.get("route").and_then(Value::as_str))
        .collect();
    let variants = routes
        .iter()
        .flat_map(|route| {
            route
                .get("spelling_variants")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|variant| {
            Some(format!(
                "{} to {}",
                variant.get("query")?.as_str()?,
                variant.get("alternative")?.as_str()?
            ))
        })
        .collect::<Vec<_>>();
    match (labels.is_empty(), variants.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!(" · via {}", labels.join(", ")),
        (true, false) => format!(" · spelling {}", variants.join(", ")),
        (false, false) => format!(
            " · via {} · spelling {}",
            labels.join(", "),
            variants.join(", ")
        ),
    }
}

pub(super) fn append_routes_md(md: &mut Md, value: &Value) {
    let Some(routes) = value.get("lexical_routes").and_then(Value::as_array) else {
        return;
    };
    let labels: Vec<&str> = routes
        .iter()
        .filter_map(|route| route.get("label").and_then(Value::as_str))
        .collect();
    if labels.is_empty() {
        return;
    }
    md.blank().heading(3, "Lexical Routes").line(&format!(
        "Ranked routes fused into this page: {}",
        labels.join(", ")
    ));
    for route in routes {
        if route.get("route").and_then(Value::as_str) != Some("alias") {
            continue;
        }
        let Some(strict_query) = route.get("strict_query").and_then(Value::as_str) else {
            continue;
        };
        let Some(alternative) = route.get("alternative").and_then(Value::as_str) else {
            continue;
        };
        md.line(&format!("Strict query: `{strict_query}`"))
            .line(&format!("Alternative tried: `{alternative}`"))
            .line("Reason: configured vocabulary alias");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tracedecay_query::retrieval::lexical::{LexicalRouteMatchV1, MAX_LEXICAL_ANCHORS_V1};

    use super::*;

    #[test]
    fn routing_args_decode_and_reject_typed_violations() {
        let routing = routing_from_args(&json!({
            "query": "memoization",
            "lexical_anchors": ["reserve_stock", "Foo::bar"],
            "prefer_symbol": true,
            "lexical_aliases": [{
                "strict_query": "memoization",
                "alternative": "cache"
            }],
            "lexical_phrases": ["durable cache"],
            "lexical_proximities": [{
                "terms": ["durable", "owner"],
                "maximum_gap": 5
            }],
            "lexical_field_filters": [{
                "field": "documentation",
                "include": true
            }],
        }))
        .expect("valid routing");
        assert_eq!(routing.anchors.len(), 2);
        assert!(routing.prefer_symbol);
        assert_eq!(routing.aliases[0].strict_query, "memoization");
        assert_eq!(routing.aliases[0].alternative, "cache");
        assert_eq!(routing.phrases, ["durable cache"]);
        assert_eq!(routing.proximities[0].terms, ["durable", "owner"]);
        assert_eq!(
            routing.field_filters,
            [tracedecay_query::retrieval::lexical::LexicalFieldFilterV1 {
                field: tracedecay_query::retrieval::lexical::LexicalFieldV1::Documentation,
                include: true,
            }]
        );

        let plain = routing_from_args(&json!({"query": "inventory"})).expect("query only");
        assert_eq!(plain, LexicalRoutingV1::default());

        let too_many: Vec<String> = (0..=MAX_LEXICAL_ANCHORS_V1)
            .map(|index| format!("anchor_{index}"))
            .collect();
        let error = routing_from_args(&json!({"lexical_anchors": too_many}))
            .expect_err("anchor count is bounded");
        assert!(
            error.to_string().contains("at most 8 anchors"),
            "typed bound in the message: {error}"
        );
        let error = routing_from_args(&json!({"lexical_anchors": ["ok", ""]}))
            .expect_err("empty anchors are rejected");
        assert!(error.to_string().contains("anchor 1 is empty"), "{error}");
        let error = routing_from_args(&json!({"lexical_anchors": ["two words"]}))
            .expect_err("multi-term anchors are rejected");
        assert!(error.to_string().contains("one identifier"), "{error}");
        let error = routing_from_args(&json!({"lexical_anchors": "reserve_stock"}))
            .expect_err("a bare string is not an anchor list");
        assert!(error.to_string().contains("array of strings"), "{error}");
        let error = routing_from_args(&json!({"lexical_anchors": [1]}))
            .expect_err("anchors must be strings");
        assert!(
            error.to_string().contains("[0] must be a string"),
            "{error}"
        );
        let error = routing_from_args(&json!({"prefer_symbol": "yes"}))
            .expect_err("prefer_symbol must be a boolean");
        assert!(error.to_string().contains("must be a boolean"), "{error}");

        let aliases = (0..=tracedecay_query::retrieval::lexical::MAX_LEXICAL_ALIASES_V1)
            .map(|index| {
                json!({
                    "strict_query": "memoization",
                    "alternative": format!("cache_{index}")
                })
            })
            .collect::<Vec<_>>();
        assert!(
            routing_from_args(&json!({"lexical_aliases": aliases}))
                .expect_err("aliases are bounded")
                .to_string()
                .contains("at most 8")
        );
    }

    #[test]
    fn route_evidence_is_attached_only_when_additional_routes_ran() {
        let mut output = json!({"results": []});
        let mut results = vec![json!({"candidate": {"anchor_id": "code-symbol:reserve"}})];
        let query_only = LexicalRouteReceiptV1 {
            routes: vec![LexicalRouteKindV1::Query],
            matches_by_anchor: BTreeMap::new(),
        };
        attach_route_evidence(&mut output, &mut results, &query_only).expect("attach");
        assert!(output.get("lexical_routes").is_none());
        assert!(results[0].get("lexical_routes").is_none());
        assert_eq!(result_route_suffix(&results[0]), "");

        let anchor = LexicalRoutingV1::new(vec!["reserve_stock".to_owned()], true)
            .expect("routing")
            .anchors
            .remove(0);
        let anchor_route = LexicalRouteKindV1::Anchor { anchor };
        let symbol_route = LexicalRouteKindV1::PreferredSymbol {
            tokens: vec!["stock".to_owned()],
        };
        let alias_route = LexicalRouteKindV1::Alias {
            strict_query: "memoization".to_owned(),
            alternative: "cache".to_owned(),
            reason: tracedecay_query::retrieval::lexical::LexicalAlternativeReasonV1::ConfiguredVocabularyAlias,
        };
        let receipt = LexicalRouteReceiptV1 {
            routes: vec![
                LexicalRouteKindV1::Query,
                anchor_route.clone(),
                symbol_route.clone(),
                alias_route,
            ],
            matches_by_anchor: BTreeMap::from([(
                tracedecay_domain::RetrievalAnchorId::new("code-symbol:reserve").expect("anchor"),
                vec![
                    LexicalRouteMatchV1 {
                        route: anchor_route,
                        score_micros: 900_000,
                        matched_terms: vec!["reserve_stock".to_owned()],
                        spelling_variants: Vec::new(),
                    },
                    LexicalRouteMatchV1 {
                        route: symbol_route,
                        score_micros: 100_000,
                        matched_terms: vec!["stock".to_owned()],
                        spelling_variants: vec![
                            tracedecay_query::retrieval::lexical::LexicalSpellingVariantV1 {
                                query: "stokc".to_owned(),
                                alternative: "stock".to_owned(),
                            },
                        ],
                    },
                ],
            )]),
        };
        attach_route_evidence(&mut output, &mut results, &receipt).expect("attach");
        assert_eq!(
            output["lexical_routes"],
            json!([
                {"route": "query", "label": "query"},
                {"route": "anchor", "anchor": "reserve_stock", "label": "anchor:reserve_stock"},
                {"route": "preferred_symbol", "tokens": ["stock"], "label": "symbol:stock"},
                {
                    "route": "alias",
                    "strict_query": "memoization",
                    "alternative": "cache",
                    "reason": "configured_vocabulary_alias",
                    "label": "alias:cache"
                },
            ])
        );
        assert_eq!(
            results[0]["lexical_routes"],
            json!([
                {"route": "anchor:reserve_stock", "score_micros": 900_000, "matched_terms": ["reserve_stock"]},
                {
                    "route": "symbol:stock",
                    "score_micros": 100_000,
                    "matched_terms": ["stock"],
                    "spelling_variants": [{"query": "stokc", "alternative": "stock"}]
                },
            ])
        );
        assert_eq!(
            result_route_suffix(&results[0]),
            " · via anchor:reserve_stock, symbol:stock · spelling stokc to stock"
        );

        let mut md = Md::new();
        append_routes_md(&mut md, &output);
        let rendered = md.render();
        assert!(
            rendered.contains(
                "Ranked routes fused into this page: query, anchor:reserve_stock, symbol:stock"
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains("Strict query: `memoization`"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Alternative tried: `cache`"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Reason: configured vocabulary alias"),
            "{rendered}"
        );
    }
}
