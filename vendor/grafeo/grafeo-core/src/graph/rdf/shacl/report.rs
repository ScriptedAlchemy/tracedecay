//! SHACL validation report.
//!
//! Implements the W3C SHACL Validation Report vocabulary, providing
//! structured results that can be materialized as RDF triples.

use std::fmt;

use crate::graph::rdf::Term;

use super::shape::{PropertyPath, RDF, SH, Severity};

/// A SHACL validation report.
#[derive(Debug, Clone)]
pub struct ValidationReport {
    /// Whether the data conforms to all shapes (`true` = no violations).
    pub conforms: bool,
    /// Individual validation results.
    pub results: Vec<ValidationResult>,
}

impl ValidationReport {
    /// Creates a conforming report (no violations).
    #[must_use]
    pub fn conforming() -> Self {
        Self {
            conforms: true,
            results: Vec::new(),
        }
    }

    /// Creates a report from a set of results. Conforms if no violations.
    #[must_use]
    pub fn from_results(results: Vec<ValidationResult>) -> Self {
        let conforms = results.iter().all(|r| r.severity != Severity::Violation);
        Self { conforms, results }
    }
}

/// A single SHACL validation result.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// The focus node that was validated.
    pub focus_node: Term,
    /// The constraint component that produced this result (IRI).
    pub source_constraint_component: String,
    /// The shape that produced this result.
    pub source_shape: Term,
    /// The value node that caused the violation (if applicable).
    pub value: Option<Term>,
    /// The property path (for property shape results).
    pub result_path: Option<PropertyPath>,
    /// The severity level.
    pub severity: Severity,
    /// A human-readable message.
    pub message: Option<String>,
}

impl fmt::Display for ValidationReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let violations = self
            .results
            .iter()
            .filter(|r| r.severity == Severity::Violation)
            .count();
        let warnings = self
            .results
            .iter()
            .filter(|r| r.severity == Severity::Warning)
            .count();

        if self.conforms {
            write!(f, "Validation Report: PASSED")?;
        } else {
            write!(f, "Validation Report: FAILED")?;
        }
        if violations > 0 || warnings > 0 {
            write!(f, " ({violations} violation(s), {warnings} warning(s))")?;
        }
        writeln!(f)?;

        for result in &self.results {
            let severity = match result.severity {
                Severity::Violation => "Violation",
                Severity::Warning => "Warning",
                Severity::Info => "Info",
            };
            write!(f, "  [{severity}] {}", result.focus_node)?;
            if let Some(ref msg) = result.message {
                write!(f, " - {msg}")?;
            }
            writeln!(f)?;
        }

        Ok(())
    }
}

/// Materializes the validation report as RDF triples.
///
/// Produces triples following the W3C SHACL Validation Report vocabulary
/// (section 3.6 of the spec).
impl ValidationReport {
    /// Converts the report to a set of RDF triples.
    #[must_use]
    pub fn to_triples(&self) -> Vec<crate::graph::rdf::Triple> {
        use crate::graph::rdf::Triple;

        let mut triples = Vec::new();
        let report_node = Term::blank("report");
        let rdf_type = Term::iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");

        // Report type
        triples.push(Triple::new(
            report_node.clone(),
            rdf_type.clone(),
            Term::iri(SH::VALIDATION_REPORT),
        ));

        // sh:conforms
        triples.push(Triple::new(
            report_node.clone(),
            Term::iri(SH::CONFORMS),
            Term::typed_literal(
                if self.conforms { "true" } else { "false" },
                "http://www.w3.org/2001/XMLSchema#boolean",
            ),
        ));

        // Each result
        for (i, result) in self.results.iter().enumerate() {
            let result_node = Term::blank(format!("result_{i}"));
            triples.push(Triple::new(
                report_node.clone(),
                Term::iri(SH::RESULT),
                result_node.clone(),
            ));
            triples.push(Triple::new(
                result_node.clone(),
                rdf_type.clone(),
                Term::iri(SH::VALIDATION_RESULT),
            ));

            // Focus node
            triples.push(Triple::new(
                result_node.clone(),
                Term::iri(SH::FOCUS_NODE),
                result.focus_node.clone(),
            ));

            // Source constraint component
            triples.push(Triple::new(
                result_node.clone(),
                Term::iri(SH::SOURCE_CONSTRAINT_COMPONENT),
                Term::iri(&*result.source_constraint_component),
            ));

            // Source shape
            triples.push(Triple::new(
                result_node.clone(),
                Term::iri(SH::SOURCE_SHAPE),
                result.source_shape.clone(),
            ));

            // Value (optional)
            if let Some(ref value) = result.value {
                triples.push(Triple::new(
                    result_node.clone(),
                    Term::iri(SH::VALUE),
                    value.clone(),
                ));
            }

            // Result path (optional)
            let mut path_counter = 0;
            if let Some(ref path) = result.result_path {
                let path_node = serialize_path(path, i, &mut triples, &mut path_counter);
                triples.push(Triple::new(
                    result_node.clone(),
                    Term::iri(SH::RESULT_PATH),
                    path_node,
                ));
            }

            // Severity
            let severity_iri = match result.severity {
                Severity::Violation => SH::SEVERITY_VIOLATION,
                Severity::Warning => SH::SEVERITY_WARNING,
                Severity::Info => SH::SEVERITY_INFO,
            };
            triples.push(Triple::new(
                result_node.clone(),
                Term::iri(SH::RESULT_SEVERITY),
                Term::iri(severity_iri),
            ));

            // Message (optional)
            if let Some(ref msg) = result.message {
                triples.push(Triple::new(
                    result_node,
                    Term::iri(SH::RESULT_MESSAGE),
                    Term::literal(msg.as_str()),
                ));
            }
        }

        triples
    }
}

