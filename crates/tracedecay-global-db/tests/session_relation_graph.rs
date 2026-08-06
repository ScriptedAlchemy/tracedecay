use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_domain::{
    CopyProofV1, MessageOccurrenceIdV1, ObservationId, ProjectId, RetrievalAnchorId, SessionId,
    TemporalValidityV1, UtcMicros,
};
use tracedecay_global_db::session_temporal::relations::{
    LogicalCopyRelation, SessionRelationError, SessionRelationGraphStore,
    SessionRelationProjection, SummaryRelationNode, SummarySourceRef, SummarySourceVisit,
};
use tracedecay_graph_db::{
    GraphDb, GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphFormatVersion,
    NeverCancelled,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn projection() -> SessionRelationProjection {
    SessionRelationProjection {
        project_id: id::<ProjectId>("project.session-relations"),
        session_id: id::<SessionId>("session.relation-dag"),
        generation: 7,
        summaries: vec![
            SummaryRelationNode {
                summary_id: "summary.shared".to_owned(),
                sources: vec![SummarySourceRef::Anchor {
                    anchor_id: id::<RetrievalAnchorId>("anchor.original"),
                }],
                predecessor_summary_id: None,
            },
            SummaryRelationNode {
                summary_id: "summary.left".to_owned(),
                sources: vec![SummarySourceRef::Summary {
                    summary_id: "summary.shared".to_owned(),
                }],
                predecessor_summary_id: None,
            },
            SummaryRelationNode {
                summary_id: "summary.right".to_owned(),
                sources: vec![SummarySourceRef::Summary {
                    summary_id: "summary.shared".to_owned(),
                }],
                predecessor_summary_id: None,
            },
            SummaryRelationNode {
                summary_id: "summary.root".to_owned(),
                sources: vec![
                    SummarySourceRef::Summary {
                        summary_id: "summary.left".to_owned(),
                    },
                    SummarySourceRef::Summary {
                        summary_id: "summary.right".to_owned(),
                    },
                ],
                predecessor_summary_id: Some("summary.previous".to_owned()),
            },
        ],
        logical_copies: Vec::new(),
        thread_hierarchy: Vec::new(),
        agent_hierarchy: Vec::new(),
    }
}

fn persistent(path: &std::path::Path) -> GraphDb {
    GraphDb::open(GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(path.to_path_buf()),
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Sync,
        cancellation: Arc::new(NeverCancelled),
    })
    .unwrap()
}

#[test]
fn shared_summary_sources_are_lossless_and_survive_reopen() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("session-relations.grafeo");
    let expected = vec![
        SummarySourceVisit::summary("summary.root", "summary.left", 0, 1),
        SummarySourceVisit::summary("summary.root", "summary.right", 1, 1),
        SummarySourceVisit::summary("summary.left", "summary.shared", 0, 2),
        SummarySourceVisit::summary("summary.right", "summary.shared", 0, 2),
        SummarySourceVisit::anchor("summary.shared", id("anchor.original"), 0, 3),
        SummarySourceVisit::anchor("summary.shared", id("anchor.original"), 0, 3),
    ];
    {
        let graph = SessionRelationGraphStore::new(Arc::new(persistent(&path)));
        graph.replace(&projection()).unwrap();
        assert_eq!(
            graph
                .summary_sources(
                    &id("project.session-relations"),
                    &id("session.relation-dag"),
                    7,
                    "summary.root",
                    16,
                )
                .unwrap(),
            expected
        );
        let loaded = graph
            .load_projection(
                &id("project.session-relations"),
                &id("session.relation-dag"),
                7,
            )
            .unwrap();
        assert_eq!(loaded.project_id, projection().project_id);
        assert_eq!(loaded.session_id, projection().session_id);
        assert_eq!(loaded.summaries.len(), 4);
    }

    let reopened = SessionRelationGraphStore::new(Arc::new(persistent(&path)));
    assert_eq!(
        reopened
            .summary_sources(
                &id("project.session-relations"),
                &id("session.relation-dag"),
                7,
                "summary.root",
                16,
            )
            .unwrap(),
        expected
    );
}

#[test]
fn summary_cycle_is_rejected_before_replacing_the_readable_projection() {
    let graph = SessionRelationGraphStore::memory().unwrap();
    graph.replace(&projection()).unwrap();
    let mut cyclic = projection();
    cyclic.summaries[0].sources = vec![SummarySourceRef::Summary {
        summary_id: "summary.root".to_owned(),
    }];

    assert!(graph.replace(&cyclic).is_err());
    assert_eq!(
        graph
            .summary_sources(
                &id("project.session-relations"),
                &id("session.relation-dag"),
                7,
                "summary.root",
                16,
            )
            .unwrap()
            .len(),
        6
    );
}

