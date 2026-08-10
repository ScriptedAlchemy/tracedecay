//! `tracedecay_circular` — bounded cyclic-dependency reporting.

use super::*;

/// Default and ceiling for the number of cycles `tracedecay_circular` reports
/// in one call. A whole-repository cycle list runs to tens of kilobytes, which
/// the response budget then truncates into a retrieval handle; a declared limit
/// keeps the answer inside the budget and states what it left out.
const CIRCULAR_DEFAULT_LIMIT: usize = 25;
const CIRCULAR_MAX_LIMIT: usize = 200;

/// Default and ceiling for member files listed per reported cycle.
///
/// Bounding the cycle count alone does not bound the answer: a single
/// strongly connected component in a real workspace can contain hundreds of
/// files, so `limit: 3` still rendered tens of kilobytes and landed in the
/// truncation envelope. Each entry therefore reports a bounded member list
/// plus its true member count, so a declared bound always fits the budget.
const CIRCULAR_DEFAULT_MEMBER_LIMIT: usize = 12;
const CIRCULAR_MAX_MEMBER_LIMIT: usize = 200;

/// One reported cycle: the members that fit the declared member bound, plus
/// the component's true size so the omission is stated rather than hidden.
#[derive(Debug, PartialEq, Eq)]
struct BoundedCycle {
    members: Vec<String>,
    member_count: usize,
    omitted_member_count: usize,
}

/// Handles `tracedecay_circular` tool calls.
pub(crate) async fn handle_circular(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(CIRCULAR_DEFAULT_LIMIT, |limit| {
            (limit as usize).clamp(1, CIRCULAR_MAX_LIMIT)
        });
    let member_limit = args
        .get("member_limit")
        .and_then(Value::as_u64)
        .map_or(CIRCULAR_DEFAULT_MEMBER_LIMIT, |limit| {
            (limit as usize).clamp(1, CIRCULAR_MAX_MEMBER_LIMIT)
        });

    let all_cycles = graph.find_circular_dependencies().await?;
    let cycle_count = all_cycles.len();
    let (cycles, omitted) = bound_cycles(all_cycles, limit, member_limit);

    let output = circular_output(&cycles, cycle_count, omitted, limit, member_limit);

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        vec![],
        || render_circular_md(&cycles, cycle_count, omitted, limit),
    ))
}

fn circular_output(
    cycles: &[BoundedCycle],
    cycle_count: usize,
    omitted: usize,
    limit: usize,
    member_limit: usize,
) -> Value {
    let items: Vec<Value> = cycles
        .iter()
        .map(|cycle| {
            json!({
                "members": cycle.members,
                "member_count": cycle.member_count,
                "omitted_member_count": cycle.omitted_member_count,
            })
        })
        .collect();
    json!({
        "cycle_count": cycle_count,
        "reported_cycle_count": cycles.len(),
        "omitted_cycle_count": omitted,
        "limit": limit,
        "member_limit": member_limit,
        "cycles": items,
    })
}

/// Renders file-level dependency cycles as arrow chains that preserve cycle
/// order (`a.rs -> b.rs -> a.rs`) instead of collapsing the members into a
/// directory tree, which destroys the cyclic relationship. Each SCC's member
/// files are joined with ` -> ` and the first is repeated at the end to close
/// the loop.
/// Orders cycles largest-first and bounds them to `limit` cycles of
/// `member_limit` members each, returning the bounded page and the number of
/// cycles it leaves out.
///
/// The largest strongly connected components are the ones worth breaking, so a
/// bounded page reports the worst offenders rather than an arbitrary prefix.
/// Ties fall back to path order so repeated calls agree. Both the omitted cycle
/// count and each component's true member count are returned rather than
/// dropped: the caller always states what it left out.
fn bound_cycles(
    mut cycles: Vec<Vec<String>>,
    limit: usize,
    member_limit: usize,
) -> (Vec<BoundedCycle>, usize) {
    let omitted = cycles.len().saturating_sub(limit);
    cycles.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    cycles.truncate(limit);
    let bounded = cycles
        .into_iter()
        .map(|mut members| {
            let member_count = members.len();
            members.truncate(member_limit);
            BoundedCycle {
                omitted_member_count: member_count.saturating_sub(members.len()),
                members,
                member_count,
            }
        })
        .collect();
    (bounded, omitted)
}

fn render_circular_md(
    cycles: &[BoundedCycle],
    cycle_count: usize,
    omitted: usize,
    limit: usize,
) -> String {
    use std::fmt::Write as _;

    if cycle_count == 0 {
        return "No circular dependencies found.\n".to_string();
    }
    let mut out = String::new();
    let _ = writeln!(out, "# Circular Dependencies ({cycle_count})\n");
    for (i, cycle) in cycles.iter().enumerate() {
        let Some(entry) = cycle.members.first() else {
            continue;
        };
        let mut chain = cycle.members.join(" -> ");
        if cycle.omitted_member_count > 0 {
            // An elided component is not a closed loop; say so instead of
            // rendering a chain that reads as the whole cycle.
            let _ = write!(
                chain,
                " -> … ({} further member(s) not shown of {} at member_limit)",
                cycle.omitted_member_count, cycle.member_count
            );
        } else {
            // Close the loop by repeating the entry file.
            let _ = write!(chain, " -> {entry}");
        }
        let _ = writeln!(out, "{}. {chain}", i + 1);
    }
    if omitted > 0 {
        let _ = writeln!(
            out,
            "\n{omitted} further cycle(s) not shown at limit {limit}; raise `limit` (max {CIRCULAR_MAX_LIMIT}) to see more."
        );
    }
    out
}
#[cfg(test)]
mod circular_render_tests {
    use super::{
        CIRCULAR_DEFAULT_MEMBER_LIMIT, CIRCULAR_MAX_LIMIT, bound_cycles, circular_output,
        render_circular_md,
    };

