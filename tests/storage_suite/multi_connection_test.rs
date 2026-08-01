#![cfg(unix)]

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use serde_json::json;
use tempfile::TempDir;
use tracedecay::db::{Database, DatabaseAuthority};

use crate::common;

mod harness;
use harness::*;

include!("multi_connection_test/ownership.rs");
include!("multi_connection_test/fail_closed.rs");
include!("multi_connection_test/recovery.rs");
