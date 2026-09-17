//! Tarjan's strongly-connected-components algorithm.
//!
//! Shared by `tracedecay_circular` (file-level cycle grouping) and
//! `tracedecay_port_order` (intra-cycle visibility). Both tools were
//! emitting either every walk through an SCC (`circular`'s 73-cycle
//! tail-overlap explosion) or a single flat blob of every cycle node
//! (`port_order`'s 200+ symbol mega-cycle). SCCs replace both with the
//! correct primitive: one component per mutually-recursive group.
//!
//! The implementation is iterative (no recursion) so deep graphs don't
//! blow the stack. SCCs are returned in reverse-topological order —
//! "leaves" (components with no outgoing inter-component edges) come
//! first, which is exactly the order needed for port ranking.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::hash::{BuildHasher, Hash};

use tracedecay_graph_db::GraphCancellation;

/// The caller's cancellation authority fired while Tarjan was still
/// traversing; no components are reported because the partial set would look
/// like a complete cycle report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SccCancelled;

/// Computes the strongly-connected components of the directed graph
/// described by `adj`. Every node that appears as a key OR as a value
/// becomes part of exactly one SCC in the result.
///
/// SCCs are emitted in reverse-topological order over the condensation:
/// if SCC `A` depends on SCC `B`, `B` appears in the result before `A`.
/// This matches Tarjan's natural emission order.
#[hotpath::measure(label = "usecases.graph.tarjan_scc")]
pub fn tarjan_scc<N, S1, S2>(adj: &HashMap<N, HashSet<N, S2>, S1>) -> Vec<Vec<N>>
where
    N: Eq + Hash + Clone,
    S1: BuildHasher,
    S2: BuildHasher,
{
    match tarjan_scc_checked(adj, || Ok::<(), Infallible>(())) {
        Ok(components) => components,
        Err(never) => match never {},
    }
}

/// [`tarjan_scc`] that consults `cancellation` once per discovered node and
/// refuses with [`SccCancelled`] as soon as it has fired, so a cancelled
/// request stops the CPU traversal instead of finishing the whole graph.
#[hotpath::measure(label = "usecases.graph.tarjan_scc_cancellable")]
pub fn tarjan_scc_cancellable<N, S1, S2>(
    adj: &HashMap<N, HashSet<N, S2>, S1>,
    cancellation: &dyn GraphCancellation,
) -> Result<Vec<Vec<N>>, SccCancelled>
where
    N: Eq + Hash + Clone,
    S1: BuildHasher,
    S2: BuildHasher,
{
    tarjan_scc_checked(adj, || {
        if cancellation.is_cancelled() {
            Err(SccCancelled)
        } else {
            Ok(())
        }
    })
}

/// A node's position in the interned node list, or `UNVISITED` while Tarjan
/// has not reached it yet.
const UNVISITED: usize = usize::MAX;

/// Iterative Tarjan over compact ordinals.
///
/// Node values stay borrowed from `adj` for the whole computation: every
/// distinct node is interned once into `nodes`, edges become ordinal lists,
/// and all traversal state (index, lowlink, stack membership, DFS frames) is
/// indexed by ordinal. The only owned `N` values are the ones cloned into the
/// returned components. `checkpoint` runs once per discovered node; its error
/// aborts the traversal.
fn tarjan_scc_checked<N, S1, S2, E>(
    adj: &HashMap<N, HashSet<N, S2>, S1>,
    mut checkpoint: impl FnMut() -> Result<(), E>,
) -> Result<Vec<Vec<N>>, E>
where
    N: Eq + Hash + Clone,
    S1: BuildHasher,
    S2: BuildHasher,
{
    // Gather every node mentioned, sources or targets, so unreachable
    // nodes still appear as singleton SCCs.
    let mut nodes: Vec<&N> = Vec::with_capacity(adj.len());
    let mut ordinal_of: HashMap<&N, usize> = HashMap::with_capacity(adj.len());
    let mut edges: Vec<Vec<usize>> = Vec::with_capacity(adj.len());
    for (source, targets) in adj {
        let source = intern(source, &mut nodes, &mut ordinal_of);
        let targets = targets
            .iter()
            .map(|target| intern(target, &mut nodes, &mut ordinal_of))
            .collect::<Vec<_>>();
        if edges.len() <= source {
            edges.resize_with(source + 1, Vec::new);
        }
        edges[source] = targets;
    }
    edges.resize_with(nodes.len(), Vec::new);
    drop(ordinal_of);

    let mut state = TarjanState::new(nodes.len());
    let mut sccs: Vec<Vec<N>> = Vec::new();

    for root in 0..nodes.len() {
        if state.index[root] != UNVISITED {
            continue;
        }
        checkpoint()?;
        state.discover(root);

        while let Some(&(node, position)) = state.work.last() {
            let Some(&next) = edges[node].get(position) else {
                // Finished this node — pop frame, update parent's lowlink,
                // and emit an SCC if this node is a Tarjan root.
                state.work.pop();
                if state.lowlink[node] == state.index[node] {
                    let mut component: Vec<N> = Vec::new();
                    while let Some(top) = state.stack.pop() {
                        state.on_stack[top] = false;
                        component.push(nodes[top].clone());
                        if top == node {
                            break;
                        }
                    }
                    sccs.push(component);
                }
                if let Some(&(parent, _)) = state.work.last() {
                    state.lowlink[parent] = state.lowlink[parent].min(state.lowlink[node]);
                }
                continue;
            };
            if let Some(frame) = state.work.last_mut() {
                frame.1 = position + 1;
            }

            if state.index[next] == UNVISITED {
                checkpoint()?;
                state.discover(next);
            } else if state.on_stack[next] {
                state.lowlink[node] = state.lowlink[node].min(state.index[next]);
            }
        }
    }

    Ok(sccs)
}

