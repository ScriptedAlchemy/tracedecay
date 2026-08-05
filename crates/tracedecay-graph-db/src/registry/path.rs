use std::path::{Path, PathBuf};

use same_file::Handle;
use tracedecay_domain::framed_log::{DirectorySyncPolicy, sync_directory};
use tracedecay_store::GRAPH_STORE_PRIVATE_DIRECTORY;

use crate::GraphDbError;

pub(super) struct RetainedGraphFile {
    _file: std::fs::File,
}

pub(super) struct GraphPathAuthority {
    parent_identity: Handle,
    _parent_file: std::fs::File,
    file_identity: Handle,
    file: std::fs::File,
    path: PathBuf,
    created: bool,
}

#[derive(Debug)]
pub(super) struct GraphPathVerification {
    _parent_identity: Handle,
    _parent_file: std::fs::File,
    _file_identity: Handle,
    _file: std::fs::File,
}

pub(super) enum GraphPathPreparation {
    Existing(GraphPathAuthority),
    Missing {
        parent_identity: Handle,
        parent_file: std::fs::File,
        path: PathBuf,
    },
}

pub(super) struct GraphPathAcquisitionFailure {
    pub(super) retained_file: Option<RetainedGraphFile>,
    pub(super) error: GraphDbError,
}

impl GraphPathPreparation {
    pub(super) fn prepare(path: &Path) -> Result<Self, GraphDbError> {
        let parent_path = path
            .parent()
            .ok_or_else(|| GraphDbError::invalid("canonical graph path has no parent"))?;
        validate_managed_graph_path(path)?;
        let (parent_identity, parent_file) = anchor_private_directory(parent_path, "anchor")?;
        let file = match tracedecay_private_fs::open_private_file(path) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(GraphDbError::unavailable(format!(
                    "failed to open canonical graph database {}: {error}",
                    path.display()
                )));
            }
        };
        let (current_parent, _current_parent_file) =
            anchor_private_directory(parent_path, "re-anchor")?;
        if parent_identity != current_parent {
            return Err(GraphDbError::Conflict);
        }
        match file {
            Some(file) => {
                let authority = GraphPathAuthority::from_file(
                    parent_identity,
                    parent_file,
                    path.to_path_buf(),
                    file,
                    false,
                )
                .map_err(|(_file, error)| GraphDbError::unavailable(error.to_string()))?;
                drop(authority.verify(path)?);
                Ok(Self::Existing(authority))
            }
            None => {
                require_path_absent(path)?;
                Ok(Self::Missing {
                    parent_identity,
                    parent_file,
                    path: path.to_path_buf(),
                })
            }
        }
    }

    #[must_use]
    pub(super) const fn creates_file(&self) -> bool {
        matches!(self, Self::Missing { .. })
    }

    pub(super) fn acquire(self) -> Result<GraphPathAuthority, GraphPathAcquisitionFailure> {
        match self {
            Self::Existing(authority) => Ok(authority),
            Self::Missing {
                parent_identity,
                parent_file,
                path,
            } => {
                let file = match tracedecay_private_fs::create_private_file_retained(&path) {
                    Ok(file) => file,
                    Err(failure) => {
                        let (error, retained_file) = failure.into_parts();
                        return Err(private_creation_failure(&path, error, retained_file));
                    }
                };
                let authority = match GraphPathAuthority::from_file(
                    parent_identity,
                    parent_file,
                    path,
                    file,
                    true,
                ) {
                    Ok(authority) => authority,
                    Err((file, error)) => {
                        return Err(GraphPathAcquisitionFailure {
                            retained_file: Some(RetainedGraphFile { _file: file }),
                            error: initialization_failure(error),
                        });
                    }
                };
                if let Err(error) = sync_graph_parent(&authority.path) {
                    return Err(GraphPathAcquisitionFailure {
                        retained_file: Some(authority.into_retained_file()),
                        error: initialization_failure(error),
                    });
                }
                Ok(authority)
            }
        }
    }
}

impl GraphPathAuthority {
    fn from_file(
        parent_identity: Handle,
        parent_file: std::fs::File,
        path: PathBuf,
        file: std::fs::File,
        created: bool,
    ) -> Result<Self, (std::fs::File, std::io::Error)> {
        let identity_file = match file.try_clone() {
            Ok(identity_file) => identity_file,
            Err(error) => return Err((file, error)),
        };
        let file_identity = match Handle::from_file(identity_file) {
            Ok(file_identity) => file_identity,
            Err(error) => return Err((file, error)),
        };
        Ok(Self {
            parent_identity,
            _parent_file: parent_file,
            file_identity,
            file,
            path,
            created,
        })
    }

