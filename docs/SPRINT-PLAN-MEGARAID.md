# Sprint plan: a userspace MegaRAID driver (megaraidd)

Status: ACTIVE, started 2026-11-02 on `feature/udrv-megaraid`. Verified in QEMU
10.2 under KVM (Dell R630 test host) against `-device megasas-gen2`. Real
MegaRAID hardware: not driven. The Dell PERC H730 is a fusion controller and the
fusion path is **not built** (see "The real H730").

## Why

The owner asked for more drivers now that stronger hardware is available, and
the lab server is a Dell R630 whose disks sit behind a PERC H730 Mini. A RAID
controller is also a good second userspace driver after the e1000
(`SPRINT-PLAN-USERSPACE-DRIVERS.md`): it is a storage device (crash recovery has
to keep a filesystem consistent, not just bring a link back), it DMAs a lot
(frames, a reply queue, data), and a firmware-driven controller is exactly the
kind of complex, vendor-specific code that should not run in ring 0.

The root disk stays on the existing in-kernel drivers. MegaRAID is a second
storage path, driven only from ring 3: EuroOS has no in-kernel MegaRAID driver
and gets none.

## The hardware

Sources: QEMU `hw/scsi/megasas.c` and `hw/scsi/mfi.h` at v10.2.0 (`mfi.h` is the
BSD-licensed FreeBSD `mfireg.h`), FreeBSD `sys/dev/mfi` and `sys/dev/mrsas`
(BSD), Linux `megaraid_sas` (GPL, read for understanding only, nothing taken
over). Register offsets, state values, opcodes and structure layouts are
hardware facts and are named with their source in the code; no code is derived
from any of them, so there is nothing to add to NOTICE.

### Families

| PCI id | Chip | Interface | Here |
|---|---|---|---|
| `1000:0060` | SAS1078 | MFI, "ppc" registers | recognised, not driven (QEMU `megasas` could prove it later) |
| `1000:0078`, `1000:0079` | SAS2108 | **MFI gen2** | **driven** (QEMU `megasas-gen2` = `1000:0079`, "LSI MegaRAID SAS 9260-8i") |
| `1000:0071`, `1000:0073` | SAS2008 "skinny" | MFI skinny | recognised, not driven |
| `1000:005b` | SAS2208 Thunderbolt | **fusion** | recognised, left untouched |
| `1000:005d` | SAS3108 Invader (**Dell PERC H730/H730P/H830**) | **fusion** | recognised, left untouched |
| `1000:005f` | SAS3008 Fury | fusion | recognised, left untouched |

Correction to the brief: QEMU's `megasas-gen2` emulates the SAS2108
(`PCI_DEVICE_ID_LSI_SAS0079`), not the SAS2208. The 2208 is already a fusion
part. Both generations share the firmware handshake and the MFI frame and DCMD
formats; they differ in how commands are posted and how completions come back.

### MFI gen2, as driven here

- **Registers** (memory BAR, the first one: BAR 1 on gen2; 32-bit):
  `IDB 0x20` inbound doorbell, `OSTS 0x30` outbound interrupt status,
  `OMSK 0x34` outbound interrupt mask, `IQP 0x40` inbound queue port,
  `ODCR0 0xA0` outbound doorbell clear, `OSP0 0xB0` firmware status.
- **Firmware status word** (OSP0): state in bits 28..31 (`READY 0xB`,
  `OPERATIONAL 0xC`, `FAULT 0xF`, `WAIT_HANDSHAKE 0x6`, `BOOT_MSG_PENDING 0x9`,
  firmware-internal states in between), SGE limit in bits 16..23, command count
  in bits 0..15 (also the reply-ring length), MSI-X capable in bit 26.
- **Handshake to READY**: `WAIT_HANDSHAKE` -> IDB `CLEAR_HANDSHAKE|HOTPLUG`;
  `BOOT_MSG_PENDING` -> IDB `HOTPLUG`; `OPERATIONAL` (a previous driver) ->
  mask interrupts, IDB `READY|MFIMODE|ABORT`; every other state is firmware work
  that is waited out, 180 s per state; `FAULT` is an error.
