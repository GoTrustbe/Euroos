# EuroOS — Storage Interoperability Sprint Plan (external & network filesystems)

*A sovereign OS still has to **read the world's data**: USB sticks formatted FAT/exFAT,
a NAS over SMB, a Unix server over NFS. Today EuroOS can mount only its own EuroFS
volumes — so it's an island. This plan adds a real **mount framework** and a series of
**foreign filesystem drivers**, foundation-up. Created 2026-06-14. Builds on the
forward plan (Sprints 1–5 ✅, see [`SPRINT-PLAN-FORWARD.md`](SPRINT-PLAN-FORWARD.md)).*

Conventions: `🔒` security-sensitive · `🏗️` large/multi-component · `🧪` host-testable.
Definition of done (unchanged): **host-tested core → thin kernel glue → `[xx]` boot
self-test → docs/status updated → honest label. Never fake-as-real.**

---

## Where we are (honest baseline)

- **VFS** (`crates/eurofs/src/vfs.rs:48`): `mount(point, Box<dyn FileSystem>)` /
  `umount` / `mount_points`, longest-prefix routing. **Any** type that implements the
  `FileSystem` trait can already be mounted at a path — this is the clean extension point.
- **`FileSystem` trait** (`crates/eurofs/src/fs.rs`): required = `read_file`,
  `write_file`, `remove_file`, `create_dir`, `list_dir`, `exists`, `metadata`,
  `space_info`. The rest (`rename`/flags/snapshots/`scrub`/`df`) have default impls, so a
  **read-only** foreign FS can implement the readers and return `Unsupported` for writers.
- **Block I/O**: `virtio_blk::{read_io_dev,write_io_dev}` (per-device sectors);
  `eurofs::BlockDevice` is the abstraction a driver reads through; `rootblk::RootBlk`
  already wraps a virtio device + partition window as a `BlockDevice`.
- **FAT**: `eurofat` has a FAT32 *builder* + a *sectored* small-root-file reader
  (`sectored.rs`: BPB parse, FAT-chain step, LFN dir parse). A full mountable reader is a
  bounded extension of this; FAT *write* is more (cluster allocation + dir mutation).
- **USB**: `xhci::usb_read_block` reads sectors off a USB mass-storage device
  (boot-verified). **No USB block *write* yet** → USB mounts start read-only.
- **Partitions**: `gpt.rs` finds EuroFS partitions by type GUID; foreign disks need
  generic partition enumeration + FS-type detection (FAT BPB signature, etc.).
- **Network**: `euronet` TCP (`TcpConn`) + `eurotls` 1.3 are the transport SMB/NFS ride on.
- **Not present:** any `mount`/`umount`/`lsblk` shell command, any FAT/exFAT/NTFS reader
  mounted into the VFS, any SMB/CIFS or NFS client, any standalone `format`/`mkfs`.

---

