use serde_json::Value;
use tracedecay_contracts::retrieval::{
    ContextLexicalAnchorV1, LexicalAnchorDropReasonV1, LexicalAnchorDropV1,
    SearchLexicalAlternativeReasonV1, SearchLexicalFieldV1, SearchLexicalRouteV1,
    SearchResultRowV1, SearchRouteMatchV1, SearchSpellingVariantV1, SearchSurfaceRequestV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_query::retrieval::lexical::{
    LexicalAliasV1, LexicalAlternativeReasonV1, LexicalAnchorOutcomeV1, LexicalAnchorReceiptV1,
    LexicalFieldFilterV1, LexicalFieldV1, LexicalProximityV1, LexicalRouteKindV1,
    LexicalRouteReceiptV1, LexicalRoutingV1,
};

use crate::tools::render::Md;

/// The kernel routing for one typed search request. The kernel bounds and
/// validates the anchors and aliases; a violation is a typed request error.
pub(super) fn routing_from_request(request: &SearchSurfaceRequestV1) -> Result<LexicalRoutingV1> {
    let aliases = request
        .lexical_aliases
        .iter()
        .flatten()
        .map(|alias| LexicalAliasV1 {
            strict_query: alias.strict_query.clone(),
            alternative: alias.alternative.clone(),
        })
        .collect();
    let mut routing = routing_from_parts(
        request.lexical_anchors.clone().unwrap_or_default(),
        request.prefer_symbol.unwrap_or(false),
    )?
    .with_aliases(aliases)
    .map_err(|error| TraceDecayError::Config {
        message: error.to_string(),
    })?;
    routing.phrases = request.lexical_phrases.clone().unwrap_or_default();
    routing.proximities = request
        .lexical_proximities
        .iter()
        .flatten()
        .map(|proximity| LexicalProximityV1 {
            terms: proximity.terms.clone(),
            maximum_gap: proximity.maximum_gap,
        })
        .collect();
    routing.field_filters = request
        .lexical_field_filters
        .iter()
        .flatten()
        .map(|filter| LexicalFieldFilterV1 {
            field: lexical_field(filter.field),
            include: filter.include,
        })
        .collect();
    Ok(routing)
}

fn lexical_field(field: SearchLexicalFieldV1) -> LexicalFieldV1 {
    match field {
        SearchLexicalFieldV1::SymbolName => LexicalFieldV1::SymbolName,
        SearchLexicalFieldV1::QualifiedName => LexicalFieldV1::QualifiedName,
        SearchLexicalFieldV1::Path => LexicalFieldV1::Path,
        SearchLexicalFieldV1::Signature => LexicalFieldV1::Signature,
        SearchLexicalFieldV1::Documentation => LexicalFieldV1::Documentation,
        SearchLexicalFieldV1::BodyText => LexicalFieldV1::BodyText,
        SearchLexicalFieldV1::PreambleText => LexicalFieldV1::PreambleText,
        SearchLexicalFieldV1::ExactTerm => LexicalFieldV1::ExactTerm,
        SearchLexicalFieldV1::Subtoken => LexicalFieldV1::Subtoken,
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

/// The page-level route and anchor evidence, attached only when a route
/// beyond the strict query ran or ranked a result, and each ranked row's
/// matching routes.
pub(super) fn route_evidence(
    results: &mut [SearchResultRowV1],
    receipt: &LexicalRouteReceiptV1,
) -> (
    Option<Vec<SearchLexicalRouteV1>>,
    Option<Vec<ContextLexicalAnchorV1>>,
) {
    if !receipt.has_disclosure() {
        return (None, None);
    }
    for row in results.iter_mut() {
        let Some(matches) = receipt.matches_by_anchor.get(&row.candidate.anchor_id) else {
            continue;
        };
        row.lexical_routes = Some(
            matches
                .iter()
                .map(|route_match| SearchRouteMatchV1 {
                    route: route_label(&route_match.route),
                    score_micros: route_match.score_micros,
                    matched_terms: route_match.matched_terms.clone(),
                    spelling_variants: route_match
                        .spelling_variants
                        .iter()
                        .map(|variant| SearchSpellingVariantV1 {
                            query: variant.query.clone(),
                            alternative: variant.alternative.clone(),
                        })
                        .collect(),
                })
                .collect(),
        );
    }
    let routes = receipt.routes.iter().map(search_route).collect();
    let anchors =
        (!receipt.anchors.is_empty()).then(|| receipt.anchors.iter().map(anchor_outcome).collect());
    (Some(routes), anchors)
}

fn search_route(route: &LexicalRouteKindV1) -> SearchLexicalRouteV1 {
    let label = route_label(route);
    match route {
        LexicalRouteKindV1::Query => SearchLexicalRouteV1::Query { label },
        LexicalRouteKindV1::Anchor { anchor } => SearchLexicalRouteV1::Anchor {
            anchor: anchor.as_str().to_owned(),
            label,
        },
        LexicalRouteKindV1::PreferredSymbol { tokens } => SearchLexicalRouteV1::PreferredSymbol {
            tokens: tokens.clone(),
            label,
        },
        LexicalRouteKindV1::IdentifierSplit {
            strict_query,
            terms,
        } => SearchLexicalRouteV1::IdentifierSplit {
            strict_query: strict_query.clone(),
            terms: terms.clone(),
            label,
        },
        LexicalRouteKindV1::Alias {
            strict_query,
            alternative,
            reason: LexicalAlternativeReasonV1::ConfiguredVocabularyAlias,
        } => SearchLexicalRouteV1::Alias {
            strict_query: strict_query.clone(),
            alternative: alternative.clone(),
            reason: SearchLexicalAlternativeReasonV1::ConfiguredVocabularyAlias,
            label,
        },
    }
}

/// One caller anchor's kernel receipt in its wire shape.
pub(super) fn anchor_outcome(receipt: &LexicalAnchorReceiptV1) -> ContextLexicalAnchorV1 {
    let anchor = receipt.anchor.as_str().to_owned();
    match &receipt.outcome {
        LexicalAnchorOutcomeV1::Matched {
            matched,
            admitted,
            dropped,
        } => ContextLexicalAnchorV1::Matched {
            anchor,
            matched: *matched,
            admitted: *admitted,
            dropped: dropped.clone(),
        },
        LexicalAnchorOutcomeV1::Unmatched => ContextLexicalAnchorV1::Unmatched { anchor },
        LexicalAnchorOutcomeV1::NotServed => ContextLexicalAnchorV1::NotServed { anchor },
    }
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

/// How many rows carried a matched anchor, how many of its sites this
/// response returns, and why any other admitted site is missing.
pub(super) fn matched_anchor_line(
    anchor: &str,
    matched: u64,
    admitted: u64,
    dropped: &[LexicalAnchorDropV1],
) -> String {
    let line = format!("- `{anchor}`: {matched} matches, {admitted} returned");
    if dropped.is_empty() {
        return line;
    }
    let reasons = dropped
        .iter()
        .map(|drop| {
            let reason = match drop.reason {
                LexicalAnchorDropReasonV1::DiversityCap => "diversity cap",
                LexicalAnchorDropReasonV1::OutsidePage => "outside this page",
                LexicalAnchorDropReasonV1::NotHydrated => "not hydrated",
                LexicalAnchorDropReasonV1::OutOfScope => "out of scope",
            };
            format!("{} {reason}", drop.sites)
        })
        .collect::<Vec<_>>();
    format!("{line}, dropped {}", reasons.join(", "))
}

/// One human line per caller anchor: its matched and returned counts, or
/// that it matched nothing / was not served.
fn anchor_receipt_line(receipt: &LexicalAnchorReceiptV1) -> String {
    let anchor = receipt.anchor.as_str();
    match &receipt.outcome {
        LexicalAnchorOutcomeV1::Matched {
            matched,
            admitted,
            dropped,
        } => matched_anchor_line(anchor, *matched, *admitted, dropped),
        LexicalAnchorOutcomeV1::Unmatched => format!("- `{anchor}`: no matches"),
        LexicalAnchorOutcomeV1::NotServed => format!("- `{anchor}`: route not served"),
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
    for anchor in value
        .get("lexical_anchors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Ok(receipt) = serde_json::from_value::<LexicalAnchorReceiptV1>(anchor.clone()) else {
            continue;
        };
        md.line(&anchor_receipt_line(&receipt));
    }
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

    use serde_json::json;
    use tracedecay_query::retrieval::lexical::{LexicalRouteMatchV1, MAX_LEXICAL_ANCHORS_V1};

    use super::*;

    fn routing(arguments: &Value) -> Result<LexicalRoutingV1> {
        let request: SearchSurfaceRequestV1 =
            crate::handlers::support::decode_primitive_request(arguments, "tracedecay_search")?;
        routing_from_request(&request)
    }

    #[test]
    fn routing_requests_decode_and_reject_typed_violations() {
        let routing_plan = routing(&json!({
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
        assert_eq!(routing_plan.anchors.len(), 2);
        assert!(routing_plan.prefer_symbol);
        assert_eq!(routing_plan.aliases[0].strict_query, "memoization");
        assert_eq!(routing_plan.aliases[0].alternative, "cache");
        assert_eq!(routing_plan.phrases, ["durable cache"]);
        assert_eq!(routing_plan.proximities[0].terms, ["durable", "owner"]);
        assert_eq!(
            routing_plan.field_filters,
            [LexicalFieldFilterV1 {
                field: LexicalFieldV1::Documentation,
                include: true,
            }]
        );

        let plain = routing(&json!({"query": "inventory"})).expect("query only");
        assert_eq!(plain, LexicalRoutingV1::default());

        let too_many: Vec<String> = (0..=MAX_LEXICAL_ANCHORS_V1)
            .map(|index| format!("anchor_{index}"))
            .collect();
        let error = routing(&json!({"query": "q", "lexical_anchors": too_many}))
            .expect_err("anchor count is bounded");
        assert!(
            error.to_string().contains("at most 8 anchors"),
            "typed bound in the message: {error}"
        );
        let error = routing(&json!({"query": "q", "lexical_anchors": ["ok", ""]}))
            .expect_err("empty anchors are rejected");
        assert!(error.to_string().contains("anchor 1 is empty"), "{error}");
        let error = routing(&json!({"query": "q", "lexical_anchors": ["two words"]}))
            .expect_err("multi-term anchors are rejected");
        assert!(error.to_string().contains("one identifier"), "{error}");
        assert_eq!(
            routing(&json!({"query": "q", "lexical_anchors": "reserve_stock"}))
                .expect_err("a bare string is not an anchor list")
                .to_string(),
            "config error: invalid arguments for tracedecay_search: invalid type: string \"reserve_stock\", expected a sequence"
        );
        assert_eq!(
            routing(&json!({"query": "q", "prefer_symbol": "yes"}))
                .expect_err("prefer_symbol must be a boolean")
                .to_string(),
            "config error: invalid arguments for tracedecay_search: invalid type: string \"yes\", expected a boolean"
        );

        let aliases = (0..=tracedecay_query::retrieval::lexical::MAX_LEXICAL_ALIASES_V1)
            .map(|index| {
                json!({
                    "strict_query": "memoization",
                    "alternative": format!("cache_{index}")
                })
            })
            .collect::<Vec<_>>();
        assert!(
            routing(&json!({"query": "q", "lexical_aliases": aliases}))
                .expect_err("aliases are bounded")
                .to_string()
                .contains("at most 8")
        );
    }

    fn row(anchor: &str) -> SearchResultRowV1 {
        SearchResultRowV1 {
            candidate: serde_json::from_value(json!({
                "anchor_id": anchor,
                "logical_evidence_id": "evidence.reserve",
                "occurrences": [],
                "exact_class": "approximate",
                "utility_micros": 0,
                "contributions": [],
                "freshness": [],
                "decisions": [],
            }))
            .expect("fused candidate"),
            final_ordinal: 0,
            node_id: None,
            display: None,
            lexical_routes: None,
        }
    }

    #[test]
    fn route_evidence_is_attached_only_when_additional_routes_ran() {
        let mut results = vec![row("code-symbol:reserve")];
        let query_only = LexicalRouteReceiptV1 {
            routes: vec![LexicalRouteKindV1::Query],
            matches_by_anchor: BTreeMap::new(),
            anchors: Vec::new(),
            dropped_sites: BTreeMap::new(),
        };
        assert_eq!(route_evidence(&mut results, &query_only), (None, None));
        assert_eq!(results[0].lexical_routes, None);
        assert_eq!(
            result_route_suffix(&serde_json::to_value(&results[0]).unwrap()),
            ""
        );
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
            anchors: vec![
                LexicalAnchorReceiptV1 {
                    anchor: LexicalRoutingV1::new(vec!["reserve_stock".to_owned()], false)
                        .expect("routing")
                        .anchors
                        .remove(0),
                    outcome: LexicalAnchorOutcomeV1::Matched {
                        matched: 4,
                        admitted: 1,
                        dropped: vec![LexicalAnchorDropV1 {
                            reason: LexicalAnchorDropReasonV1::OutsidePage,
                            sites: 2,
                        }],
                    },
                },
                LexicalAnchorReceiptV1 {
                    anchor: LexicalRoutingV1::new(vec!["release_stock".to_owned()], false)
                        .expect("routing")
                        .anchors
                        .remove(0),
                    outcome: LexicalAnchorOutcomeV1::Unmatched,
                },
            ],
            dropped_sites: BTreeMap::new(),
        };
        let (routes, anchors) = route_evidence(&mut results, &receipt);
        let output = json!({"lexical_routes": routes, "lexical_anchors": anchors});
        let results = [serde_json::to_value(&results[0]).unwrap()];
        assert_eq!(
            output["lexical_anchors"],
            json!([
                {
                    "anchor": "reserve_stock",
                    "outcome": "matched",
                    "matched": 4,
                    "admitted": 1,
                    "dropped": [{"reason": "outside_page", "sites": 2}],
                },
                {"anchor": "release_stock", "outcome": "unmatched"},
            ])
        );
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
        assert!(
            rendered
                .contains("- `reserve_stock`: 4 matches, 1 returned, dropped 2 outside this page"),
            "{rendered}"
        );
        assert!(
            rendered.contains("- `release_stock`: no matches"),
            "{rendered}"
        );
    }
}
