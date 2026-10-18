//! Compression request ownership across the planning and commit phases.
//!
//! A daemon compression that needs an authoritative summary runs a planning
//! pass (`HermesAuxiliary`) and, once a summary exists, a final commit pass
//! over the same message corpus. The corpus must be owned once by the request
//! for the whole journey; a second copy per pass duplicates the
//! transcript-sized JSON tree exactly where LCM is already ingesting,
//! planning, and assembling.
//!
//! The regression is measured as copies, not wall time: every corpus message
//! body has one distinctive odd byte length, and a counting allocator on the
//! test thread counts allocations of exactly that size. Ingest, replay, and
//! summary-source assembly each copy a body a fixed number of times; a
//! whole-request clone adds one more copy of every body.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use serde_json::json;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;

use super::super::DaemonLcmEffectService;
use super::{daemon_summary_request, session};

const CORPUS_MESSAGES: usize = 32;
/// Odd length so buffer doubling, page-sized reads, and tokenizer scratch
/// never coincide with a message body copy.
const MESSAGE_CONTENT_BYTES: usize = 32 * 1024 + 7;

/// Body copies the two-phase journey makes per corpus message when the request
/// is borrowed across passes.
///
/// Measured on this fixture (`claude` provider, so the planning pass ends in
/// `needs_authoritative_summary`): raw ingest, replay assembly,
/// summary-source selection, and native-evidence scanning copy each body 38
/// times (1216 copies for 32 messages). Deep-cloning the request before the
/// planning pass copied every body once more (1249 copies, 39.03 per
/// message). The budget tolerates half a corpus of incidental copies and
/// fails on one more copy per message.
const BODY_COPIES_PER_MESSAGE: usize = 38;

/// Counts allocations of exactly one message body on the current thread. The
/// journey runs on a current-thread runtime, so the compression request and
/// every pass over it allocate here.
struct BodyCopyCounter;

thread_local! {
    static BODY_COPIES: Cell<usize> = const { Cell::new(0) };
}

fn count_body_copy(size: usize) {
    if size == MESSAGE_CONTENT_BYTES {
        BODY_COPIES.with(|counter| counter.set(counter.get() + 1));
    }
}

unsafe impl GlobalAlloc for BodyCopyCounter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_body_copy(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_body_copy(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count_body_copy(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: BodyCopyCounter = BodyCopyCounter;

fn corpus_message(ordinal: usize) -> serde_json::Value {
    let role = if ordinal.is_multiple_of(2) {
        "user"
    } else {
        "assistant"
    };
    let filler = format!("message {ordinal} durable context ");
    let mut content = String::with_capacity(MESSAGE_CONTENT_BYTES + filler.len());
    while content.len() < MESSAGE_CONTENT_BYTES {
        content.push_str(&filler);
    }
    content.truncate(MESSAGE_CONTENT_BYTES);
    json!({ "role": role, "content": content })
}

#[tokio::test]
async fn authoritative_summary_journey_owns_the_message_corpus_once() {
    let harness = RegisteredGlobalDbHarness::open("lcm-compression-corpus-ownership").await;
    let db = harness.registered.clone();
    let session_id = "claude-corpus-ownership";
    assert!(db.upsert_session(&session("claude", session_id)).await);

    let mut request = daemon_summary_request("claude", session_id);
    request.messages = (0..CORPUS_MESSAGES).map(corpus_message).collect();
    request.leaf_chunk_tokens = Some(1);
    request.fresh_tail_count = Some(1);
    for message in &request.messages {
        assert_eq!(
            message["content"].as_str().map(str::len),
            Some(MESSAGE_CONTENT_BYTES)
        );
    }

    let service = DaemonLcmEffectService::new(db, None, None);
    let before = BODY_COPIES.with(Cell::get);
    let response = service.compress(request).await.unwrap();
    let body_copies = BODY_COPIES.with(Cell::get) - before;

    // The planning pass ran and the pending result kept every field the host
    // contract needs, including the summary request it could not satisfy.
    assert_eq!(response.status, "needs_summary");
    assert_eq!(response.reason, "authoritative_summarizer_unavailable");
    assert_eq!(
        response.retry_status.as_deref(),
        Some("needs_authoritative_summary")
    );
    let summary_request = response
        .summary_request
        .as_ref()
        .expect("unavailable summary keeps the pending summary request");
    assert!(!summary_request.source_messages.is_empty());
    assert_eq!(response.summary_nodes_created, 0);

    let budget = CORPUS_MESSAGES * BODY_COPIES_PER_MESSAGE + CORPUS_MESSAGES / 2;
    assert!(
        body_copies <= budget,
        "two-phase compression copied {body_copies} message bodies for {CORPUS_MESSAGES} \
         messages (budget {budget}); a whole-corpus copy was added between the planning and \
         commit passes"
    );
}
