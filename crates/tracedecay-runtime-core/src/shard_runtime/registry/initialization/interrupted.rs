//! Retain a joined, interrupted schema for the next authorized installer.
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracedecay_private_fs::capability_dir::{self, Dir};
use tracedecay_private_fs::{create_private_directory, open_private_directory};

use super::{
    InitializationDirectory, InitializationFiles, LeafIdentity, PreparedFamily, SIDECARS,
    STAGED_MEMBERS, ambiguous, leaf, present, present_at, require_schema_members, sha256_file,
    surviving_sidecars, verify_named_directory,
};
use crate::sqlite_read_snapshot::with_suffix;

/// Only a joined, handled failure can deposit the original members under
/// the lease's private recovery names. No alias or backup is created.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InterruptedInstall {
    locator: [u8; 32],
    nonce: [u8; 16],
    sidecar_digests: [[u8; 32]; 3],
}

impl InterruptedInstall {
    pub(super) fn directory(&self, destination: &Path) -> PathBuf {
        destination.with_file_name(format!(
            ".tracedecay-schema-{:032x}",
            u128::from_le_bytes(self.nonce)
        ))
    }
}

impl InitializationDirectory {
    pub(crate) fn hold_interrupted(
        &mut self,
        runtime_identity: u64,
        locator: [u8; 32],
        (main, sidecars): InitializationFiles,
    ) -> io::Result<()> {
        self.verify_directory_name()?;
        let stage = self.stage_cap()?;
        require_schema_members(&stage)?;
        let [wal, shm, journal] = surviving_sidecars(sidecars)?;
        let member = |name: &str, original: Option<File>| -> io::Result<File> {
            if present_at(&stage, name.as_ref())? {
                let file =
                    original.ok_or_else(|| ambiguous("SQLite member lost its original handle"))?;
                LeafIdentity::from_file(&file)?.pin_at(&stage, name.as_ref(), false)?;
                Ok(file)
            } else {
                capability_dir::create_private_file(&stage, name.as_ref())
            }
        };
        LeafIdentity::from_file(&main)?.pin_at(&stage, STAGED_MEMBERS[0].as_ref(), false)?;
        let files = [
            main,
            member(STAGED_MEMBERS[1], wal)?,
            member(STAGED_MEMBERS[2], shm)?,
            member(STAGED_MEMBERS[3], journal)?,
        ];
        let [main, wal, shm, journal] = files.each_ref().map(LeafIdentity::from_file);
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|error| io::Error::other(error.to_string()))?;
        let [d0, d1, d2, d3] = files.each_ref().map(sha256_file);
        let retry = InterruptedInstall {
            locator,
            nonce,
            sidecar_digests: [d1?, d2?, d3?],
        };
        let recovery = retry.directory(&self.destination);
        if present(&recovery)? {
            return Err(ambiguous("SQLite recovery directory is already occupied"));
        }
        let family = PreparedFamily {
            retry: Some(retry),
            runtime_identity,
            main_sha256: d0?,
            main: main?,
            sidecars: [wal?, shm?, journal?],
        };
        let mut pins = Vec::new();
        for (name, file) in STAGED_MEMBERS.into_iter().zip(&files) {
            file.sync_all()?;
            pins.push(LeafIdentity::from_file(file)?.pin_at(&stage, name.as_ref(), false)?);
        }
        // Windows keeps the live worker's main handle non-delete-sharing.
        // Its successor pins are already open and match that original handle.
        drop(files);
        self.record(family)?;
        create_private_directory(&recovery)?;
        let recovery = Dir::from_std_file(open_private_directory(&recovery)?);
        for (name, original) in STAGED_MEMBERS.into_iter().zip(&pins) {
            capability_dir::rename_noreplace(&stage, name.as_ref(), &recovery, name.as_ref())?;
            LeafIdentity::from_file(original)?.pin_at(&recovery, name.as_ref(), false)?;
        }
        capability_dir::sync_directory(&recovery)?;
        capability_dir::sync_directory(&self.parent)?;
        capability_dir::sync_directory(&stage)
    }

    pub(crate) fn resume_interrupted(
        &mut self,
        locator: [u8; 32],
    ) -> io::Result<Option<InitializationFiles>> {
        let Some(family) = &self.prepared else {
            return Ok(None);
        };
        let Some(retry) = &family.retry else {
            return Ok(None);
        };
        if retry.locator != locator {
            return Err(ambiguous(
                "interrupted SQLite install belongs to a different store locator",
            ));
        }
        self.verify_directory_name()?;
        for suffix in SIDECARS {
            if present_at(&self.parent, leaf(&with_suffix(&self.destination, suffix))?)? {
                return Err(ambiguous(
                    "canonical SQLite sidecar is occupied during retry",
                ));
            }
        }
        let stage = self.stage_cap()?;
        let identities = [
            &family.main,
            &family.sidecars[0],
            &family.sidecars[1],
            &family.sidecars[2],
        ];
        let recovery_path = retry.directory(&self.destination);
        require_schema_members(&stage)?;
        let recovery = match open_private_directory(&recovery_path) {
            Ok(pin) => Some(Dir::from_std_file(pin)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if let Some(directory) = &recovery {
            require_schema_members(directory)?;
        }
        // After a crash a file may still occupy the active SQLite namespace.
        // A byte digest cannot prove an allocation (all empty WALs hash alike).
        // Only the sheltered name, or a kernel generation in the active stage,
        // can authorize restoration. Inspect the whole family before moving it.
        let [wal, shm, journal] = &retry.sidecar_digests;
        let digests = [&family.main_sha256, wal, shm, journal];
        let pin = |index: usize| -> io::Result<(File, bool)> {
            let name = STAGED_MEMBERS[index].as_ref();
            let held = match &recovery {
                Some(directory) if present_at(directory, name)? => Some(directory),
                _ => None,
            };
            let (directory, move_back) = match (present_at(&stage, name)?, held) {
                (false, Some(directory)) => (directory, true),
                (true, None) if identities[index].allocation.is_some() => (&stage, false),
                _ => {
                    return Err(ambiguous(
                        "SQLite recovery member is missing, duplicated, or lacks its allocation generation",
                    ));
                }
            };
            let file = capability_dir::open_writable_regular_nofollow(directory, name)?;
            if LeafIdentity::from_file(&file)? != *identities[index]
                || sha256_file(&file)? != *digests[index]
            {
                return Err(ambiguous(
                    "interrupted SQLite member no longer matches its held contents",
                ));
            }
            Ok((file, move_back))
        };
        let files = [pin(0)?, pin(1)?, pin(2)?, pin(3)?];
        for (index, (_, move_back)) in files.iter().enumerate() {
            let name = STAGED_MEMBERS[index].as_ref();
            if *move_back {
                let directory = recovery
                    .as_ref()
                    .ok_or_else(|| ambiguous("SQLite recovery directory disappeared"))?;
                capability_dir::rename_noreplace(directory, name, &stage, name)?;
            }
            identities[index].pin_at(&stage, name, false)?;
        }
        if let Some(directory) = recovery {
            capability_dir::sync_directory(&directory)?;
            verify_named_directory(&directory.into_std_file(), &recovery_path)?;
            self.parent.remove_dir(leaf(&recovery_path)?)?;
        }
        capability_dir::sync_directory(&stage)?;
        capability_dir::sync_directory(&self.parent)?;
        // Keep the receipt until the succeeding install replaces it. A crash
        // after the empty directory was removed can still prove eligible files.
        let files = files.map(|(file, _)| file);
        let [main, wal, shm, journal] = files;
        Ok(Some((main, [wal, shm, journal])))
    }
}
