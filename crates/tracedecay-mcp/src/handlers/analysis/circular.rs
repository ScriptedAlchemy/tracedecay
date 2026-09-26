//! `tracedecay_circular`, bounded cyclic-dependency reporting.

use tracedecay_contracts::retrieval::{
    CircularCycleV1, CircularResultV1, CircularSurfaceRequestV1,
};

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

#[hotpath::measure(future = true, label = "mcp.analysis.circular.total")]
pub(super) async fn compute_circular(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
) -> Result<GraphToolCompletionV1> {
    let request: CircularSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_circular")?;
    let limit = request.limit.map_or(CIRCULAR_DEFAULT_LIMIT, |limit| {
        (limit as usize).clamp(1, CIRCULAR_MAX_LIMIT)
    });
    let member_limit = request
        .member_limit
        .map_or(CIRCULAR_DEFAULT_MEMBER_LIMIT, |limit| {
            (limit as usize).clamp(1, CIRCULAR_MAX_MEMBER_LIMIT)
        });

    let all_cycles = hotpath::future!(
        graph.find_circular_dependencies(),
        label = "mcp.analysis.circular.graph"
    )
    .await?;
    let result = hotpath::measure_block!(
        "mcp.analysis.circular.compute",
        bound_cycles(all_cycles, limit, member_limit)
    );
    Ok(graph_tool_completion(
        GraphToolResultV1::Circular(result),
        Vec::new(),
    ))
}

/// Orders cycles largest-first and bounds them to `limit` cycles of
/// `member_limit` members each.
///
/// The largest strongly connected components are the ones worth breaking, so a
/// bounded page reports the worst offenders rather than an arbitrary prefix.
/// Ties fall back to path order so repeated calls agree. Both the omitted cycle
/// count and each component's true member count are reported rather than
/// dropped: the answer always states what it left out.
fn bound_cycles(
    mut cycles: Vec<Vec<String>>,
    limit: usize,
    member_limit: usize,
) -> CircularResultV1 {
    let cycle_count = cycles.len();
    cycles.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    cycles.truncate(limit);
    let cycles: Vec<CircularCycleV1> = cycles
        .into_iter()
        .map(|mut members| {
            let member_count = members.len();
            members.truncate(member_limit);
            CircularCycleV1 {
                omitted_member_count: member_count.saturating_sub(members.len()) as u64,
                members,
                member_count: member_count as u64,
            }
        })
        .collect();
    CircularResultV1 {
        cycle_count: cycle_count as u64,
        reported_cycle_count: cycles.len() as u64,
        omitted_cycle_count: cycle_count.saturating_sub(cycles.len()) as u64,
        limit: limit as u64,
        member_limit: member_limit as u64,
        cycles,
    }
}

/// Renders file-level dependency cycles as arrow chains that preserve cycle
/// order (`a.rs -> b.rs -> a.rs`) instead of collapsing the members into a
/// directory tree, which destroys the cyclic relationship. Each SCC's member
/// files are joined with ` -> ` and the first is repeated at the end to close
/// the loop.
pub fn render_circular_md(result: &CircularResultV1) -> String {
    use std::fmt::Write as _;

    let cycle_count = result.cycle_count;
    if cycle_count == 0 {
        return "No circular dependencies found.\n".to_string();
    }
    let mut out = String::new();
    let _ = writeln!(out, "# Circular Dependencies ({cycle_count})\n");
    for (i, cycle) in result.cycles.iter().enumerate() {
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
    let omitted = result.omitted_cycle_count;
    if omitted > 0 {
        let limit = result.limit;
        let _ = writeln!(
            out,
            "\n{omitted} further cycle(s) not shown at limit {limit}; raise `limit` (max {CIRCULAR_MAX_LIMIT}) to see more."
        );
    }
    out
}
#[cfg(test)]
mod circular_render_tests {
    use super::{CIRCULAR_DEFAULT_MEMBER_LIMIT, bound_cycles, render_circular_md};
    use crate::MAX_RESPONSE_CHARS;

    fn cycle(files: &[&str]) -> Vec<String> {
        files.iter().map(|file| (*file).to_string()).collect()
    }

    #[test]
    fn bounded_page_keeps_the_largest_cycles_and_counts_the_rest() {
        let cycles = vec![
            cycle(&["small-b.rs", "small-b2.rs"]),
            cycle(&["big.rs", "big2.rs", "big3.rs", "big4.rs"]),
            cycle(&["small-a.rs", "small-a2.rs"]),
        ];

        let page = bound_cycles(cycles, 2, CIRCULAR_DEFAULT_MEMBER_LIMIT);

        assert_eq!(
            page.omitted_cycle_count, 1,
            "the omitted cycle must be counted, not dropped"
        );
        assert_eq!(page.cycles.len(), 2);
        assert_eq!(
            page.cycles[0].members,
            cycle(&["big.rs", "big2.rs", "big3.rs", "big4.rs"])
        );
        assert_eq!(page.cycles[0].member_count, 4);
        assert_eq!(page.cycles[0].omitted_member_count, 0);
        // Ties resolve by path order so repeated calls agree.
        assert_eq!(
            page.cycles[1].members,
            cycle(&["small-a.rs", "small-a2.rs"])
        );
    }

    /// A single strongly connected component can hold hundreds of files. The
    /// declared bound must shape the answer before rendering, so both the JSON
    /// payload and the markdown stay inside the response budget and state the
    /// component's true size.
    #[test]
    fn wide_component_is_bounded_within_the_response_budget() {
        let members: Vec<String> = (0..400)
            .map(|index| {
                format!("crates/tracedecay-contracts/src/deeply/nested/module_{index:04}.rs")
            })
            .collect();
        let member_count = members.len() as u64;

        let page = bound_cycles(vec![members], 3, CIRCULAR_DEFAULT_MEMBER_LIMIT);

        assert_eq!(page.omitted_cycle_count, 0);
        assert_eq!(page.cycles[0].member_count, member_count);
        assert_eq!(page.cycles[0].members.len(), CIRCULAR_DEFAULT_MEMBER_LIMIT);
        assert_eq!(
            page.cycles[0].omitted_member_count,
            member_count - CIRCULAR_DEFAULT_MEMBER_LIMIT as u64
        );

        let serialized = serde_json::to_string_pretty(&page).expect("payload serializes");
        assert!(
            serialized.len() <= MAX_RESPONSE_CHARS,
            "bounded payload is {} chars, over the {MAX_RESPONSE_CHARS} budget",
            serialized.len()
        );

        let markdown = render_circular_md(&page);
        assert!(
            markdown.len() <= MAX_RESPONSE_CHARS,
            "bounded markdown is {} chars, over the {MAX_RESPONSE_CHARS} budget",
            markdown.len()
        );
        assert!(
            markdown.contains("further member(s) not shown"),
            "the bounded member list must state its omission: {markdown}"
        );
    }
}
