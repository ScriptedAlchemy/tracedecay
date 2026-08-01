use crate::errors::Result;
use crate::tracedecay::TraceDecay;
use crate::types::*;

use super::search::node_name_matches;

impl TraceDecay {
    /// Returns `impl` blocks matching the given trait and/or implementing type.
    ///
    /// Both filters are optional:
    /// - With only `trait_name`: every impl of that trait, regardless of the
    ///   implementing type.
    /// - With only `type_name`: every impl block for that type (trait impls
    ///   and inherent impls).
    /// - With both: the intersection.
    /// - With neither: every `impl` node in the graph (use sparingly).
    ///
    /// Each result carries the impl node plus, when available, the resolved
    /// trait node it implements. Matching uses substring containment on the
    /// trait/type names so callers can pass either short or qualified names.
    pub async fn get_impls(
        &self,
        trait_name: Option<&str>,
        type_name: Option<&str>,
    ) -> Result<Vec<(Node, Option<Node>)>> {
        use crate::types::EdgeKind;

        // Candidate impl blocks.
        let mut impls = self.db.get_nodes_by_kind(NodeKind::Impl).await?;

        // Filter by implementing type if requested. The impl node's `name`
        // field holds the type identifier (e.g. "MyType" for `impl Foo for MyType`).
        if let Some(type_q) = type_name {
            impls.retain(|n| node_name_matches(n, type_q));
        }

        // Gather Implements edges per impl, then batch-fetch every trait node
        // in one `get_nodes_by_ids` call to avoid an N+1 across impl blocks.
        let mut per_impl_trait_id: Vec<Option<String>> = Vec::with_capacity(impls.len());
        let mut trait_target_ids: Vec<String> = Vec::new();
        for impl_node in &impls {
            let edges = self
                .db
                .get_outgoing_edges(&impl_node.id, &[EdgeKind::Implements])
                .await
                .unwrap_or_default();
            let target = edges.into_iter().next().map(|e| e.target);
            if let Some(ref t) = target {
                trait_target_ids.push(t.clone());
            }
            per_impl_trait_id.push(target);
        }
        let trait_nodes = if trait_target_ids.is_empty() {
            Vec::new()
        } else {
            self.db.get_nodes_by_ids(&trait_target_ids).await?
        };
        let trait_map: std::collections::HashMap<String, Node> =
            trait_nodes.into_iter().map(|n| (n.id.clone(), n)).collect();

        let mut out: Vec<(Node, Option<Node>)> = Vec::with_capacity(impls.len());
        for (impl_node, trait_id) in impls.into_iter().zip(per_impl_trait_id) {
            let trait_node = trait_id.and_then(|id| trait_map.get(&id).cloned());

            // Trait filter: drop inherent impls when a trait was requested.
            if let Some(trait_q) = trait_name {
                let matched = trait_node
                    .as_ref()
                    .is_some_and(|t| node_name_matches(t, trait_q));
                if !matched {
                    continue;
                }
            }

            out.push((impl_node, trait_node));
        }
        Ok(out)
    }

    /// Resolves a trait method node to the concrete method nodes that satisfy
    /// it across every `impl` block of the enclosing trait.
    ///
    /// Returns an empty vec when the input is not a method whose parent (via
    /// `Contains`) is a trait. Used by `tracedecay_callees` to surface concrete
    /// dispatch targets in addition to the trait method itself.
    pub async fn get_trait_dispatch_targets(&self, method: &Node) -> Result<Vec<Node>> {
        use crate::types::EdgeKind;

        // Only method-kind nodes can be trait methods.
        if !matches!(method.kind, NodeKind::Method | NodeKind::Function) {
            return Ok(Vec::new());
        }

        // Find the trait that contains this method. parent_id points at
        // the enclosing scope after v9; verify it's actually a Trait.
        let Some(parent_id) = method.parent_id.as_deref() else {
            return Ok(Vec::new());
        };
        let Some(trait_node) = self.db.get_node_by_id(parent_id).await? else {
            return Ok(Vec::new());
        };
        if trait_node.kind != NodeKind::Trait {
            return Ok(Vec::new());
        }

        // Find every impl block of that trait.
        let impl_edges = self
            .db
            .get_incoming_edges(&trait_node.id, &[EdgeKind::Implements])
            .await?;
        let impl_ids: Vec<String> = impl_edges.into_iter().map(|e| e.source).collect();
        if impl_ids.is_empty() {
            return Ok(Vec::new());
        }

        // For each impl block, surface the method whose name matches the
        // trait method. Multiple impls may share names with unrelated nodes,
        // so we filter by both kind and name.
        let mut targets = Vec::new();
        for impl_id in impl_ids {
            let candidates = self.db.get_children_of(&impl_id).await?;
            for n in candidates {
                if matches!(n.kind, NodeKind::Method | NodeKind::Function) && n.name == method.name
                {
                    targets.push(n);
                }
            }
        }
        Ok(targets)
    }
}