    /// Mirrors [`crate::mcp::tools::MAX_RESPONSE_CHARS`], the point at which a
    /// response is replaced by a preview envelope plus a retrieval handle.
    const RESPONSE_BUDGET: usize = 15_000;

    fn cycle(files: &[&str]) -> Vec<String> {
        files.iter().map(|file| (*file).to_string()).collect()
    }

    fn bounded(files: &[&str], member_limit: usize) -> Vec<super::BoundedCycle> {
        bound_cycles(vec![cycle(files)], 1, member_limit).0
    }

    #[test]
    fn renders_arrow_chain_closing_the_loop() {
        let cycles = bounded(&["a.rs", "b.rs"], CIRCULAR_DEFAULT_MEMBER_LIMIT);
        let out = render_circular_md(&cycles, 1, 0, 25);
        assert!(out.contains("a.rs -> b.rs -> a.rs"), "got: {out}");
        assert!(out.contains("Circular Dependencies (1)"), "got: {out}");
    }

    #[test]
    fn renders_empty_state() {
        let out = render_circular_md(&[], 0, 0, 25);
        assert!(out.contains("No circular dependencies found"), "got: {out}");
    }

    #[test]
    fn numbers_multiple_cycles() {
        let (cycles, _) = bound_cycles(
            vec![cycle(&["c.rs", "d.rs", "e.rs"]), cycle(&["a.rs", "b.rs"])],
            2,
            CIRCULAR_DEFAULT_MEMBER_LIMIT,
        );
        let out = render_circular_md(&cycles, 2, 0, 25);
        assert!(
            out.contains("1. c.rs -> d.rs -> e.rs -> c.rs"),
            "got: {out}"
        );
        assert!(out.contains("2. a.rs -> b.rs -> a.rs"), "got: {out}");
    }

    #[test]
    fn bounded_page_keeps_the_largest_cycles_and_counts_the_rest() {
        let cycles = vec![
            cycle(&["small-b.rs", "small-b2.rs"]),
            cycle(&["big.rs", "big2.rs", "big3.rs", "big4.rs"]),
            cycle(&["small-a.rs", "small-a2.rs"]),
        ];

        let (page, omitted) = bound_cycles(cycles, 2, CIRCULAR_DEFAULT_MEMBER_LIMIT);

        assert_eq!(omitted, 1, "the omitted cycle must be counted, not dropped");
        assert_eq!(page.len(), 2);
        assert_eq!(
            page[0].members,
            cycle(&["big.rs", "big2.rs", "big3.rs", "big4.rs"])
        );
        assert_eq!(page[0].member_count, 4);
        assert_eq!(page[0].omitted_member_count, 0);
        // Ties resolve by path order so repeated calls agree.
        assert_eq!(page[1].members, cycle(&["small-a.rs", "small-a2.rs"]));
    }

    #[test]
    fn unbounded_page_reports_no_omission() {
        let cycles = vec![cycle(&["a.rs", "b.rs"])];
        let (page, omitted) = bound_cycles(cycles, 25, CIRCULAR_DEFAULT_MEMBER_LIMIT);
        assert_eq!(omitted, 0);
        assert_eq!(page.len(), 1);
    }

    #[test]
    fn omission_notice_states_the_remainder_and_the_ceiling() {
        let cycles = bounded(&["a.rs", "b.rs"], CIRCULAR_DEFAULT_MEMBER_LIMIT);
        let out = render_circular_md(&cycles, 9, 8, 1);
        assert!(out.contains("Circular Dependencies (9)"), "got: {out}");
        assert!(
            out.contains("8 further cycle(s) not shown at limit 1"),
            "got: {out}"
        );
        assert!(out.contains(&CIRCULAR_MAX_LIMIT.to_string()), "got: {out}");
    }

    /// A single strongly connected component can hold hundreds of files. The
    /// declared bound must shape the answer before rendering, so both the JSON
    /// payload and the markdown stay inside the response budget and state the
    /// component's true size.
    #[test]
    fn wide_component_is_bounded_within_the_response_budget() {
        let members: Vec<String> = (0..400)
            .map(|index| {
                format!("crates/tracedecay-application/src/deeply/nested/module_{index:04}.rs")
            })
            .collect();
        let member_count = members.len();

        let (page, omitted) = bound_cycles(vec![members], 3, CIRCULAR_DEFAULT_MEMBER_LIMIT);

        assert_eq!(omitted, 0);
        assert_eq!(page[0].member_count, member_count);
        assert_eq!(page[0].members.len(), CIRCULAR_DEFAULT_MEMBER_LIMIT);
        assert_eq!(
            page[0].omitted_member_count,
            member_count - CIRCULAR_DEFAULT_MEMBER_LIMIT
        );

        let payload = circular_output(&page, 1, omitted, 3, CIRCULAR_DEFAULT_MEMBER_LIMIT);
        let serialized = serde_json::to_string_pretty(&payload).expect("payload serializes");
        assert!(
            serialized.len() <= RESPONSE_BUDGET,
            "bounded payload is {} chars, over the {RESPONSE_BUDGET} budget",
            serialized.len()
        );

        let markdown = render_circular_md(&page, 1, omitted, 3);
        assert!(
            markdown.len() <= RESPONSE_BUDGET,
            "bounded markdown is {} chars, over the {RESPONSE_BUDGET} budget",
            markdown.len()
        );
        assert!(
            markdown.contains("further member(s) not shown"),
            "the bounded member list must state its omission: {markdown}"
        );
    }
}
