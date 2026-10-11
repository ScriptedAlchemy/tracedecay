//! Crash and recovery harness for the registered writer (#3402 U13).
//!
//! Each crash point re-executes this binary as a child (`crash_child`) that
//! drives a seeded workload through the production repository factory until
//! an armed crash point aborts it. The parent then reopens the store and
//! checks the recovery invariants in `verify`. Every point prints
//! `TD_CRASH_POINT <mode> seed=<seed> nth=<n>`; rerun one seed by running its
//! mode's test and reading that line.
//!
//! Abort failpoints run on both engines. CrashIo modes (kill at the Nth
//! write or sync, and power loss that drops writes since the last fsync) need
//! `--config=native` and are skipped with a printed note otherwise.
#![cfg(unix)]

mod child;
mod scenario;
mod store;
mod verify;

use tracedecay_sql::native::crash_io::{
    CrashEffect::{Kill, PowerLoss},
    CrashOp::{Sync, Write},
    CrashTarget::{Main, Wal},
};

use scenario::{Io, Mode};

const CHILD_ENV: &str = "TD_CRASH_CHILD";

/// 3 x 70 = 210 abort points on the default engine.
const FAILPOINT_SEEDS: u64 = 70;
/// 8 x 25 = 200 CrashIo points on the bare native driver.
const DRIVER_SEEDS: u64 = 25;
/// 4 x 10 = 40 CrashIo points on the native registered writer.
const WRITER_IO_SEEDS: u64 = 10;

#[test]
fn crash_child() {
    child::run_if_requested();
}

macro_rules! crash_points {
    ($($name:ident => $mode:expr, $seeds:expr;)+) => {$(
        #[test]
        fn $name() {
            verify::run($mode, $seeds);
        }
    )+};
}

crash_points! {
    failpoint_before_commit => Mode::Failpoint("before_commit"), FAILPOINT_SEEDS;
    failpoint_after_commit_before_watermark =>
        Mode::Failpoint("after_commit_before_watermark"), FAILPOINT_SEEDS;
    failpoint_mid_checkpoint => Mode::Failpoint("mid_checkpoint"), FAILPOINT_SEEDS;

    native_writer_wal_write_kill => Mode::WriterIo(Io::new(Wal, Write, Kill)), WRITER_IO_SEEDS;
    native_writer_wal_sync_kill => Mode::WriterIo(Io::new(Wal, Sync, Kill)), WRITER_IO_SEEDS;
    native_writer_wal_write_power_loss =>
        Mode::WriterIo(Io::new(Wal, Write, PowerLoss)), WRITER_IO_SEEDS;
    native_writer_main_write_power_loss =>
        Mode::WriterIo(Io::new(Main, Write, PowerLoss)), WRITER_IO_SEEDS;

    native_driver_wal_write_kill => Mode::DriverIo(Io::new(Wal, Write, Kill)), DRIVER_SEEDS;
    native_driver_wal_sync_kill => Mode::DriverIo(Io::new(Wal, Sync, Kill)), DRIVER_SEEDS;
    native_driver_wal_write_power_loss =>
        Mode::DriverIo(Io::new(Wal, Write, PowerLoss)), DRIVER_SEEDS;
    native_driver_wal_sync_power_loss =>
        Mode::DriverIo(Io::new(Wal, Sync, PowerLoss)), DRIVER_SEEDS;
    native_driver_main_write_kill => Mode::DriverIo(Io::new(Main, Write, Kill)), DRIVER_SEEDS;
    native_driver_main_sync_kill => Mode::DriverIo(Io::new(Main, Sync, Kill)), DRIVER_SEEDS;
    native_driver_main_write_power_loss =>
        Mode::DriverIo(Io::new(Main, Write, PowerLoss)), DRIVER_SEEDS;
    native_driver_main_sync_power_loss =>
        Mode::DriverIo(Io::new(Main, Sync, PowerLoss)), DRIVER_SEEDS;
}
