//! Its own test binary: the install is process-global, and any earlier native
//! helper call in the same process would start the default runtime first.

use tracedecay_rusqlite_runtime::native_runtime::{
    self, ForeignThreadCounts, InstallError, NativeTask, NativeTaskOutcome, RuntimeBuilder,
};

fn thread_name() -> Option<String> {
    std::thread::current().name().map(str::to_owned)
}

#[test]
fn helpers_run_on_the_installed_runtime_and_a_later_install_is_refused() {
    native_runtime::install(
        RuntimeBuilder::multi_thread()
            .worker_threads(1)
            .thread_name_prefix("installed-native"),
    )
    .expect("the first install takes the process runtime");

    let worker = native_runtime::block_on(async {
        NativeTask::spawn(async { thread_name() })
            .expect("native task")
            .await
    })
    .expect("installed runtime");
    let NativeTaskOutcome::Completed(Some(worker)) = worker else {
        panic!("expected a named worker, got {worker:?}");
    };
    assert!(
        worker.starts_with("installed-native"),
        "native tasks ran on {worker}, not on the installed runtime"
    );

    only_tokio_and_storage_threads_reach_the_installed_runtime_from_outside_it();

    assert_eq!(
        native_runtime::install(RuntimeBuilder::multi_thread()).err(),
        Some(InstallError::AlreadyStarted),
        "a runtime the helpers already use must not be replaced"
    );
}

fn only_tokio_and_storage_threads_reach_the_installed_runtime_from_outside_it() {
    let before = native_runtime::foreign_thread_counts();

    let on_pool = native_runtime::block_on(async {
        native_runtime::spawn_blocking(|| {
            (
                native_runtime::handle().is_some(),
                native_runtime::cx().is_some(),
            )
        })
        .await
    })
    .expect("installed runtime")
    .expect("blocking work");
    assert_eq!(
        on_pool,
        (true, true),
        "the runtime's own blocking threads are native"
    );
    let queued_directly = native_runtime::block_on(async {
        let (sender, receiver) = std::sync::mpsc::channel();
        native_runtime::handle()
            .expect("native root")
            .spawn_blocking(move || {
                let _ = sender.send(native_runtime::handle().is_some());
            })
            .expect("blocking pool");
        native_runtime::spawn_blocking(move || receiver.recv().expect("direct blocking work")).await
    })
    .expect("installed runtime")
    .expect("blocking work");
    assert!(
        queued_directly,
        "work queued straight on the pool still runs on a native thread"
    );
    assert_eq!(native_runtime::foreign_thread_counts(), before);

    let bare = std::thread::spawn(|| {
        (
            native_runtime::handle().is_none(),
            native_runtime::cx().is_none(),
        )
    })
    .join()
    .expect("bare thread");
    assert_eq!(bare, (true, true), "a bare thread is refused");
    let refused = native_runtime::foreign_thread_counts();
    assert_eq!(refused.refused_bare, before.refused_bare + 2);

    let entered = std::thread::spawn(|| native_runtime::block_on(async { 7 }))
        .join()
        .expect("dedicated thread");
    assert_eq!(entered, Ok(7), "a dedicated thread enters through block_on");
    assert_eq!(native_runtime::foreign_thread_counts(), refused);

    let tokio = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");
    let on_tokio = tokio.block_on(async {
        (
            native_runtime::handle().is_some(),
            native_runtime::cx().is_some(),
        )
    });
    assert_eq!(on_tokio, (true, true), "a Tokio adapter thread is admitted");
    assert_eq!(
        native_runtime::foreign_thread_counts(),
        ForeignThreadCounts {
            tokio: refused.tokio + 2,
            ..refused
        },
        "each admission is counted"
    );
}
