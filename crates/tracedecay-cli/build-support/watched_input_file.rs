#[derive(Debug, PartialEq, Eq)]
pub struct WatchedInputFile {
    env_name: &'static str,
    path: Option<std::path::PathBuf>,
}

impl WatchedInputFile {
    pub fn from_env(env_name: &'static str) -> Self {
        Self {
            env_name,
            path: std::env::var_os(env_name).map(std::path::PathBuf::from),
        }
    }

    pub fn from_value(env_name: &'static str, value: Option<std::ffi::OsString>) -> Self {
        Self {
            env_name,
            path: value.map(std::path::PathBuf::from),
        }
    }

    pub fn cargo_directives(&self) -> Vec<String> {
        let mut directives = vec![format!("cargo::rerun-if-env-changed={}", self.env_name)];
        if let Some(path) = &self.path {
            directives.push(format!("cargo::rerun-if-changed={}", path.display()));
        }
        directives
    }

    pub fn emit(&self) {
        for directive in self.cargo_directives() {
            println!("{directive}");
        }
    }
}
