//! Retain a joined, interrupted schema for the next authorized installer.
use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_private_fs::capability_dir::{self, Dir};
use tracedecay_private_fs::{create_private_directory, open_private_directory};

use super::{
    InitializationDirectory, InitializationFiles, LeafIdentity, PreparedFamily, SIDECARS,
    STAGED_MEMBERS, ambiguous, leaf, present, present_at, require_schema_members,
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
    digests: [[u8; 32]; 4],
}

impl InterruptedInstall {
    pub(super) fn directory(&self, destination: &Path) -> PathBuf {
        destination.with_file_name(format!(
            ".tracedecay-schema-{:032x}",
            u128::from_le_bytes(self.nonce)
        ))
    }
}

fn digest(mut file: &File) -> io::Result<[u8; 32]> {
    file.rewind()?;
    let length = file.metadata()?.len();
    let mut input = file.take(length);
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    if input.limit() != 0 || file.metadata()?.len() != length {
        return Err(ambiguous(
            "SQLite recovery member changed length during verification",
        ));
    }
    Ok(digest.finalize().into())
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
        let [d0, d1, d2, d3] = files.each_ref().map(digest);
        let retry = InterruptedInstall {
            locator,
            nonce,
            digests: [d0?, d1?, d2?, d3?],
        };
        let recovery = retry.directory(&self.destination);
        if present(&recovery)? {
            return Err(ambiguous("SQLite recovery directory is already occupied"));
        }
        let family = PreparedFamily {
            retry: Some(retry),
            runtime_identity,
            main: main?,
            sidecars: [wal?, shm?, journal?],
        };
        for file in &files {
            file.sync_all()?;
        }
        self.record(family)?;
        create_private_directory(&recovery)?;
        let recovery = Dir::from_std_file(open_private_directory(&recovery)?);
        for (name, original) in STAGED_MEMBERS.into_iter().zip(&files) {
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
        self.require_empty()?;
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
        let recovery = Dir::from_std_file(open_private_directory(&recovery_path)?);
        require_schema_members(&recovery)?;
        let pin = |index: usize| -> io::Result<File> {
            let name = STAGED_MEMBERS[index];
            let file = capability_dir::open_writable_regular_nofollow(&recovery, name.as_ref())?;
            if LeafIdentity::from_file(&file)? != *identities[index]
                || digest(&file)? != retry.digests[index]
            {
                return Err(ambiguous(
                    "interrupted SQLite member no longer matches its held contents",
                ));
            }
            Ok(file)
        };
        let files = [pin(0)?, pin(1)?, pin(2)?, pin(3)?];
        for (index, source) in STAGED_MEMBERS.into_iter().enumerate() {
            capability_dir::rename_noreplace(&recovery, source.as_ref(), &stage, source.as_ref())?;
            identities[index].pin_at(&stage, source.as_ref(), false)?;
        }
        capability_dir::sync_directory(&recovery)?;
        verify_named_directory(&recovery.into_std_file(), &recovery_path)?;
        self.parent.remove_dir(leaf(&recovery_path)?)?;
        capability_dir::sync_directory(&stage)?;
        capability_dir::sync_directory(&self.parent)?;
        self.lease.set_len(0)?;
        self.lease.sync_all()?;
        self.prepared = None;
        let [main, wal, shm, journal] = files;
        Ok(Some((main, [wal, shm, journal])))
    }
}
