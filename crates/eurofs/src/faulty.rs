//! **Fault-injecting block device** for crash-consistency proof (Sprint 5).
//!
//! Wraps a real [`BlockDevice`] and simulates a POWER FAILURE: after the `k`-th
//! write operation all subsequent writes vanish (as if the machine were dead),
//! or — with `tear_at` — a single block is written HALF (a "torn write") and then
//! everything stops. This lets a test crash at ANY write point and prove that EuroFS
//! on remount always recovers a consistent, previously-committed state (A/B superblock +
//! generation + checksum) — 0 data loss of confirmed commits, 0 panics.
//!
//! Pure `no_std` logic; used only in tests, but deliberately a real (not a mock)
//! `BlockDevice` so that the injection hits exactly the path the kernel also uses.

use crate::block::{BlockDevice, BlockResult};
use alloc::vec::Vec;

pub struct FaultyBlockDevice<D: BlockDevice> {
    inner: D,
    /// Number of `write_blocks` calls so far (1-based index at the call).
    writes: u64,
    /// After this write index every further write vanishes (power failure).
    crash_after: Option<u64>,
    /// At this write index: write only the first half of the block (torn) and crash.
    tear_at: Option<u64>,
    crashed: bool,
    /// Write indices that hit a superblock LBA (1 or 2) — for diagnosis.
    pub sb_write_ops: Vec<u64>,
}

impl<D: BlockDevice> FaultyBlockDevice<D> {
    pub fn new(inner: D) -> Self {
        Self { inner, writes: 0, crash_after: None, tear_at: None, crashed: false, sb_write_ops: Vec::new() }
    }
    /// After `k` writes, make all subsequent writes vanish (power failure).
    pub fn crash_after(inner: D, k: u64) -> Self {
        Self { inner, writes: 0, crash_after: Some(k), tear_at: None, crashed: false, sb_write_ops: Vec::new() }
    }
    /// Write the `k`-th write HALF (torn write) and then stop.
    pub fn tear_at(inner: D, k: u64) -> Self {
        Self { inner, writes: 0, crash_after: None, tear_at: Some(k), crashed: false, sb_write_ops: Vec::new() }
    }
    /// Total number of writes this device saw (for the clean-run measurement).
    pub fn writes(&self) -> u64 {
        self.writes
    }
    pub fn crashed(&self) -> bool {
        self.crashed
    }
    pub fn into_inner(self) -> D {
        self.inner
    }
}

impl<D: BlockDevice> BlockDevice for FaultyBlockDevice<D> {
    fn block_size(&self) -> u32 {
        self.inner.block_size()
    }
    fn block_count(&self) -> u64 {
        self.inner.block_count()
    }
    fn read_blocks(&self, start: u64, count: u32, buf: &mut [u8]) -> BlockResult<()> {
        self.inner.read_blocks(start, count, buf)
    }
    fn write_blocks(&mut self, start: u64, count: u32, buf: &[u8]) -> BlockResult<()> {
        // Once crashed: nothing reaches the disk anymore (the power is gone). We return
        // Ok so the caller finishes its in-memory path; the disk state is
        // frozen at the crash moment — that is what a remount will read back later.
        if self.crashed {
            return Ok(());
        }
        self.writes += 1;
        let idx = self.writes;
        if start <= 2 {
            self.sb_write_ops.push(idx); // possibly hits a superblock slot (LBA 1/2)
        }
        if let Some(tear) = self.tear_at {
            if idx == tear {
                // Torn write: only the first half lands, the rest does not → checksum breaks.
                let half = buf.len() / 2;
                let mut partial = buf.to_vec();
                for b in partial[half..].iter_mut() {
                    *b = 0;
                }
                let _ = self.inner.write_blocks(start, count, &partial);
                self.crashed = true;
                return Ok(());
            }
        }
        if let Some(k) = self.crash_after {
            if idx > k {
                self.crashed = true;
                return Ok(());
            }
        }
        self.inner.write_blocks(start, count, buf)
    }
    fn flush(&mut self) -> BlockResult<()> {
        if self.crashed {
            return Ok(());
        }
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::MemoryBlockDevice;
    use crate::disk::EuroFs;
    use crate::fs::FileSystem;

    /// The workload: format + commit a series of versions of /data (each
    /// `write_file` is one commit/checkpoint). Returns the number of writes
    /// after format and after each commit (the checkpoint boundaries), plus the versions.
    fn versions() -> Vec<&'static [u8]> {
        alloc::vec![
            b"versie-0-AAAA".as_slice(),
            b"versie-1-BBBBBBBB".as_slice(),
            b"versie-2-CCCCCCCCCCCC".as_slice(),
            b"versie-3-DDDDDDDDDDDDDDDD".as_slice(),
            b"versie-4-EEEEEEEEEEEEEEEEEEEE".as_slice(),
        ]
    }

