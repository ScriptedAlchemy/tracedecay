use serde_json::json;
use tracedecay_mcp::ToolResult;
use tracedecay_mcp::server::{IdenticalReadCoalescer, ReadFlightClaim};

#[tokio::test]
async fn identical_reads_share_only_the_in_flight_result() {
    let coalescer = IdenticalReadCoalescer::default();
    let leader = match coalescer.claim(
        "graph-main",
        "tracedecay_outline",
        &json!({"path": "src/lib.rs"}),
        None,
    ) {
        ReadFlightClaim::Leader(leader) => leader,
        ReadFlightClaim::Follower(_) => panic!("first request must lead"),
    };
    let follower = match coalescer.claim(
        "graph-main",
        "tracedecay_outline",
        &json!({"path": "src/lib.rs"}),
        None,
    ) {
        ReadFlightClaim::Follower(follower) => follower,
        ReadFlightClaim::Leader(_) => panic!("identical in-flight request must follow"),
    };
    let waiting = tokio::spawn(async move { follower.wait().await });

    leader.complete(ToolResult::new(
        json!({"content": [{"type": "text", "text": "shared"}]}),
        vec!["src/lib.rs".to_owned()],
    ));
    let shared = waiting
        .await
        .expect("follower task")
        .expect("leader result");
    assert_eq!(shared.value["content"][0]["text"], json!("shared"));

    assert!(matches!(
        coalescer.claim(
            "graph-main",
            "tracedecay_outline",
            &json!({"path": "src/lib.rs"}),
            None,
        ),
        ReadFlightClaim::Leader(_)
    ));
}
