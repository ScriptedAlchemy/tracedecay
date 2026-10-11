#!/usr/bin/env python3
"""Rewrites fully qualified owned-store file-name constants and path helpers
onto `tracedecay_store::StoreFile`, the single owner of store file names.

Bare imported uses are migrated by hand; this script covers the qualified
spellings that appear across crates. Run from the repository root.
"""

import pathlib
import re
import sys

REWRITES = [
    (r"tracedecay_sessions::runtime::user_sessions_db_path\(",
     "tracedecay_store::StoreFile::ProfileSessions.path("),
    (r"tracedecay_session_memory::memory::user::user_memory_db_path\(",
     "tracedecay_store::StoreFile::ProfileMemory.path("),
    (r"tracedecay_sessions::runtime::USER_SESSIONS_DB_FILENAME\b",
     "tracedecay_store::StoreFile::ProfileSessions.file_name()"),
    (r"tracedecay_runtime_core::config::GLOBAL_DB_FILENAME\b",
     "tracedecay_store::StoreFile::Profile.file_name()"),
    (r"tracedecay_runtime_core::config::DB_FILENAME\b",
     "tracedecay_store::StoreFile::Project.file_name()"),
    (r"tracedecay_runtime_core::storage::SESSIONS_DB_FILENAME\b",
     "tracedecay_store::StoreFile::ProjectSessions.file_name()"),
    (r"(?<![\w:])(?:crate::)?config::DB_FILENAME\b",
     "tracedecay_store::StoreFile::Project.file_name()"),
]


def main() -> int:
    changed = 0
    for path in pathlib.Path("crates").rglob("*.rs"):
        text = path.read_text()
        updated = text
        for pattern, replacement in REWRITES:
            updated = re.sub(pattern, replacement, updated)
        if updated != text:
            path.write_text(updated)
            changed += 1
            print(path)
    print(f"{changed} files rewritten", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