    /// Measure the write counter at each checkpoint boundary via separate deterministic runs:
    /// bounds[0] = after format (empty fs), bounds[i+1] = after version i. Returns (bounds, total).
    fn measure() -> (Vec<u64>, u64) {
        let mut bounds = Vec::new();
        bounds.push(run_count(0)); // format only
        for i in 0..versions().len() {
            bounds.push(run_count(i + 1));
        }
        let total = *bounds.last().unwrap();
        (bounds, total)
    }

    /// Run format + the first `commits` versions and return the total number of
    /// writes (the checkpoint boundary).
    fn run_count(commits: usize) -> u64 {
        let mem = MemoryBlockDevice::new(4096, 4096);
        let mut dev = FaultyBlockDevice::new(mem);
        {
            let mut fs = EuroFs::format(&mut dev, [7u8; 16], 1).unwrap();
            for v in versions().iter().take(commits) {
                fs.write_file("/data", v).unwrap();
            }
        }
        dev.writes()
    }

    /// Run format + `commits` versions, crash after write `k`, and return the
    /// remounted /data content (None = absent/unreadable) — without ever panicking.
    fn replay_crash(commits: usize, k: u64) -> Option<Vec<u8>> {
        let mem = MemoryBlockDevice::new(4096, 4096);
        let mut dev = FaultyBlockDevice::crash_after(mem, k);
        {
            // format itself may already crash; that is allowed (empty/invalid disk).
            if let Ok(mut fs) = EuroFs::format(&mut dev, [7u8; 16], 1) {
                for v in versions().iter().take(commits) {
                    let _ = fs.write_file("/data", v);
                }
            }
        }
        match EuroFs::mount(&mut dev, 2) {
            Ok(fs) => fs.read_file("/data").ok(),
            Err(_) => None,
        }
    }

    #[test]
    fn crash_at_every_write_point_keeps_a_consistent_checkpoint() {
        let all = versions();
        let m = all.len();
        let (bounds, total) = measure();
        // Valid checkpoint contents: index 0 = empty (None), index i+1 = version i.
        let content = |cp: usize| -> Option<Vec<u8>> {
            if cp == 0 {
                None
            } else {
                Some(all[cp - 1].to_vec())
            }
        };

        // Crash after EVERY write 0..=total and prove the invariants.
        for k in 0..=total {
            let recovered = replay_crash(m, k);

            // Last FULLY completed checkpoint at crash point k.
            let mut last_completed: isize = -1;
            for (cp, &b) in bounds.iter().enumerate() {
                if b <= k {
                    last_completed = cp as isize;
                }
            }

            // (1) INTEGRITY: the recovered result is never "garbage" — it is
            // absent or exactly one of the known checkpoint contents (no torn data).
            let valid_contents: Vec<Option<Vec<u8>>> = (0..=m).map(content).collect();
            assert!(
                valid_contents.contains(&recovered),
                "crash@{k}: recovered content {:?} is not a valid checkpoint",
                recovered.as_ref().map(|v| v.len())
            );

            if last_completed >= 0 {
                // (2) MOUNTABILITY: after a completed checkpoint the fs must mount.
                //     (last_completed==0 = empty fs ⇒ recovered None is valid.)
                // (3) DURABILITY: never roll back BELOW the last-completed
                //     checkpoint. Determine the index of the recovered checkpoint.
                let rec_cp = (0..=m).find(|&cp| content(cp) == recovered).unwrap();
                assert!(
                    rec_cp as isize >= last_completed,
                    "crash@{k}: rolled back to checkpoint {rec_cp} < last-completed {last_completed} (DURABILITY LOSS)"
                );
            }
        }
    }

