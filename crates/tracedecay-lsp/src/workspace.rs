//! Typed, fenced workspace-folder mutations emitted by one protocol actor.

use std::collections::BTreeSet;

use serde_json::Value;
use tracedecay_domain::ManifestDigest;

use crate::gateway::AdmittedRoot;
use crate::rpc::RpcFailure;
use crate::session::{AuthorizedLspWorkspace, MAX_LSP_WORKSPACE_ROOTS};

/// One validated `workspace/didChangeWorkspaceFolders` intent. The actor does
/// not apply it locally; its daemon owner resolves and authorizes every URI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceFolderMutation {
    pub observed_scope_digest: Option<ManifestDigest>,
    pub active_root_uri: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub next_root_uris: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceFolderMutationApplyError {
    StaleWorkspace,
}

impl WorkspaceFolderMutation {
    pub(crate) fn parse(
        params: &Value,
        workspace: &AuthorizedLspWorkspace,
    ) -> Result<Option<Self>, RpcFailure> {
        let event = params
            .get("event")
            .and_then(Value::as_object)
            .ok_or_else(|| RpcFailure::invalid_params("event must be an object"))?;
        let added = parse_folders(event.get("added"), "event.added")?;
        let removed = parse_folders(event.get("removed"), "event.removed")?;
        if added.is_empty() && removed.is_empty() {
            return Ok(None);
        }
        let added_set = added.iter().collect::<BTreeSet<_>>();
        let removed_set = removed.iter().collect::<BTreeSet<_>>();
        if added_set.len() != added.len()
            || removed_set.len() != removed.len()
            || added_set.iter().any(|uri| removed_set.contains(uri))
        {
            return Err(RpcFailure::invalid_params(
                "workspace folder URIs must be unique and cannot be both added and removed",
            ));
        }
        if added.iter().any(|uri| {
            workspace
                .roots()
                .iter()
                .any(|root| root.matches_root_uri(uri))
        }) || removed.iter().any(|uri| {
            workspace
                .roots()
                .iter()
                .filter(|root| root.matches_root_uri(uri))
                .count()
                != 1
        }) {
            return Err(RpcFailure::invalid_params(
                "workspace folder changes must add new roots and remove exact admitted roots",
            ));
        }
        let active_root_uri = workspace.primary().uri().to_owned();
        if removed
            .iter()
            .any(|uri| workspace.primary().matches_root_uri(uri))
        {
            return Err(RpcFailure::invalid_params(
                "the active workspace root cannot be removed",
            ));
        }

        let mut next_root_uris = workspace
            .roots()
            .iter()
            .filter(|root| {
                !removed
                    .iter()
                    .any(|candidate| root.matches_root_uri(candidate))
            })
            .map(|root| root.uri().to_owned())
            .collect::<Vec<_>>();
        next_root_uris.extend(added.iter().cloned());
        next_root_uris.sort();
        next_root_uris.dedup();
        if next_root_uris.is_empty() || next_root_uris.len() > MAX_LSP_WORKSPACE_ROOTS {
            return Err(RpcFailure::invalid_params(
                "workspace folder change exceeds the admitted root bound",
            ));
        }
        Ok(Some(Self {
            observed_scope_digest: workspace.scope_set_digest().cloned(),
            active_root_uri,
            added,
            removed,
            next_root_uris,
        }))
    }
}

fn parse_folders(value: Option<&Value>, _field: &'static str) -> Result<Vec<String>, RpcFailure> {
    let folders = value
        .and_then(Value::as_array)
        .ok_or_else(|| RpcFailure::invalid_params("workspace folder changes must be arrays"))?;
    folders
        .iter()
        .map(|folder| {
            let object = folder.as_object().ok_or_else(|| {
                RpcFailure::invalid_params("workspace folder entries must be objects")
            })?;
            let uri = object
                .get("uri")
                .and_then(Value::as_str)
                .filter(|uri| !uri.is_empty())
                .ok_or_else(|| {
                    RpcFailure::invalid_params("workspace folder entries require a non-empty uri")
                })?;
            if !AdmittedRoot::new(uri).is_valid() {
                return Err(RpcFailure::invalid_params(
                    "workspace folder entries require a valid file URI",
                ));
            }
            Ok(uri.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mutation_is_fenced_to_the_observed_workspace_and_root_bound() {
        let workspace =
            AuthorizedLspWorkspace::single(AdmittedRoot::new("file:///workspace/root-a"));
        let mutation = WorkspaceFolderMutation::parse(
            &json!({
                "event": {
                    "added": [{"uri": "file:///workspace/root-b", "name": "B"}],
                    "removed": []
                }
            }),
            &workspace,
        )
        .expect("valid mutation")
        .expect("non-empty mutation");

        assert_eq!(mutation.observed_scope_digest, None);
        assert_eq!(mutation.active_root_uri, "file:///workspace/root-a");
        assert_eq!(mutation.added, vec!["file:///workspace/root-b"]);
        assert!(mutation.removed.is_empty());
        assert_eq!(
            mutation.next_root_uris,
            vec!["file:///workspace/root-a", "file:///workspace/root-b"]
        );
    }

    #[test]
    fn mutation_rejects_overlap_and_removing_the_last_root() {
        let workspace =
            AuthorizedLspWorkspace::single(AdmittedRoot::new("file:///workspace/root-a"));
        assert!(
            WorkspaceFolderMutation::parse(
                &json!({
                    "event": {
                        "added": [{"uri": "file:///workspace/root-b"}],
                        "removed": [{"uri": "file:///workspace/root-b"}]
                    }
                }),
                &workspace,
            )
            .is_err()
        );
        assert!(
            WorkspaceFolderMutation::parse(
                &json!({
                    "event": {
                        "added": [],
                        "removed": [{"uri": "file:///workspace/root-a"}]
                    }
                }),
                &workspace,
            )
            .is_err()
        );
    }
}