> **Progress 2026-06-14:** **IO-1 ✅ done + verified** and **IO-2 ✅ core done + verified.**
> New crate `eurofatfs` (FAT32 `FileSystem` over a 512-byte `BlockDevice`) — read **and**
> write — with **9 host tests cross-validated against the independent `eurofat` reader**
> (so the on-disk format is genuinely valid FAT32). Kernel `fatmount.rs`: `SectorDev`
> (virtio→512-byte block device), partition enumeration (`gpt::all_partitions_on`),
> FS-type detection, and `mount`/`umount`/`lsblk` shell commands wired into the VFS via
> three new `FileSystem` trait methods (`mount_fs`/`umount_fs`/`list_mounts`, overridden
> by `Vfs`). Boot-verified in the kernel (no_std): `[io1]` mount+read, `[io2]`
> create/mkdir/nested/overwrite-grow/remove. `xhci::usb_write_block` added (SCSI WRITE(10)).
> **Honest remainders:** live mount of a *real* FAT virtio/USB partition + removable-media
> **auto-mount** are coded but need a dedicated FAT-disk / usb-storage harness to verify
> end-to-end (the driver + plumbing are proven; the last mile is harness work).
>
> **IO-3 ✅ done + verified (2026-06-14):** RAM-light streaming FAT32 formatter
> `eurofat::format_fat32` (size-based cluster size; writes only BPB/FSInfo/FAT/root via
> sector callbacks). **Cross-validated with real tools:** `fsck.fat -n -v` reads it clean
> (129008 clusters, 0 errors) and `mtools` mounts it + copies files in. Kernel `format
> <devN> [--fs fat32|eurofs] [--label L] [--force]` command (FAT32 via the formatter,
> EuroFS via `EuroFs::format`), with a `--force` erase guard; `mount` extended to also
> mount native EuroFS data volumes. `[io3]` boot self-test (format RAM volume → mount →
> write+read).
>
> **IO-4 ✅ read done + verified (2026-06-14):** new crate `euroexfat` — a read-only exFAT
> `FileSystem` (boot region, FAT + NoFatChain contiguous files, 0x85/0xC0/0xC1 entry sets,
> long names). **Host-tested against a real `mkfs.exfat` image** (`fsck.exfat`-clean
> fixture): reads files (incl. multi-cluster + subdirectories + long names with spaces).
> Kernel `mount`/`lsblk` detect + mount exFAT (read-only). exFAT **write deferred**
> (bitmap + entry-set management). 744 host tests.
>
> **IO-5 ✅ done + verified (2026-06-14):** new crate `eurosmb` — a from-scratch SMB2/3
> client: NEGOTIATE → SESSION_SETUP (**NTLMv2**) → TREE_CONNECT → CREATE / QUERY_DIRECTORY
> / READ / WRITE / CLOSE, over a transport-agnostic `Transport` (TCP). Own **MD4 + MD5 +
> HMAC-MD5** (RFC-vector tested) + NTLMv2 (MS-NLMP NTOWFv2 vector). **Verified against a
> real Samba server**: the host example authenticates, lists the share, reads root + nested
> files, and writes + reads back a file. `SmbFs` wraps it as a `FileSystem`. Kernel
> `smbfs.rs`: a `TcpConn`-backed transport + `mount //<ip>/<share> <point> [user] [pass]`.
> **`[io5]` boot-verified end-to-end**: the kernel mounts the build host's Samba over the
> live NIC (SLIRP 10.0.2.2:445) and reads a file — real SMB2+NTLMv2 from the kernel. SMB3
> encryption / Kerberos / signing are deferred. 750 host tests.
>
> **IO-6 ✅ done + verified (2026-06-14):** new crate `euronfs` — an NFSv3 client: ONC RPC
> (RFC 1057) + XDR over TCP, AUTH_UNIX (no crypto), portmap GETPORT → MOUNT MNT → NFS
> LOOKUP / READ / READDIR / WRITE / CREATE. `NfsFs` wraps it as a `FileSystem`. **Verified
> against a real Linux `nfsd`**: the host example mounts the export, lists it, reads root +
> nested files, and writes + reads back. Kernel `nfsmount.rs`: a `Connector` over `TcpConn`
> + `mount nfs://<ip>/<export> <point>`. **`[io6]` boot-verified end-to-end** over the live
> NIC (SLIRP → host nfsd): mounted, listed, read a file. NFSv4 / Kerberos deferred.
>
> **Multi-disk load + functional testing (2026-06-14):** `disktest` kernel module +
> `run-disktest.py` harness — boots with virtio disks of varying sizes and, on each,
> formats → fills (to a cap or until full) → verifies → deletes → reformats, then copies a
> file between two disks, reporting RTC wall-clock timing/throughput.
> **Result across 8 MiB → 2 GiB: all disks PASS** (format, fill, true DISK-FULL/NoSpace on
> the 8 MiB disk, first+last read-back verified, delete→empty, reformat→empty) and the
> cross-disk 1 MiB copy verifies byte-for-byte. Large disks (4 KiB clusters, `spc=8`) fill
> ~5× faster than 512 B-cluster disks (≈4096 vs ≈800 KiB/s wall-clock, TCG).
> **The test paid for itself:** it found (a) a **data-loss bug** — a multi-cluster
> directory left a `0x00` gap on extension so files past the first cluster became
> unreadable (`reserve_dir_slots` now reserves a contiguous run that may span the cluster
> boundary; regression-tested), and (b) an **O(n²) allocator** (fixed with an O(1)
> next-free cursor). Max FAT32 volume ≈ 2 TiB (u32 sector count).
>
> **Big load / stress test (2026-06-14):** `stresstest` kernel module + `run-stresstest.py`
> harness — armed by the `EUROSTRESS` sentinel on disk 0, it runs LATE in boot (after
> ring3 / interrupts / VFS are up) with the **root on a real on-disk EuroFS partition**
> (disk 0; disk 1 = `/mnt`; disk 2+ = free scratch). Phases: (A) write/rename/delete/rewrite
> **churn** on every free disk with read-back integrity checks; (B) **cross-disk move**
> (copy disk→disk + delete source); (C) fill the **on-disk root filesystem until full** —
> the genuine "boot disk is full" case, not a RAM disk — verifying an existing file survives
> and the FS is writable again after freeing; (D) run **multiple programs** (repeated
> synchronous `/bin/hello` runs + 2 concurrent background tasks) with a **frame-leak check**
> and a final root **scrub**. **All phases PASS, no frame leak, scrub 0 errors.** The test
> earned its keep twice: it caught the wiring hazard of churning the *live root disk* (now
> disk 0/1 are reserved) and a wrong-ABI program-launch hang (Linux-ABI `forktest`/`execee`
> must not be run as native standalone — `fork`/`exec` are proven separately at boot).

