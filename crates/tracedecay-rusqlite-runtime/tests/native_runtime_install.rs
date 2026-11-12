//! Its own test binary: the install is process-global, and any earlier native
//! helper call in the same process would start the default runtime first.

use tracedecay_rusqlite_runtime::native_runtime::{
    self, Builder, InstallError, NativeTask, NativeTaskOutcome,
};

fn thread_name() -> Option<String> {
    std::thread::current().name().map(str::to_owned)
}

#[test]
fn helpers_run_on_the_installed_runtime_and_a_later_install_is_refused() {
    let mut builder = Builder::new_multi_thread();
    builder.worker_threads(1).thread_name("installed-native");
    native_runtime::install(builder).expect("the first install takes the process runtime");

    let task = NativeTask::spawn(async { thread_name() }).expect("native task");
    let worker = native_runtime::block_on(task.wait()).expect("installed runtime");
    let NativeTaskOutcome::Completed(Some(worker)) = worker else {
        panic!("expected a named worker, got {worker:?}");
    };
    assert!(
        worker.starts_with("installed-native"),
        "native tasks ran on {worker}, not on the installed runtime"
    );

    const SITE: &str = "tests.native_runtime_install";
    let process_handle = native_runtime::tokio_handle(SITE).expect("installed process runtime");
    let selected = native_runtime::block_on(process_handle.spawn(async { thread_name() }))
        .expect("process root")
        .expect("task completed");
    assert_eq!(selected.as_deref(), Some("installed-native"));

    let alternate = Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("alternate-tokio")
        .build()
        .expect("alternate runtime");
    let selected = alternate.block_on(async {
        native_runtime::tokio_handle(SITE)
            .expect("entered runtime takes precedence")
            .spawn(async { thread_name() })
            .await
            .expect("alternate task completed")
    });
    assert_eq!(selected.as_deref(), Some("alternate-tokio"));
    assert_eq!(native_runtime::tokio_runtime_missing_count(SITE), 0);

    assert_eq!(
        native_runtime::install(Builder::new_multi_thread()).err(),
        Some(InstallError::AlreadyStarted),
        "a runtime the helpers already use must not be replaced"
    );
}