/// Per-ordinal Tarjan bookkeeping plus the explicit DFS stack.
struct TarjanState {
    index: Vec<usize>,
    lowlink: Vec<usize>,
    on_stack: Vec<bool>,
    stack: Vec<usize>,
    /// Frame: (node ordinal, position in that node's edge list).
    work: Vec<(usize, usize)>,
    next_index: usize,
}

impl TarjanState {
    fn new(node_count: usize) -> Self {
        Self {
            index: vec![UNVISITED; node_count],
            lowlink: vec![0; node_count],
            on_stack: vec![false; node_count],
            stack: Vec::new(),
            work: Vec::new(),
            next_index: 0,
        }
    }

    /// Assign `node` its DFS index, push it on the Tarjan stack, and open its
    /// DFS frame.
    fn discover(&mut self, node: usize) {
        self.index[node] = self.next_index;
        self.lowlink[node] = self.next_index;
        self.next_index += 1;
        self.stack.push(node);
        self.on_stack[node] = true;
        self.work.push((node, 0));
    }
}

/// The ordinal of `node`, assigning the next one when it is first seen.
fn intern<'a, N: Eq + Hash>(
    node: &'a N,
    nodes: &mut Vec<&'a N>,
    ordinal_of: &mut HashMap<&'a N, usize>,
) -> usize {
    *ordinal_of.entry(node).or_insert_with(|| {
        nodes.push(node);
        nodes.len() - 1
    })
}

