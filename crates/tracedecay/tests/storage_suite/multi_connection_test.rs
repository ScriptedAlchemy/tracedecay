#![cfg(unix)]

use std::io::Write;
use std::process::Stdio;
use std::sync::{Arc, Barrier};

use serde_json::json;
use tempfile::TempDir;

use crate::common;

mod harness;
use harness::*;

include!("multi_connection_test/ownership.rs");
include!("multi_connection_test/fail_closed.rs");
