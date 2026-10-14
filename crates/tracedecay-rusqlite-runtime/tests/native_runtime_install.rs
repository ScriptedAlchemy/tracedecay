//! Its own test binary: the install is process-global, and any earlier native
//! helper call in the same process would start the default runtime first.

use tracedecay_rusqlite_runtime::native_runtime::{
    self, InstallError, NativeTask, NativeTaskOutcome, RuntimeBuilder,
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

    let task = NativeTask::spawn(async { thread_name() }).expect("native task");
    let worker = native_runtime::block_on(task.wait()).expect("installed runtime");
    let NativeTaskOutcome::Completed(Some(worker)) = worker else {
        panic!("expected a named worker, got {worker:?}");
    };
    assert!(
        worker.starts_with("installed-native"),
        "native tasks ran on {worker}, not on the installed runtime"
    );

    assert_eq!(
        native_runtime::install(RuntimeBuilder::multi_thread()).err(),
        Some(InstallError::AlreadyStarted),
        "a runtime the helpers already use must not be replaced"
    );
}