    #[must_use]
    pub(super) const fn was_created(&self) -> bool {
        self.created
    }

    pub(super) fn clone_file(&self) -> Result<std::fs::File, GraphDbError> {
        self.file.try_clone().map_err(|error| {
            if self.created {
                initialization_failure(error)
            } else {
                GraphDbError::unavailable(format!(
                    "failed to clone authoritative graph file: {error}"
                ))
            }
        })
    }

    pub(super) fn verify(&self, path: &Path) -> Result<GraphPathVerification, GraphDbError> {
        validate_managed_graph_path(path)?;
        let (current_parent_identity, current_parent_file) = anchor_private_directory(
            path.parent()
                .ok_or_else(|| GraphDbError::invalid("canonical graph path has no parent"))?,
            "verify",
        )?;
        if self.parent_identity != current_parent_identity {
            return Err(GraphDbError::Conflict);
        }
        let current_file = tracedecay_private_fs::open_private_file(path).map_err(|error| {
            GraphDbError::unavailable(format!(
                "failed to verify canonical graph database {}: {error}",
                path.display()
            ))
        })?;
        let identity_file = current_file.try_clone().map_err(|error| {
            GraphDbError::unavailable(format!(
                "failed to retain verified graph file identity: {error}"
            ))
        })?;
        let current_file_identity = Handle::from_file(identity_file).map_err(|error| {
            GraphDbError::unavailable(format!("failed to identify verified graph file: {error}"))
        })?;
        if self.file_identity != current_file_identity {
            return Err(GraphDbError::Conflict);
        }
        Ok(GraphPathVerification {
            _parent_identity: current_parent_identity,
            _parent_file: current_parent_file,
            _file_identity: current_file_identity,
            _file: current_file,
        })
    }

    pub(super) fn into_retained_file(self) -> RetainedGraphFile {
        RetainedGraphFile { _file: self.file }
    }
}

fn private_creation_failure(
    path: &Path,
    error: std::io::Error,
    retained_file: Option<std::fs::File>,
) -> GraphPathAcquisitionFailure {
    if let Some(file) = retained_file {
        return GraphPathAcquisitionFailure {
            retained_file: Some(RetainedGraphFile { _file: file }),
            error: initialization_failure(error),
        };
    }
    GraphPathAcquisitionFailure {
        retained_file: None,
        error: if error.kind() == std::io::ErrorKind::AlreadyExists {
            GraphDbError::Conflict
        } else {
            GraphDbError::unavailable(format!(
                "failed to create private graph database {}: {error}",
                path.display()
            ))
        },
    }
}

pub(super) fn retained_initialization_failure(
    authority: GraphPathAuthority,
    error: GraphDbError,
) -> GraphPathAcquisitionFailure {
    if authority.was_created() {
        GraphPathAcquisitionFailure {
            retained_file: Some(authority.into_retained_file()),
            error: initialization_failure(error),
        }
    } else {
        GraphPathAcquisitionFailure {
            retained_file: None,
            error,
        }
    }
}

fn initialization_failure(error: impl std::fmt::Display) -> GraphDbError {
    GraphDbError::DurabilityUncertain {
        message: format!(
            "created graph initialization failed after the durable file identity was retained: {error}"
        ),
    }
}

fn anchor_private_directory(
    path: &Path,
    operation: &str,
) -> Result<(Handle, std::fs::File), GraphDbError> {
    let file = tracedecay_private_fs::open_private_directory(path).map_err(|error| {
        GraphDbError::unavailable(format!(
            "failed to {operation} private graph directory: {error}"
        ))
    })?;
    let identity_file = file.try_clone().map_err(|error| {
        GraphDbError::unavailable(format!(
            "failed to retain private graph directory handle: {error}"
        ))
    })?;
    let identity = Handle::from_file(identity_file).map_err(|error| {
        GraphDbError::unavailable(format!(
            "failed to identify private graph directory: {error}"
        ))
    })?;
    Ok((identity, file))
}

fn require_path_absent(path: &Path) -> Result<(), GraphDbError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(GraphDbError::Conflict),
        Err(error) => Err(GraphDbError::unavailable(format!(
            "failed to verify absent graph database {}: {error}",
            path.display()
        ))),
    }
}

fn sync_graph_parent(path: &Path) -> Result<(), GraphDbError> {
    let parent = path
        .parent()
        .ok_or_else(|| GraphDbError::invalid("canonical graph path has no parent"))?;
    sync_directory(parent, DirectorySyncPolicy::Strict).map_err(|error| {
        GraphDbError::DurabilityUncertain {
            message: format!("failed to persist canonical graph database creation: {error}"),
        }
    })
}