- **Init frame**: command `0x00` with the address of a 32-byte queue-info block:
  flags (0 = 32-bit contexts), ring entries, ring address, producer-index and
  consumer-index addresses. Polled (status byte in the frame goes from `0xFF`
  to the result). Afterwards the firmware is `OPERATIONAL`.
- **Frames**: 64 bytes, 32-byte aligned, a 24-byte header (command, sense
  length, status, SCSI status, target, LUN, CDB length, SGE count, 64-bit
  context, flags, timeout, data length) and a body. Posted by writing
  `address | extra_frames << 1 | 1` to IQP (gen2: 32-bit address only).
- **DCMD** (command `0x05`): opcode + 12-byte mailbox + SGL.
  `CTRL_GET_INFO 0x01010000` (a 2 KiB block: PCI ids, firmware components,
  product name at `0x540`, serial at `0x590`, limits, LD/PD counts, package
  version at `0x640`), `LD_GET_LIST 0x03010000` (count + 16-byte entries:
  target, state, size in sectors), `CTRL_CACHE_FLUSH 0x01101000`.
- **LD I/O**: `LD_READ 0x01` / `LD_WRITE 0x02` with target, LBA, sense address
  and one SGL entry; `data_len` counts **sectors**. **SCSI pass-through** to an
  LD: `LD_SCSI_IO 0x03` with a CDB (used for READ CAPACITY(16)).
- **Reply queue**: a ring of contexts in host memory. The firmware writes the
  context of a finished frame at the producer index, advances it modulo the
  firmware's command count and writes the producer word; the host consumes up
  to it and writes its consumer word.
- **Interrupts** (gen2): unmask with `OMSK = ~0x5`; acknowledge by reading OSTS
  and writing the value to ODCR0. With MSI-X enabled, every completion raises
  vector 0.

### QEMU specifics found on the way

- `megasas-gen2`: BAR 0 is I/O, BAR 1 the 16 KiB register BAR (64-bit), BAR 3 an
  unused 256 KiB queue BAR. The **MSI-X table (0x2000) and PBA (0x3800) live in
  the register BAR**: the grant has to cut them off (below).
- The producer index only advances while the interrupt is unmasked; writing
  all-ones to OMSK, or `READY` to IDB, soft-resets the firmware (state READY,
  reply queue forgotten, outstanding requests cancelled).
- Every posted frame address is mapped with **2 KiB** behind it: frames must
  sit where 2 KiB after them is still device-visible (inside the window).
- The LD list comes back in reverse target order; the driver sorts by target id.
- Under an IOMMU, a data transfer whose buffer cannot be mapped is parked by
  QEMU's DMA helper and never completes, while DCMD data and frame accesses
  fail cleanly. The DMA probes below use the clean cases.
- A cancelled request may still be in flight in the block layer after a reset;
  a cache-flush DCMD drains it (`blk_drain_all`).
- OVMF places 64-bit BARs in a high aperture (with `-cpu host` at 56 TiB), above
  the 512 GiB EuroOS identity-maps. A grant there is refused (correctly). The
  proof boots with `-fw_cfg name=opt/ovmf/X-PciMmio64Mb,string=0`, which keeps
  them in the 32-bit hole, where the real H730's BARs are as well.

### Redox

Studied `redox-os/base` @ `6f881b09`: its storage drivers are `ahcid`, `ided`,
`nvmed`, `usbscsid`, `virtio-blkd` (+ `virtio-fsd`, `bcm2835-sdhcid`) over a
shared `driver-block` library. **Redox has no MegaRAID driver**; nothing to
compare against, nothing taken over.

## Design

### Shape

