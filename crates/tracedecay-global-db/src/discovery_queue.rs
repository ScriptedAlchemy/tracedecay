use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use tracedecay_runtime_core::db::engine::params;

use crate::RegisteredGlobalDb;

const DISCOVERY_QUEUE_PREFIX: &str = "host-discovery-queue://";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostDiscoveryQueueEntry {
    pub sequence: u64,
    pub path: PathBuf,
}

fn provider_prefix(provider: &str) -> Result<String, String> {
    if provider.is_empty()
        || !provider
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("invalid host discovery queue provider".to_owned());
    }
    Ok(format!("{DISCOVERY_QUEUE_PREFIX}{provider}/v1/"))
}

fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        path.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect()
    }
    #[cfg(not(any(unix, windows)))]
    {
        path.to_string_lossy().as_bytes().to_vec()
    }
}

fn queue_key(prefix: &str, path: &Path) -> String {
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(path_bytes(path)))
}

fn decode_path(prefix: &str, key: &str) -> Result<PathBuf, String> {
    let encoded = key
        .strip_prefix(prefix)
        .ok_or_else(|| "host discovery queue key has the wrong provider prefix".to_owned())?;
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|error| format!("decode host discovery queue path: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        Ok(std::ffi::OsString::from_vec(bytes).into())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt as _;
        if bytes.len() % 2 != 0 {
            return Err("host discovery queue Windows path has an odd byte count".to_owned());
        }
        let wide = bytes
            .chunks_exact(2)
            .map(|chunk| {
                <[u8; 2]>::try_from(chunk)
                    .map(u16::from_le_bytes)
                    .map_err(|_| "host discovery queue Windows path chunk is invalid".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(std::ffi::OsString::from_wide(&wide).into())
    }
    #[cfg(not(any(unix, windows)))]
    {
        String::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|error| format!("decode host discovery queue UTF-8: {error}"))
    }
}

fn decode_entry(
    prefix: &str,
    sequence: i64,
    key: String,
) -> Result<HostDiscoveryQueueEntry, String> {
    Ok(HostDiscoveryQueueEntry {
        sequence: u64::try_from(sequence)
            .map_err(|_| "host discovery queue sequence is negative".to_owned())?,
        path: decode_path(prefix, &key)?,
    })
}

impl RegisteredGlobalDb {
    pub async fn enqueue_host_discovery_paths(
        &self,
        provider: &str,
        paths: Vec<PathBuf>,
    ) -> Result<Option<HostDiscoveryQueueEntry>, String> {
        let prefix = provider_prefix(provider)?;
        let Some(last_path) = paths.last().cloned() else {
            return Ok(None);
        };
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("begin host discovery queue transaction: {error}"))?;
        let pattern = format!("{prefix}%");
        let mut sequence_rows = transaction
            .query(
                "SELECT COALESCE(MAX(byte_offset), 0)
                 FROM parse_offsets
                 WHERE file_path LIKE ?1 AND file_id = 1",
                params![pattern],
            )
            .await
            .map_err(|error| format!("read host discovery queue tail: {error}"))?;
        let sequence_row = sequence_rows
            .next()
            .await
            .map_err(|error| format!("read host discovery queue tail: {error}"))?
            .ok_or_else(|| "host discovery queue tail is unavailable".to_owned())?;
        let mut sequence = sequence_row
            .get::<i64>(0)
            .map_err(|error| format!("decode host discovery queue tail: {error}"))?;
        if sequence < 0 {
            return Err("host discovery queue tail is negative".to_owned());
        }
        drop(sequence_rows);
        for path in paths {
            let key = queue_key(&prefix, &path);
            let proposed = sequence
                .checked_add(1)
                .ok_or_else(|| "host discovery queue sequence is exhausted".to_owned())?;
            let inserted = transaction
                .execute(
                    "INSERT INTO parse_offsets (file_path, byte_offset, mtime, file_id)
                     VALUES (?1, ?2, 0, 1)
                     ON CONFLICT(file_path) DO NOTHING",
                    params![key, proposed],
                )
                .await
                .map_err(|error| format!("enqueue host discovery path: {error}"))?;
            if inserted > 0 {
                sequence = proposed;
            }
        }
        let last_key = queue_key(&prefix, &last_path);
        let mut rows = transaction
            .query(
                "SELECT byte_offset, file_path
                 FROM parse_offsets
                 WHERE file_path = ?1 AND file_id = 1",
                params![last_key],
            )
            .await
            .map_err(|error| format!("resolve host discovery queue identity: {error}"))?;
        let row = rows
            .next()
            .await
            .map_err(|error| format!("read host discovery queue identity: {error}"))?
            .ok_or_else(|| "enqueued host discovery path is unavailable".to_owned())?;
        let entry = decode_entry(
            &prefix,
            row.get::<i64>(0)
                .map_err(|error| format!("decode host discovery queue sequence: {error}"))?,
            row.get::<String>(1)
                .map_err(|error| format!("decode host discovery queue key: {error}"))?,
        )?;
        drop(rows);
        transaction
            .commit()
            .await
            .map_err(|error| format!("commit host discovery queue: {error}"))?;
        Ok(Some(entry))
    }

    pub async fn host_discovery_paths_after(
        &self,
        provider: &str,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<HostDiscoveryQueueEntry>, String> {
        let prefix = provider_prefix(provider)?;
        let pattern = format!("{prefix}%");
        let after_sequence = i64::try_from(after_sequence)
            .map_err(|_| "host discovery queue cursor exceeds SQLite range".to_owned())?;
        let limit = i64::try_from(limit)
            .map_err(|_| "host discovery queue limit exceeds SQLite range".to_owned())?;
        let mut rows = self
            .read_connection()
            .query(
                "SELECT byte_offset, file_path
                 FROM parse_offsets
                 WHERE file_path LIKE ?1 AND file_id = 1 AND byte_offset > ?2
                 ORDER BY byte_offset ASC
                 LIMIT ?3",
                params![pattern, after_sequence, limit],
            )
            .await
            .map_err(|error| format!("query host discovery queue: {error}"))?;
        let mut entries = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("read host discovery queue: {error}"))?
        {
            entries.push(decode_entry(
                &prefix,
                row.get::<i64>(0)
                    .map_err(|error| format!("decode host discovery queue sequence: {error}"))?,
                row.get::<String>(1)
                    .map_err(|error| format!("decode host discovery queue key: {error}"))?,
            )?);
        }
        Ok(entries)
    }

    pub async fn host_discovery_path(
        &self,
        provider: &str,
        sequence: u64,
    ) -> Result<Option<HostDiscoveryQueueEntry>, String> {
        let prefix = provider_prefix(provider)?;
        let pattern = format!("{prefix}%");
        let sequence = i64::try_from(sequence)
            .map_err(|_| "host discovery queue sequence exceeds SQLite range".to_owned())?;
        let mut rows = self
            .read_connection()
            .query(
                "SELECT byte_offset, file_path
                 FROM parse_offsets
                 WHERE file_path LIKE ?1 AND file_id = 1 AND byte_offset = ?2",
                params![pattern, sequence],
            )
            .await
            .map_err(|error| format!("query host discovery queue identity: {error}"))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("read host discovery queue identity: {error}"))?
        else {
            return Ok(None);
        };
        decode_entry(
            &prefix,
            row.get::<i64>(0)
                .map_err(|error| format!("decode host discovery queue sequence: {error}"))?,
            row.get::<String>(1)
                .map_err(|error| format!("decode host discovery queue key: {error}"))?,
        )
        .map(Some)
    }
}
