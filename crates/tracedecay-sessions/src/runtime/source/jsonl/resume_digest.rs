//! The canonical JSONL prefix proof, including its first complete frame.

use sha2::{Digest, Sha256};

/// A header decision must describe the first frame of the admitted prefix,
/// including resumed pages whose own frames start after that header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::runtime) struct JsonlFirstFrameWitness([u8; 32]);

impl JsonlFirstFrameWitness {
    /// `bytes` is one complete frame, including its terminating newline.
    pub(in crate::runtime) fn for_frame(bytes: &[u8]) -> Self {
        let mut digest = ResumeDigest::new();
        digest.extend(bytes);
        Self(digest.witness(bytes.len() as u64))
    }
}

#[derive(Clone)]
pub(in crate::runtime) struct ResumeDigest {
    hasher: Sha256,
    hashed_bytes: u64,
    first_frame: Option<JsonlFirstFrameWitness>,
}

impl ResumeDigest {
    pub(in crate::runtime) fn new() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"tracedecay-jsonl-resume-prefix-v2");
        Self {
            hasher,
            hashed_bytes: 0,
            first_frame: None,
        }
    }

    pub(super) fn extend(&mut self, bytes: &[u8]) {
        if self.first_frame.is_none()
            && let Some(newline) = bytes.iter().position(|byte| *byte == b'\n')
        {
            let (first, rest) = bytes.split_at(newline + 1);
            self.hasher.update(first);
            self.hashed_bytes = self.hashed_bytes.saturating_add(first.len() as u64);
            self.first_frame = Some(JsonlFirstFrameWitness(self.witness(self.hashed_bytes)));
            self.hasher.update(rest);
            self.hashed_bytes = self.hashed_bytes.saturating_add(rest.len() as u64);
        } else {
            self.hasher.update(bytes);
            self.hashed_bytes = self.hashed_bytes.saturating_add(bytes.len() as u64);
        }
    }

    pub(super) fn first_frame_witness(&self) -> Option<JsonlFirstFrameWitness> {
        self.first_frame
    }

    /// 64-bit check of the hashed prefix plus `position`. This is not SHA-256
    /// mid-state: the domain cursor (`StoredCursor` + optional resume
    /// fingerprint) has no field that can carry hasher bytes without
    /// displacing `file_id` / generation, which would collapse rewrite and
    /// file-identity detection. A first append after a durable resume must
    /// therefore re-walk `[0, cursor)` to rebuild this digest.
    pub(in crate::runtime) fn fingerprint(&self, position: u64) -> u64 {
        let mut hasher = self.hasher.clone();
        hasher.update(position.to_le_bytes());
        digest_prefix_u64(hasher.finalize())
    }

    pub(in crate::runtime) fn witness(&self, verified_position: u64) -> [u8; 32] {
        let mut hasher = self.hasher.clone();
        hasher.update(verified_position.to_le_bytes());
        hasher.finalize().into()
    }
}

pub(super) fn digest_prefix_u64(digest: sha2::digest::Output<Sha256>) -> u64 {
    let [
        first,
        second,
        third,
        fourth,
        fifth,
        sixth,
        seventh,
        eighth,
        ..,
    ] = <[u8; 32]>::from(digest);
    u64::from_be_bytes([first, second, third, fourth, fifth, sixth, seventh, eighth])
}

#[cfg(test)]
mod tests {
    use super::{JsonlFirstFrameWitness, ResumeDigest};

    #[test]
    fn durable_prefix_fingerprint_and_witness_keep_their_existing_bytes() {
        let bytes = b"header\nbody\n";
        // Existing v2 prefix output, before retaining first-frame evidence.
        let expected = [
            0xda, 0x80, 0x66, 0x51, 0x3f, 0xf3, 0xd0, 0x96, 0xc0, 0xaf, 0xd4, 0x77, 0x3a, 0x51,
            0x0d, 0xa4, 0xd7, 0x61, 0x9a, 0xff, 0x2d, 0x3d, 0x53, 0xf8, 0x57, 0xc9, 0x8f, 0x48,
            0x23, 0x4f, 0xe7, 0xeb,
        ];
        for width in 1..=bytes.len() {
            let mut digest = ResumeDigest::new();
            for chunk in bytes.chunks(width) {
                digest.extend(chunk);
            }
            assert_eq!(
                digest.fingerprint(bytes.len() as u64),
                15_744_696_796_438_581_398
            );
            assert_eq!(digest.witness(bytes.len() as u64), expected);
            assert_eq!(
                digest.first_frame_witness(),
                Some(JsonlFirstFrameWitness::for_frame(b"header\n")),
            );
        }
        assert_eq!(
            ResumeDigest::new().fingerprint(0),
            10_930_478_499_083_391_767
        );
    }

    #[test]
    fn first_frame_proof_waits_for_the_newline_across_chunk_boundaries() {
        for frame in [b"\n".as_slice(), b"header\n", b"header\r\n"] {
            for split in 0..frame.len() {
                let mut digest = ResumeDigest::new();
                digest.extend(&frame[..split]);
                assert_eq!(digest.first_frame_witness(), None);
                digest.extend(&frame[split..]);
                let proof = Some(JsonlFirstFrameWitness::for_frame(frame));
                assert_eq!(digest.first_frame_witness(), proof);
                digest.extend(b"next\npartial tail");
                assert_eq!(digest.first_frame_witness(), proof);
            }
        }
    }

    #[test]
    fn resumed_digest_retains_its_first_frame_proof() {
        let mut committed = ResumeDigest::new();
        committed.extend(b"header\nfirst record\n");
        let mut resumed = committed.clone();
        resumed.extend(b"next record\n");
        let mut cold = ResumeDigest::new();
        cold.extend(b"header\nfirst record\nnext record\n");
        assert_eq!(
            resumed.first_frame_witness(),
            committed.first_frame_witness()
        );
        assert_eq!(resumed.first_frame_witness(), cold.first_frame_witness());
        assert_eq!(resumed.witness(32), cold.witness(32));
    }

    #[test]
    fn first_frame_proof_covers_changes_after_the_native_identity_window() {
        let mut first = vec![b'x'; 2048];
        first.push(b'\n');
        let mut changed = first.clone();
        changed[1500] = b'y';
        assert_eq!(&first[..1024], &changed[..1024]);
        assert_ne!(
            JsonlFirstFrameWitness::for_frame(&first),
            JsonlFirstFrameWitness::for_frame(&changed),
        );
    }
}
