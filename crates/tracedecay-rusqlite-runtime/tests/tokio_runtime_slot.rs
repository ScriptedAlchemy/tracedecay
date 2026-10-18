//! Its own test binary: the installed Tokio runtime is process-global.

use tracedecay_rusqlite_runtime::native_runtime::{self, InstallError};

const SITE: &str = "tests.tokio_runtime_slot";

fn tokio_from_a_native_root() -> Option<tokio::runtime::Handle> {
    native_runtime::block_on(async {
        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "the native root has no Tokio entered"
        );
        native_runtime::tokio_handle(SITE)
    })
    .expect("native root")
}

#[test]
fn a_native_root_reaches_the_installed_tokio_runtime_until_it_is_uninstalled() {
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("installed-tokio")
        .build()
        .expect("Tokio runtime");
    assert!(tokio_from_a_native_root().is_none());
    assert_eq!(native_runtime::tokio_runtime_missing_count(SITE), 1);

    native_runtime::install_tokio_runtime(tokio.handle().clone()).expect("first install");
    assert_eq!(
        native_runtime::install_tokio_runtime(tokio.handle().clone()),
        Err(InstallError::AlreadyStarted)
    );
    let installed = tokio_from_a_native_root().expect("the installed runtime");
    let ran_on = tokio
        .block_on(installed.spawn(async { std::thread::current().name().map(str::to_owned) }))
        .expect("spawned on the installed runtime");
    assert_eq!(ran_on.as_deref(), Some("installed-tokio"));
    assert_eq!(native_runtime::tokio_runtime_missing_count(SITE), 1);

    native_runtime::uninstall_tokio_runtime();
    tokio.shutdown_background();

    assert!(
        tokio_from_a_native_root().is_none(),
        "a caller after the uninstall must miss, not reach a dead runtime"
    );
    assert_eq!(native_runtime::tokio_runtime_missing_count(SITE), 2);
}