#[test]
fn summary_successor_cycle_is_rejected() {
    let graph = SessionRelationGraphStore::memory().unwrap();
    let mut cyclic = projection();
    cyclic.summaries[0].predecessor_summary_id = Some("summary.root".to_owned());
    cyclic.summaries[3].predecessor_summary_id = Some("summary.shared".to_owned());

    assert_eq!(graph.replace(&cyclic), Err(SessionRelationError::Cycle));
}

#[test]
fn logical_copy_cycle_is_rejected() {
    let graph = SessionRelationGraphStore::memory().unwrap();
    let mut cyclic = projection();
    let first = id::<MessageOccurrenceIdV1>("occurrence.first");
    let second = id::<MessageOccurrenceIdV1>("occurrence.second");
    cyclic.logical_copies = vec![
        LogicalCopyRelation {
            occurrence_id: first.clone(),
            copied_from_occurrence_id: second.clone(),
            proof: CopyProofV1::ProviderLinkage {
                source_occurrence_id: second.clone(),
                provider_record_id: id::<ObservationId>("observation.second"),
            },
            knowledge_at: UtcMicros(1),
            valid_time: TemporalValidityV1::Unknown,
        },
        LogicalCopyRelation {
            occurrence_id: second,
            copied_from_occurrence_id: first.clone(),
            proof: CopyProofV1::ProviderLinkage {
                source_occurrence_id: first,
                provider_record_id: id::<ObservationId>("observation.first"),
            },
            knowledge_at: UtcMicros(2),
            valid_time: TemporalValidityV1::Unknown,
        },
    ];

    assert_eq!(graph.replace(&cyclic), Err(SessionRelationError::Cycle));
}

#[test]
fn immutable_generations_with_reused_domain_ids_remain_independently_readable() {
    let graph = SessionRelationGraphStore::memory().unwrap();
    let first = projection();
    let mut second = first.clone();
    second.generation = 8;
    second.summaries[3].sources.reverse();

    graph.replace(&first).unwrap();
    graph.replace(&second).unwrap();

    let first_sources = graph
        .summary_sources(
            &id("project.session-relations"),
            &id("session.relation-dag"),
            7,
            "summary.root",
            16,
        )
        .unwrap();
    let second_sources = graph
        .summary_sources(
            &id("project.session-relations"),
            &id("session.relation-dag"),
            8,
            "summary.root",
            16,
        )
        .unwrap();

    assert_eq!(
        first_sources[0],
        SummarySourceVisit::summary("summary.root", "summary.left", 0, 1)
    );
    assert_eq!(
        second_sources[0],
        SummarySourceVisit::summary("summary.root", "summary.right", 0, 1)
    );
}

#[test]
fn published_generation_accepts_exact_replay_and_rejects_different_topology() {
    let graph = SessionRelationGraphStore::memory().unwrap();
    let original = projection();
    let mut conflict = original.clone();
    conflict.summaries[3].sources.reverse();

    let first = graph.replace(&original).unwrap();
    assert_eq!(graph.replace(&original).unwrap(), first);
    assert!(graph.replace(&conflict).is_err());
    assert_eq!(
        graph
            .summary_sources(
                &id("project.session-relations"),
                &id("session.relation-dag"),
                7,
                "summary.root",
                16,
            )
            .unwrap()[0],
        SummarySourceVisit::summary("summary.root", "summary.left", 0, 1)
    );
}

#[test]
fn projection_read_paginates_entities_and_relations_independently() {
    let graph = SessionRelationGraphStore::memory().unwrap();
    let mut large = projection();
    large.summaries = (0..1_002)
        .map(|ordinal| SummaryRelationNode {
            summary_id: format!("summary.page.{ordinal:04}"),
            sources: if ordinal == 0 {
                vec![SummarySourceRef::Anchor {
                    anchor_id: id("anchor.page"),
                }]
            } else {
                Vec::new()
            },
            predecessor_summary_id: None,
        })
        .collect();

    graph.replace(&large).unwrap();
    let loaded = graph
        .load_projection(&large.project_id, &large.session_id, large.generation)
        .unwrap();
    assert_eq!(loaded.summaries.len(), 1_002);
    let first = loaded
        .summaries
        .iter()
        .find(|summary| summary.summary_id == "summary.page.0000")
        .unwrap();
    assert_eq!(first.sources.len(), 1);
}