## IO-1 — Mount framework + read-only FAT32 driver `🏗️🧪` ✅ *(done — see progress note above)*
**Goal.** Plug in a FAT32 disk/partition and read its files.
- **Generic block enumeration:** a `BlockDevice` wrapper over `virtio_blk` *and* the USB
  block path; a partition scanner that reads the GPT/MBR and reports each partition's
  (device, first-LBA, size, detected FS) → backs a `lsblk`/`blkid` command.
- **`eurofatfs` read driver:** a `FileSystem` impl over a `BlockDevice` — BPB geometry,
  FAT-chain traversal, **subdirectory** descent, **arbitrary file read across clusters**,
  LFN names (reuse `eurofat::sectored` parsing). Writers return `Unsupported` for now.
- **Shell:** `mount <dev|partN> <mountpoint>` → `vfs.mount(point, Box::new(eurofatfs))`;
  `umount <point>`; `lsblk`.
- **Done:** a QEMU disk carrying a FAT32 partition (populated on the host with
  `mkfs.fat` + `mtools`) is mounted read-only; `ls`/`cat` show the real files
  (`[io1]` self-test + a `run-fatmount.py` harness). Host-test the FAT reader against a
  `mkfs.fat`-built image.

## IO-2 — FAT32 write + USB block write + removable media `🏗️`
**Goal.** Read/write a USB stick, like a normal desktop.
- **FAT32 write:** free-cluster allocation, FAT-chain extend/truncate, directory-entry
  create/update/delete, file create/write/remove, FSInfo upkeep. (Host-test by reading
  the result back with `mtools` for byte-equality.)
- **USB block write:** `usb_write_block` via the xHCI bulk-OUT path + SCSI `WRITE(10)`
  (mirrors the existing `READ(10)`).
- **Removable media:** detect a USB mass-storage device, scan its partitions, auto-mount
  the first FAT volume at `/media/usb` (and `umount` cleanly).
- **Done:** write a file on a mounted FAT USB stick, verify it on the host (`mtools`);
  hotplug-mount boot-verified (`[io2]`).

## IO-3 — `format` / `mkfs` command `🔒`
**Goal.** Prepare a fresh data drive without doing a full OS install.
- **`format <dev> --fs eurofs|fat32 [--label L] [--gpt]`:** EuroFS via the existing
  `EuroFs::format`; FAT32 via the `eurofat` builder generalized for a **data** volume
  (not just an ESP). Optional GPT partitioning of a blank disk.
- **Guards:** refuse a non-blank disk without `--force`; clear, auditable confirmation
  (this erases data) — reuse the `euroinstall` safety pattern.