- `crates/euromegaraid` (host-tested, `no_std`, no alloc, `forbid(unsafe_code)`):
  `regs` (offsets, families, the queue-port value), `fw` (status word and the
  handshake state machine), `frame` (init, DCMD, LD I/O and pass-through frame
  builders), `info` (controller info, LD list, READ CAPACITY decoders; every
  field bounded, strings cleaned for the log), `layout` (the DMA window),
  `mfi` (bring-up, posting, the reply ring, synchronous DCMDs against a `Hal`
  trait), `grant` (the register window minus MSI-X), `blkif` (the block
  messages), and a simulated gen2 controller for the tests, modelled on QEMU's
  behaviour including its soft resets and with fault injection.
- `userland/megaraidd/` (Rust, `x86_64-unknown-none`, static PIE): the driver
  process. `Hal` with volatile accesses to the granted windows, the message
  loop. The only `unsafe` is those accesses and the syscall instruction.
- `kernel/src/udrv_mr.rs`: supervisor, grant, driver syscalls, MSI-X handler,
  block-port pump, quarantine, self-tests `[mr-*]`.
- `kernel/src/mrblk.rs`: **the thin block layer** (see below).

### Grant

`eurodrv::grant::DeviceGrant`, as for e1000d, with two storage-specific rules:

- **Registers without the MSI-X table**: `euromegaraid::grant::register_window`
  grants the BAR pages below the first MSI-X structure in that BAR. On
  `megasas-gen2` that is `0x0000..0x2000` of 16 KiB; on the real H730 it would
  be `0x0000..0xE000` of 64 KiB (table at `0xE000`, PBA at `0xF000`, read
  with `lspci -vv` on the test host). The kernel programs the table.
- **DMA window below 4 GiB**: gen2 posts frames through a 32-bit port. 69
  pages, laid out in `euromegaraid::layout`: 32 frames, queue info, producer
  and consumer words, a reply ring of up to 2048 contexts, 32 sense buffers, a
  4 KiB DCMD buffer and **four 64 KiB data slots**.

Under VT-d the device gets its own domain with exactly those 69 pages
(`iommu::attach_runtime`, domain `megaraidd`). Without an IOMMU the log says the
DMA is not confined.

### Interrupts

MSI-X entry 0 on a vector from the userspace-driver pool (`0x4C..=0x4F`,
`udrv::alloc_vector`; a fixed `0x4D` on the branch) on the BSP. The pool
vector's stub bumps EuroIPC notification slot 3 and sends EOI; the driver's next receive returns an `Irq` message. The
driver acknowledges OSTS/ODCR0 and reaps the ring. After posting, the driver
also watches the ring for a short bounded spin before it sleeps, so a fast
completion does not wait for the 10 ms tick.

### The block layer (the thin, swappable part)

`euromegaraid::blkif` + `kernel/src/mrblk.rs`. Messages carry no data: a request
names a data slot of the driver's window, the kernel copies write data into the
slot before `Write` and read data out after `Done`. Neither side passes a raw
address for data. Messages: `Disk` and `Ready` (driver -> kernel at bring-up),
`Read`/`Write`/`Flush` (kernel -> driver), `Done` (driver -> kernel), plus
`Irq`, `Crash`, `CrashDuringWrite`, `DmaProbe`, `Report` for the self-tests. The
driver validates every request (slot index, size <= slot, LBA range) and answers
an invalid one with `Done` + an error status.

`mrblk::MrBlock` is a `eurofs::BlockDevice` (4 KiB blocks) per logical drive;
`mrblk::{read, write, flush}` work in 512-byte sectors. A transfer is cut into
64 KiB chunks, up to four in flight. When the lead merges this with the storage
team's block-over-IPC interface, `mrblk.rs` and `blkif.rs` are the two files to
replace; the MFI logic, the driver loop's MFI side and the supervisor stay.

### Crash and restart

`udrv_mr::supervise` (desktop loop, and from inside a block wait on task 0):