pub(super) fn validate_managed_graph_path(path: &Path) -> Result<(), GraphDbError> {
    let parent = path
        .parent()
        .ok_or_else(|| GraphDbError::invalid("canonical graph path has no parent"))?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(|error| {
        GraphDbError::unavailable(format!(
            "failed to resolve canonical graph directory {}: {error}",
            parent.display()
        ))
    })?;
    if canonical_parent != parent {
        return Err(GraphDbError::invalid(
            "graph database parent must be the exact canonical directory",
        ));
    }
    if parent.file_name().and_then(|name| name.to_str()) != Some(GRAPH_STORE_PRIVATE_DIRECTORY) {
        return Err(GraphDbError::invalid(
            "graph database parent is not the daemon private graph directory",
        ));
    }
    tracedecay_private_fs::validate_private_directory(parent)
        .map_err(|error| map_private_path_error("graph database parent", error))?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            GraphDbError::invalid("canonical graph database must be a real file"),
        ),
        Ok(metadata) => validate_private_graph_file(path, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(GraphDbError::unavailable(format!(
            "failed to inspect canonical graph database {}: {error}",
            path.display()
        ))),
    }
}

fn validate_private_graph_file(
    path: &Path,
    _metadata: &std::fs::Metadata,
) -> Result<(), GraphDbError> {
    tracedecay_private_fs::validate_private_file(path)
        .map_err(|error| map_private_path_error("canonical graph database", error))
}

fn map_private_path_error(description: &str, error: std::io::Error) -> GraphDbError {
    match error.kind() {
        std::io::ErrorKind::InvalidInput | std::io::ErrorKind::PermissionDenied => {
            GraphDbError::invalid(format!("{description} is not private to the daemon owner"))
        }
        _ => {
            GraphDbError::unavailable(format!("failed to validate private {description}: {error}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{GraphPathPreparation, private_creation_failure, retained_initialization_failure};
    use crate::GraphDbError;

    fn graph_path(temp: &tempfile::TempDir) -> std::path::PathBuf {
        let private_directory = temp
            .path()
            .join(tracedecay_store::GRAPH_STORE_PRIVATE_DIRECTORY);
        tracedecay_private_fs::create_private_directory(&private_directory).unwrap();
        private_directory.join("graph.grafeo")
    }

    #[test]
    fn missing_preparation_does_not_create_the_graph_file() {
        let temp = tempdir().unwrap();
        let path = graph_path(&temp);

        let preparation = GraphPathPreparation::prepare(&path).unwrap();

        assert!(!path.exists());
        assert!(preparation.creates_file());
    }

    #[test]
    fn created_initialization_failure_is_uncertain_and_retains_the_file() {
        let temp = tempdir().unwrap();
        let path = graph_path(&temp);
        let authority = GraphPathPreparation::prepare(&path)
            .unwrap()
            .acquire()
            .unwrap();

        let failure = retained_initialization_failure(
            authority,
            GraphDbError::unavailable("injected initialization failure"),
        );

        assert!(failure.retained_file.is_some());
        let GraphDbError::DurabilityUncertain { message } = failure.error else {
            panic!("created-file failure must report uncertain durability");
        };
        assert!(message.contains("injected initialization failure"));
        assert!(path.is_file(), "failed initialization must retain its file");
    }

    #[test]
    fn post_creation_validation_failure_is_uncertain_and_retains_the_file() {
        let temp = tempdir().unwrap();
        let path = graph_path(&temp);
        let file = tracedecay_private_fs::create_private_file(&path).unwrap();

        let failure = private_creation_failure(
            &path,
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected post-create validation failure",
            ),
            Some(file),
        );

        assert!(failure.retained_file.is_some());
        assert!(matches!(
            failure.error,
            GraphDbError::DurabilityUncertain { .. }
        ));
        assert!(path.is_file(), "post-create failure must retain its file");
    }

    #[test]
    fn authoritative_file_identity_detects_replacement_without_deleting_it() {
        let temp = tempdir().unwrap();
        let path = graph_path(&temp);
        let authority = GraphPathPreparation::prepare(&path)
            .unwrap()
            .acquire()
            .unwrap();
        let moved = path.with_file_name("moved.grafeo");
        std::fs::rename(&path, &moved).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        tracedecay_private_fs::make_private_file(&path).unwrap();

        assert_eq!(authority.verify(&path).unwrap_err(), GraphDbError::Conflict);
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
    }
}