/// Serializes a [`PropertyPath`] as RDF triples and returns the term representing it.
///
/// Simple predicate paths return the IRI directly. Complex paths produce blank-node
/// structures following the SHACL path vocabulary. The `counter` parameter ensures
/// deterministic blank-node IDs within a single `to_triples()` call.
fn serialize_path(
    path: &PropertyPath,
    result_idx: usize,
    triples: &mut Vec<crate::graph::rdf::Triple>,
    counter: &mut usize,
) -> Term {
    use crate::graph::rdf::Triple;

    /// Allocates a fresh blank node scoped to this result.
    fn next_bnode(result_idx: usize, counter: &mut usize) -> Term {
        let id = *counter;
        *counter += 1;
        Term::blank(format!("path_{result_idx}_{id}"))
    }

    match path {
        PropertyPath::Predicate(iri) => iri.clone(),
        PropertyPath::Inverse(inner) => {
            let bnode = next_bnode(result_idx, counter);
            let inner_term = serialize_path(inner, result_idx, triples, counter);
            triples.push(Triple::new(
                bnode.clone(),
                Term::iri(SH::INVERSE_PATH),
                inner_term,
            ));
            bnode
        }
        PropertyPath::ZeroOrMore(inner) => {
            let bnode = next_bnode(result_idx, counter);
            let inner_term = serialize_path(inner, result_idx, triples, counter);
            triples.push(Triple::new(
                bnode.clone(),
                Term::iri(SH::ZERO_OR_MORE_PATH),
                inner_term,
            ));
            bnode
        }
        PropertyPath::OneOrMore(inner) => {
            let bnode = next_bnode(result_idx, counter);
            let inner_term = serialize_path(inner, result_idx, triples, counter);
            triples.push(Triple::new(
                bnode.clone(),
                Term::iri(SH::ONE_OR_MORE_PATH),
                inner_term,
            ));
            bnode
        }
        PropertyPath::ZeroOrOne(inner) => {
            let bnode = next_bnode(result_idx, counter);
            let inner_term = serialize_path(inner, result_idx, triples, counter);
            triples.push(Triple::new(
                bnode.clone(),
                Term::iri(SH::ZERO_OR_ONE_PATH),
                inner_term,
            ));
            bnode
        }
        PropertyPath::Sequence(paths) => serialize_rdf_list(paths, result_idx, triples, counter),
        PropertyPath::Alternative(paths) => {
            let bnode = next_bnode(result_idx, counter);
            let list_head = serialize_rdf_list(paths, result_idx, triples, counter);
            triples.push(Triple::new(
                bnode.clone(),
                Term::iri(SH::ALTERNATIVE_PATH),
                list_head,
            ));
            bnode
        }
    }
}

/// Serializes a list of paths as an RDF collection (rdf:first/rdf:rest chain).
fn serialize_rdf_list(
    paths: &[PropertyPath],
    result_idx: usize,
    triples: &mut Vec<crate::graph::rdf::Triple>,
    counter: &mut usize,
) -> Term {
    use crate::graph::rdf::Triple;

    if paths.is_empty() {
        return Term::iri(RDF::NIL);
    }

    let mut head = None;
    let mut prev: Option<Term> = None;

    for path in paths {
        let id = *counter;
        *counter += 1;
        let node = Term::blank(format!("list_{result_idx}_{id}"));
        let item = serialize_path(path, result_idx, triples, counter);
        triples.push(Triple::new(node.clone(), Term::iri(RDF::FIRST), item));

        if let Some(prev_node) = prev {
            triples.push(Triple::new(prev_node, Term::iri(RDF::REST), node.clone()));
        }
        if head.is_none() {
            head = Some(node.clone());
        }
        prev = Some(node);
    }

    // Close the list
    if let Some(last) = prev {
        triples.push(Triple::new(last, Term::iri(RDF::REST), Term::iri(RDF::NIL)));
    }

    head.unwrap_or_else(|| Term::iri(RDF::NIL))
}

