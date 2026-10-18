// Its own test binary: the counting #[global_allocator] below is binary-global.
//! CLI dispatch resolves bindings without paying for executable JSON Schema bodies.
//!
//! Resolving one CLI call needs bindings, schema references, and the capability
//! deadline. The schema bodies are SDK and discovery output: generating them
//! walks every request and result type through schemars, canonicalizes, and
//! hashes the document. A counting allocator measures the first-call dispatch
//! path of this fresh process, then the full catalog composition. Dispatch must
//! come in under the full composition by at least the encoded size of the
//! bodies it leaves out; a dispatch path that composed the full catalog would
//! leave the later full composition nothing to allocate.
//!
//! This binary holds exactly one test because the counter is process-global.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;
use tracedecay_contracts::{CancellationSignal, Deadline, PageRequest, RequestId};
use tracedecay_daemon_protocol::{RequestedOutputFormat, parse_application_surface_request};
use tracedecay_daemon_service::application_surface::{
    application_operation_deadline_ceiling, application_surface_binding_catalog_ref,
    application_surface_catalog_ref, resolve_application_surface_dispatch_with_controls,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingSurface};

struct CountingAllocator;

static TRACK_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static BYTES: AtomicUsize = AtomicUsize::new(0);

impl CountingAllocator {
    fn record(layout: Layout) {
        if TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record(layout);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::record(layout);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record(Layout::from_size_align(new_size, layout.align()).unwrap_or(layout));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// Bytes allocated by `work`, measured with the counter armed.
fn measure<T>(work: impl FnOnce() -> T) -> (T, usize) {
    BYTES.store(0, Ordering::Relaxed);
    TRACK_ALLOCATIONS.store(true, Ordering::Relaxed);
    let output = work();
    TRACK_ALLOCATIONS.store(false, Ordering::Relaxed);
    (output, BYTES.load(Ordering::Relaxed))
}

#[test]
fn cli_dispatch_does_not_generate_executable_schema_bodies() {
    let operations = [
        (
            ApplicationSurfaceOperation::Search,
            json!({"query": "alpha", "limit": 3}),
        ),
        (
            ApplicationSurfaceOperation::Context,
            json!({"task": "what calls alpha", "mode": "plan", "max_nodes": 4}),
        ),
        (
            ApplicationSurfaceOperation::CodeCallers,
            json!({"node_id": "symbol.v1.example", "maximum_depth": 1}),
        ),
        (
            ApplicationSurfaceOperation::FileDependents,
            json!({"file": "src/lib.rs"}),
        ),
    ];
    let (resolved, dispatch_bytes) = measure(|| {
        operations
            .into_iter()
            .enumerate()
            .map(|(index, (operation, arguments))| {
                let ceiling =
                    application_operation_deadline_ceiling(operation).expect("capability deadline");
                let request = parse_application_surface_request(operation, arguments)
                    .expect("surface request");
                let dispatched = resolve_application_surface_dispatch_with_controls(
                    BindingSurface::Cli,
                    operation,
                    RequestId::new(format!("request.cli-dispatch-schema-bodies.{index}"))
                        .expect("request id"),
                    request,
                    PageRequest::first(10).expect("page"),
                    Some(Deadline::new(UtcMicros(60_000_000)).expect("deadline")),
                    CancellationSignal::active(format!(
                        "cancel.cli-dispatch-schema-bodies.{index}"
                    ))
                    .expect("cancellation"),
                    RequestedOutputFormat::Json,
                )
                .expect("cli dispatch");
                (operation, ceiling, dispatched.invocation)
            })
            .collect::<Vec<_>>()
    });
    let (full_catalog, full_bytes) =
        measure(|| application_surface_catalog_ref().expect("full catalog"));
    let binding_catalog = application_surface_binding_catalog_ref().expect("binding catalog");

    let body_bytes: usize = full_catalog
        .capabilities()
        .filter_map(|capability| full_catalog.executable_schema(capability.capability_id()))
        .map(|schema| {
            serde_json::to_vec(schema.request_schema().body())
                .expect("request body")
                .len()
                + serde_json::to_vec(schema.result_schema().body())
                    .expect("result body")
                    .len()
        })
        .sum();
    eprintln!(
        "first-call cli dispatch allocated {dispatch_bytes} bytes; full catalog composition \
         allocated {full_bytes} bytes; encoded schema bodies {body_bytes} bytes"
    );
    assert!(
        dispatch_bytes + body_bytes <= full_bytes,
        "cli dispatch allocated {dispatch_bytes} bytes against {full_bytes} for the full \
         catalog carrying {body_bytes} bytes of schema bodies"
    );

    let mut resolved_operations = Vec::new();
    for (operation, ceiling, invocation) in resolved {
        let binding = binding_catalog
            .binding(&invocation.binding_id)
            .expect("resolved binding is in the dispatch catalog");
        assert_eq!(binding.surface(), BindingSurface::Cli);
        let full_binding = full_catalog
            .binding(&invocation.binding_id)
            .expect("resolved binding is in the full catalog");
        assert_eq!(full_binding.capability_id(), binding.capability_id());
        let capability = binding_catalog
            .capability(binding.capability_id())
            .expect("dispatch capability");
        let full_capability = full_catalog
            .capability(binding.capability_id())
            .expect("full capability");
        assert_eq!(&invocation.request_schema, full_capability.request_schema());
        assert_eq!(&invocation.result_schema, full_capability.result_schema());
        assert_eq!(
            capability.request_schema(),
            full_capability.request_schema()
        );
        assert_eq!(capability.result_schema(), full_capability.result_schema());
        assert_eq!(
            ceiling,
            Duration::from_millis(full_capability.deadline().maximum_millis())
        );
        assert!(
            binding_catalog
                .executable_schema(binding.capability_id())
                .is_none(),
            "{} dispatch catalog carries a schema body",
            operation.as_str()
        );
        assert!(
            full_catalog
                .executable_schema(binding.capability_id())
                .is_some(),
            "{} full catalog is missing its schema body",
            operation.as_str()
        );
        resolved_operations.push((binding.operation().as_str().to_owned(), ceiling));
    }
    assert_eq!(
        resolved_operations
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["search", "context", "callers", "file_dependents"]
    );
    assert_eq!(resolved_operations[1].1, Duration::from_secs(10));
}
