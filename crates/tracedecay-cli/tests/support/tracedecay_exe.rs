/// The `tracedecay` binary these suites drive: the runfiles path Bazel and
/// nextest pass at run time, else the artifact Cargo compiled in.
pub(crate) fn tracedecay_exe() -> &'static std::path::Path {
    static EXE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    EXE.get_or_init(|| {
        std::env::var_os("CARGO_BIN_EXE_tracedecay")
            .map_or_else(|| env!("CARGO_BIN_EXE_tracedecay").into(), Into::into)
    })
}