#[cfg(test)]
mod tests {
    use super::super::shape::{PropertyPath, RDF, SH, Severity};
    use super::*;
    use crate::graph::rdf::Triple;

    /// Creates a minimal `ValidationResult` with the given `result_path`.
    fn result_with_path(path: PropertyPath) -> ValidationResult {
        ValidationResult {
            focus_node: Term::iri("http://ex.org/alix"),
            source_constraint_component: format!("{}MinCountConstraintComponent", SH::NS),
            source_shape: Term::iri("http://ex.org/PersonShape"),
            severity: Severity::Violation,
            result_path: Some(path),
            value: None,
            message: None,
        }
    }

    /// Returns true if any triple in `triples` has the given predicate and object.
    fn has_triple_po(triples: &[Triple], predicate: &str, object: &Term) -> bool {
        let pred = Term::iri(predicate);
        triples
            .iter()
            .any(|t| *t.predicate() == pred && *t.object() == *object)
    }

    /// Returns true if any triple in `triples` has the given predicate IRI.
    fn has_predicate(triples: &[Triple], predicate: &str) -> bool {
        let pred = Term::iri(predicate);
        triples.iter().any(|t| *t.predicate() == pred)
    }

    /// Finds the object of the first triple with the given subject and predicate.
    fn find_object(triples: &[Triple], subject: &Term, predicate: &str) -> Option<Term> {
        let pred = Term::iri(predicate);
        triples
            .iter()
            .find(|t| *t.subject() == *subject && *t.predicate() == pred)
            .map(|t| t.object().clone())
    }

    #[test]
    fn test_serialize_predicate_path() {
        let name_iri = Term::iri("http://ex.org/name");
        let report = ValidationReport::from_results(vec![result_with_path(
            PropertyPath::Predicate(name_iri.clone()),
        )]);

        let triples = report.to_triples();

        // sh:resultPath should point directly to the predicate IRI
        assert!(
            has_triple_po(&triples, SH::RESULT_PATH, &name_iri),
            "Expected sh:resultPath pointing to <http://ex.org/name>"
        );

        // No blank-node indirection for a simple predicate path
        assert!(
            !has_predicate(&triples, SH::INVERSE_PATH),
            "Simple predicate path should not produce sh:inversePath"
        );
    }

    #[test]
    fn test_serialize_inverse_path() {
        let name_iri = Term::iri("http://ex.org/name");
        let report = ValidationReport::from_results(vec![result_with_path(PropertyPath::Inverse(
            Box::new(PropertyPath::Predicate(name_iri.clone())),
        ))]);

        let triples = report.to_triples();

        // sh:resultPath should point to a blank node (the inverse wrapper)
        let result_node = Term::blank("result_0");
        let path_bnode = find_object(&triples, &result_node, SH::RESULT_PATH)
            .expect("Missing sh:resultPath triple");
        assert!(
            path_bnode.is_blank_node(),
            "Inverse path should be represented by a blank node"
        );

        // That blank node should have sh:inversePath -> name IRI
        let inner = find_object(&triples, &path_bnode, SH::INVERSE_PATH)
            .expect("Missing sh:inversePath triple");
        assert_eq!(inner, name_iri);
    }

    #[test]
    fn test_serialize_sequence_path() {
        let knows = Term::iri("http://ex.org/knows");
        let name = Term::iri("http://ex.org/name");
        let report =
            ValidationReport::from_results(vec![result_with_path(PropertyPath::Sequence(vec![
                PropertyPath::Predicate(knows.clone()),
                PropertyPath::Predicate(name.clone()),
            ]))]);

        let triples = report.to_triples();

        // sh:resultPath should point to the head of an rdf:list
        let result_node = Term::blank("result_0");
        let list_head = find_object(&triples, &result_node, SH::RESULT_PATH)
            .expect("Missing sh:resultPath triple");
        assert!(
            list_head.is_blank_node(),
            "Sequence path should start with a blank node (rdf:list head)"
        );

        // First element: rdf:first -> knows
        let first =
            find_object(&triples, &list_head, RDF::FIRST).expect("Missing rdf:first on list head");
        assert_eq!(first, knows);

        // rdf:rest -> second node
        let rest =
            find_object(&triples, &list_head, RDF::REST).expect("Missing rdf:rest on list head");
        assert!(
            rest.is_blank_node(),
            "rdf:rest should point to a blank node"
        );

        // Second element: rdf:first -> name
        let second = find_object(&triples, &rest, RDF::FIRST)
            .expect("Missing rdf:first on second list node");
        assert_eq!(second, name);

        // rdf:rest -> rdf:nil (end of list)
        let nil =
            find_object(&triples, &rest, RDF::REST).expect("Missing rdf:rest on last list node");
        assert_eq!(nil, Term::iri(RDF::NIL));
    }

