//! Caller-facing decoding and rendering of additive lexical routes
//! (`lexical_anchors`, `prefer_symbol`) for the search and context tools.
//!
//! Validation is owned by the retrieval kernel (`LexicalRoutingV1::new`);
//! this module only turns raw tool arguments into that typed value and turns
//! the executor's route receipt into response evidence.

use serde_json::{Value, json};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_mcp::tools::render::Md;
use tracedecay_query::retrieval::lexical::{
    LexicalRouteKindV1, LexicalRouteReceiptV1, LexicalRoutingV1,
};

/// Decode `lexical_anchors` / `prefer_symbol` from raw tool arguments.
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
    routing_from_parts(anchors, prefer_symbol)
}

/// Build the typed routing from already-decoded request fields.
pub(super) fn routing_from_parts(
    anchors: Vec<String>,
    prefer_symbol: bool,
) -> Result<LexicalRoutingV1> {
    LexicalRoutingV1::new(anchors, prefer_symbol).map_err(|error| TraceDecayError::Config {
        message: error.to_string(),
    })
}

/// Short human label for one route, used in rendered evidence.
pub(super) fn route_label(route: &LexicalRouteKindV1) -> String {
    match route {
        LexicalRouteKindV1::Query => "query".to_owned(),
        LexicalRouteKindV1::Anchor { anchor } => format!("anchor:{}", anchor.as_str()),
        LexicalRouteKindV1::PreferredSymbol { tokens } => {
            format!("symbol:{}", tokens.join("|"))
        }
    }
}

/// Attach route evidence to a search response: the executed routes at the
/// top level and, per result, the routes that ranked it. Emitted only when
/// the caller asked for routes beyond the query, so a plain query's response
/// bytes are unchanged.
pub(super) fn attach_route_evidence(
    output: &mut Value,
    results: &mut [Value],
    receipt: &LexicalRouteReceiptV1,
) -> Result<()> {
    if !receipt.has_additional_routes() {
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
                    json!({
                        "route": route_label(&route_match.route),
                        "score_micros": route_match.score_micros,
                        "matched_terms": route_match.matched_terms,
                    })
                })
                .collect::<Vec<_>>()
        );
    }
    Ok(())
}

/// The ` · via …` suffix naming the routes that ranked one rendered result.
pub(super) fn result_route_suffix(result: &Value) -> String {
    let Some(routes) = result.get("lexical_routes").and_then(Value::as_array) else {
        return String::new();
    };
    let labels: Vec<&str> = routes
        .iter()
        .filter_map(|route| route.get("route").and_then(Value::as_str))
        .collect();
    if labels.is_empty() {
        String::new()
    } else {
        format!(" · via {}", labels.join(", "))
    }
}

/// The routes section of a rendered search page.
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
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tracedecay_query::retrieval::lexical::{LexicalRouteMatchV1, MAX_LEXICAL_ANCHORS_V1};

    use super::*;

    #[test]
    fn routing_args_decode_and_reject_typed_violations() {
        let routing = routing_from_args(&json!({
            "query": "inventory",
            "lexical_anchors": ["reserve_stock", "Foo::bar"],
            "prefer_symbol": true,
        }))
        .expect("valid routing");
        assert_eq!(routing.anchors.len(), 2);
        assert!(routing.prefer_symbol);

        let plain = routing_from_args(&json!({"query": "inventory"})).expect("query only");
        assert!(plain.is_query_only());

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
        let receipt = LexicalRouteReceiptV1 {
            routes: vec![
                LexicalRouteKindV1::Query,
                anchor_route.clone(),
                symbol_route.clone(),
            ],
            matches_by_anchor: BTreeMap::from([(
                tracedecay_domain::RetrievalAnchorId::new("code-symbol:reserve").expect("anchor"),
                vec![
                    LexicalRouteMatchV1 {
                        route: anchor_route,
                        score_micros: 900_000,
                        matched_terms: vec!["reserve_stock".to_owned()],
                    },
                    LexicalRouteMatchV1 {
                        route: symbol_route,
                        score_micros: 100_000,
                        matched_terms: vec!["stock".to_owned()],
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
            ])
        );
        assert_eq!(
            results[0]["lexical_routes"],
            json!([
                {"route": "anchor:reserve_stock", "score_micros": 900_000, "matched_terms": ["reserve_stock"]},
                {"route": "symbol:stock", "score_micros": 100_000, "matched_terms": ["stock"]},
            ])
        );
        assert_eq!(
            result_route_suffix(&results[0]),
            " · via anchor:reserve_stock, symbol:stock"
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
    }
}
