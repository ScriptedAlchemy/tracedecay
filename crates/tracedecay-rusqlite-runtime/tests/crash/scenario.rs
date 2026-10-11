use std::fmt;

use tracedecay_sql::native::crash_io::{CrashEffect, CrashOp, CrashPlan, CrashTarget};

/// One crash model. `Failpoint` aborts the registered writer at a named
/// failpoint (both engines). `WriterIo` and `DriverIo` crash through CrashIo
/// (native engine only): under the registered writer through the real
/// factory, and under the bare `tracedecay-sql` driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Failpoint(&'static str),
    WriterIo(Io),
    DriverIo(Io),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Io {
    pub target: CrashTarget,
    pub op: CrashOp,
    pub effect: CrashEffect,
}

impl Io {
    pub const fn new(target: CrashTarget, op: CrashOp, effect: CrashEffect) -> Self {
        Self { target, op, effect }
    }

    pub fn plan(self, nth: u64) -> CrashPlan {
        CrashPlan {
            target: self.target,
            op: self.op,
            nth,
            effect: self.effect,
        }
    }
}

pub const FAILPOINTS: [&str; 3] = [
    "before_commit",
    "after_commit_before_watermark",
    "mid_checkpoint",
];

impl Mode {
    pub fn parse(text: &str) -> Self {
        let mut parts = text.split(':');
        let kind = parts.next().expect("mode kind");
        if kind == "fp" {
            let name = parts.next().expect("failpoint name");
            return Self::Failpoint(
                FAILPOINTS
                    .into_iter()
                    .find(|known| *known == name)
                    .unwrap_or_else(|| panic!("unknown failpoint {name}")),
            );
        }
        let target = match parts.next() {
            Some("main") => CrashTarget::Main,
            Some("wal") => CrashTarget::Wal,
            other => panic!("unknown crash target {other:?}"),
        };
        let op = match parts.next() {
            Some("write") => CrashOp::Write,
            Some("sync") => CrashOp::Sync,
            other => panic!("unknown crash op {other:?}"),
        };
        let effect = match parts.next() {
            Some("kill") => CrashEffect::Kill,
            Some("power") => CrashEffect::PowerLoss,
            Some("fail") => CrashEffect::Fail,
            other => panic!("unknown crash effect {other:?}"),
        };
        let io = Io::new(target, op, effect);
        match kind {
            "writer" => Self::WriterIo(io),
            "driver" => Self::DriverIo(io),
            other => panic!("unknown mode kind {other}"),
        }
    }

    /// Upper bound for the crash point: how many matching events every run of
    /// this mode is certain to reach.
    fn reachable(self, requests: u64) -> u64 {
        let checkpoints = requests / CHECKPOINT_EVERY as u64;
        match self {
            Self::Failpoint("mid_checkpoint") => checkpoints,
            // Concurrent pairs may share one batch, so commits >= requests / 2.
            Self::Failpoint(_) => requests / 2,
            Self::WriterIo(io) | Self::DriverIo(io) => match io.target {
                CrashTarget::Wal => requests / 2,
                CrashTarget::Main => checkpoints,
            },
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, io) = match self {
            Self::Failpoint(name) => return write!(formatter, "fp:{name}"),
            Self::WriterIo(io) => ("writer", io),
            Self::DriverIo(io) => ("driver", io),
        };
        let target = match io.target {
            CrashTarget::Main => "main",
            CrashTarget::Wal => "wal",
        };
        let op = match io.op {
            CrashOp::Write => "write",
            CrashOp::Sync => "sync",
        };
        let effect = match io.effect {
            CrashEffect::Kill => "kill",
            CrashEffect::PowerLoss => "power",
            CrashEffect::Fail => "fail",
        };
        write!(formatter, "{kind}:{target}:{op}:{effect}")
    }
}

/// A checkpoint is requested after every `CHECKPOINT_EVERY` workload steps.
pub const CHECKPOINT_EVERY: usize = 3;

/// The workload and crash point one seed selects. Parent and child derive it
/// from the same `(mode, seed)`, so the parent knows what the child ran.
#[derive(Debug)]
pub struct Plan {
    pub requests: usize,
    /// Step `i` submits requests `i` and `i + 1` concurrently.
    pub concurrent: Vec<bool>,
    /// After step `i`, resubmit this acknowledged request (index into acks).
    pub replay: Vec<Option<u64>>,
    /// The checkpoint after step `i` truncates instead of running passive.
    pub truncate: Vec<bool>,
    pub nth: u64,
}

impl Plan {
    pub fn new(mode: Mode, seed: u64) -> Self {
        let salt = mode
            .to_string()
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            });
        let mut rng = Rng(seed ^ salt);
        let requests = 9 + rng.below(7) as usize;
        let concurrent = (0..requests).map(|_| rng.below(3) == 0).collect();
        let replay = (0..requests)
            .map(|_| (rng.below(4) == 0).then(|| rng.next()))
            .collect();
        let truncate = (0..requests).map(|_| rng.below(2) == 0).collect();
        let nth = 1 + rng.below(mode.reachable(requests as u64));
        Self {
            requests,
            concurrent,
            replay,
            truncate,
            nth,
        }
    }
}

/// SplitMix64: deterministic across platforms, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}