    #[test]
    fn test_serialize_alternative_path() {
        let knows = Term::iri("http://ex.org/knows");
        let name = Term::iri("http://ex.org/name");
        let report = ValidationReport::from_results(vec![result_with_path(
            PropertyPath::Alternative(vec![
                PropertyPath::Predicate(knows.clone()),
                PropertyPath::Predicate(name.clone()),
            ]),
        )]);

        let triples = report.to_triples();

        // sh:resultPath -> blank node (the alternative wrapper)
        let result_node = Term::blank("result_0");
        let alt_bnode = find_object(&triples, &result_node, SH::RESULT_PATH)
            .expect("Missing sh:resultPath triple");
        assert!(alt_bnode.is_blank_node());

        // That blank node has sh:alternativePath -> head of rdf:list
        let list_head = find_object(&triples, &alt_bnode, SH::ALTERNATIVE_PATH)
            .expect("Missing sh:alternativePath triple");
        assert!(
            list_head.is_blank_node(),
            "sh:alternativePath should point to an rdf:list head"
        );

        // Verify the list contains both IRIs
        let first = find_object(&triples, &list_head, RDF::FIRST)
            .expect("Missing rdf:first on alternative list");
        assert_eq!(first, knows);

        let rest = find_object(&triples, &list_head, RDF::REST)
            .expect("Missing rdf:rest on alternative list head");
        let second = find_object(&triples, &rest, RDF::FIRST)
            .expect("Missing rdf:first on second alternative list node");
        assert_eq!(second, name);
    }

    #[test]
    fn test_serialize_zero_or_more_path() {
        let knows = Term::iri("http://ex.org/knows");
        let report = ValidationReport::from_results(vec![result_with_path(
            PropertyPath::ZeroOrMore(Box::new(PropertyPath::Predicate(knows.clone()))),
        )]);

        let triples = report.to_triples();

        let result_node = Term::blank("result_0");
        let path_bnode = find_object(&triples, &result_node, SH::RESULT_PATH)
            .expect("Missing sh:resultPath triple");
        assert!(path_bnode.is_blank_node());

        let inner = find_object(&triples, &path_bnode, SH::ZERO_OR_MORE_PATH)
            .expect("Missing sh:zeroOrMorePath triple");
        assert_eq!(inner, knows);
    }

    #[test]
    fn test_serialize_one_or_more_path() {
        let knows = Term::iri("http://ex.org/knows");
        let report = ValidationReport::from_results(vec![result_with_path(
            PropertyPath::OneOrMore(Box::new(PropertyPath::Predicate(knows.clone()))),
        )]);

        let triples = report.to_triples();

        let result_node = Term::blank("result_0");
        let path_bnode = find_object(&triples, &result_node, SH::RESULT_PATH)
            .expect("Missing sh:resultPath triple");
        assert!(path_bnode.is_blank_node());

        let inner = find_object(&triples, &path_bnode, SH::ONE_OR_MORE_PATH)
            .expect("Missing sh:oneOrMorePath triple");
        assert_eq!(inner, knows);
    }

    #[test]
    fn test_serialize_zero_or_one_path() {
        let knows = Term::iri("http://ex.org/knows");
        let report = ValidationReport::from_results(vec![result_with_path(
            PropertyPath::ZeroOrOne(Box::new(PropertyPath::Predicate(knows.clone()))),
        )]);

        let triples = report.to_triples();

        let result_node = Term::blank("result_0");
        let path_bnode = find_object(&triples, &result_node, SH::RESULT_PATH)
            .expect("Missing sh:resultPath triple");
        assert!(path_bnode.is_blank_node());

        let inner = find_object(&triples, &path_bnode, SH::ZERO_OR_ONE_PATH)
            .expect("Missing sh:zeroOrOnePath triple");
        assert_eq!(inner, knows);
    }

    #[test]
    fn test_serialize_empty_sequence() {
        let report =
            ValidationReport::from_results(vec![result_with_path(PropertyPath::Sequence(vec![]))]);

        let triples = report.to_triples();

        // Empty sequence should serialize as rdf:nil directly
        assert!(
            has_triple_po(&triples, SH::RESULT_PATH, &Term::iri(RDF::NIL)),
            "Empty sequence should produce sh:resultPath -> rdf:nil"
        );

        // No rdf:first or rdf:rest triples for an empty list
        assert!(
            !has_predicate(&triples, RDF::FIRST),
            "Empty sequence should not produce any rdf:first triples"
        );
        assert!(
            !has_predicate(&triples, RDF::REST),
            "Empty sequence should not produce any rdf:rest triples"
        );
    }
}