1. Notice the dead process (the ring-3 fault path marks it).
2. Quiesce: bus mastering off (no DMA, no MSI-X), the IOMMU domain back to the
   empty table, the driver port dropped, the process reaped.
3. **Quarantine** the old DMA window instead of freeing it: a controller that
   still holds addresses into it could otherwise write into memory that has
   been handed to someone else once bus mastering is on again (without an
   IOMMU). It is freed when the next instance says `Ready`, i.e. after that
   instance has reset the controller.
4. Restart per `eurodrv::restart::Policy` (5 deaths per minute, then give up and
   leave the device off).

The new instance: handshake (`OPERATIONAL` -> mask, `READY|ABORT` -> `READY`),
zero the queue memory, init frame with a fresh reply queue, a **new context
generation** (late completions of the previous instance are recognised and
ignored: every context carries the generation, names an in-flight frame, and
that frame's status byte must no longer read pending), a cache flush that
drains whatever the old instance left in the controller, then `Disk` + `Ready`.

The block layer **replays** every chunk that was sent to the dead instance and
not acknowledged: block writes are idempotent, the write data is copied in again
from the caller's buffer. With replay switched off (one self-test), the caller
gets an I/O error instead; EuroFS's copy-on-write then has to keep the
filesystem consistent on its own.

## Steps and proofs

| Step | Host tests | Boot proof |
|---|---|---|
| 1. Study + plan | | this document |
| 2. MFI core against a simulated controller | `euromegaraid` (handshake incl. fault, timeout, flapping; frame layouts; info/LD-list decoding with hostile input; ring wrap at the firmware's command count; stray and stale contexts; frames running out; restart with a new generation; DMA probes outside the window) | |
| 3. Grant, IOMMU domain, driver up | `grant` (QEMU and H730 layouts, refusals) | `[mr-grant]`, `[mr-iommu]` |
| 4. Controller info + LD list | | `[mr-info]` + the `[megaraidd]` lines |
| 5. Read/write with data verification | | `[mr-io]` + host check of `ld0.raw` |
| 6. MSI-X -> EuroIPC | | `[mr-irq]` |
| 7. EuroFS on the second LD, across a reboot | | `[mr-fs]` (boot 1 writes, boot 2 reads back) + host check of `ld1.raw` |
| 8. Driver crash mid-write | | `[mr-crash]` (with replay; and without) |
| 9. IOMMU fault for DMA outside the domain | | `[mr-dma]` |
| 10. Docs | | STATUS, HARDWARE-COMPAT, deep reference |

QEMU line (`scripts/megaraid-proof.sh` runs both boots and the host check):
`-machine q35 -m 1024M -smp 4 -enable-kvm -cpu host -device intel-iommu,intremap=off
-fw_cfg name=opt/ovmf/X-PciMmio64Mb,string=0 -device megasas-gen2,id=mr0
-drive if=none,id=ld0,file=ld0.raw,format=raw -device scsi-hd,bus=mr0.0,drive=ld0,scsi-id=0
-drive if=none,id=ld1,file=ld1.raw,format=raw -device scsi-hd,bus=mr0.0,drive=ld1,scsi-id=1`.

The destructive self-tests only run in `selftest` builds and only on a disk that
is blank where they write or carries their own mark (LD0 sector 0), or for LD1,
already holds EuroFS or is blank. Any other disk is left untouched and the line
says so.

## Status per step

| Step | Status | Proof |
|---|---|---|
| 1. Study + plan | ✅ | this document |
| 2. MFI core, simulated controller | ✅ host | `cargo test -p euromegaraid` |
| 3. Grant, own IOMMU domain, driver up | ✅ QEMU | `[mr-grant]`, `[mr-iommu]` |
| 4. Controller info + LD list (DCMD) + READ CAPACITY(16) (pass-through) | ✅ QEMU | `[mr-info]` |
| 5. LD read/write, data verified (also from the host) | ✅ QEMU | `[mr-io]`, `host-check.txt` |
| 6. MSI-X as EuroIPC messages | ✅ QEMU | `[mr-irq]` |
| 7. EuroFS on LD1, written, rebooted, read back | ✅ QEMU | `[mr-fs]` boot 1 + boot 2 |
| 8. Crash mid-write, restart, filesystem consistent | ✅ QEMU | `[mr-crash]` x2 |
| 9. IOMMU fault for DMA outside the domain | ✅ QEMU with `intel-iommu` · ⬜ real VT-d | `[mr-dma]` |
| Real MegaRAID gen2 hardware | ⬜ | no such card in the lab |
| Dell PERC H730 (fusion) | ⬜ not built | see below |

## The real H730 (fusion): what is missing

The H730 on the test host is `1000:005d` (SAS3108 Invader), subsystem "Dell PERC
H730 Mini", BAR 1 = 64 KiB registers at `0x91d00000` (below 4 GiB), BAR 3 =
1 MiB, MSI-X with 97 vectors, table at BAR 1 `+0xE000`, PBA at `+0xF000`. It is
the host's root disk and was only ever read with `lspci`; nothing was written to
it, it was not unbound and not passed through. megaraidd recognises the family
and refuses it; the kernel logs it as "left untouched".

What carries over unchanged: the PCI grant with the MSI-X cut (host test
`dell_perc_h730_layout`), the firmware status word and the handshake states
(fusion reads them from the same `0xB0` scratch pad), the MFI frame and DCMD
formats (fusion still runs DCMDs as MFI frames), the info/LD-list decoders, the
whole supervisor (quarantine, generation contexts, drain, replay), `blkif` and
`mrblk`.

What the fusion path needs, and why it is not built: it cannot be tested here.
QEMU has no fusion emulation (`megasas.c` is MFI only), and the only fusion
controller in reach is the host's root disk, which must not be touched. Building
it untested would put code that has never run in front of a RAID controller
holding someone's data. Concretely (from FreeBSD `mrsas`):

1. Scratch pads 2..4 (`0xB4..0xBC`): chain-frame size, extended-MSI-X and
   capability bits read before init.
2. Memory: an MPI2 request-frame pool (256-byte frames, one per command), a
   chain-frame pool, a **reply-descriptor post queue** (8-byte descriptors,
   depth a multiple of 16, all bytes 0xFF when unused), sense buffers.
3. **IOC INIT**: an MPI2 `IOC_INIT` request (`Function 0x02`, frame size, reply
   queue depth and address, request-frame base, MSI-X count) wrapped in an MFI
   init frame, posted as a 64-bit **request descriptor** to `0xC0/0xC4` (low
   then high) with type `MFA`; polled on the MFI frame's status.
4. Commands become request descriptors (SMID = command index): DCMDs through
   an MFI pass-through descriptor, LD I/O as MPI2 SCSI-IO requests with a RAID
   context (`MRSAS_REQ_DESCRIPT_FLAGS_LD_IO`).
5. Completions: walk the reply-descriptor queue from the last index until a
   descriptor reads all ones, map SMID back to the command, write each consumed
   descriptor back to 0xFF, then the reply-post host index (`0x6C`); per MSI-X
   vector on multi-queue parts.
6. Optional later: the RAID map (`LD_MAP_GET_INFO 0x0300e101`) for fast-path I/O
   straight to the physical disks; online controller reset (OCR) handling.

How it could be proven without risking data: a second, spare PERC (or a
spare MegaRAID 9361/9271) in a lab machine, passed through with VFIO to a VM,
with EuroOS writing only to a scratch virtual disk created on it.

## Honest limits

- One controller generation (MFI gen2), one emulation, no real MegaRAID card.
- QEMU needs `X-PciMmio64Mb=0` (or a BAR below 512 GiB some other way): grants
  above the identity map are refused rather than mapped.
- No interrupt remapping (`intremap=off`); the MSI-X table is kernel-only,
  which is the mitigation, not a proof.
- Latency: the driver sleeps one 10 ms tick when it has nothing to do, and the
  kernel side waits the same way; 1 MiB writes in about 350 ms. Correctness
  first; this is not a performance driver yet.
- The block layer is synchronous per caller (up to four 64 KiB chunks in
  flight), task context with interrupts on; a caller with interrupts off gets
  an I/O error instead of a hang (the same limit the e1000 sprint documents for
  blocking syscalls). The drives are not mounted into the global VFS yet.
- 512-byte sectors only (a 4Kn LD is reported and not exported); 32-bit
  contexts; one MSI-X vector.
- Replay relies on the new instance's controller reset and drain; on QEMU a
  cancelled request is drained by the cache flush, on real firmware the reset
  to READY with ABORT completes or aborts outstanding commands first.
- Both userspace drivers' supervisors call `ring3::reap_dead`, which reaps every
  dead process; with both active, one of them can find the other's process
  already reaped (death detection is unaffected, only the logged reason).

## Shared files touched

`Cargo.toml` (workspace members), `Cargo.lock`, `kernel/Cargo.toml` (dependency),
`kernel/src/main.rs` (two `mod` lines, `udrv_mr::boot` after `udrv::boot`,
`udrv_mr::supervise` in the desktop loop), `kernel/src/interrupts.rs` (vector
`0x4D` + IDT entry), `kernel/src/ring3.rs` (the `driver` profile routes to
`udrv_mr::syscall` first, then `udrv::syscall`), `userland/build.sh` (build and
sign `megaraidd.elf`), `.gitignore` (`megaraidd.elf.sig`). No change to
`udrv.rs`, `eurodrv`, `iommu.rs`, `euroipc.rs` or `eurosandbox`.

Since the merge with nvmed and bcm5720d (`feature/integrate-drivers`) the
vector comes from the pool in `udrv.rs`, the ports (`0x4E60`/`0x4E70`), the
notification slot (3) and the pid range from `udrv::RES`, and `ring3.rs`
calls only `udrv::syscall`, which knows megaraidd from its grant and speaks
`blkif` to it; `udrv_mr::syscall` is gone. The block layer stays `blkif`
(not nvmed's interface; the comparison is in
`docs/SPRINT-PLAN-USERSPACE-STORAGE.md`, "Two block interfaces").

## Log

- 2026-11-02: hardware study (QEMU `megasas.c`/`mfi.h` v10.2.0, FreeBSD `mfi` and
  `mrsas`, Redox `base` @ `6f881b09`); the H730's PCI layout read with `lspci`
  on the test host.
- 2026-11-02: built and verified in QEMU 10.2 under KVM on the R630, `-m 1024M
  -smp 4`, two boots on the same disk images (`scripts/megaraid-proof.sh`).
  Logs: `docs/proof/megaraid-2026-11-02/`. Abridged, boot 1 with
  `-device intel-iommu,intremap=off`:

  ```
  [mr] 00:03.0 1000:0079 MegaRAID MFI gen2 (SAS2108): BAR1 0x81100000+0x4000, MSI-X table at BAR1 +0x2000 -> grant 0x81100000+0x2000
  [iommu] runtime domain megaraidd (id 8): 00:03.0 -> 69 page(s) at 0xb780000, nothing else
  [megaraidd pid 135168] firmware: status 0xb45003f0 -> READY -> init frame OK -> OPERATIONAL in 0 ms; 1008 commands, 80 SGEs, reply ring 1008 entries, generation 0x5a69
  [megaraidd pid 135168] controller: 'LSI MegaRAID SAS 9260-8i' 1000:0079 (sub 1000:9261), serial 'QEMU123456', firmware package '2.5+-QEMU', APP '1.80-QEMU'
  [mr-grant] ... next page (MSI-X table)=false, page before=false; DMA window 69 pages mapped=true, page after=false; LAPIC=false, kernel code=false → OK (exactly the granted ranges) ✓
  [mr-iommu] device 00:03.0 has its own VT-d domain 'megaraidd' with 69 page(s) = the DMA window, nothing else → OK ✓
  [mr-info] ... LD0=target 0 131072 sectors (64 MiB) optimal, LD1=target 1 524288 sectors (256 MiB) optimal → OK ✓
  [mr-io] LD0 (blank, marked for the self-test): 1 sector write/read equal · 1 MiB at LBA 4096 ... byte-for-byte equal · cache flush ok · read past the end refused=true → OK ✓
  [mr-irq] MSI-X vector 0x4d: 58 interrupt(s) raised by reply-queue completions, 58 delivered to megaraidd as EuroIPC Irq messages → OK ✓
  [mr-fs] wrote generation 1 (...): written=true · remounted from disk, read back equal=true · scrub: 5 objects, 0 errors, superblock ok=true → OK ✓
  [megaraidd pid 135168] crash requested by the self-test: faulting with write #3 (tag 307) in flight at the controller
  [mr] megaraidd pid 135168 died (memory isolation: access 0x1000 denied) -> device quiesced: bus-master off, IOMMU domain revoked, DMA window quarantined ...; restart at tick 1520 (death #1)
  [megaraidd pid 135169] firmware: status 0xc45003f0 -> READY -> init frame OK -> OPERATIONAL ...
  [mr-crash] megaraidd pid 135168 killed by a real fault ... restarted as pid 135169; 1 request(s) replayed; 6/6 files written, after a remount 6/6 read back equal; scrub: 11 objects, 0 errors ... → OK ✓
  [mr-crash] without replay: ... write_file returned Some(IoError); driver back=true; remount: earlier files 6/6 equal, /mr/lost.bin absent (the old checkpoint); scrub: ... 0 errors ... → OK ✓
  [iommu] DMA FAULT blocked: device 00:03.0 tried to write physical 0x174000 (reason 0x5) — outside its domain
  [iommu] DMA FAULT blocked: device 00:03.0 tried to read physical 0x175000 (reason 0x6) — outside its domain
  [mr-dma] malicious megaraidd: ... canary intact=true; ... page intact=true, 1 stray completion(s) ignored · driver still works=true → OK ✓
  ```

  Boot 2 on the same disks: `[mr-fs] EuroFS on LD1 mounted from disk: generation
  1 written by the previous boot; /mr/gen-1.bin 196608 bytes, content matches
  ... → OK (persisted across the reboot) ✓`. Host side, after boot 2: the self-test
  mark in sector 0 and the 1 MiB pattern at LBA 4096 of `ld0.raw` match, the
  text of `/mr/hello.txt` and the EuroFS magic are on `ld1.raw`.
  Also run: the same two boots without `-device intel-iommu` (everything passes,
  `[mr-iommu]`/`[mr-dma]` report "not confined" ⬜), and one boot with
  `net.driver=user`, where `e1000d` and `megaraidd` run side by side and every
  `[udrv-*]` and `[mr-*]` marker passes. The rest of the dev self-test suite
  reports the same set of results as an image built from the base commit
  `813e627` on the same machine with the same controller attached; the few
  markers that fail on this host fail in both runs and are unrelated (the tests
  that depend on pre-signed artifacts, and the glibc churn/app-policy tests).
- 2026-11-02, merged with nvmed and bcm5720d (`feature/integrate-drivers`):
  `scripts/megaraid-proof.sh` again, now under TCG on the build server, with
  and without `intel-iommu`: per-marker verdicts identical to the KVM proof
  above, and the host-side check of both images passes. One TCG boot (with
  `-smp 4` and with `-smp 1`) with e1000d, nvmed and megaraidd together, and on
  the R630 (KVM) one boot with all four userspace drivers: every `[mr-*]`
  marker OK, megaraidd on its own vector from the pool. Logs:
  `docs/proof/integrate-drivers-2026-11-02/`.