    #[test]
    fn heavy_io_load_stays_consistent() {
        // J2: heavy I/O load — many files of varying size, repeatedly
        // overwritten — must stay intact: everything reads back exactly and a scrub
        // (XXH3 over superblock + inodes + data blocks) finds 0 errors.
        let mem = MemoryBlockDevice::new(16384, 4096);
        let mut dev = FaultyBlockDevice::new(mem);
        let mut fs = EuroFs::format(&mut dev, [1u8; 16], 1).unwrap();
        let n = 64usize;
        // Write n files of growing size (inline → multiple extents).
        for i in 0..n {
            let path = alloc::format!("/f{i}");
            let content: Vec<u8> = (0..(i * 37 + 1)).map(|b| (b ^ i) as u8).collect();
            fs.write_file(&path, &content).unwrap();
        }
        // Overwrite half (CoW pressure + block reuse).
        for i in (0..n).step_by(2) {
            let path = alloc::format!("/f{i}");
            let content: Vec<u8> = (0..(i * 53 + 7)).map(|b| (b.wrapping_mul(3) ^ i) as u8).collect();
            fs.write_file(&path, &content).unwrap();
        }
        // Read everything back and check exactly.
        let mut ok = true;
        for i in 0..n {
            let path = alloc::format!("/f{i}");
            let expect: Vec<u8> = if i % 2 == 0 {
                (0..(i * 53 + 7)).map(|b| (b.wrapping_mul(3) ^ i) as u8).collect()
            } else {
                (0..(i * 37 + 1)).map(|b| (b ^ i) as u8).collect()
            };
            ok &= fs.read_file(&path).unwrap() == expect;
        }
        assert!(ok, "a file did not read back exactly under load");
        // Scrub: integrity of the whole on-disk structure.
        let r = fs.scrub();
        assert!(r.superblock_ok, "superblock not OK after load");
        assert_eq!(r.errors, 0, "scrub found {} errors under load", r.errors);
        assert_eq!(r.data_unrecoverable, 0, "unrecoverable data under load");
        // Survives a remount (everything stays readable).
        drop(fs);
        let fs2 = EuroFs::mount(&mut dev, 2).unwrap();
        assert_eq!(fs2.read_file("/f1").unwrap(), (0..(1 * 37 + 1)).map(|b| (b ^ 1) as u8).collect::<Vec<u8>>());
    }

    #[test]
    fn torn_superblock_write_falls_back_to_previous_generation() {
        // Measure where the superblock writes of commit 3 fall, tear that one, and
        // prove that the mount falls back to an earlier, valid generation (the other slot).
        let all = versions();
        // Do format + 4 commits, tear at a late superblock write point.
        let mem = MemoryBlockDevice::new(4096, 4096);
        let mut probe = FaultyBlockDevice::new(mem);
        {
            let mut fs = EuroFs::format(&mut probe, [9u8; 16], 1).unwrap();
            for v in all.iter().take(4) {
                fs.write_file("/data", v).unwrap();
            }
        }
        let sb_ops = probe.sb_write_ops.clone();
        assert!(sb_ops.len() >= 2, "expected multiple superblock writes");
        let tear = *sb_ops.last().unwrap(); // the last superblock write (newest generation)

        let mem = MemoryBlockDevice::new(4096, 4096);
        let mut dev = FaultyBlockDevice::tear_at(mem, tear);
        {
            let mut fs = EuroFs::format(&mut dev, [9u8; 16], 1).unwrap();
            for v in all.iter().take(4) {
                let _ = fs.write_file("/data", v);
            }
        }
        // Mount MUST succeed (the other slot is valid) and return a REAL earlier version.
        let fs = EuroFs::mount(&mut dev, 2).expect("mount after torn superblock must succeed (A/B slot)");
        let recovered = fs.read_file("/data").ok();
        let valid: Vec<Option<Vec<u8>>> =
            core::iter::once(None).chain(all.iter().map(|v| Some(v.to_vec()))).collect();
        assert!(valid.contains(&recovered), "torn superblock gave garbage: {:?}", recovered);
    }
}
