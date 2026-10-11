#!/usr/bin/env python3
"""Per-file literal rewrites that move bare, locally imported owned-store
file-name constants onto `StoreFile`. Companion to
codemod-store-file-names.py. Each rewrite must match exactly once or more;
a rewrite that matches nothing fails the run so drift is visible.
"""

import pathlib
import sys

R = "crates/tracedecay-store-runtime/src/store_locator_resolver.rs"
SR = "crates/tracedecay-maintenance/src/retention/storage_report.rs"
REWRITES = {
    R: [
        ("use tracedecay_session_memory::memory::user::user_memory_db_path;\n"
         "use tracedecay_sessions::runtime::user_sessions_db_path;\n", ""),
        ("use tracedecay_store::{\n    BrainId,", "use tracedecay_store::{\n    BrainId, StoreFile,"),
        ('const PROFILE_DATABASE_FILENAME: &str = "global.db";\n'
         'const REMOTE_NODE_DATABASE_FILENAME: &str = "remote.db";\n', ""),
        ("canonical_profile_root.join(PROFILE_DATABASE_FILENAME)",
         "StoreFile::Profile.path(&canonical_profile_root)"),
        ("user_memory_db_path(&canonical_profile_root)",
         "StoreFile::ProfileMemory.path(&canonical_profile_root)"),
        ("user_sessions_db_path(&canonical_profile_root)",
         "StoreFile::ProfileSessions.path(&canonical_profile_root)"),
        ("canonical_store_root.join(REMOTE_NODE_DATABASE_FILENAME)",
         "StoreFile::RemoteNode.path(&canonical_store_root)"),
        ("fixture.profile_root.join(PROFILE_DATABASE_FILENAME)",
         "StoreFile::Profile.path(&fixture.profile_root).into_path_buf()"),
        ("user_memory_db_path(&fixture.profile_root)",
         "StoreFile::ProfileMemory.path(&fixture.profile_root).into_path_buf()"),
        ("user_sessions_db_path(&fixture.profile_root)",
         "StoreFile::ProfileSessions.path(&fixture.profile_root).into_path_buf()"),
        (".join(REMOTE_NODE_DATABASE_FILENAME)",
         ".join(StoreFile::RemoteNode.file_name())"),
        (".join(storage::SESSIONS_DB_FILENAME)",
         ".join(StoreFile::ProjectSessions.file_name())"),
    ],
    SR: [
        ("use tracedecay_store::StoreFile::ProjectSessions.file_name();\n",
         "use tracedecay_store::StoreFile;\n"),
        ('const GLOBAL_DB_FILENAME: &str = "global.db";\n'
         'const USER_SESSIONS_DB_FILENAME: &str = "user-sessions.db";\n', ""),
        ("starts_with(SESSIONS_DB_FILENAME)",
         "starts_with(StoreFile::ProjectSessions.file_name())"),
        ("profile_root.join(GLOBAL_DB_FILENAME)", "StoreFile::Profile.path(&profile_root)"),
        ("profile_root.join(USER_SESSIONS_DB_FILENAME)",
         "StoreFile::ProfileSessions.path(&profile_root)"),
        ('format!("{USER_SESSIONS_DB_FILENAME}-wal")',
         'format!("{}-wal", StoreFile::ProfileSessions.file_name())'),
    ],
    "crates/tracedecay-cli/src/commands/stale_store_reset.rs": [
        ("use tracedecay_runtime_core::config::{GLOBAL_DB_FILENAME, ProfileRoot};",
         "use tracedecay_runtime_core::config::ProfileRoot;"),
        ("    SESSIONS_DB_FILENAME, STORE_MANIFEST_FILENAME,", "    STORE_MANIFEST_FILENAME,"),
        ("use tracedecay_store::StoreFile::ProfileSessions.file_name();\n",
         "use tracedecay_store::StoreFile;\n"),
        ("DatabaseFamily(GLOBAL_DB_FILENAME)", "DatabaseFamily(StoreFile::Profile)"),
        ("DatabaseFamily(USER_SESSIONS_DB_FILENAME)", "DatabaseFamily(StoreFile::ProfileSessions)"),
        ("DatabaseFamily(SESSIONS_DB_FILENAME)", "DatabaseFamily(StoreFile::ProjectSessions)"),
    ],
    "crates/tracedecay-store-runtime/src/session_registry/mounts.rs": [
        ("    SESSIONS_DB_FILENAME, STORE_MANIFEST_FILENAME,", "    STORE_MANIFEST_FILENAME,"),
        ("store_root.join(SESSIONS_DB_FILENAME).is_file()",
         "tracedecay_store::StoreFile::ProjectSessions.path(&store_root).is_file()"),
        ('entry.path().join("remote.db")',
         "tracedecay_store::StoreFile::RemoteNode.path(&entry.path()).into_path_buf()"),
    ],
    "crates/tracedecay-global-db/src/registry_maintenance/orphan.rs": [
        ("use tracedecay_store::StoreFile::Project.file_name();\n",
         "use tracedecay_store::StoreFile;\n"),
        ("branch_dir.join(DB_FILENAME)", "StoreFile::Project.path(&branch_dir)"),
        ("data_root.join(DB_FILENAME)", "StoreFile::Project.path(&data_root)"),
    ],
    "crates/tracedecay/src/daemon/profile_retained.rs": [
        ("use tracedecay_sessions::runtime::user_sessions_db_path;\n", ""),
        ("user_sessions_db_path(profile_identity.profile_root())",
         "tracedecay_store::StoreFile::ProfileSessions.path(profile_identity.profile_root())"),
    ],
    "crates/tracedecay/tests/storage_suite/global_registry_test.rs": [
        (" SESSIONS_DB_FILENAME,", ""),
        ("PathBuf::from(SESSIONS_DB_FILENAME)",
         "PathBuf::from(tracedecay_store::StoreFile::ProjectSessions.file_name())"),
    ],
    "crates/tracedecay-session-temporal-store/src/projection/tests.rs": [
        ("use tracedecay_sessions::runtime::user_sessions_db_path;\n",
         "use tracedecay_store::StoreFile;\n"),
        ("user_sessions_db_path(tmp.path())", "StoreFile::ProfileSessions.path(tmp.path())"),
    ],
    "crates/tracedecay-runtime-core/src/db/access/path_layout.rs": [
        ('name != "remote.db"', "name != tracedecay_store::StoreFile::RemoteNode.file_name()"),
    ],
}


def main() -> int:
    failed = False
    for name, rewrites in REWRITES.items():
        path = pathlib.Path(name)
        text = path.read_text()
        for old, new in rewrites:
            if old not in text and new and new in text:
                continue
            if old not in text:
                print(f"{name}: no match for {old!r}", file=sys.stderr)
                failed = True
                continue
            text = text.replace(old, new)
        path.write_text(text)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