- **Done:** format a blank virtio disk → mount → write/read round-trip → `fsck` clean
  (`[io3]`).

## IO-4 — exFAT read (+ write) `🏗️`
**Goal.** Large media (>32 GB USB/SD ship exFAT).
- A separate `euroexfat` driver: exFAT boot region, the exFAT FAT, the **directory entry
  set** (file + stream-extension + name entries), the allocation bitmap, up-case table.
  Read first; write as a follow-on.
- **Done:** mount an exFAT image (host `mkfs.exfat`) read-only; `ls`/`cat` match
  (`[io4]`). Honest: exFAT is a distinct, more complex on-disk format than FAT32.

## IO-5 — SMB/CIFS client (network shares) `🏗️🔒` *(the marquee feature)*
**Goal.** Mount a NAS / Windows / Samba share.
- **SMB2/3 over TCP 445** on `euronet`: `NEGOTIATE` → `SESSION_SETUP` → `TREE_CONNECT` →
  `CREATE`/`READ`/`WRITE`/`QUERY_DIRECTORY`/`CLOSE`, surfaced as a `FileSystem` impl
  mountable into the VFS (`mount //host/share /mnt -o user=…`).
- **Auth:** NTLMv2 (needs MD4 + HMAC-MD5 + the NTLM message structs) for user/password,
  plus anonymous/guest for open shares. SMB3 encryption (AES-CCM/GCM) and Kerberos are
  later add-ons.
- **Testing:** a local Samba (or Python `impacket`) server in the sandbox exports a share;
  the client mounts it and round-trips a file.
- **Done:** `mount //127.0.0.1/euro /mnt` → `ls /mnt` + `cat` + write a file the server
  sees (`[io5]`). Honest staging: read path first, then write, then SMB3 encryption.

> **IO-7 ✅ ext2/3/4 read done + verified (2026-06-14):** new crate `euroext` — one
> read-only driver covering ext2/ext3/ext4: superblock + block-group descriptors, inodes,
> **ext4 extent trees** *and* classic direct/indirect block pointers (ext2/3), linear
> directory entries. **Verified against a real `mkfs.ext4` image** (extents, 1 KiB blocks):
> reads root + nested files, a multi-block file via the extent tree, long names, and
> directory listings. Kernel `extmount.rs` detects ext (magic 0xEF53 at byte 1024) and
> `mount`/`lsblk` mount it read-only; **`[io7]` boot-verified** against an ext4 virtio
> disk. ext write deferred (jbd2 journal). btrfs/xfs/NTFS remain parked.

## IO-6 — NFS client (Unix-native shares) `🏗️`
**Goal.** Mount an NFS export.
- ONC RPC + **NFSv3** (portmap/mount protocol + NFS proc) over TCP, as a VFS-mountable
  `FileSystem`. (NFSv4 — stateful, integrated mount — as a later option.)
- **Done:** mount a local `nfsd`/Python NFS export, `ls`/`cat`/write round-trip (`[io6]`).

---

## Cross-cutting: a `MountManager`
A kernel module tying it together: device/partition enumeration, the `mount`/`umount`/
`lsblk`/`blkid` shell commands, FS-type auto-detection, and (IO-2) hotplug auto-mount.
The VFS already does the path routing; this is the discovery + lifecycle layer above it.

## Deferred (explicit — not this horizon)
- **NTFS** (very complex; read-only at best) · **SMB3 encryption + Kerberos** ·
  **NFSv4 / Kerberos (`sec=krb5`)** · **write-back caching** for network FS ·
  a **FUSE-style** userspace-filesystem API.

## Recommended order
**IO-1 first** (mount framework + FAT32 read — unlocks everything and is self-contained),
then **IO-2 + IO-3** (make removable media + formatting real), then **IO-5 (SMB)** as the
headline network feature, with **IO-4 (exFAT)** / **IO-6 (NFS)** by appetite. Every step
lands a `[xx]` boot self-test and a small harness; the VFS + `FileSystem` trait mean each
new driver is additive and can't destabilize EuroFS.