/// True when an SCC represents a genuine cycle. A single-node component
/// is only cyclic if it has an explicit self-edge in `adj`; otherwise
/// it's just an isolated vertex. Components of size >= 2 are always
/// cyclic by Tarjan's definition.
#[allow(clippy::implicit_hasher)]
pub fn is_cyclic_scc<N>(scc: &[N], adj: &HashMap<N, HashSet<N>>) -> bool
where
    N: Eq + Hash,
{
    if scc.len() >= 2 {
        return true;
    }
    if let Some(only) = scc.first()
        && let Some(neighbors) = adj.get(only)
    {
        return neighbors.contains(only);
    }
    false
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracedecay_graph_db::NeverCancelled;

    fn edge<N: Eq + Hash + Clone>(adj: &mut HashMap<N, HashSet<N>>, from: N, to: N) {
        adj.entry(from).or_default().insert(to);
    }

    /// The textbook recursive Tarjan, walking nodes and neighbours in the
    /// same iteration order the iterative kernel uses, so its emission order
    /// is the reference the production output must reproduce exactly.
    fn reference_tarjan<N: Eq + Hash + Clone>(adj: &HashMap<N, HashSet<N>>) -> Vec<Vec<N>> {
        struct Walk<'a, N> {
            adj: &'a HashMap<N, HashSet<N>>,
            index: HashMap<&'a N, usize>,
            lowlink: HashMap<&'a N, usize>,
            stack: Vec<&'a N>,
            output: Vec<Vec<N>>,
        }
        fn visit<'a, N: Eq + Hash + Clone>(walk: &mut Walk<'a, N>, node: &'a N) {
            let position = walk.index.len();
            walk.index.insert(node, position);
            walk.lowlink.insert(node, position);
            walk.stack.push(node);
            for next in walk.adj.get(node).into_iter().flatten() {
                if !walk.index.contains_key(next) {
                    visit(walk, next);
                    let low = walk.lowlink[next].min(walk.lowlink[node]);
                    walk.lowlink.insert(node, low);
                } else if walk.stack.contains(&next) {
                    let low = walk.index[next].min(walk.lowlink[node]);
                    walk.lowlink.insert(node, low);
                }
            }
            if walk.lowlink[node] == walk.index[node] {
                let mut component = Vec::new();
                while let Some(top) = walk.stack.pop() {
                    component.push(top.clone());
                    if top == node {
                        break;
                    }
                }
                walk.output.push(component);
            }
        }
        let mut walk = Walk {
            adj,
            index: HashMap::new(),
            lowlink: HashMap::new(),
            stack: Vec::new(),
            output: Vec::new(),
        };
        for (source, targets) in adj {
            if !walk.index.contains_key(source) {
                visit(&mut walk, source);
            }
            for target in targets {
                if !walk.index.contains_key(target) {
                    visit(&mut walk, target);
                }
            }
        }
        walk.output
    }

    /// Owned long path names with cycles, a target-only sink, a self loop,
    /// and an isolated source, shaped like the file adjacency the query
    /// builds.
    fn path_fixture() -> HashMap<String, HashSet<String>> {
        let path = |name: &str| format!("crates/tracedecay-graph-query/src/context/{name}.rs");
        let mut adj = HashMap::new();
        for (from, to) in [
            ("read_modes", "source_read"),
            ("source_read", "read_modes"),
            ("source_read", "budget"),
            ("budget", "budget"),
            ("budget", "target_only"),
            ("verified", "projection"),
            ("projection", "queries"),
            ("queries", "verified"),
            ("queries", "budget"),
        ] {
            edge(&mut adj, path(from), path(to));
        }
        adj.insert(path("isolated"), HashSet::new());
        adj
    }

    #[test]
    fn iterative_kernel_matches_the_reference_emission_exactly() {
        let adj = path_fixture();

        let sccs = tarjan_scc(&adj);

        assert_eq!(sccs, reference_tarjan(&adj));
        assert_eq!(sccs.len(), 5, "{sccs:?}");
        let cyclic = sccs
            .iter()
            .filter(|component| is_cyclic_scc(component, &adj))
            .count();
        assert_eq!(cyclic, 3, "two multi-file cycles plus the self loop");
    }

    /// Cancellation fires between node discoveries: the traversal stops with
    /// the typed refusal rather than a plausible-looking partial cycle list.
    #[test]
    fn cancellation_mid_traversal_refuses_instead_of_reporting_partial_components() {
        struct CancelAfter {
            remaining: AtomicUsize,
        }
        impl GraphCancellation for CancelAfter {
            fn is_cancelled(&self) -> bool {
                self.remaining
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                        left.checked_sub(1)
                    })
                    .is_err()
            }
        }
        let adj = path_fixture();

        assert_eq!(
            tarjan_scc_cancellable(&adj, &NeverCancelled),
            Ok(tarjan_scc(&adj))
        );
        let cancel_after_three = CancelAfter {
            remaining: AtomicUsize::new(3),
        };
        assert_eq!(
            tarjan_scc_cancellable(&adj, &cancel_after_three),
            Err(SccCancelled)
        );
        let already_cancelled = CancelAfter {
            remaining: AtomicUsize::new(0),
        };
        assert_eq!(
            tarjan_scc_cancellable(&adj, &already_cancelled),
            Err(SccCancelled)
        );
    }

    #[test]
    fn dag_has_only_trivial_sccs() {
        let mut adj: HashMap<&str, HashSet<&str>> = HashMap::new();
        edge(&mut adj, "a", "b");
        edge(&mut adj, "b", "c");
        let sccs = tarjan_scc(&adj);
        assert_eq!(sccs.len(), 3);
        for s in &sccs {
            assert_eq!(s.len(), 1);
            assert!(!is_cyclic_scc(s, &adj));
        }
    }

    #[test]
    fn reverse_topological_order() {
        // a -> b -> c. Tarjan emits in reverse-topo: leaves first.
        let mut adj: HashMap<&str, HashSet<&str>> = HashMap::new();
        edge(&mut adj, "a", "b");
        edge(&mut adj, "b", "c");
        let sccs = tarjan_scc(&adj);
        let order: Vec<&str> = sccs.iter().map(|s| s[0]).collect();
        let pos_a = order.iter().position(|n| *n == "a").unwrap();
        let pos_c = order.iter().position(|n| *n == "c").unwrap();
        assert!(
            pos_c < pos_a,
            "c (leaf) should come before a (root); got {order:?}"
        );
    }

    #[test]
    fn deep_graph_is_stack_safe() {
        const NODE_COUNT: usize = 20_000;
        let mut adj: HashMap<usize, HashSet<usize>> = HashMap::new();
        for node in 0..NODE_COUNT - 1 {
            adj.entry(node).or_default().insert(node + 1);
        }

        let sccs = tarjan_scc(&adj);
        assert_eq!(sccs.len(), NODE_COUNT);
        assert!(sccs.iter().all(|component| component.len() == 1));
    }
}
