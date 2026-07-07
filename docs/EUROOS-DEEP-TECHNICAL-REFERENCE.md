# EuroOS — Deep Technical Reference

*A complete, code-grounded description of every subsystem in EuroOS: what it does, how it works, the data structures and algorithms behind it, the on-disk and on-the-wire formats, and exactly what is implemented and verified versus what is a stub or a planned next step.*

**Version of record:** build `2026.06.08` · 57 library crates + the kernel · ~63 000 lines of `no_std` Rust · **793 host tests (current)**.
**Source tree:** `/home/user/eurokernel`. **Companion docs:** `docs/TECHNICAL-OVERVIEW.md` (condensed), `docs/ROADMAP.md` (plan), `docs/SECURITY-AUDIT.md`.

---

## 0. How to read this document

EuroOS is a **from-scratch, sovereign operating system written in `no_std` Rust** for x86-64 UEFI machines. It is **not** a Linux distribution and **not** based on BSD. It has its own kernel, memory manager, scheduler, filesystem, network stack, TLS implementation, cryptography, capability security model, display server, desktop, package format, and identity system. The Linux/POSIX syscall ABI exists only as an **opt-in compatibility bridge** (EuroCompat, inside `kernel/src/ring3.rs`) so that software compiled for musl/`x86_64-linux` can run — it is a guest on the system, never the system's identity. Where this document says "EuroGuard capability," "EuroFS," "EuroID," it means the native sovereign subsystem, not a Linux analogue.

This reference is organised into six **Parts**, each covering one cluster of subsystems, followed by an **Appendix** with the consolidated boot self-test index, a global honesty matrix, build/run instructions, and a glossary:

- **Part 1 — Boot, Core Kernel & Hardware Enumeration**: the UEFI loader, boot sequence, memory & paging, CPU init (GDT/IDT/APIC/HPET), SMP, the scheduler & processes, IPC, power/init/logging, and hardware discovery (PCI/ACPI/AML/device model/USB).
- **Part 2 — Storage & Filesystem**: EuroFS (copy-on-write, A/B superblock, snapshots, scrub, immutability), GPT, NVMe & virtio-blk drivers, the block cache, full-disk encryption, atomic A/B system updates, fault-driven swap, and the on-disk audit trail.
- **Part 3 — Networking, Secure Transport & PKI/Crypto**: EuroNet (Ethernet→TCP, DNS, sockets, HTTP), EuroTLS 1.3, the firewall, the sovereign VPN, WiFi protocol core, the TPM stack, the local CA, remote attestation, and document signing.
- **Part 4 — Security Model, Identity & Observability**: the EuroGuard capability model, EuroPol policy engine, EuroVault secrets store, EuroID (Sprint K1 user management with from-scratch Argon2id), EuroIDM tokens, EuroSandbox containers, the legacy login path, metrics, health, and crash dumps.
- **Part 5 — Display, Desktop, Input, Audio, Accessibility & Print**: the software framebuffer, the compositor & dirty-rect desktop loop, the native virtio-GPU driver, the Wayland protocol layer, font/text rendering, the EuroOS Design System, PS/2 + USB-HID input, the HD-Audio driver, the accessibility layer, and IPP printing.
- **Part 6 — Userland Runtimes, Agents, Localisation, Packaging, Web, Office & Apps**: the Linux ABI bridge, EuroCoreutils, the WASM/WASI runtime, EuroAgent, EuroLocale (24 languages), the installer, the package manager, reproducible builds, the EuroWeb browser engine + EuroJS, the EuroSuite office engines, and the EuroApps.

### Reading conventions

- **`file:line` citations** point into the real source tree (e.g. `paging.rs:465`); they are the ground truth for every claim.
- **`[xx]` markers** (e.g. `[k1]`, `[bb2]`, `[aa]`) are the serial-console self-test lines the kernel prints **during boot**. Each subsystem proves itself live at boot by emitting one of these; the Appendix lists them all. They are the primary "this actually works" evidence, alongside the host-run `cargo test` suite.
- **Honesty labels.** EuroOS has a hard project rule against presenting demos or mocks as finished software. This document therefore consistently distinguishes *"engine works"* (the core logic is real and verified) from *"full app"* (a complete GUI program), and flags every stub, simplification, mock fallback, or hardware-attended item explicitly. Each Part ends with a status summary; the Appendix consolidates them.

### Architecture at a glance

```
                         ┌─────────────────────────────────────────────────────────┐
   Userland / apps       │ EuroSuite · EuroWeb+EuroJS · EuroApps · EuroAgent (WASM) │
   (Part 6)              │ EuroCoreutils · Linux-ABI compat binaries (musl)         │
                         └───────────────▲───────────────────────▲─────────────────┘
                                         │ native syscalls        │ Linux syscalls (bridge)
                         ┌───────────────┴───────────────────────┴─────────────────┐
   Desktop & services    │ Compositor · DispServer · Wayland · EuroInit · Shell     │  Part 5
                         ├──────────────────────────────────────────────────────────┤
   Security & identity   │ EuroGuard caps · EuroPol · EuroVault · EuroID · Sandbox   │  Part 4
                         ├──────────────────────────────────────────────────────────┤
   Networking & crypto   │ EuroNet (TCP/IP) · EuroTLS 1.3 · EuroFW · EuroVPN · TPM   │  Part 3
                         ├──────────────────────────────────────────────────────────┤
   Storage               │ EuroFS (CoW) · FDE (ChaCha20) · A/B update · cache        │  Part 2
                         ├──────────────────────────────────────────────────────────┤
   Core kernel           │ Scheduler · paging/W^X · SMP · IDT/APIC · IPC · drivers   │  Part 1
                         ├──────────────────────────────────────────────────────────┤
   Hardware              │ x86-64 · UEFI · PCI · virtio · NVMe · xHCI · HDA · TPM    │
                         └──────────────────────────────────────────────────────────┘
   Identity-mapped lower 512 GiB (1 GiB huge pages) → BAR-MMIO + heap are phys=virt,
   enabling DMA without an IOMMU. Per-process 2 MiB arenas carry the USER + W^X mappings.
```

The recurring design themes you will see throughout:

1. **Sans-IO, host-tested cores.** The fiddly, security-critical logic (packet framing, TLS state machine, Argon2id, the filesystem, the AML interpreter, the office formats) lives in pure `crates/euro*` libraries that compile and unit-test under `std` on a normal host with `cargo test`. The kernel modules are thin glue that wire those verified cores onto real hardware. This is why there are 793 host tests *and* a boot self-test for almost everything.
2. **Capabilities, not ambient authority.** There is no root, no setuid. A process holds a capability bitmask checked at the syscall boundary; rights can be dropped but never regained. Policy (EuroPol) can only *reduce* the set, never add.
3. **Sovereignty by construction.** Cryptography (Argon2id, Blake2b, ChaCha20-Poly1305, X25519, Ed25519, the TLS key schedule, RSA/ECDSA verification) is either implemented from scratch and pinned to official RFC test vectors, or built on audited RustCrypto primitives — never a dependency on a non-EU service. The trust store is EU-CA-first. Data formats (`EUROFS01`, `.euroa`, `.eupkg`, EuroCA certs) are owned.
4. **Tamper-evidence everywhere.** Append-only audit logs (filesystem-enforced), hash-chained audit records (cryptographically enforced), Ed25519-signed binaries verified before execution, and reproducible-build consensus.


---

## Part 1 — Boot, Core Kernel & Hardware Enumeration

This part documents the boot path, the core kernel runtime (memory, CPU, SMP, scheduling, IPC, power/init/logging), and hardware enumeration for EuroOS. It is **not** Linux or BSD; the Linux syscall ABI appears only as an opt-in compatibility bridge inside the native ring-3 layer.

All claims below are traced to real code with `file:line` citations. The kernel proves each subsystem at boot by emitting tagged serial markers (`[xx]`) over COM1; these self-tests are the ground truth for "implemented & boot-verified" vs "stub/partial".

### Boot

#### Two-stage A/B UEFI loader (`loader/`)

**Purpose.** A tiny `BOOTX64.EFI` (`loader/src/main.rs`) is what UEFI firmware launches. It implements the Android/ChromeOS A/B model: it picks which kernel image to boot, loads it, and can roll back to slot A if the chosen slot's image is missing.

**How it works, step by step:**
1. Brings up COM1 directly via port I/O (`com1_init`/`putc`, `loader/src/main.rs:35-59`) so it can log under Boot Services.
2. Reads `\slot_config` from the ESP, deserializes it with `euroupdate::SlotConfig::deserialize`, and returns `cfg.next_boot` (`read_slot`, `loader/src/main.rs:67-72`). `None` → defaults to `Slot::A`.
3. Maps the slot to a path: `\EFI\BOOT\eurokernel-A.efi` or `…-B.efi` (`main`, `loader/src/main.rs:79-83`).
4. Reads the image into a buffer; if the slot's image is absent it falls back to reading the A image, and only fails fatally if A is also missing (`loader/src/main.rs:88-100`).
5. `boot::load_image(FromBuffer{…})` + `boot::start_image` (`loader/src/main.rs:103-114`). The kernel itself performs `ExitBootServices`, so `start_image` does not normally return.

**Design rationale.** Making A/B a *loader* decision (not a kernel decision) means a kernel that won't even boot can be rolled back; the running kernel continues to own `slot_config` (attempt counter / mark-good), per the module doc comment (`loader/src/main.rs:1-10`). **Implemented & verified** via the `[loader]` serial lines.

#### Kernel boot sequence (`kernel/src/main.rs`, `#[entry] fn main`)

The kernel is `#![no_std] #![no_main]` with `feature(abi_x86_interrupt)` (`kernel/src/main.rs:1-3`). The boot sequence runs entirely inside `main` (`kernel/src/main.rs:159`) and is ordered deliberately around the `ExitBootServices` jump:

**Phase A — still in UEFI Boot Services:**
1. `allocator::init()` then `serial::init()` (`main.rs:162-164`) — heap and COM1 first, because both must survive `ExitBootServices`.
2. `build_frame_allocator()` from the UEFI memory map (`main.rs:170`).
3. Acquire the GOP framebuffer, pick the best mode, capture base/width/height/stride/pixel-format into the global `FB_INFO` (`main.rs:174-187`), then build a **buffered** `FrameBuffer` whose backing pointer stays valid after exit (`main.rs:185`).
4. Pull the ACPI RSDP from the UEFI config table (`ACPI2_GUID` preferred, else `ACPI_GUID`) and store it via `acpi::set_rsdp` — this is the only point at which RSDP is reachable (`main.rs:191-199`).

**The jump:** `boot::exit_boot_services(MemoryType::LOADER_DATA)` (`main.rs:203`). After this there are no UEFI services.

**Phase B — kernel mode bring-up:**
5. `interrupts::disable()`, `gdt::init()`, `interrupts::init()` (`main.rs:207-211`).
6. `paging::init(&mut allocator)` loads the kernel's own page tables and records the boot PML4 for the scheduler via `sched::set_boot_pml4(pml4)` (`main.rs:216-217`).
7. A2 guarded boot stack (`paging::setup_guarded_stack`) with a non-destructive verification (write/read a magic into the top page, confirm the guard page is not present) → `[a2]` marker (`main.rs:223-237`).
8. Reserve the 64 MiB process frame pool (`procpool::install`) → `[mm]` marker (`main.rs:242-249`).
9. Storage + filesystem: `virtio_blk::init`, optional `nvme::init`/`self_test`, then mount-or-format EuroFS on disk (GPT partition) or RAM (live mode) (`main.rs:252-291`).
10. A long series of subsystem self-tests with tagged markers — `[j2]` bad-block remap, `[j3]`/`[j3-fault]` swap, `[y]` crash-dump, `[j1]`/`[j1-cache]` block cache, container/display/AF_UNIX, `[hpet]`, `[auth]`, env/EuroGuard config, live networking (DHCP/ARP/ICMP/DNS/HTTP/TLS/IPv6), Ed25519 verify + tamper test, and the exec-by-name boot script (`main.rs:294-1024`).
11. `int3` breakpoint to prove the IDT (`[euro] breakpoint-exceptie afgehandeld`, `main.rs:1026-1028`).
12. Hardware enumeration + late-boot wiring: `acpi::parse()` MADT dump, `pci::enumerate`, `eurodevice::init/selftest` (`[r]`), `euroaml` DSDT interpretation (`[i3-aml]`), TPM, then the timer/SMP/IRQ-routing chain described below (`main.rs:1126-1223`).
13. Enable interrupts (`x86_64::instructions::interrupts::enable()`, `main.rs:1222`) → preemptive multitasking including ring 3.

The late ordering is precise: `interrupts::init_timer(100)` (`main.rs:1192`) calibrates and starts the LAPIC timer; `smp::setup_guarded_stacks` + `smp::init()` bring up APs **while the BSP is still on the boot PML4 with interrupts off** (`main.rs:1196-1204`, comment); only then `route_io_apic`, mouse/xHCI/HDA init, and the global `interrupts::enable()`.

### Memory & paging

#### Frame allocator (`crates/euromm`)

A bitmap physical frame allocator: 1 bit per 4 KiB frame, 64 frames per `u64` word (`crates/euromm/src/frame.rs:23-36`). Key fields of `FrameAllocator`: `bitmap: Vec<u64>`, `total_frames`, `free_frames`, `usable_total` (RAM usable at init, before any allocation), `hint` (search cursor), plus S6 hardening counters `double_frees` and `high_water` (`frame.rs:24-36`).

- `from_regions(regions, reserve_below)` sizes the bitmap to the **highest usable** region (not far-away MMIO, which would bloat the bitmap), marks usable regions free, unusable regions used, then reserves the low `reserve_below` bytes (IVT/BIOS/kernel image) (`frame.rs:56-78`).
- `allocate()` scans words from `hint`, uses `trailing_ones()` to find the first free bit (`frame.rs:104-120`).
- `allocate_contiguous(count)` and `allocate_aligned(count, align)` — the latter jumps directly to aligned frame boundaries (e.g. `align=512` → 2 MiB), avoiding the over-allocate-and-trim waste of `allocate_contiguous` (`frame.rs:123-177`). `run_args` uses `allocate_aligned(512, 512)` for the per-process 2 MiB arena (`ring3.rs:3249`).
- `free()` detects double-frees (bit already clear) and increments `double_frees` rather than corrupting state (`frame.rs:198-214`).

Fully **host-tested** (8 unit tests, `frame.rs:240-324`).

#### Page tables (`kernel/src/paging.rs`)

4-level paging, built from scratch. Flag constants: `PRESENT/WRITABLE/USER/HUGE(1<<7)/NX(1<<63)/MIB2/GIB` (`paging.rs:13-19`).

**Boot address space (`paging::init`, `paging.rs:453-501`):**
- PML4[0] → a PDPT that identity-maps the lower 512 GiB with **1 GiB huge pages**, all **supervisor** (no USER bit) so SMEP/SMAP won't fault on kernel code/stack/heap/page-tables in the low 1 GiB (`paging.rs:465-478`, comment).
- PML4[1] → a second PDPT identity-mapping 512 GiB–1 TiB supervisor, because QEMU q35 places some 64-bit BARs high (~768 GiB NVMe MMIO) (`paging.rs:480-491`). This high PDPT is shared via `HIGH_PDPT` atomic with every later process PML4 (`paging.rs:25-34`, `paging.rs:492-495`).
- Loads CR3 (`paging.rs:498`). Everything stays identity-mapped so existing pointers keep working.

**Per-process isolated address spaces (`build_address_space`, `paging.rs:42-106`):** allocates 4 table frames (PML4, PDPT, PD, and a 4 KiB **arena-PT**). Lower 1 GiB is split to PD granularity; only the one 2 MiB arena block (`arena_phys`, 2 MiB-aligned, < 1 GiB) is mapped through the fine-grained PT with the **USER** bit. All other PD/PDPT entries stay supervisor huge pages so kernel code/stacks remain valid after a CR3 switch. The arena-PT enforces **W^X** per 4 KiB page from the ELF loader's `exec_pages`/`writ_pages` bitmaps: `exec & !writ → R-X`, `exec & writ → RWX` (mixed RWE segment, unavoidable), `!exec → RW + NX` (`paging.rs:91-103`).

Variants: `build_address_space_rwx` (single 2 MiB RWX USER page for the raw machine-code counter demo, `paging.rs:113-139`); `build_address_space_remap` / `fill_remap_tables` / `fill_remap_tables_wx` for `fork()` children, which map the **parent's virtual** arena slot onto the **child's physical** frames so absolute pointers in the copied stack/code still resolve (`paging.rs:147-227`). `free_address_space` walks the chain to reclaim the 4 table frames (`paging.rs:432-450`); `table_frames`/`arena_pt`/`arena_set_writable`/`arena_set_wx` support execve in-place reloads (`paging.rs:360-408`).

**Guarded kernel stacks (A2/G1, `paging.rs:229-357`).** A pool in the shared high region (512 GiB+) of uniform 5-page units = 1 unmapped guard page + 4 stack pages (16 KiB), ≤ 102 units in one shared PT (`paging.rs:240-244`). `ensure_guard_pt` replaces the 1 GiB huge mapping of 512–513 GiB in the shared high PDPT with a fine PD→PT (`paging.rs:267-290`). `guarded_stack_alloc` leaves the guard page absent and maps 4 real frames, returning the stack top (`paging.rs:296-319`). `is_stack_guard(addr)` is an O(1) region+modulo test used by the page-fault handler (`paging.rs:252-262`). Because the region lives in the shared high PDPT, the same guarded stacks are valid in every address space — used for IST, AP, and scheduler-task stacks. Verified by `[a2]` and the deliberate-overflow `[g1]` self-tests.

#### Kernel heap (`kernel/src/allocator.rs`)

A `#[global_allocator]` `linked_list_allocator::LockedHeap` over a static 96 MiB `.bss` region `HEAP` (`allocator.rs:11-31`). Sized to 96 MiB because the browser engine's DOM (~3000 elements) OOM'd at 32 MiB; the VM has 256 MiB (`allocator.rs:16-20`). Using the kernel's own heap (not the uefi crate's) is essential so `alloc` works both before and after `ExitBootServices` (`allocator.rs:1-9`). `init()` must be the very first action in `main` (`allocator.rs:23-31`). `stats()` returns `(used, free)`. EuroMM's own slab allocator is noted as the intended replacement (Track 3.4) — **the current heap is the linked-list allocator, not a custom slab**.

### CPU init

#### GDT + TSS (`kernel/src/gdt.rs`)

`init()` builds a `GlobalDescriptorTable` in the order `kernel_code, kernel_data, user_data, user_code, tss` — order is load-bearing for the SYSCALL/SYSRET selector layout (`gdt.rs:51-71`). A lazily-built `TaskStateSegment` (`gdt.rs:25-41`) provides:
- `interrupt_stack_table[0]` = double-fault IST stack (`DOUBLE_FAULT_IST_INDEX`).
- `interrupt_stack_table[1]` = page-fault IST stack (`PAGE_FAULT_IST_INDEX`) — G1: lets a kernel-stack overflow that faults on its guard page be handled on a fresh stack instead of escalating to a double fault (`gdt.rs:14-18`).
- `privilege_stack_table[0]` (rsp0) = kernel stack for ring3→ring0 transitions.

`init()` reloads CS/SS/DS/ES to the kernel selectors (the UEFI selectors aren't in this GDT and would `#GP` on the first `iretq`) and loads the TSS (`gdt.rs:96-109`). `set_rsp0(addr)` rewrites rsp0 per ring-3 task so each process gets its own interrupt stack (`gdt.rs:84-94`); `rsp0_top()` returns the static fallback. `init_ap()` loads the **shared** GDT and kernel segments on an AP but loads **no TSS** (the only TSS is the BSP's; APs run timer-only/parked without ring-3 or IST) (`gdt.rs:111-125`).

#### IDT + exceptions (`kernel/src/interrupts.rs`)

The `IDT` (`interrupts.rs:75-112`) wires: breakpoint, invalid-opcode, GP-fault, page-fault (with `set_stack_index(PAGE_FAULT_IST_INDEX)`), double-fault (`DOUBLE_FAULT_IST_INDEX`); the timer vector `0x20` points at the **scheduler context-switch stub** via `set_handler_addr(sched::stub_addr())` (not an ordinary handler, because it must save all registers); keyboard `0x21`, mouse `0x2C`; AP-timer `0x41` → `sched::ap_stub_addr()`; cross-CPU IPIs `0x43` ping / `0x44` halt / `0x45` TLB-shootdown; MSI-X vectors `0x46` xHCI / `0x47` virtio-blk; and a spurious handler at `0xFF`.

Notable handler semantics:
- **Page fault** (`page_fault_handler`, `interrupts.rs:260-326`): reads CR2; if `USER_MODE`, kills only that process (synchronous foreground exec → `fg_force_exit`; background task → `mark_current_dead` + `note_isolation_kill`, then `hlt` loops until the scheduler switches away — the rest of the system keeps running). If the fault address is a stack guard (`paging::is_stack_guard`), reports `[g1] KERNEL STACK OVERFLOW` and either kills the task (current != 0, recoverable) or halts on the boot stack. Otherwise tries `swapmgr::try_swap_in(addr)` (transparent fault-driven swap-in, `[j3-fault]`); else captures a crash dump and halts.
- **GP fault** (`gp_handler`, `interrupts.rs:227-258`): same ring-3-kill-only policy; for ring-0 captures a crash dump (vector 13) and dumps the top stack words.
- **Double fault** (`interrupts.rs:328-332`) and **page fault** capture minidumps via `crashdump::capture` before halting.

`init_timer(hz)` (`interrupts.rs:58-73`): initializes the 8259 PICs, masks IRQ0 (PIT) but keeps IRQ1/IRQ2/IRQ12, then calls `apic::init(hz, TIMER_VECTOR)` to drive the scheduler tick from the LAPIC timer, and `klog::mark_apic_ready()`. `route_io_apic(madt)` masks the 8259 fully and routes keyboard (IRQ1) and mouse (IRQ12) through the IO-APIC to the BSP using `madt.gsi_for()` overrides (`interrupts.rs:193-211`). Tick count lives in `TICKS` (~100 Hz).

#### Local APIC + IO-APIC + timer (`kernel/src/apic.rs`)

LAPIC MMIO at `0xFEE00000` (identity-mapped). `init(hz, vector)` (`apic.rs:84-106`): global-enable via `IA32_APIC_BASE` bit 11, software-enable + spurious vector `0xFF`, **virtual-wire** (`LINT0 = ExtINT` so the 8259 keyboard/mouse keep working, `LINT1 = NMI`), then **calibrate** against PIT channel 2 and start the periodic timer divided by 16. `calibrate(hz)` (`apic.rs:199-232`) runs the LAPIC timer at max while polling PIT ch2's OUT2 status bit, with a safety guard against a hung calibration. `lapic_id()`, `eoi()`, `ioapic_route(base, gsi, vector, dest)` (`apic.rs:62-71`), `send_ipi`/`send_init`/`send_sipi` (the INIT-SIPI-SIPI primitives for SMP, `apic.rs:125-149`), `busy_wait_us` (interrupt-independent delay using the running timer's current-count, `apic.rs:165-195`), and `start_timer_on_this_cpu(vector)` (AP per-CPU timer using the BSP's calibrated count, `apic.rs:110-121`). Verified by `[apic]`.

#### HPET (`kernel/src/hpet.rs`)

High-precision free-running counter at MMIO base `0xFED00000` as a HAL time source alongside RTC (wall clock) and APIC (scheduling). `init()` reads the CAP register, validates the period (1 fs … 100 ns), sets `ENABLE_CNF`, and records `PERIOD_FS` (`hpet.rs:24-39`). `counter()`, `freq_hz()` (10¹⁵ fs/s ÷ period), `ns()`, `us()`. Verified by the `[hpet]` marker measuring 1M spin iterations (`main.rs:486-499`). **Implemented**; used for SPERF profiling and delays.

#### RTC/CMOS (`kernel/src/rtc.rs`)

Reads real wall-clock time from CMOS registers via ports 0x70/0x71. `now()` reads repeatedly until two successive reads agree (avoids mid-tick values), handles BCD↔binary and 12h↔24h conversion from CMOS register B (`rtc.rs:34-89`). `epoch()` computes Unix time with proper leap-year handling — this feeds `clock_gettime(CLOCK_REALTIME)`/`gettimeofday` in the Linux-compat layer and EuroFS timestamps (`rtc.rs:94-115`, used at `main.rs:272`, `283`). `weekday` (Sakamoto), `clock_string`, `date_string` for the status panel. **Implemented & verified** (real time appears in EuroFS checkpoints and the desktop).

#### MSI-X (`kernel/src/msix.rs`)

Walks the PCI capability list for the MSI-X capability (id `0x11`), maps the MSI-X table (BAR+offset from the table-offset/BIR register), and programs one entry: message address `0xFEE00000 | (dest_apic << 12)`, message data = vector (fixed/edge), vector-control unmasked; enables MSI-X (bit 15), clears function-mask, and disables legacy INTx (command bit 10) so only MSI-X delivers (`msix.rs:31-78`). Returns the table size. `bar_base` handles 64-bit BARs (`msix.rs:18-26`). This is the reusable interrupt-delivery layer; verified at boot by `[j2]` (xHCI event-ring MSI-X count `> 0` and virtio-blk completion MSI-X).

### SMP (`kernel/src/smp.rs`)

**AP bring-up.** A build-time-assembled real-mode→long-mode trampoline blob (`OUT_DIR/trampoline.bin`) is copied to physical `0x8000`; the BSP patches CR3 (`OFF_CR3=0xF00`), per-AP stack (`OFF_STACK=0xF08`) and the `ap_main` entry (`OFF_ENTRY=0xF10`) (`smp.rs:20-27`, `141-146`). `init()` (`smp.rs:113`) parses the MADT (`acpi::parse`), gets the BSP id and boot PML4, then for each enabled non-BSP core sends **INIT-SIPI-SIPI** with `busy_wait_us` gaps and waits ~100 ms for the AP to bump `AP_ONLINE` (`smp.rs:170-195`). `MAX_AP = 7` (8 cores total). Each AP's `ap_main` (`smp.rs:86-108`) increments `AP_ONLINE`, computes its slice of a parallel sum, then sets up its **per-CPU scheduler**: `gdt::init_ap` (shared GDT), `interrupts::init` (shared IDT), `sched::ap_setup(id)` (own run-queue), `apic::start_timer_on_this_cpu(0x41)`, `sti`, and parks in an idle `hlt` loop that becomes its idle task.

**Per-CPU run queues** are in `sched.rs` (see below). `init()` also: verifies a parallel sum `0..WORK_N` across all cores equals the closed-form expected value (`[smp] parallelle som …`, `smp.rs:204-240`); reads per-CPU worker counters; performs cross-CPU **ping IPIs** and a **TLB shootdown** (`tlb_shootdown`, `smp.rs:300-308`) verified via `IPI_COUNT`/`TLB_COUNT`; and does **load-balancing** by enqueuing an extra worker on the least-loaded AP (`ap_enqueue_worker`, `smp.rs:284-297`). Guarded AP stacks come from `setup_guarded_stacks` (`smp.rs:49-60`). `halt_others` stops all other cores (for shutdown/panic). **Implemented & verified** by the `[smp]` markers.

### Scheduling & processes

#### Scheduler (`kernel/src/sched.rs`)

Preemptive mini-CFS, `MAX_TASKS = 48`, 16 KiB stacks. The timer interrupt enters the assembly stub `timer_switch` (`sched.rs:249-261`) which pushes all 15 GP registers, calls `schedule_tick(rsp)` (sysv64, because the UEFI target would otherwise use Win64 ABI, `sched.rs:276-277`), switches `rsp` to the chosen task and `iretq`s.

`Task` fields (`sched.rs:45-72`): `rsp`, `kstack` (rsp0 for ring3→ring0; 0 = kernel task), `fs_base` (per-task musl TLS pointer, IA32_FS_BASE, saved/restored each switch), `cr3` (0 = shared boot PML4; nonzero = isolated address space), `state`, `nice` (-20..19), `vruntime` (virtual runtime; smallest-first selection), `pid`/`ppid`, and `stack_bottom` (S6 canary location). `State` is a full machine: `Ready / Sleeping(wake) / Blocked(chan) / Zombie(code) / Dead` (`sched.rs:30-43`).

`schedule_tick` (`sched.rs:277-346`): bumps `TICKS`, EOIs the timer, saves the outgoing `rsp`, checks the S6 stack canary (`STACK_CANARY`, panics on overflow), saves/restores `fs_base`, wakes sleepers whose `wake` time arrived, advances the outgoing task's `vruntime` by `vstep(nice)` (`(nice+21)*64`), selects the **Ready** task with smallest `vruntime` scanning from `cur+1` with strict `<` for fair round-robin at equal nice, switches CR3 if the incoming task has a different address space, and sets `TSS.rsp0` to the incoming task's `kstack`.

State APIs: `block_on`/`unblock`/`wake` (futex-like wait channels), `sleep_ticks` (real timed wait), `exit_current`/`reap`/`take_zombie_child` (waitpid/zombie reaping), `set_ident`/`set_nice`, `mark_current_dead`/`mark_dead`/`kill`. `spawn_user` (ring-3 task with its own kernel stack), `spawn_thread` (clone: shares CR3, own kstack+fs_base, child inherits parent regs with `rax=0`, mapping musl's `__clone` register layout exactly, `sched.rs:472-517`). `init()` (`sched.rs:394-415`) starts kernel background tasks: `task_a/b/c` (equal workload, nice -10/0/+10 to demonstrate priority via `[S2]` counters), `task_sleeper` (real `sleep_ticks` — `[S2]`), and `task_overflow` (deliberately overflows its guarded stack to prove `[g1]` recovery).

**Per-CPU AP scheduler** (`sched.rs:601-763`): each AP has its own `AP_SCHED[cpu]` (idle + 2 workers + room for one balanced task) with its own `ap_timer_switch` stub on vector `0x41` and `ap_schedule_tick` doing simple round-robin touching only its own queue (single-writer, no cross-CPU contention). Verified by `[smp] … per-CPU scheduler`.

#### Process frame pool (`kernel/src/procpool.rs`)

A separate global `FrameAllocator` behind a `Mutex` over a 64 MiB contiguous region reserved from the main allocator at boot (`procpool::install`, `procpool.rs:18-21`). Rationale: the main allocator is owned by `main`/the desktop loop and is unreachable from inside a syscall, but `fork()`/`execve()` must allocate (arena + page tables + kernel stack for the child) while running in a syscall — so they draw from this pool (`procpool.rs:1-8`). API: `alloc`/`alloc_contiguous`/`free`/`free_frames`. **Implemented & verified** (`[mm]` marker; fork tests run).

#### Ring-3 processes, ELF loading, syscall ABI (`kernel/src/ring3.rs`)

**Capability model.** Each process gets exactly the caps it needs, enforced at the syscall boundary (no root/non-root): `CAP_CONSOLE/CAP_PROC_INFO/CAP_FILE/CAP_NET/CAP_IMMUTABLE_ADMIN` (`ring3.rs:23-27`). `required_cap(num)` (native) and `linux_required_cap(num, a1)` (Linux ABI) map syscalls to required caps; denial returns `-EPERM` with a `[cap]` log (`ring3.rs:75-83`, `2685-2707`). Programs are registered with caps+ABI via `register_program` (boot installs 22 binaries, `main.rs:573-599`).

**SYSCALL/SYSRET setup** (`init_syscall_msrs`, `ring3.rs:3173-3198`): calls `enable_smep_smap()` first, detects NX (CPUID 8000_0001h EDX bit 20), writes `EFER` (SCE + NXE), `STAR` (kernel/user selector pairs), `LSTAR = syscall_entry`, `FMASK = 0x200` (clears IF on entry → syscalls run non-preemptively), and sets the syscall kernel stack. `enable_smep_smap()` (`ring3.rs:3131-3156`) reads CPUID leaf 7 and sets CR4 SMEP (bit 7) / SMAP (bit 20) when available, logging `[sec]`.

**The syscall entry stub** (`global_asm!`, `ring3.rs:1444-1553`): saves user RSP, switches to the kernel stack, opens a SMAP window by setting RFLAGS.AC (`bts … 18`) so ring 0 may touch user (U=1) pages for the syscall's duration — safe because IF=0 makes it non-preemptive (`ring3.rs:1450-1457`). It saves all callee-preserved user registers, records `SAVED_REGS`/`USER_RIP` (for clone), shuffles arguments into the sysv64 order, and `call syscall_dispatch`. On a normal return it restores registers and `sysretq`; on `sys_exit` (`EXITED != 0`) it closes the AC window and returns into the kernel caller. `enter_ring3(cs, ss, rip, rsp)` enters ring 3 via `iretq` with **IF=0** for synchronous foreground execs (a timer preemption would corrupt the stack); scheduled ring-3 tasks instead go through `sched::spawn_user` with IF=1 (`ring3.rs:1532-1552`). `force_kernel_return` is the page-fault-handler trampoline to abort a faulted foreground exec cleanly.

**ELF loading** (`load_elf64`, `ring3.rs:2021-2098`): validates the ELF64/little-endian/x86-64 header, walks program headers, loads `PT_LOAD` segments with overflow-safe bound checks (audit H11), zeroes `.bss`, records `exec_pages`/`writ_pages` per-page W^X bitmaps (PF_X/PF_W), resolves the PHDR vaddr (from `PT_PHDR` or inferred), and applies relocations (`apply_relocations`, no-op for static/non-PIE). `load_program` falls back to a flat RWX blob for non-ELF (`ring3.rs:2100-2121`). An in-kernel **dynamic linker** (H3) loads `DT_NEEDED` `.so`s into the same arena and resolves `R_X86_64_JUMP_SLOT`/`GLOB_DAT` (`run_dynamic`/`needed_libs`, verified by `[h3]`/`[h3-fs]`).

**Per-process address space & run path** (`run_args`, `ring3.rs:3221-3293`): sets caps/ABI/app-identity, resets the fd table, allocates an aligned 2 MiB arena (`allocate_aligned(512,512)`), lays out code/heap/stack, loads the program, builds the SysV stack (argc/argv/envp/auxv via `setup_user_stack`), builds the W^X PML4 (`build_address_space`), sets rsp0, switches CR3, `enter_ring3`, then on return switches back to the boot CR3 and frees the 512 arena frames + the page tables (no leak per exec). User-pointer validation against the arena is done via `in_user_arena` (`ring3.rs:58-68`, audit C1).

**Native syscalls** (`syscall_dispatch`, `ring3.rs:2569-2678`): `0 exit`, `1 write` (NUL-terminated, CAP_CONSOLE), `2 getpid`, `4 uname`, `12 sbrk` (overflow-safe), `20/21/22 open/close/read` (CAP_FILE), `60 net` (CAP_NET). The dispatcher also routes daemon tasks (`daemon_dispatch`), preemptive per-process musl PCBs (`bg_dispatch`, with `do_fork`/`do_wait4` for syscalls 57/58/61), and the **Linux ABI** (`linux_dispatch`, `ring3.rs:2700+`) for binaries compiled for `x86_64-linux` (write/read/open/close/stat/socket/getdents64/openat/arch_prctl/clone/etc.), translating to the native handlers. A `/proc` synthesizer (`ensure_proc`, `ring3.rs:296-340`) produces live `version/cpuinfo/meminfo/uptime/self/maps` so Linux programs reading `/proc` get real values. Per-syscall profiling feeds `syscall_profile_lines`.

**Verified** by `[ring3→sys_write]`, `[linux-abi]`, `[cap]`, `[exec]`, `[h3]`, `[isolatie]` markers and the boot-script execs. This is the OS's **native** identity; the Linux ABI is explicitly a compat bridge.

### IPC (`kernel/src/euroipc.rs`)

A simple in-kernel message bus. A `Port { port: u32, owner_pid: u64, queue: Vec<(sender_pid, Vec<u8>)> }` (`euroipc.rs:14-18`); a global `PORTS` and a 6-line audit ring (`euroipc.rs:20-30`). API: `register(pid, port)` (claim, returns 1/0), `send(sender_pid, port, data)` (tags with sender pid + audits, returns bytes or `-ESRCH`), `recv(pid, buf, max)` (copies one message into the receiving process's user arena, returns bytes / `-EAGAIN` / `-ESRCH`), `audit_lines()` (`euroipc.rs:33-84`). Every message carries the sender's **app identity** and is audited.

**Status: partial.** The permission check is an open hook ("nu: toegestaan" — all allowed), with EuroGuard-policy coupling marked as a future step (`euroipc.rs:6-7`, `52-53`). Exercised at boot via the `ipcsend`/`ipcrecv` musl programs (`main.rs:1121-1124`).

### Power / init / logging

#### Power management (`kernel/src/power.rs`)

Real ACPI shutdown/reboot. `shutdown()` (`power.rs:32-49`) flushes the block cache + virtio-blk, computes the S5 PM1a value `(SLP_TYPa<<10)|SLP_EN(bit13)` using the AML-evaluated `\_S5` SLP_TYP (set by `set_s5_slp_typ`, fed from the AML interpreter at `main.rs:1176`) or 0 on QEMU, writes the FADT `pm1a_cnt` port plus QEMU fallback ports, and halts. `reboot()` (`power.rs:53-69`) uses the FADT reset register if supported, else the PCI reset port `0xCF9`, else the 8042 `0xFE` pulse. Using the firmware-correct SLP_TYP (not a hardcoded 0) matters on real hardware. **Implemented**; correctness on QEMU is exercised via the FADT path.

#### EuroInit service supervisor (`kernel/src/init.rs`)

The PID-1 role. `Service { name, bin, restart: Restart, pid, starts, max_starts }` (`init.rs:24-31`). `start_all` registers declarative defaults (the `ticker` service) and spawns them via `ring3::spawn_bg_musl` after Ed25519 `verify_program` (`init.rs:48-70`). `supervise` (`init.rs:74-88`) runs in the desktop loop (where the allocator + FS are reachable) and restarts dead `Always`-policy services up to `max_starts` (anti-storm). `flush_log` (eurologd) writes the kmsg ring to `/var/log/messages` every ~256 ticks (`init.rs:92-102`). `status_lines` backs the `services` command. **Implemented & verified** via `[init]` markers (ticker restart proof).

#### COM1 serial (`kernel/src/serial.rs`)

A 16550 UART at `0x3F8`, 38400 baud 8N1, FIFO on (`serial.rs:27-41`). Works after `ExitBootServices` — the primary bring-up debug channel (QEMU `-serial file:serial.log`). `_print` **tees** every byte into the klog ring (lock order UART→RING, no deadlock) (`serial.rs:71-85`); `write_raw` is a panic-safe path using `try_lock` and **no** tee-back (`serial.rs:89-98`). Provides the `serial_print!`/`serial_println!` macros used pervasively for the `[xx]` markers.

#### klog / kmsg ring (`kernel/src/klog.rs`)

In-memory ring buffer (512 lines × 160 bytes) + leveled logging + rich panic context. **Lock-free MPSC** ring (J1): writers claim a slot with `HEAD.fetch_add(1)` and publish length with Release; the *partial* (not-yet-newline) line is **per-CPU** (`PCUR`/`PLEN`) so cores never share a lock on the log path — critical so the panic handler can never block (`klog.rs:39-122`). `tee` line-buffers per CPU until `\n`; `record(level, args)` adds an uptime timestamp + level tag; `snapshot`/`with_recent` read lock-free. `cpu_slot()` only reads `lapic_id()` after `mark_apic_ready()` (else CPU 0). `dump_registers_and_backtrace` (`klog.rs:190-252`) dumps RSP/RBP/CR2/CR3/RFLAGS and walks the RBP frame-pointer chain (falling back to a stack scan), deriving the real `.text` range from the address of the function itself (UEFI relocates the PE image off the link base). Macros `kinfo!/kwarn!/kerr!/kdebug!`. Verified by `[j1]` lock-free self-test.

#### Crash dump (`kernel/src/crashdump.rs`)

Kernel side of EuroCrash. `capture(vector, error_code, rip, rsp, rflags)` builds an `eurocrash::CrashDump`, fills CR2/CR3/uptime/seq, encodes it and writes it to a reserved sector (`CRASH_LBA = 300`) via virtio-blk (`crashdump.rs:31-48`). Called from the GP/PF/DF handlers just before halt. `read_last`/`selftest` provide cross-boot recovery: on boot it reports any prior-boot dump (distinguishing a real crash from the `0xFE` test sentinel) and round-trips a fresh test dump (`crashdump.rs:51-97`). Builds on G1 (PF/DF run on their own IST stacks, so a dump can be written even on stack exhaustion). Verified by `[y]`. (Full format detail in Part 4.)

### Hardware enumeration

#### PCI (`kernel/src/pci.rs`)

Legacy config-space access via ports `0xCF8`/`0xCFC` (`cfg_read32`/`cfg_write32`, `pci.rs:100-120`). `enumerate()` scans buses 0..=8, dev 0..32, handling multi-function devices via the header bit (`pci.rs:123-154`). `PciDevice` exposes `bar(n)`, `bar_addr(n)` (masks flag bits, handles **64-bit memory BARs** by combining the next BAR — used by modern virtio MMIO, `pci.rs:39-51`), `irq_line`, `enable(bits)` (command register, e.g. bus-master + MMIO), and `virtio_cap(cfg_type)` which walks the capability list for the virtio vendor cap (`0x09`) of the requested type (common/notify/isr/device), returning the identity-mapped MMIO `addr`/`length`/`notify_mult` (`pci.rs:56-98`). `class_name`/`device_name` give human labels (virtio IDs, Intel Q35/ICH9, QEMU VGA). **Implemented & verified** (`[pci]` device list).

#### ACPI (`kernel/src/acpi.rs` + `crates/euroacpi`)

`crates/euroacpi` is the **pure, host-tested, `#![forbid(unsafe_code)]` parser core**: `SdtHeader::parse`, checksum validation (byte-sum == 0), RSDT/XSDT and FADT decoding (`euroacpi/src/lib.rs:1-45+`). `kernel/src/acpi.rs` provides the physical-memory access layer over it. RSDP is captured pre-exit (`set_rsdp`). `find_table(sig)` walks the RSDT (rev<2) or XSDT (rev≥2) pointer list (`acpi.rs:72-100`). `fadt()` decodes `pm1a_cnt`, reset support/space/addr/val (`acpi.rs:112-129`). `dsdt_aml()` returns the DSDT AML body via the FADT's DSDT/X_DSDT pointer (`acpi.rs:134-158`). `parse()` (`acpi.rs:161-241`) finds the `"APIC"` MADT and walks its entries: type 0 Processor Local APIC (`Core{apic_id, enabled}`), type 1 IO-APIC (`ioapic_addr`/`gsi_base`), type 2 Interrupt Source Override (`Override{source_irq, gsi, flags}`). `Madt::gsi_for(irq)` applies overrides (e.g. IRQ0→GSI2 on QEMU). **Implemented & verified** (`[acpi]` MADT dump feeds SMP + IO-APIC routing).

#### AML interpreter (`crates/euroaml`)

A minimal **ACPI AML bytecode interpreter** — a deliberate subset (`#![forbid(unsafe_code)]`, host-tested). It decodes opcodes for constants, `Name`/`Scope`/`Method`/`Package`/`Buffer`, dual/multi-name prefixes, root/parent prefixes, simple arithmetic (`Add/Subtract/Multiply`), and `Return` (`euroaml/src/lib.rs:24-88`). `AmlValue` is `Integer/Buffer/Package`; `Object` is either a `Value` or a `Method` (raw body bytes) (`euroaml/src/lib.rs:60-87`). `AmlNamespace::parse(aml)` builds a flat 4-char-name → object map; `evaluate("_S5_")`, `contains(name)`, `len()` drive the boot path. At boot (`main.rs:1165-1186`) it interprets the real firmware DSDT, extracts `\_S5` SLP_TYPa/b → `power::set_s5_slp_typ`, and counts which known methods (`_STA/_TMP/_BST/_PSR/_PTS/_WAK`) are present → `[i3-aml]`.

**Status: partial by design.** No `OperationRegion`/`Field` side-effects, no control flow, no full AML2.0 machine (`euroaml/src/lib.rs:9-13`) — sufficient for read-out methods and `\_S5`.

#### EuroDevice device model (`kernel/src/eurodevice.rs` + `crates/eurodevice`)

`crates/eurodevice` is the unified, host-tested (`#![forbid(unsafe_code)]`) device model: a `DeviceTree` (parent/child boom of `DeviceNode`s with stable `DeviceId` handles), a `DriverRegistry` matching drivers to devices via `fn(&DeviceNode) -> bool` predicates, a `trait Driver` lifecycle (start/stop/suspend/resume), and a `HotplugQueue` FIFO of `HotplugEvent::Attached/Detached` (`eurodevice/src/lib.rs:1-118`, `223-339`). `DeviceNode` carries kind/name/vendor/device/class/subclass/prog_if/parent/children/state/driver/resources; `DeviceState` is `Unbound/Bound/Failed/Suspended`. `bind`/`bind_all` set the driver + state; `unbound`/`unbind` manage lifecycle.

The kernel side (`kernel/src/eurodevice.rs`) builds the tree from `pci::enumerate()` (mapping class/vendor to `DeviceKind`, attaching IRQ + bus address as `DeviceResources`), registers the real driver match-predicates (`virtio-blk`/`virtio-net`/`euronvme`/`xhci-usb`/`euro-hda`/`virtio-gpu`/`pci-bridge`, `eurodevice.rs:20-74`), and binds them (`init`, `eurodevice.rs:43-85`). `probe_lines`/`selftest` print the whole tree with bindings → `[r]`. **Implemented & verified.** This replaces ad-hoc per-driver discovery; the matchers reference the *existing* kernel drivers (binding here is registration, while the drivers' actual MMIO init lives in their own modules).

#### USB / xHCI (`kernel/src/xhci.rs` + `crates/eurousb`)

`crates/eurousb` is the host-tested parser core: `DeviceDescriptor::parse`, `Configuration::parse`, `Endpoint`/`Interface` (incl. `is_boot_keyboard`/`is_boot_mouse`), `BootKeyboard::feed` → `KeyEvent`s, `parse_mouse` → `MouseEvent`, and mass-storage BOT helpers (`cbw`/`parse_csw`/SCSI `inquiry`/`read_capacity10`/`read10`/`write10`) (`eurousb/src/lib.rs:32-299`).

`kernel/src/xhci.rs` is the full MMIO hardware layer. `init(falloc)` (`xhci.rs:231+`) finds the xHCI controller (PCI class `0C:03:30`), reads the 64-bit MMIO BAR0, enables memory-space + bus-master, reads capability registers (CAPLENGTH/HCSPARAMS/HCCPARAMS, slots/ports/ctx-size, DBOFF/RTSOFF), resets the controller (stop → wait HCH → HCRST → wait CNR clear), and sets up the **DCBAA**, **command ring**, **event ring** (ERST) and **device-context array** (`xhci.rs:252-290`). It then runs the real USB enumeration per root port: Enable Slot → Address Device → GET_DESCRIPTOR(device/config) → SET_CONFIGURATION → Configure Endpoint → SET_PROTOCOL(boot) → interrupt-IN poll (`xhci.rs:7-12`). All DMA structures come from the identity-mapped frame allocator, so device-visible physical addresses equal the kernel's pointers (`xhci.rs:18-20`). TRB types and ring management (`RING_TRBS=256`, Link TRB with Toggle-Cycle) are implemented (`xhci.rs:56-89`). `poll()` is invoked from the MSI-X handler (vector `0x46`) to harvest HID reports in interrupt context (so USB input works with HLT-idle/preemption, `interrupts.rs:149-153`); decoded reports flow into the same paths as PS/2 (`ps2::push_scancode` / `mouse`). Also exposes `usb_read_block`/`usb_disk_present` (mass storage), `present`, `hid_count`, `ack_interrupt` (`xhci.rs:692-1041`). **Implemented & verified** (`[xhci]`, `hid_count`, `[j2]` MSI-X count) on a controller present in the VM.

### Part 1 — status: implemented & boot-verified vs partial

**Implemented & boot-verified** (with markers): loader A/B (`[loader]`), heap + serial, frame allocator (+8 host tests), 4-level paging incl. per-process W^X isolation + guarded stacks (`[a2]`/`[g1]`), GDT/TSS, IDT + fault handlers (`int3`, `[isolatie]`, `[g1]`), LAPIC/IO-APIC + calibrated timer (`[apic]`), HPET (`[hpet]`), RTC (real time in EuroFS), MSI-X (`[j2]`), SMP AP bring-up + per-CPU schedulers + IPIs/TLB-shootdown/load-balance (`[smp]`), mini-CFS scheduler with full state machine, process frame pool (`[mm]`), ring-3 processes with SMEP/SMAP/NX + ELF/dynamic loading + native and Linux syscall ABIs + capabilities (`[sec]`/`[cap]`/`[ring3→…]`/`[exec]`/`[h3]`), EuroInit supervisor (`[init]`), lock-free klog (`[j1]`), crash dump (`[y]`), PCI enumeration (`[pci]`), ACPI/MADT/FADT parsing (`[acpi]`), EuroDevice model (`[r]`), AML DSDT interpretation (`[i3-aml]`), xHCI USB (`[xhci]`).

**Partial / stub:**
- **EuroIPC** (`euroipc.rs`): functional message bus + audit, but the permission check is an open "allow-all" hook pending EuroGuard-policy coupling.
- **EuroAML** (`euroaml`): intentional subset — no OperationRegion/Field side-effects, no control flow; enough for read-out methods and `\_S5`.
- **Kernel heap**: the linked-list allocator, not yet the planned EuroMM slab allocator.
- **AP TSS**: APs load no TSS (timer-only/parked; no AP ring-3 or IST yet).
- **Scheduler**: no recycling of dead task slots yet (`alloc_slot`, `sched.rs:432-443`); bounded by `MAX_TASKS=48`.


---

## Part 2 — Storage & Filesystem

EuroOS implements a complete from-scratch storage stack in `no_std` Rust, from PCI block drivers up through a copy-on-write filesystem with snapshots, full-disk encryption, atomic A/B system updates, per-file immutability, and a self-healing scrubber. None of this is Linux/BSD-derived: the on-disk formats (`EUROFS01` superblock, `SNFS` snapshot table, `EUPD` slot config), the `BlockDevice` trait, and the capability gates (`CAP_IMMUTABLE_ADMIN`) are sovereign. The pure-logic crates (`eurofs`, `eurofde`, `euroupdate`) compile with `std` under `cargo test` and are host-tested; the kernel drivers and wiring are boot-verified via `[xx]` serial markers under QEMU.

The layering from bottom to top:

```
NVMe / virtio-blk PCI drivers      (kernel/src/{nvme,virtio_blk}.rs)
  → RootBlk + write-back cache       (kernel/src/rootblk.rs)
    → EncryptedBlockDevice (ChaCha20) (crates/eurofde)        [optional]
      → EuroFs (CoW filesystem)        (crates/eurofs/src/disk.rs)
        → VFS / immutability / audit / scrub  (kernel/src/*)
```

Every layer implements or wraps the central `eurofs::BlockDevice` trait, which makes them stackable.

### The `BlockDevice` abstraction

`crates/eurofs/src/block.rs:18` defines the universal storage interface that the whole stack composes around:

```rust
pub trait BlockDevice {
    fn block_size(&self) -> u32;
    fn block_count(&self) -> u64;
    fn read_blocks(&self, start_block: u64, count: u32, buffer: &mut [u8]) -> BlockResult<()>;
    fn write_blocks(&mut self, start_block: u64, count: u32, buffer: &[u8]) -> BlockResult<()>;
    fn flush(&mut self) -> BlockResult<()>;   // durability barrier
}
```

`flush()` is documented (`block.rs:29`) as **mandatory before a CoW checkpoint commit** — this is the contract that makes crash consistency possible end-to-end. A blanket `impl BlockDevice for &mut D` (`block.rs:37`) lets a test format then re-mount a device without surrendering ownership (used pervasively in the remount tests). `MemoryBlockDevice` (`block.rs:56`) is the in-memory test backing, and crucially exposes a public `flush_count` so tests can *prove* that the A/B commit issues exactly two flushes (barrier + superblock).

`BlockError` (`block.rs:8`) has three variants: `OutOfBounds`, `IoError`, `NotAligned` (buffer length not a multiple of block size). **Implemented & host-tested.**

### EuroFS on-disk filesystem

`crates/eurofs/src/disk.rs` (1588 lines) is the core: a copy-on-write, crash-consistent, per-block-integrity filesystem. Block size is fixed at **4096 bytes** for this phase (`BS`, `disk.rs:33`). **Implemented & boot-verified** — mounted as the real root, on `/var`, on a second virtio disk, and on NVMe (`main.rs:272,1275,1304`).

#### Design choices (explicitly documented at `disk.rs:1-20`)

- **Copy-on-write**: a mutation never overwrites live data. New data and a new inode go to *free* blocks; only the atomic superblock update (checkpoint bump + flush) makes them live. A crash before that update leaves the old state fully intact.
- **No on-disk free-space bitmap**: free space is reconstructed at `mount` by scanning everything reachable from the committed superblock. Leaked (uncommitted) blocks are automatically free again — a "space scan" rather than a bitmap that could de-sync.
- **Object map** (OID → inode-block): a flat, CoW-rewritten table. A B+tree is the noted Phase-3 replacement; the flat table is deliberate for now.
- **Inode fills one 4 KiB block**: header + up to 8 extents + inline data + checksum.
- Directories are "files" whose data is a sequence of 64-byte dir-entries — one data path for files and directories alike.

#### The superblock (A/B redundancy, torn-write protection)

`crates/eurofs/src/superblock.rs`. The on-disk superblock is **exactly 512 bytes** (`#[repr(C, packed)]`, compile-time-asserted at `superblock.rs:53`), little-endian, written redundantly to **two slots**: `SUPERBLOCK_BLOCK = 1` and `SUPERBLOCK_BACKUP_BLOCK = 2` (`superblock.rs:21-22`). `RESERVED_BLOCKS = 16` (block 0 = boot, 1/2 = super A/B, 3..15 = slack including the snapshot table at block 8).

Key fields (`EuroFsSuperblock`, `superblock.rs:29-51`):

| Field | Meaning |
|---|---|
| `magic: [u8;8]` | `b"EUROFS01"` (`EUROFS_MAGIC`) |
| `version_major/minor: u16` | 0 / 1 |
| `uuid: [u8;16]` | volume UUID |
| `block_size: u32` | 4096 |
| `total_blocks`, `free_blocks`, `reserved_blocks: u64` | geometry |
| `created_at`, `last_mounted`, `last_written: u64` | clock timestamps |
| `checkpoint_id: u64` | **generation counter** — the heart of A/B selection |
| `object_map_root: u64` | first block of the CoW object-map table |
| `extent_tree_root: u64` | **reused field** = number of object-map blocks (`disk.rs:327` comments this explicitly) |
| `root_dir_oid: u64` | 1 |
| `encryption: u8`, `kdf_params: [u8;64]`, `wrapped_key: [u8;48]` | reserved FDE metadata slots (currently zeroed; FDE keying is external, see below) |
| `checksum: u64` | XXH3 over all bytes *before* this field |
| `_padding: [u8;271]` | pad to 512 |

The packed struct is handled correctly: the module comment (`superblock.rs:7`) warns it never takes a reference to a field, instead copying Copy fields to locals (e.g. `is_valid` at `superblock.rs:111`) and (de)serializing via `read_unaligned` (`from_bytes`, `superblock.rs:100`). `compute_checksum` (`superblock.rs:104`) hashes `bytes[..offset_of!(checksum)]`. `is_valid` (`superblock.rs:111`) checks magic + version + power-of-two block size ≥ 512 + checksum.

##### A/B commit algorithm (the torn-write fix), `write_to`, `superblock.rs:151`

This is the top reliability mechanism. The superblock carries a generation (`checkpoint_id`); a commit **always writes to the slot holding the oldest generation**, so the other slot retains the previous valid state:

1. **`dev.flush()`** — barrier: all data/objmap blocks are made durable first, so the superblock can never land before the blocks it points to.
2. Read both slots' generations (`ga`, `gb`). If `(None, None)` (format / both empty), write **both** slots — there's no prior state to lose so a backup exists immediately. Otherwise, write **only** the target = the oldest (or corrupt) slot: `(None,_)→A`, `(_,None)→B`, `a<=b→A`, else `B` (`superblock.rs:175-180`).
3. **`dev.flush()`** again — the superblock is durable before the commit counts as succeeded.

A torn write of this commit can only corrupt the *oldest* slot; the newer valid slot survives, and `read_from` falls back to it.

##### Recovery / self-healing

- `read_from` (`superblock.rs:223`): reads both slots, returns the one with the **highest valid `checkpoint_id`**. If the newest is torn, it automatically falls back to the older consistent slot. Both corrupt → `FsError::Corruption`.
- `degraded_slots` (`superblock.rs:192`): returns 0 / 1 / 2 — how many slots are invalid. 1 = still mountable *and* repairable.
- `heal_slots` (`superblock.rs:204`): if exactly one slot is valid, rewrite the corrupt one from the valid copy + flush; returns count healed. Both valid or both corrupt → does nothing (no good source).

**Auto-healing on mount**: `EuroFs::mount` (`disk.rs:261`) calls `heal_slots` whenever `degraded_slots == 1`, so redundancy is silently restored right after mount without a manual `fsck`. Host-tested by `mount_heelt_gedegradeerd_slot_automatisch` (`disk.rs:1291`) and the torn-write fallback test `ab_torn_write_valt_terug_op_vorige_generatie` (`superblock.rs:280`).

#### Inodes

In-memory `Inode` (`disk.rs:85`); on-disk it's one 4 KiB block with this byte layout (offsets from `disk.rs:37-53`):

| Offset | Field | Type |
|---|---|---|
| 0 | `INODE_MAGIC` = `0x4546494E` ("EFIN") | u32 |
| 8 | `oid` | u64 |
| 16 | `parent` | u64 |
| 24 | `otype` (1=file, 2=dir) | u8 |
| 26 | `mode` (POSIX, 0o644/0o755) | u16 |
| 28 | `flags` (immutability) | u32 |
| 32 | `size` | u64 |
| 40 | `inline_len` | u32 |
| 44 | `extent_count` | u32 |
| 48 | `mtime` | u64 |
| 56 | `data_checksum` (XXH3 over full file data, 0 = legacy) | u64 |
| 64 | 8 extents × 16 bytes = `(phys: u64, count: u32)` | — |
| 192 | inline data (cap `INLINE_CAP = 3896` bytes) | — |
| 4088 | XXH3 checksum over `[..4088]` | u64 |

`encode` (`disk.rs:117`) writes this and stamps the inode checksum; `decode` (`disk.rs:143`) rejects on bad magic or checksum mismatch → `FsError::Corruption`. Files ≤ 3896 bytes are stored **inline** in the inode block; larger files allocate a contiguous extent (`write_object`, `disk.rs:556`). Up to 8 extents (`MAX_EXTENTS`).

#### Directories

Directory data is a flat array of 64-byte entries (`DIRENT_SIZE`): `oid: u64` at 0, `otype: u8` at 8, `name_len: u8` at 9, then up to 48 bytes of UTF-8 name at offset 16 (`DIRENT_NAME_CAP = 48`). `read_dir_entries`/`encode_dir_entries` at `disk.rs:585,606`. Path resolution (`resolve`, `disk.rs:620`) walks components from `ROOT_OID = 1`. `rename` (`disk.rs:826`) handles same-dir rename, cross-dir move (rewriting a moved directory's `parent` pointer), target-file replacement, refuses overwriting a directory, and has an anti-loop check preventing a directory from moving into its own subtree.

#### Data-path integrity (checksums)

Two checksum levels, both XXH3-64 (`crates/eurofs/src/checksum.rs`, via the `twox-hash` crate — chosen as pure-Rust no_std, fast bit-rot detection; explicitly *not* cryptographic, `checksum.rs:1-6`):

1. **Inode checksum** at offset 4088 covers the inode block itself.
2. **`data_checksum`** in the inode covers the *entire file contents* across extents (set in `write_object`, `disk.rs:560`). `read_data` (`disk.rs:531`) verifies it on every read and returns `FsError::Corruption` on mismatch — so bit-rot in a *data* block (outside the inode) surfaces as an error instead of silently-wrong bytes. Host-tested by `data_path_scrub_detecteert_bitrot_in_datablok` (`disk.rs:1128`).

#### Copy-on-write, allocator, and the atomic commit

The in-memory allocator is a `Vec<bool>` (`used`, `disk.rs:199`); `alloc_block`/`alloc_contiguous` (`disk.rs:280,290`) find free runs. There is **no persistent bitmap**.

`commit` (`disk.rs:510`) is the atomic checkpoint:
1. `write_objmap` — serialize the OID→block table to **fresh** contiguous blocks (CoW), returning `(root, block_count)`.
2. Set `sb.object_map_root`/`extent_tree_root`, bump `checkpoint_id`, update `last_written` + `free_blocks`, recompute `sb.checksum`.
3. `sb.write_to(dev)` — the A/B commit (flush → write oldest slot → flush).
4. `rebuild_allocator` — re-derive free space, reclaiming the now-orphaned old blocks.

`rebuild_allocator` (`disk.rs:317`) is the space-scan: clear `used`, mark reserved blocks, mark objmap blocks, then for every inode mark its block + all extent blocks, and finally **pin** snapshot state blocks. Crash-consistency is proven by `crash_voor_checkpoint_behoudt_oude_staat` (`disk.rs:1218`): simulating a lost superblock update preserves the old contents.

#### EuroSnap — CoW snapshots

Snapshots are frozen root-pointers (cheap thanks to CoW). The snapshot table lives in reserved block 8 (`SNAPSHOT_TABLE_BLOCK`, `disk.rs:175`), magic `0x5346_4E53` ("SNFS"), up to `MAX_SNAPSHOTS = 32`, each entry 80 bytes (`SNAP_ENTRY_LEN`): id, parent, timestamp, objmap_root, map_blocks, checkpoint_id, flags, 28-char label. `SnapshotEntry` at `disk.rs:182`; load/save at `disk.rs:411,441` (save also flushes).

- **`snapshot_create`** (`disk.rs:731`): commit first (clean atomic root), then push an entry capturing the current `object_map_root`/`extent_tree_root`/`checkpoint_id`, save the table. The label is cut on a char boundary (`disk.rs:749`, audit-fix against UTF-8 split panic).
- **Pinning**: `rebuild_allocator` calls `mark_state_blocks` (`disk.rs:364`) for each snapshot, which walks the snapshot's frozen objmap and marks its table blocks + inode blocks + data extents as in-use — so CoW reclaim never overwrites a frozen state. Proven by `snap_pint_grote_bestand_blokken` (`disk.rs:1451`), which writes 40 KB, snapshots, overwrites + allocates heavily, then rolls back to byte-identical old data.
- **`snapshot_rollback`** (`disk.rs:769`): point `object_map_root`/`extent_tree_root` back at the frozen state, reload the objmap, then `commit` (which writes a fresh objmap+superblock and reclaims the abandoned state's blocks unless pinned by another snapshot).
- **`snapshot_delete`** (`disk.rs:781`): drop the entry, save, commit → GC reclaims exclusively-owned blocks.

`SNAP_AUTO_ROLLBACK` flag (`fs.rs:38`) is intended to auto-delete a snapshot after a successful boot (ties into G4 updates). **Implemented & boot-verified** (`[s]` marker, `main.rs:1360`; host tests `snap_create_modify_rollback`, `snap_overleeft_remount`, `snap_list_en_delete_gc`).

#### IMMUTABLE + APPEND_ONLY flags (filesystem enforcement)

`FLAG_IMMUTABLE = 1<<0`, `FLAG_APPEND_ONLY = 1<<1` (`fs.rs:29,33`), stored in the inode `flags` field, persistent across remount.

- **IMMUTABLE**: `write_file` (`disk.rs:684`), `remove_file` (`disk.rs:716`), and `rename` (`disk.rs:846`) all return `PermissionDenied` — the file cannot be modified, deleted, or renamed *at the FS layer*, independent of POSIX mode or root.
- **APPEND_ONLY**: `write_file` (`disk.rs:687`) requires the new data to *extend* the old (same prefix, `data.len() >= old.len() && data[..old.len()] == old`); truncation or a different prefix → `PermissionDenied`. Deletion is refused. This is the on-disk basis for the tamper-evident audit log.

`set_flags` (`disk.rs:792`) rewrites the inode with the same data + new flags via CoW; the capability check (`CAP_IMMUTABLE_ADMIN`) is layered *above* in the kernel (`immutable.rs`, below). Host-tested by `l1_immutable_blokkeert_wijzigingen` (`disk.rs:1382`), `l1_append_only_alleen_uitbreiden` (`disk.rs:1401`), `l1_vlaggen_overleven_remount` (`disk.rs:1417`).

#### Scrub / fsck (self-healing)

`scrub` (`disk.rs:983`) is a full integrity pass returning a `ScrubReport` (`fs.rs:71`):
1. **Superblock**: `degraded_slots` — 0 ok, 1 = degraded-but-mountable-and-repairable (one error logged), 2 = both corrupt (`superblock_ok = false`).
2. **Inodes + extents** cross-checked against a fresh reference bitmap: every inode's `data_checksum` is verified via `read_data` (data-path scrub, counted as `data_verified`; failures counted `data_unrecoverable` since a single disk has no redundancy — a mirror/RAID "B3" would be needed). Extents are checked for being within the disk, for **cross-links** (a block referenced twice), and for agreement with the in-memory used-bitmap.

`repair` (`disk.rs:1075`) first calls `heal_slots` to restore superblock A/B redundancy, then returns a fresh scrub with `repaired` set. `repair_block` (`fs.rs:184`) is the redundancy-recovery interface — currently `Unsupported` on a single disk (honestly returns `Err`, tested at `disk.rs:1156`). Host-tested: `repair_heelt_gedegradeerd_backup_slot`, `repair_heelt_primair_slot_uit_backup`, `repair_kan_niet_helen_bij_twee_corrupte_slots`.

The kernel-side scrubber `kernel/src/scrub.rs` (`[g5]` marker) runs `fs.scrub()` once at boot (`main.rs:1670`) and rate-limited (~60 s, `INTERVAL_TICKS = 6000`) from the desktop tick (`main.rs:2673`), appending results to `/var/log/fsck.log` on the real EuroVar partition. **Boot-verified.**

#### Bad-block remapping

`crates/eurofs/src/badblocks.rs` — a `BadBlockTable` (magic `0x42425400` "BBT\0") mapping bad LBAs to a spare pool `[spare_base, spare_base+spare_count)`. `translate` (`badblocks.rs:48`) is the hot-path redirect; `mark_bad` (`badblocks.rs:60`) is idempotent and returns `None` when the spare pool is exhausted (block unrecoverable). Serializable (sum-checksum) so the remap survives reboot. **Pure logic, host-tested** (7 tests); it is the building block for J2 but is not yet wired into a live device wrapper in the kernel.

#### Other metadata

`mtime`/`mode` are stored per inode and tracked from a settable kernel clock (`set_clock`, `disk.rs:1071`; `set_clock` flows the RTC epoch in). `list_dir`/`metadata` (`disk.rs:937,962`) surface size/mode/mtime per entry. `space_info`/`df` report total and free bytes.

### GPT partitioning

`kernel/src/gpt.rs` — a minimal GPT reader/writer. Layout: LBA0 = protective MBR (`0xEE` type, `gpt.rs:189`), LBA1 = GPT header (`b"EFI PART"`), LBA2.. = 128×128-byte partition array; first partition at LBA 2048 (1 MiB aligned). A custom **EuroFS type-GUID** (`EUROFS_TYPE`, 16 raw bytes, `gpt.rs:17`) identifies EuroFS partitions.

`install` (`gpt.rs:146`) writes the **A/B layout (G4)**: four EuroFS partitions — `EuroOS-A` (root slot A, 34%), `EuroOS-B` (root slot B for updates, 34%), `EuroVar` (writable data, 20%), `EuroBoot` (kernel images/config, remainder), 4 KiB (8-sector) aligned. It computes CRC32 over the partition array and the header (custom bitwise CRC32, `gpt.rs:20`). Readers verify both the header CRC (with the CRC field zeroed, audit-fix H10 at `gpt.rs:50`) and the array CRC before trusting any LBA fields — a torn/corrupt GPT is rejected rather than trusted. `find_partition_by_name` (UTF-16LE name match) and `find_eurofs_partition` return `(first_sector, 4k_block_count)`. **Implemented & boot-verified** — `main.rs:266` installs or finds the EuroFS partition for the root mount.

### NVMe driver

`kernel/src/nvme.rs` — a minimal NVM Express 1.4 driver, polling (no interrupts), identity-mapped DMA. Found via PCI class 0x01/subclass 0x08/prog-if 0x02 (`nvme.rs:100`). Init (`nvme.rs:99`) resets the controller (CC.EN=0, wait CSTS.RDY=0), allocates admin SQ/CQ (one 4 KiB frame each), programs AQA/ASQ/ACQ, sets CC (IOSQES=6→64B, IOCQES=4→16B, MPS=0, EN=1), waits for RDY, then issues **Identify Controller** (CNS=1, model string) and **Identify Namespace 1** (CNS=0 → capacity `NSZE`, LBA size from `flbas`/`lbads`), and creates an I/O CQ + SQ (qid 1).

`Queue` (`nvme.rs:21`) manages a submission/completion ring with `submit` (64-byte command + tail doorbell) and `wait` (polls the phase tag, 8M-iteration timeout, returns the status field). I/O read (0x02)/write (0x01) via a single 4 KiB PRP1 data buffer (`rw`, `nvme.rs:272`). `read_sectors`/`write_sectors` (`nvme.rs:289,306`) copy through that buffer (≤4096 bytes/op, zero-padding partial sectors on write).

SMART/Health log page (LID=0x02): `smart` (`nvme.rs:339`) returns (temperature K, percent used); `smart_log` (`nvme.rs:362`) returns the full 512-byte page for EuroHealth (Z). `NvmeBlock` (`nvme.rs:385`) wraps the controller as a `BlockDevice` (4 KiB blocks = 8×512 LBAs); its `flush` is a no-op because the poll-on-completion model is already synchronous/durable. `self_test` (`[nvme]` marker, `nvme.rs:431`) writes a pattern to LBA 1000, reads back, verifies, and prints SMART. **Implemented & boot-verified** (when an NVMe device is present; `main.rs:257,1304`).

### virtio-blk driver (with FLUSH barrier)

`kernel/src/virtio_blk.rs` — a legacy virtio-blk-pci driver (PIO via BAR0, split-ring virtqueue). Supports up to `MAX_BLK = 4` disks (root + extra mounts for B3 multi-disk). A block request is a 3-descriptor chain: `[16-byte header | 512..4096-byte data | 1-byte status]` (`submit`, `virtio_blk.rs:216`). `kick_and_wait` (`virtio_blk.rs:244`) places the descriptor in the avail ring, notifies the device, and busy-polls the used ring (works with interrupts off — exactly what a fault handler needs).

**FLUSH barrier** (the durability guarantee): the driver negotiates **only** `VIRTIO_BLK_F_FLUSH` (`virtio_blk.rs:161`) if the device offers it. `submit_flush` (`virtio_blk.rs:264`) sends `VIRTIO_BLK_T_FLUSH` (= 4) — a 2-descriptor request (header + status, no data) that forces the disk's own write-back cache to the persistent medium. `flush()`/`flush_dev` (`virtio_blk.rs:287,292`) expose this; if the device has no FLUSH feature (no volatile cache) it's a successful no-op. This is what makes the EuroFS A/B-superblock barrier a *hard* I/O barrier all the way to the medium.

Hardening: rejects queue size < 3 (would cause OOB descriptor writes, audit C2, `virtio_blk.rs:95`); rejects out-of-range/over-4096 transfers instead of silent truncation (audit C3, `virtio_blk.rs:315,335`). MSI-X is enabled additively (`[j2-blk]`) while the used-ring poll remains the completion source. `self_test` (`[blk]` marker, `virtio_blk.rs:394`) round-trips sector 2048. **Implemented & boot-verified** (`main.rs` root path; `init` logs `[blk] N virtio-blk schijf/schijven geïnitialiseerd`).

### Root block device + write-back cache

`kernel/src/rootblk.rs` — `RootBlk` is the one type that carries EuroFS either in **RAM** (live mode, `RootBlk::ram`) or **directly on a virtio-blk GPT partition** (installed mode, `RootBlk::disk`/`disk_on`). In disk mode it translates EuroFS 4 KiB blocks to disk sectors via `part_start + block*SPB` where `SPB = 8`.

A direct-mapped **write-back block cache** (`CACHE`, 1024 slots × 4 KiB = 4 MiB) sits in front of disk 0, protected by a `RwLock` (J1): a cache **hit** takes only a read-lock so multiple cores read cached FS blocks concurrently; only a miss/write/flush takes the write-lock. `cache_read` (`rootblk.rs:43`) does the fast read-locked hit then a write-locked miss path (double-check, write back the dirty resident, load from disk). `cache_write` (`rootblk.rs:78`) is write-back (stays dirty until flush). `cache_flush` (`rootblk.rs:96`) writes dirty slots out.

`RootBlk::flush` (`rootblk.rs:191`) is the durability chain: for disk 0 it runs `cache_flush()` (dirty cache → disk) **then** `virtio_blk::flush()` (VIRTIO_BLK_T_FLUSH → persistent medium). Extra disks (`dev > 0`) do uncached direct I/O. **Implemented & boot-verified.**

### EuroFDE — full-disk encryption (ChaCha20, TPM-derived key)

`crates/eurofde/src/lib.rs` (`#![forbid(unsafe_code)]`). A transparent FDE layer that wraps any `BlockDevice`: writes encrypt, reads decrypt, so the FS above sees plaintext and the disk sees only ciphertext.

- **Cipher**: ChaCha20 stream cipher (IETF/European standard, no AES-hardware dependency), per-block, length-preserving. Because it's a stream cipher, encrypt == decrypt (XOR the same keystream) — `xcrypt_block` (`eurofde.rs:49`).
- **Key**: `FdeKey` (`eurofde.rs:28`) holds a 256-bit ChaCha20 key + a 32-bit volume salt (against cross-volume nonce reuse).
- **Nonce derivation** (`nonce`, `eurofde.rs:40`): the 12-byte nonce = `[salt(4 LE) | lba(8 LE)]`. Unique per (volume, block), so identical plaintext on different LBAs yields different ciphertext — defeats the watermarking/copy-and-paste attacks that a fixed nonce would allow.
- `EncryptedBlockDevice` (`eurofde.rs:57`): `read_blocks` reads ciphertext then decrypts each block at `start_block + i`; `write_blocks` encrypts into a temporary buffer (leaving the caller's plaintext intact) then writes.

**Key provisioning**: the crate doc (`eurofde.rs:8`) describes the goal as a TPM-sealed key bound to boot PCRs. In the current boot path (`[k3]`, `main.rs:1368`) the 256-bit key comes from **`tpm::get_random(32)`** (the TPM hardware RNG) with a fixed-byte fallback when no TPM is present — so the key is TPM-*sourced* but not yet PCR-*sealed*; the `kdf_params`/`wrapped_key` superblock slots reserved for sealed-key metadata are currently zeroed. The `[k3]` self-test formats a real EuroFS on top of `EncryptedBlockDevice` over a RAM volume, writes `/secret.txt`, and verifies read-after-write, logging `sleutel-van-TPM={from_tpm}`. **Implemented & boot-verified; PCR-sealing is the documented next step (under-claimed).** Host-tested: position-dependent ciphertext, ciphertext-on-disk, and a full EuroFS mounting on the encrypted volume + failing to mount with the wrong key (`eurofde.rs:114-159`).

### EuroUpdate — atomic A/B system updates

`crates/euroupdate/src/lib.rs` (host-tested state machine) + `kernel/src/update.rs` (raw-block persistence + signed apply). Two root slots (A/B); an update is written to the *inactive* slot, tried a bounded number of boots, and auto-rolled-back if it never confirms — the Android/ChromeOS/Fuchsia model, so a bad update can never brick the machine.

#### State machine (`euroupdate` crate)

`SlotConfig` (`lib.rs:78`): `active`, `next_boot`, `tries: u8`, `state_a`/`state_b` (`SlotState` ∈ Empty/Trying/Good/Failed), `generation: u32`. Serializes to a fixed **32-byte** block (`CONFIG_SIZE`), magic `0x45555044` ("EUPD"), Fletcher-32-style checksum (`lib.rs:232`), version byte.

Algorithm:
- `stage_update` (`lib.rs:130`): mark the inactive slot `Trying`, set `next_boot` to it, `tries = DEFAULT_TRIES (3)`, bump generation.
- `on_boot` (`lib.rs:140`, called once per boot before loading the kernel): if `tries > 0`, decrement and boot `next_boot`; else if `next_boot` is still `Trying` (attempts exhausted, never confirmed) → mark it `Failed`, `find_good` another slot, **roll back**; else boot the stable good slot.
- `mark_good` (`lib.rs:161`): called by EuroInit after a successful boot — marks the active slot `Good`, zeroes `tries`.
- `rollback` (`lib.rs:168`): manual forced rollback to the other slot if it's `Good`.

Host-tested: `happy_update_marks_good`, `failed_update_rolls_back_after_tries` (3 tries then auto-rollback to A), `manual_rollback_when_other_good`, `serialize_roundtrip_and_reject_corruption`, `alternating_updates_use_inactive_slot`.

#### Kernel integration (`kernel/src/update.rs`)

The slot config is stored on a **reserved raw LBA = 40** (`SLOT_LBA`, `update.rs:29`) — in the GPT gap, *outside* every EuroFS partition (the array fills LBA 2..33, first partition at 2048). This is deliberate: the anti-brick state must survive filesystem corruption, superblock torn-writes, and an unbootable slot image. It is the source of truth; `/boot/slot_config` is a human-readable FS mirror. `raw_load`/`raw_persist` (`update.rs:34,46`) read/write+flush that sector.

- `boot_init` (`[euroupdate]`/`[g4]`, `update.rs:85`): load config (raw block → FS mirror → `initial`), run `on_boot`, persist, and verify a fresh raw-block read round-trips — proving cross-reboot persistence outside EuroFS.
- `write_image_to_slot` (`update.rs:153`): the real A/B image write — finds the target slot's GPT partition by name (`EuroOS-A`/`EuroOS-B`), refuses if the image exceeds the partition, writes sector-by-sector, flushes, and **read-back-verifies the first sector**.
- `apply` (`update.rs:201`): reads `<image>` + `<image>.sig`, **verifies the Ed25519 signature via EuroGuard** (`crypto::verify`) — an unsigned/tampered update is *refused*. Then writes the image to the inactive slot's partition (falling back to a `/boot/slot_*.img` file if the multi-partition GPT isn't present), `stage_update`, persist.
- `rollback` (`update.rs:261`) and `mark_boot_good` (`update.rs:117`, called when the desktop is reached, `main.rs:1665`).
- `slot_partition_selftest` (`[g4]`, `update.rs:184`, `main.rs:296`): writes a pattern to the unused EuroOS-B partition and read-back-verifies.

**Implemented & boot-verified.**

### Fault-driven swap

`kernel/src/swapmgr.rs` (J3) — the paging half of transparent swap (the CLOCK victim-selection + `SwapArea` live in `euromm::swap`, host-tested). Mechanism: a swapped-out page's PTE is set **non-present** with the swap slot encoded in the upper bits + a `SWAPPED = 1<<9` marker bit (safe because the CPU ignores all other bits when present=0).

- `swap_out` (`swapmgr.rs:120`): walk to the 4 KiB PTE (`walk_pte`, `swapmgr.rs:53`; returns `None` for huge pages / missing levels), allocate a swap slot, write the frame's 8 sectors to `base_lba + slot*8` via `virtio_blk::write_sector` + `flush()`, set the PTE to `(slot<<12) | SWAPPED`, TLB-flush, return the freed frame to a pool.
- `try_swap_in` (`swapmgr.rs:159`): called by the page-fault handler. Uses `try_lock` (avoids deadlock if a nested fault occurs while the lock is held — it runs on the PF IST stack). Checks the `SWAPPED` marker, decodes the slot, pops a free frame, reads the 8 sectors back, restores the PTE present+writable, TLB-flush, frees the slot. The process never notices.

Swap LBA base is `FAULT_SWAP_LBA = 200` (`main.rs:366`), separate from the `[j3]` test region. **Implemented & boot-verified** (`main.rs:374-383` maps a demo page, swaps it out, and reads `swapmgr::stats()`).

### Immutability kernel gate (CAP_IMMUTABLE_ADMIN)

`kernel/src/immutable.rs` — the L2 gate above the L1 FS flags. `set_protected` (`immutable.rs:18`): setting or clearing `FLAG_IMMUTABLE`/`FLAG_APPEND_ONLY` requires `caps & CAP_IMMUTABLE_ADMIN`; otherwise `PermissionDenied` **even for root**, and the denial is recorded to the audit log. Every successful flag change is audited (`ImmutableSet`/`ImmutableCleared`). `protect_system_files` (`immutable.rs:39`) marks shipped binaries + critical config (`/bin/hello`, `/etc/shadow`, `/etc/hostname`, …) immutable. The `[l1]` self-test (`immutable.rs:59`, `main.rs:1329`) proves: (a) the cap-gate on both set and clear, and (b) the FS truly blocks write/remove of an immutable file while reads still work. **Implemented & boot-verified.**

### Append-only audit log ↔ FS APPEND_ONLY (on-disk tamper-evidence)

`kernel/src/audit.rs` (P3) — security events are held in an in-memory ring (`LOG`, monotonic `SEQ`) and persisted to `/var/log/audit.log`, which is marked with the L1 `FLAG_APPEND_ONLY` flag so the filesystem permits **only extension** — earlier lines can't be erased or rewritten, not even by root, and clearing the flag requires `CAP_IMMUTABLE_ADMIN`.

`persist` (`audit.rs:79`) is the key tie-in to the FS flag: it tracks how many events are already on disk (`PERSISTED`) and **appends only the new lines** to the existing on-disk content — so every write is strictly extending (passes the append-only FS check at `disk.rs:687`) and the trail grows monotonically across reboots. After the first write it sets `FLAG_APPEND_ONLY` (cap-gated via `immutable::set_protected`). The `[p3]` self-test (`audit.rs:113`, `main.rs:1335`) proves the trail is irreversible: events are recorded, persisted to an append-only file, a tamper attempt (`write_file(LOG_PATH, b"X")` — shrinking/overwriting) is *refused* by the FS, while a genuine new-event append succeeds and the on-disk line count grows. This is the concrete on-disk realization of NIS2/GDPR tamper-evident logging. (The richer SHA-256 hash-chain audit log for user events is EuroID's; see Part 4.) **Implemented & boot-verified.**

### Part 2 — status summary

| Component | Status |
|---|---|
| EuroFS (CoW, inodes, dirs, extents, checksums, A/B superblock, scrub, snapshots, immutability) | Implemented, host-tested + boot-verified |
| GPT (A/B 4-partition install, CRC-verified read) | Implemented + boot-verified |
| NVMe driver (+SMART) | Implemented + boot-verified (when present) |
| virtio-blk driver (+FLUSH barrier, multi-disk, MSI-X) | Implemented + boot-verified |
| RootBlk + write-back RwLock cache | Implemented + boot-verified |
| EuroFDE (ChaCha20 FDE) | Implemented + boot-verified; key is TPM-**sourced**, PCR-**sealing** is the documented next step |
| EuroUpdate (A/B atomic update, raw-block state, Ed25519-verified apply) | Implemented + boot-verified |
| Swap manager (fault-driven) | Implemented + boot-verified |
| Immutable gate / audit append-only | Implemented + boot-verified |
| Bad-block remapping | Pure logic implemented + host-tested; not yet wired into a live device wrapper |

Boot self-test markers in the storage cluster: `[nvme]`, `[blk]`, `[j2-blk]`, `[gpt]`, `[g2]` (mounts), `[g4]` (slot raw-block + image write), `[g5]` (scrub), `[k3]` (FDE), `[s]` (snapshot), `[j3]`/swap stats, `[l1]` (immutability), `[p3]` (audit).


---

## Part 3 — Networking, Secure Transport & PKI/Crypto

EuroOS ships a complete, from-scratch network and cryptographic stack written in `no_std` Rust with zero dependence on Linux/BSD networking code. The architecture is deliberately **sans-IO / host-testable**: the fiddly, security-critical logic (packet framing, checksums, TLS state machine, key schedules, X.509 parsing) lives in pure crates under `crates/` that compile and test on the host with no NIC or QEMU, while the kernel modules under `kernel/src/` provide the live MMIO transport and wire those crates onto the real virtio-net NIC. Every cluster carries a boot self-test that prints a `[xx]` marker over serial and a matching shell command for live use.

This section is honest about what is cryptographically real and verified versus what is a stub, host-attended, or planned.

### EuroNet — the network stack (`crates/euronet`, `kernel/src/net.rs`, `kernel/src/virtio_net.rs`)

#### Purpose and layering

`crates/euronet` (`src/lib.rs:1`) is RFC-conformant packet parsing/building plus the connectionless control logic (RTT/RTO estimator, Reno congestion control, ICMP rate-limiter, AF_UNIX switchboard). It is `#![forbid(unsafe_code)]`, big-endian via explicit `from_be_bytes`/`to_be_bytes` (never raw casts), and its `NetError` enum is `{TooShort, BadChecksum, Malformed, Unsupported}` (`lib.rs:38`). The **driver and the TCP/socket state machines live in the kernel** (`kernel/src/net.rs`, 1926 lines) on top of the legacy virtio-net driver (`kernel/src/virtio_net.rs`).

#### The NIC: legacy virtio-net (`kernel/src/virtio_net.rs`)

A transitional `virtio-net-pci` (0.9.5) driver using legacy PIO (`disable-modern=on`). Flow:
- PCI scan for vendor `0x1AF4`, device `0x1000`/`0x1041` (`virtio_net.rs:146`); BAR0 = I/O port base; enable I/O + bus-master (`cmd | 0x5`, `virtio_net.rs:171`).
- Status handshake `ACK → DRIVER → DRIVER_OK` (`virtio_net.rs:50`, `:221`); feature negotiation accepting **only** `VIRTIO_NET_F_MAC` (no mergeable RX, no checksum offload, `:183`).
- Two split-ring virtqueues — RX=0, TX=1 — laid out in one contiguous frame-allocator region (`setup_queue`, `:122`; `vring_size`, `:114`). 16 RX buffers of 2048 bytes (`RX_BUFS`/`BUF_SIZE`, `:59`), one synchronously-reused TX buffer.
- Identity-mapped lower 1 GiB means virt==phys, so a frame address is directly the device's physical address (`:8` comment).
- `send()` (`:240`) prepends the 10-byte all-zero `virtio_net_hdr` (`NET_HDR_LEN`, `:61`), drives the avail ring, notifies, and spins on the used ring. `poll_recv()` (`:276`) reads one used RX element, strips the header, recycles the buffer back into the avail ring. `compiler_fence(SeqCst)` orders the ring writes.

#### Ethernet / ARP / IPv4 / IPv6

- **Ethernet II** (`ethernet.rs`): 14-byte header, `MacAddr([u8;6])` with `BROADCAST`/`ZERO`/`is_multicast`, `EtherType` mapping `0x0800/0x86DD/0x0806`.
- **ARP** (RFC 826, `arp.rs`): only Ethernet+IPv4 (htype 1, ptype 0x0800, hlen 6, plen 4) accepted (`arp.rs:52`); `reply_to()` swaps sender/target. Live resolution in `net::arp_resolve` (`net.rs:72`) broadcasts a request and spins on `poll_recv`.
- **IPv4** (`ipv4.rs`): 20-byte header, **verifies the header checksum on parse** (`ipv4.rs:78`) and **refuses fragments** (MF bit or non-zero offset → `Malformed`, `:84`) since the stack does no reassembly — this is a correct, conservative security choice. `is_private()` recognises RFC 1918 ranges.
- **IPv6** (RFC 8200, `ipv6.rs`): 40-byte header, SLAAC link-local via **EUI-64** (`link_local_from_mac`, U/L bit flipped, `:27`), solicited-node multicast, `33:33`-prefixed multicast→MAC mapping, and the IPv6 pseudo-header checksum (`pseudo_checksum`, `:121`).
- **ICMPv6 + Neighbor Discovery** (RFC 4443/4861, `icmpv6.rs`): echo, Router/Neighbor Solicitation builders, and RA/NA option parsing (`ra_info` extracts prefix+router MAC, `na_mac` extracts the target link-layer address).

#### Checksums (`checksum.rs`)

One-complement 16-bit internet checksum (RFC 1071), odd-byte high-padded, with end-around carry folding (`internet_checksum`, `:6`). Used by IPv4, ICMP, UDP, TCP (with pseudo-header) and ICMPv6. `verify()` returns true when the sum over data+checksum is `0xFFFF`.

#### ICMP (`icmp.rs`)

Echo request/reply (RFC 792) with `reply_to`. Also **outbound ICMP errors**: `IcmpError::{DestUnreachable(Host|Port), TimeExceeded}` → type/code `(3,1)/(3,3)/(11,0)` (`:101`), embedding the offending datagram truncated to 28 bytes (IP header + 8) per RFC 792, plus an inverse `parse()`.

#### UDP and DNS

- **UDP** (RFC 768, `udp.rs`): builds/parses with the IPv4 pseudo-header checksum; emits `0xFFFF` when the computed checksum is 0 ("no checksum" disambiguation, `:45`); `parse()` verifies against the supplied src/dst.
- **DNS** (RFC 1035, `dns.rs`): A-record query builder (recursion-desired flags `0x0100`); answer parser that follows compression pointers (`skip_name`, `:67`). Two anti-poisoning measures: `parse_query_name` (used by EuroGuard DNS filtering, refuses pointers in a QNAME) and **`parse_response(buf, expected_id)`** which only returns A-records if the transaction-ID matches **and** the QR bit is set (`:112`).

DNS is wired up in `net::dns_query` (`net.rs:126`) with strong anti-spoofing (RFC 5452 defence-in-depth): both the **16-bit transaction ID and the ephemeral source port (49152–65535) are randomised from `rand_u64()`** (`net.rs:140`), and the reply is validated on src-port 53, the chosen dst-port, the txid and the QR bit (`net.rs:155`). Results feed an S9 **DNS cache** (name→(IP, expiry-tick), TTL 30 000 ticks ≈ 300 s, capped at 32 entries, `net.rs:1775`) with hit/miss counters exposed via `netstat`.

#### TCP — segment layer + the real state machine

**Segment layer** (`crates/euronet/src/tcp.rs`) handles build/parse with the pseudo-header checksum, flag constants `FIN/SYN/RST/PSH/ACK`, `parse_checked()` (rejects bad checksums, `:131`), and a correct **`reset_to()`** implementing RFC 793 §3.4: never reset a reset (avoids RST storms, `:94`); if incoming has ACK then `seq=incoming.ack, flags=RST`, else `seq=0, ack=incoming.seq+seg_len, flags=RST|ACK` where SYN and FIN each count as one sequence number.

**The TCP state machine lives in `kernel/src/net.rs` as `TcpConn`** (`net.rs:358`), a synchronous poll-based connection (fits the non-preemptive ring-3 model). State fields include `my_seq`, `their_seq`, `snd_una` (oldest unacked), `open`, an in-order `rx` VecDeque, and a `retx` buffer of `(seq, bytes)` for retransmission.

Protocol behaviour:
- **Active open / 3-way handshake** (`connect`, `net.rs:418`): ISN `0x1000`, SYN with up to **4 retransmissions** (a lost SYN/SYN-ACK otherwise fails immediately), then on SYN-ACK sets `their_seq = synack.seq+1`, sends ACK, `open=true`.
- **Passive open** (`accept_from`, `net.rs:459`): **randomised ISN** (`rand_u64() | 1`), emits SYN|ACK, retransmits it up to 4 rounds, accepts the closing ACK (and any piggybacked request data), resets on RST.
- **Reliable send** (`send`, `net.rs:564`): segments into 1024-byte MTU-safe chunks (PSH|ACK), records each in `retx`, then **pumps ACKs and retransmits unacked segments** up to 5 rounds.
- **Pump / receive** (`pump`, `net.rs:512`): processes RST (→closed), cumulative ACK via `ack_upto` (wrapping-aware sequence comparison using the `< 0x8000_0000` half-space test, `net.rs:548`), buffers in-order payload and ACKs it, handles FIN (`their_seq+1`, ACK, close). `recv()` (`net.rs:592`) blocks bounded until data or EOF.
- **Teardown** (`close`, `net.rs:603`): FIN|ACK, `my_seq+1`, brief wait for the peer's FIN/ACK.

**Congestion/timing logic** is host-tested in `tcpcc.rs`: an RFC 6298 `RttEstimator` (SRTT/RTTVAR with α=1/8, β=1/4, K=4; `MIN_RTO`=1 s, `MAX_RTO`=60 s; Karn's algorithm; exponential backoff) and an RFC 5681 `RenoCc` (slow-start +MSS/ACK, congestion-avoidance ≈MSS²/cwnd, timeout → ssthresh=max(flight/2, 2·MSS) & cwnd=1·MSS, triple-dup-ACK → fast recovery cwnd=ssthresh). Honest note: this is a complete, independently-tested estimator module; the live `TcpConn` send loop uses fixed round counts rather than driving cwnd/RTO from these estimators — they are wired as a verified library, not yet the live pacing.

#### Inbound packet service and server-side TCP

`net::service()` (`net.rs:256`) is the cooperative RX dispatcher (called each desktop tick). It answers ARP requests for our IP, replies to ICMP echo, and for IPv4 traffic destined to us:
1. Runs the **EuroFW packet filter** first (`firewall::inbound_allowed`); a blocked packet is **silently dropped (stealth, no RST/ICMP)** (`net.rs:289`).
2. TCP SYN handling: if the background HTTP server is on and dst-port 80 → `serve_connection`; else if a userspace LISTEN socket has room → passive open via `try_accept_listener` (`net.rs:235`); else send a rate-limited RST ("connection refused").
3. Unsolicited UDP → ICMP **port unreachable** (rate-limited).

Outbound ICMP/RST errors are governed by a **token-bucket rate-limiter** (`ratelimit.rs` `TokenBucket`, fixed-point, monotonic ticks) configured at 20/s, 20-burst (`net.rs:227`) — anti-amplification against spoofed-source reflection.

#### Socket layer + HTTP

A BSD-style socket API (`net.rs:848`) backs the Linux syscalls socket/bind/listen/accept/connect/send/recv/close, with socket fds from `SOCK_FD_BASE=500` (`net.rs:854`), 16 slots, and a `Sock` enum `{Reserved, Conn(TcpConn), Udp(UdpSock), Listen{...}}`. `sock_connect` consults **EuroGuard policy before any packet leaves** (`net.rs:952`) and `sock_send` does **DNS-level ad/tracker filtering** on port-53 datagrams (`net.rs:1127`); both record per-app byte stats. `sock_poll`/`sock_accept` (`net.rs:1216`, `:1065`) multiplex over fds with tick-deadline + spin ceilings so poll never blocks forever.

HTTP: `http_get` (HTTP/1.0 GET, `net.rs:614`), a background cooperative HTTP/1.1 server (`serve_connection`/`http_page`, toggled by `httpd`, `net.rs:1436`), `tcp_serve_once`, plus `http_download`/`http_fetch`/`http_post_raw`/`fetch_full` used by EuroWeb and the EuroAgent→Ollama path. `resolve()` honours `/etc/hosts` before DNS.

**AF_UNIX** (`unix.rs` `Switchboard`, wired at `net.rs:1264`): a single in-kernel switchboard owning all connections (no Rc/Arc), crossed byte-FIFOs per connection, POSIX-style errnos (`ConnRefused/AddrInUse/Backlog/BadEndpoint/BrokenPipe`), EOF-after-close semantics, slot reclaimed when both sides close. This is the building block for the live display server.

#### Entropy

`rand_u64()` (`net.rs:670`) uses RDRAND when CPUID advertises it, else mixes RDTSC ⊕ HPET ⊕ a counter — **honestly labelled functional-but-not-cryptographically-strong** on TCG/QEMU (`net.rs:644` comment). For TLS key material, `gather_entropy()` (`net.rs:687`) additionally folds in **TPM `get_random(32)`** when present and runs the whole pool through SHA-256 — this is the strong source for the ephemeral X25519 secret.

#### Self-tests / shell

`[g3]` poll/select (`poll_selftest`), `[h1]` AF_UNIX round-trip with EOF (`af_unix_selftest`). Shell commands: `ping`/`ping6`, `fetch`/`wget`, `https`, `serve`, `tcpserve <port>`, `netstat`, `net` (`shell.rs:347`–`:390`).

### EuroTLS — TLS 1.3 client (`crates/eurotls`, `kernel/src/tls_roots.rs`)

#### Purpose and ciphersuite

`crates/eurotls` (`lib.rs:1`) is a sans-IO TLS 1.3 client (RFC 8446): the kernel feeds received records in and gets bytes to send out. **Single ciphersuite `TLS_CHACHA20_POLY1305_SHA256` (0x1303)**, key exchange **X25519** (group 0x001d), advertised signature schemes Ed25519/ECDSA-P256/RSA-PSS-SHA256/RSA-PKCS1-SHA256 (`lib.rs:27`).

It depends on the RustCrypto family (`hkdf`, `sha2`, `hmac`, `chacha20poly1305`, `x25519-dalek`, `ed25519-dalek`, `p256`, `p384`, `crypto-bigint`) — so the AEAD, HKDF, X25519 and curve operations are vetted implementations, while the **protocol framing, key schedule wiring, X.509 parsing, chain logic and RSA padding are EuroOS's own code**.

#### Record layer (`record.rs`)

TLSPlaintext/TLSCiphertext framing `type(1)‖0x0303‖len(2)‖fragment`. `read_record` enforces `MAX_RECORD_LEN = 2^14+256` and returns `Err(RecordOverflow)` on an over-long claim (RFC 8446 §5.1 `record_overflow`, `:32`). `aead_aad()` builds the 5-byte AAD (outer type application_data + ciphertext length).

#### AEAD (`aead.rs`)

ChaCha20-Poly1305 (RFC 8439). The per-record nonce is `static_iv XOR seq` right-aligned in the 12-byte IV (RFC 8446 §5.3, `nonce()`, `:13`). `seal`/`open` return ciphertext‖tag (16-byte Poly1305 tag) / None on auth failure. Tests cover tampered ciphertext, tampered tag, wrong seq, wrong AAD.

#### Key schedule (`keyschedule.rs`)

Full RFC 8446 §7.1 schedule over SHA-256: `hkdf_extract`, `hkdf_expand_label` (with the mandatory `"tls13 "` label prefix and the `HkdfLabel` struct, `:24`), `derive_secret`, and a running `Transcript` (clones the SHA-256 hasher to read without finalising). `KeySchedule` builds Early → Handshake → Master secrets:
- `derive_handshake(ecdhe, th)`: Handshake Secret = `HKDF-Extract(Derive-Secret(ES,"derived",""), ECDHE)`, then `c hs traffic` / `s hs traffic` (`:130`).
- `derive_application(th)`: Master Secret = `HKDF-Extract(Derive-Secret(HS,"derived",""), 0)`, then `c ap traffic` / `s ap traffic` (`:140`).
`TrafficKeys::derive` expands `key`(32), `iv`(12), `finished`(32). The empty-transcript and derived-secret constants are pinned to the known RFC vectors (`e3b0c442…`, `33ad0a1c…`, `6f2615a1…`, `:160`–`:200`) — a regression guard proving the schedule is byte-correct.

#### Handshake state machine (`handshake.rs`)

`Tls13Client` states `Start → WaitServerHello → WaitServerFinished → Connected | Failed` (`:34`), with separate server/client epochs (None/Handshake/Application) and per-epoch sequence numbers.

Flow:
- `new()` builds the **ClientHello** (`build_client_hello`, `:155`): legacy_version 0x0303, 32-byte random reused as legacy_session_id (middlebox-compat), the one ciphersuite, and extensions SNI(0x0000), supported_versions(TLS 1.3), supported_groups(x25519), signature_algorithms, key_share(x25519 pubkey from `x25519-dalek`).
- `process()` (`:220`) reads records: ChangeCipherSpec ignored (middlebox-compat), Alert surfaces as `TlsError::Alert(code)`, plaintext Handshake = ServerHello, encrypted application_data is AEAD-opened with the current server epoch keys then `split_inner` strips zero padding and reads the inner content type (RFC 8446 §5.4).
- `handle_server_hello` (`:332`) extracts the server key_share, runs **ECDHE** (`self.secret.diffie_hellman`), derives handshake traffic keys over transcript CH‖SH.
- Certificate → `parse_certificate` collects the whole DER chain (leaf first); `validate_chain()` runs.
- CertificateVerify → `verify_certificate_verify` (`:466`) reconstructs the RFC 8446 §4.4.3 signed blob (64×0x20 ‖ `"TLS 1.3, server CertificateVerify"` ‖ 0x00 ‖ transcript-hash) and verifies against the leaf key for the negotiated scheme (ECDSA-P256 / Ed25519 / RSA-PSS-SHA256).
- Finished → the **server Finished MAC is verified** as `HMAC(server_hs.finished_key, transcript-hash)` (`:318`, always enforced regardless of cert validation). Then `finish_handshake` derives app keys, sends CCS + the client's encrypted Finished, and switches to the application epoch → `Connected`.
- `encrypt_app`/`take_app_data` carry application data.

#### Certificate validation — honest status

This is **real and enforced when the trust anchor is set**, which the kernel does for every `https` call via `tls.set_trust_anchor(rtc::epoch(), tls_roots::ROOTS)` (`net.rs:734`).

- **X.509 parser** (`x509.rs`): a from-scratch DER/ASN.1 parser whose hard rule is *never panic on untrusted bytes* — definite-length only (indefinite `0x80` rejected, `:80`), non-minimal lengths rejected, every access bounds-checked; a fuzz-style test parses every prefix of a real cert without panicking (`:569`). It extracts tbsCertificate (the signed bytes), serial, issuer/subject DER, validity (UTCTime/GeneralizedTime via Howard Hinnant's `days_from_civil`, `:128`), SPKI (EcP256/EcP384/Rsa/Ed25519), the outer signatureAlgorithm, SAN dNSNames, and basicConstraints CA flag. Hostname matching supports exact + single-label wildcard (`*.example.com`, `:458`).
- **Signature verification** (`sig.rs`): `verify(sig_alg, signer_alg, key, msg, sig)` dispatches to ECDSA-P256/SHA-256, ECDSA-P384/SHA-384 (RustCrypto `p256`/`p384`), Ed25519 (`ed25519-dalek`), and **hand-rolled RSA** PKCS1-v1.5-SHA256/384 and **RSA-PSS-SHA256** (MGF1-SHA-256, salt 32) built on `crypto-bigint` `U4096` modular exponentiation (`rsa_public_op`, `:184`) with explicit EM/DigestInfo padding checks. Algorithm and key type **must match** or it returns false. Tests verify against real OpenSSL-generated PSS signatures and a real SSL.com root→intermediate chain (`chain.rs:240`).
- **Chain validation** (`chain.rs`): a simplified RFC 5280 §6.1 path validation — leaf time-window + SAN hostname, then walk the chain anchoring as soon as a cert's issuer is a trusted root, checking name chaining (issuer DER == issuer subject DER), `basicConstraints CA:TRUE` on issuers, each issuer's validity window, and the per-step signature; tolerant of extra cross-sign certs. Errors map cleanly (`Expired/HostnameMismatch/BrokenChain/IssuerNotCa/BadSignature/UnknownCa`).
- **Trust store** (`kernel/src/tls_roots.rs`): 30 bundled `&'static` DER roots, **EU-first** (D-TRUST, Certigna, Buypass, SwissSign, QuoVadis, GlobalSign) plus the major international CAs (ISRG/Let's Encrypt X1/X2, DigiCert, SSL.com, USERTrust, Comodo AAA).

Honest caveats: there is **no OCSP/CRL revocation checking, no name-constraint or path-length enforcement, and no SCT/CT verification**; key-usage is parsed but not enforced (`x509.rs:420`). When `set_trust_anchor` is *not* called (host tests), validation is skipped but the Finished MAC and the ECDHE binding still make the handshake genuine.

#### Self-test / shell

Exercised live by `cmd_https` (`net.rs:805`): resolves via DNS, runs the X25519 + ChaCha20-Poly1305 handshake against a real HTTPS server, and prints the server cert length + SHA-256 fingerprint. Shell: `https <host>`.

### Kernel crypto primitive (`kernel/src/crypto.rs`)

A focused **verify-before-execute** primitive: Ed25519 verification of program bytes against the baked-in EuroOS developer public key (`EUROOS_PUBKEY`, included from `toolchain/eupkg/keys/dev.pub`, `:13`) using `ed25519-dalek`'s `verify_strict` (rejects non-canonical signatures, `:27`). This replaced an XXH3 integrity check with real authenticity+integrity; only signed, unmodified code runs in ring 3.

### EuroFW — stateful packet filter (`kernel/src/firewall.rs`, `crates/eurofw`)

`crates/eurofw` (`lib.rs:1`, `#![forbid(unsafe_code)]`) is a **5-tuple, first-match-wins rule engine**. `Rule` (action, direction In/Out/Both, proto Any/Icmp/Tcp/Udp, optional src/dst CIDR, optional src/dst port; `None` = wildcard) with a builder API; `cidr_match` does prefix masking. `Firewall::verdict` returns the first matching rule's action (or the default policy) and updates accepted/dropped counters; `peek` is the non-mutating variant.

The kernel side (`firewall.rs`) holds a global `Mutex<Option<Firewall>>`, defaults to **ACCEPT** with a sane block-list: inbound Telnet:23 and NetBIOS:139 dropped (stealth), plus an example block of `198.51.100.0/24` (`firewall.rs:16`). `inbound_allowed()` is called per inbound IP packet from `net::service` (`net.rs:289`); blocked packets are silently dropped. Honest note: despite the section title, this is a **stateless** filter — there is no connection-tracking table; a stricter default-deny is a policy choice deferred to EuroPol (and the host crate's tests demonstrate a default-deny allowlist configuration).

Self-test `[n3]` (`selftest`) proves Telnet-blocked / HTTPS-allowed / blocklist-source-rejected. Shell: `firewall` / `eurofw`.

### EuroVPN — sovereign VPN (`kernel/src/vpn.rs`, `crates/eurovpn`)

`crates/eurovpn` (`lib.rs:1`) is a WireGuard-style, forward-secret, mutually-authenticated tunnel — **deliberately not byte-compatible with WireGuard** (which needs BLAKE2s; this is the sovereign variant on the same principles, `:13`). Key derivation is HKDF-SHA256.

- **Identity**: static X25519 keypair from a 32-byte seed (`from_seed`, ideally the TPM RNG).
- **Handshake**: a Noise-like authenticated KEX using a **quadruple Diffie-Hellman** — `e_i·e_r`, `e_i·S_r`, `s_i·e_r`, `s_i·S_r` (`PendingInitiator::finish`, `:108`; `respond`, `:121`). The ephemeral DHs give forward secrecy; the static DHs give mutual authentication. The four shared secrets are concatenated as IKM into `Hkdf::<Sha256>::new(salt=b"EuroVPN-v1", ikm)` and expanded with labels `i2r`/`r2i` into directional keys (`derive`, `:63`).
- **Transport**: ChaCha20-Poly1305 with a per-packet counter nonce (8-byte LE counter prefixed to the packet, placed in nonce bytes 4..12, `:190`). `decrypt` verifies the Poly1305 tag first, then runs a **WireGuard-style 64-bit sliding-window anti-replay** check (`recv_ctr` high-water + `replay_window` bitmask, `:160`) rejecting both too-old and already-seen counters.

Tests verify matching keys, out-of-order acceptance, replay rejection, tamper detection, and that a wrong peer (Eve substituting her static key) cannot derive the session. Self-test `[n2]` (`vpn.rs:24`) runs a full initiator+responder handshake + encrypted round-trip with TPM-seeded keys. Shell: `vpn` / `eurovpn` prints the local public tunnel key.

### EuroWiFi — 802.11 protocol core (`kernel/src/wifi.rs`, `crates/eurowifi`)

`crates/eurowifi` is the **protocol core only** — frame parsing and key derivation; the radio driver is explicitly hardware work.

- **Frame parsing**: 24-byte 802.11 header (`parse_header`, frame type + management subtypes incl. Beacon/Probe/Auth/Assoc/Deauth, addr1/2/3). `parse_beacon` walks tagged information elements (SSID id 0, DS-param/channel id 3, RSN id 48), reads the privacy capability bit, and detects WPA3 via the **SAE AKM suite `00-0F-AC-08`** (`has_sae_akm`, `:158`). Hardened against short frames (≥36 bytes required, audit C4).
- **WPA2/3 key derivation**: the IEEE 802.11 PRF on a from-scratch **HMAC-SHA-256** (`hmac_sha256`, `:164`, manual ipad/opad). `derive_ptk` (`:210`) computes the **Pairwise Transient Key** (48 bytes = KCK‖KEK‖TK for CCMP) from `min/max(AA,SPA) ‖ min/max(ANonce,SNonce)`, making it direction-symmetric (AP and client derive the same PTK).

**Honest status**: this is the verified protocol kernel; the iwlwifi-style radio bring-up (firmware load → MAC/PHY init → TX/RX DMA rings → scan → 4-way handshake) requires real Intel hardware and is **hardware-attended, not a false checkmark**. Self-tests: `[n1]` parses a synthetic WPA3 beacon and proves PTK derivation; `[bb3]` (`bb3_selftest`, `:77`) PCI-probes for a real Intel WiFi device (AX200/210/etc.) and honestly reports "no radio in QEMU." Shell: `wifi` / `eurowifi`.

### EuroTPM — TPM 2.0 (`kernel/src/tpm.rs`, `crates/eurotpm`)

`crates/eurotpm` is the architecture-independent, byte-exact **command encoder / response parser** for TPM 2.0 (big-endian): `startup()` (Startup CLEAR), `get_random(n)`, `pcr_read(pcr)`, and `pcr_extend(pcr, digest)` with a proper password-auth session (`TPM_RS_PW`, 9-byte authArea) and a SHA-256 `TPML_DIGEST_VALUES` (`lib.rs:79`). Parsers walk the variable-length `TPML_PCR_SELECTION`/`TPML_DIGEST` structures safely (`parse_pcr_read`, `:136`). Encodings are pinned by tests.

The kernel side (`tpm.rs`) is the **TIS MMIO transport** at the fixed base `0xFED4_0000` (locality 0): locality request, FIFO write with burst-count flow control, `TPM_GO`, and response read driven by the size field in the 10-byte header (`transact`, `:83`). `init()` reads DID_VID to detect the chip and issues Startup (tolerating `TPM_RC_INITIALIZE` if firmware already started it). Public API: `get_random(n)`, `read_pcr(index)` (SHA-256, 32 bytes). Self-test `[o1]` proves a live TPM via GetRandom + measured boot: **read PCR 16 → extend with a digest → read again and confirm it changed** (`selftest`, `:209`). The TPM RNG is the strong entropy source feeding TLS (`gather_entropy`), VPN, CA and attestation seeds.

### EuroCA — sovereign local CA (`kernel/src/ca.rs`, `crates/euroca`)

`crates/euroca` is a sovereign certificate authority using a **compact own format (not X.509/ASN.1 — that's the EuroTLS compat layer)**, crypto = Ed25519 (`ed25519-dalek`) + SHA-256 fingerprints. A `Certificate` carries serial, subject, subject_key, issuer, validity window, `is_ca`, and a 64-byte Ed25519 signature over **domain-separated canonical TBS bytes** (`DOMAIN = b"EuroCA-cert-v1\0"`, length-prefixed fields, `:68`). `CertAuthority` supports `new_root` (self-signed CA from a seed), `issue` (signs a CSR, clamping validity within the CA's own window), `revoke`/`is_revoked`, and `verify_issued` — the full chain check: CA must be a valid self-signed CA, cert not revoked, leaf signature + window verify against the CA key. Tests cover tamper, expiry/not-yet-valid, revocation, wrong issuer key, and fingerprint stability.

Self-test `[ca]` (`ca.rs:18`): TPM-seeded root → issue `vpn.euro-os.eu` cert → verify → revoke → verify now fails. Shell: `euroca`.

### EuroAttest — remote attestation (`kernel/src/attest.rs`, `crates/euroattest`)

`crates/euroattest` implements zero-trust remote attestation: a machine proves a trusted state with a signed **quote** = current PCR values + a verifier-chosen nonce, signed by the attestation key (AK). `Pcr = (u8, [u8;32])`; `quote()` sorts PCRs by index and signs the domain-separated TBS (`DOMAIN = b"EuroAttest-quote-v1\0"` ‖ nonce ‖ count ‖ per-PCR idx+hash, `:52`) with Ed25519. `verify()` checks the AK signature, the nonce match (anti-replay), and that every expected PCR is present and exactly equal — returning typed errors `{BadSignature, NonceMismatch, PcrMismatch{index}, PcrMissing{index}, BadKey}`.

The kernel side reads **real PCRs** (0 and 16) via `tpm::read_pcr` with a synthetic fallback. Self-test `[o2]` (`attest.rs:16`): fresh quote accepted, replay with a different nonce rejected (`NonceMismatch`), tampered PCR state rejected (`PcrMismatch`). Shell: `euroattest` shows the AK and current PCRs.

### EuroSign — document signing (`crates/eurosign`)

A sovereign document-signing format (Sprint AC-4). It is deliberately **crypto-free** (the Ed25519 operation is delegated to eurotls/EuroVault) and host-tested. It provides: a canonical `SignManifest` (doc name, lowercased hex doc-hash, signer, timestamp, purpose) whose `canonical_bytes()` is a stable `EuroSign-v1` key=value blob signed bit-for-bit reproducibly (`:45`); a textual `.eurosig` envelope (`-----BEGIN/END EUROSIG-----`, manifest + hex signature + hex pubkey + optional visual `VisualAnchor`) with `to_text`/`from_text` round-tripping; and `verify()` which takes a caller-supplied Ed25519 checker closure and returns `Verdict::{Valid, DocumentTampered, BadSignature}` — distinguishing a valid signature over an altered document from a bad signature.

### Part 3 — cross-cutting honesty summary

- **Real and verified**: all packet framing + checksums (RFC-vector tested); the TCP active/passive open, reliable send/retransmit, wrapping-aware ACK, and teardown; DNS anti-spoofing (random txid+port, txid/QR validation); TLS 1.3 handshake incl. real X25519 ECDHE, ChaCha20-Poly1305 AEAD, the SHA-256 key schedule (pinned to RFC vectors), Finished MAC, **and certificate-chain validation (parse + per-step signature + hostname + validity) against 30 bundled EU-first roots**, including hand-rolled RSA-PKCS1/PSS verified against OpenSSL signatures; the VPN's quadruple-DH + HKDF + ChaCha20-Poly1305 with sliding-window anti-replay; WiFi PTK derivation (IEEE PRF/HMAC-SHA-256); TPM measured-boot (GetRandom + PCR extend proven against a live chip); CA issue/verify/revoke; and attestation quote/verify.
- **Stub / simplified / planned**: TLS cert validation lacks **OCSP/CRL, CT/SCT, name-constraints, path-length, and key-usage enforcement**; the firewall is **stateless** (no connection tracking); the Reno/RTO estimator is tested but not yet driving the live send loop; the RDTSC entropy fallback is functional-not-strong (mitigated by TPM RNG); TLS 1.3 session resumption (NewSessionTicket) is ignored.
- **Hardware-attended (honestly not a false checkmark)**: the WiFi **radio driver** (QEMU emulates no 802.11 radio) — only the protocol core runs in software.


---

## Part 4 — Security Model, Identity & Observability

EuroOS does not inherit a Unix security model — there is no ambient root, no setuid bit, no `/etc/sudoers` that grants a process the whole machine. The native primitive is the **capability**: a process is born with exactly the rights it needs, the kernel checks each one at the syscall boundary, and rights can be **dropped but never regained**. On top of that primitive sit a declarative policy engine (EuroPol), an encrypted secrets store (EuroVault), a from-scratch sovereign identity system (EuroID/EuroIDM), capability-scoped sandboxes (EuroSandbox), and a tamper-evident audit trail.

A note on naming before the detail: the kernel module named `euroguard.rs` is a **network policy / DNS-blocklist / per-app firewall**, not the capability engine. The actual capability bitset, syscall gating, W^X/SMEP/SMAP enforcement and Ed25519 code-authenticity all live in `kernel/src/ring3.rs` and `kernel/src/crypto.rs`. "EuroGuard" is used in the codebase as the *name of the capability model*, but its enforcement is in `ring3`. Where the docs say "EuroGuard-capability" they mean the `ring3` bitset.

### EuroGuard — the capability security model (`kernel/src/ring3.rs`, `kernel/src/crypto.rs`)

#### Purpose

Enforce least-privilege at the syscall edge: every process runs with a capability bitmask; a syscall that needs a capability the process lacks returns `-EPERM` before doing anything. This holds for both the native ABI and the Linux-compat ABI. Combined with hardware protections (SMEP/SMAP/NX → W^X) and Ed25519 verify-before-execute, an installed binary is authentic, cannot escalate, and cannot execute writable memory.

#### Capability bitset

Defined in `ring3.rs:23-27` — a `u64` bitmask, with the live set held in a single atomic:

```
CAP_CONSOLE          = 1<<0   // console write
CAP_PROC_INFO        = 1<<1   // getpid/uname
CAP_FILE             = 1<<2   // open/read/close
CAP_NET              = 1<<3   // network
CAP_IMMUTABLE_ADMIN  = 1<<4   // set/clear immutability flags (L2)
```

`static CURRENT_CAPS: AtomicU64` (`ring3.rs:29`) holds the running process's mask. `has_cap(c)` (`ring3.rs:70-72`) is `CURRENT_CAPS & c == c`. Note this is a *single-process* model — userspace runs largely before the scheduler, so there is one current cap-set, not a per-task field; that is an honest current limitation, not a multi-process capability table.

#### "Drop but never regain"

Capabilities are assigned exactly once, at program launch, by storing into `CURRENT_CAPS` (`ring3.rs:2368`, `ring3.rs:3229`). There is **no syscall to raise capabilities** — `required_cap`/`linux_required_cap` only ever *gate*, and nothing writes `CURRENT_CAPS` from a syscall handler. The program registry `PROGRAMS: Vec<(path, caps, linux_abi)>` (`ring3.rs:243`) records the *grant* per installed binary (`register_program`, `ring3.rs:246`); a fork/clone inherits, an exec re-derives from the registry. The combination with EuroPol (below) is strictly monotone-reducing: `(base | allow) & !deny`.

#### Capability-scoped syscalls

Native dispatch (`ring3.rs:2604-2609`):

```rust
let need = required_cap(num);
if need != 0 && !has_cap(need) {
    serial_println!("[cap] syscall {num} GEWEIGERD — ontbrekende capability");
    return u64::MAX; // -EPERM
}
```

`required_cap` (`ring3.rs:75-83`) maps native syscall numbers → required bit (write→CONSOLE, getpid/uname→PROC_INFO, open/read/close→FILE, net→NET). The **Linux-compat ABI is gated identically** (`linux_dispatch`, `ring3.rs:2700-2707`) via `linux_required_cap` (`ring3.rs:2685-2698`), which additionally routes socket-fd read/write/close to `CAP_NET` rather than `CAP_FILE` (`ring3.rs:2688-2690`) so a network program needs only `CAP_NET`. This is the concrete realisation of "least privilege applies even to musl/Linux binaries."

User-pointer validation is a separate defence layer: `ARENA_BASE`/`ARENA_SPAN` bound every user pointer to the process's 2 MiB arena (`in_user_arena`, `ring3.rs:58-68`), so a syscall argument cannot point the kernel at kernel memory.

#### W^X / SMEP / SMAP enforcement

`enable_smep_smap()` (`ring3.rs:3131-3156`) reads CPUID leaf 7 EBX (bit 7 = SMEP, bit 20 = SMAP) and sets the corresponding CR4 flags when supported; status is recorded in `SMEP_ON`/`SMAP_ON`. SMEP stops ring 0 from ever *executing* a user page; SMAP stops ring 0 from *touching* user pages except inside a deliberate, short AC-window opened in the syscall entry path (RFLAGS.AC toggled around the handler — `ring3.rs:1450`, `ring3.rs:1505`). NX/W^X: `init_syscall_msrs` (`ring3.rs:3173-3198`) checks CPUID `0x80000001` EDX bit 20 and sets `EFER.NXE`; without it the PTE NX bit is inert, with it data/stack/heap are non-executable.

W^X is per-page in the process arena. The ELF loader records two 8×u64 bitmaps — `exec_pages` (PF_X) and `writ_pages` (PF_W) — over the 512 4 KiB pages of the arena (`ring3.rs:2034-2035`, `2065`). `paging::arena_set_wx(pt, exec_pages, writ_pages)` (`paging.rs:389`) then applies R-X to code pages and RW+NX to the rest; fork clones the parent's W^X arena PT (`fill_remap_tables_wx`, `paging.rs:192`). Honest caveat the code itself notes: a binary with a *mixed* RWE segment cannot be W^X-separated and falls back to RWX (`build_address_space_rwx`, `paging.rs:113`; comment at `ring3.rs:1696-1697`).

`smep_active()`/`smap_active()`/`nx_active()` (`ring3.rs:3159-3171`) back the `hardening` shell line (`shell.rs:633-635`).

#### Code authenticity — Ed25519 over binaries

`crypto.rs` is real Ed25519 (`ed25519-dalek`), not a checksum:

- `EUROOS_PUBKEY: [u8;32]` is `include_bytes!`-baked from `toolchain/eupkg/keys/dev.pub` (`crypto.rs:13`).
- `verify(msg, sig)` (`crypto.rs:17-28`) uses `verify_strict`, which additionally rejects non-canonical/weak signatures.
- `verify_program(path, bytes)` (`ring3.rs:1867-1872`) looks up the baked 64-byte `.sig` for the path (`program_sig`, `ring3.rs:1829-1863`, ~30 signed binaries) and verifies it over the *actually-loaded* bytes; unknown path → untrusted → `false`.

Enforcement is real: `init.rs:50` refuses to launch a service whose signature fails; `main.rs:1000` gates every exec; the boot path loads `/bin/hello`, verifies it, then runs a **tamper test** — flipping one byte and showing the verify is rejected (`main.rs:916-924`).

#### Boot self-tests + shell

- `[cap]` lines print on any denied syscall.
- `[sec]` SMEP/SMAP line at hardware-enable (`ring3.rs:3149`).
- Verify-before-execute + tamper rejection in the `[euro]` boot lines (`main.rs:908-924`).
- Shell `caps` / `euroguard` (`shell.rs:276-294`): lists each installed program, its ABI (EuroOS-native vs linux-compat), and decoded capabilities, then the EuroGuard network policy. `hardening` shows live SMEP/SMAP/NX state.

### EuroPol — declarative capability policy engine (`crates/europol`, `kernel/src/europol.rs`)

#### Purpose

Administrators reason in policy ("firefox may not touch system files"), not in bits. EuroPol compiles a readable TOML-ish policy into a capability mask + path rules, where **deny always wins**, and the kernel enforces the resulting mask. Pure `no_std`, `#![forbid(unsafe_code)]`, host-tested.

#### Data structures

`Policy` (`europol/src/lib.rs:57-64`): `name`, `allow_caps: u64`, `deny_caps: u64`, `allow_paths: Vec<String>`, `deny_paths: Vec<String>`, `log_denied: bool`. The cap bits (`CAP_CONSOLE..CAP_IMMUTABLE_ADMIN`, `lib.rs:19-23`) are deliberately mirrored from `ring3` so the policy mask is directly usable as a `ring3` mask.

#### Capability derivation (the algorithm)

- `effective_caps(base)` = `(base | allow_caps) & !deny_caps` (`lib.rs:69-71`) — allows can add, denies subtract, deny dominates.
- `check_cap(cap)` (`lib.rs:74-82`): deny-bit → `Deny`; else allow-bit → `Allow`; else **default-deny**.
- `check_path(path)` (`lib.rs:85-93`): a deny-prefix wins; else an allow-prefix; else default-deny.
- `parse()` (`lib.rs:111-154`): a small hand-written parser for `name`, `[allow]`/`[deny]` sections, `capabilities=[...]`, `paths=[...]`, `log_denied`.

The kernel side (`europol.rs:28-33`) installs the active policy in a `Mutex<Option<Policy>>` and exposes `effective_caps(base)` to the syscall path.

#### Self-test + shell

`europol::selftest()` (`europol.rs:37-57`, marker `[x]`) parses the baked firefox policy, proves `CAP_IMMUTABLE_ADMIN` is stripped while `CAP_NET` survives, proves `/etc/shadow` is denied by path, and records a **policy violation** to the P3 audit trail (`audit::record(Event::CapDenied, ...)`). Host tests (`lib.rs:166-227`, 5) cover parse, deny-wins-over-allow, cap checks, path checks, and human-readable `explain`. Shell `europol` / `europol explain <CAP>` (`europol.rs:60-85`, `shell.rs:186`).

### EuroVault — capability-gated encrypted secrets store (`crates/eurovault`, `kernel/src/vault.rs`)

#### Purpose

Store secrets (DB passwords, TLS keys) bound to a `read_caps` capability requirement, encrypted at rest with ChaCha20-Poly1305 under a (TPM-generated, sealable) master key. A read without the right capability returns `PermissionDenied` even if you know the label.

#### Data structures

`Secret { label, value: Vec<u8>, read_caps: u64 }` (`eurovault/src/lib.rs:32-36`). The `value` is **zeroized on drop** via `core::ptr::write_volatile` (`lib.rs:38-45`) — the crate's single, documented use of `unsafe`, to defeat the optimizer. `Vault { secrets: Vec<Secret> }`.

#### Capability gate

`get(label, caller_caps)` (`lib.rs:75-81`): `NotFound` if the label is unknown; `PermissionDenied` if `read_caps != 0 && (caller_caps & read_caps) != read_caps`; else a clone of the value (caller zeroizes its own copy). `list()` returns only labels + cap requirements, never values (`lib.rs:84-86`).

#### Seal / unseal (the algorithm)

- `serialize()` (`lib.rs:96-108`): length-prefixed `count ‖ {label_len,label,read_caps,value_len,value}*`.
- `seal(master_key, nonce)` (`lib.rs:142-151`): `ChaCha20Poly1305::encrypt(nonce, serialize())`; output blob = `nonce(12) ‖ ciphertext+tag`. Tamper-evident via the Poly1305 tag.
- `unseal(blob, master_key)` (`lib.rs:155-164`): split nonce/ciphertext, `decrypt` (fails `Decrypt` on wrong key or any modified byte), then `deserialize` with bounds-checked readers (`Corrupt` on truncation).

#### Self-test + shell

`vault::selftest(master_key, from_tpm)` (`vault.rs:35-85`, marker `[u]`) proves: read-with-cap returns the value; read-without-cap → `PermissionDenied`; the sealed blob contains **no plaintext** (window scan for `euro-s3cr3t`); unseal round-trips; a wrong master key fails. The nonce is drawn fresh per seal from the TPM RNG with a monotone counter fallback (`vault.rs:48-61`) — the code explicitly notes nonce reuse under one key would break ChaCha20-Poly1305 (audit M1). Every kernel-side `get` records to the P3 audit trail (`vault.rs:25-29`). Host tests (`lib.rs:167-226`, 4): cap-gated read, list never leaks, seal/unseal round-trip, wrong-key/tamper. Shell `vault` / `vault get <label>` (`vault.rs:88`), where the caller's caps are `CAP_DB_ACCESS` only if `session_uid()==0` (`shell.rs:190`).

### EuroID — sovereign user management (`crates/euroid`, `kernel/src/euroid.rs`) — Sprint K1

This is the newest and largest identity piece. It is a from-scratch identity authority: users/groups, a from-scratch Argon2id credential store, login with timing-attack prevention, lockout, a password policy, identity→capability derivation, and a hash-chain audit log. `no_std`, `#![forbid(unsafe_code)]`, no clock or RNG inside the crate (the caller injects `Timestamp` and salt), 24 host tests (argon2 2, cred 4, audit 5, model 5, auth 5, policy 3).

#### Newtypes and model

- `UserId(u32)` / `GroupId(u32)` newtypes (`lib.rs:41,45`) — "a uid is never a bare u32." `UserId::SYSTEM`/`ROOT` = 0; regular uids from 1000, system from 100 (`lib.rs:50-56`).
- Built-in groups (`lib.rs:60-65`): wheel(0), audit(1), net(2), vault(3), agent(4), users(100), each mapped to a capability mask in `GroupDb::with_builtins()` (`model.rs:92-117`): wheel→`CAP_ALL`, net→`LOGIN|NET`, users→`LOGIN|FILE|DISPLAY`, etc.
- EuroID's own capability bitset (`lib.rs:74-90`) is richer than `ring3`'s: `CAP_LOGIN`, `CAP_FILE_READ/WRITE` (and composite `CAP_FILE`), `CAP_NET`, `CAP_DISPLAY`, `CAP_AUDIO`, `CAP_VAULT_READ/WRITE`, `CAP_AGENT_SPAWN`, `CAP_AUDIT_READ`, `CAP_USER_ADMIN`, `CAP_IMMUTABLE_ADMIN`, `CAP_SHUTDOWN`, `CAP_ALL = !0`.
- `User` (`model.rs:43-59`): uid, username, display_name, primary_gid, supplementary `groups`, home, shell, `state: UserState`, own `caps`, `created_at/by`, `password: PasswordRecord`, `tpm_enrolled`, `failed_logins`.
- `UserState` (`model.rs:14-20`): `Active`, `Locked{reason,locked_at,locked_by}`, `Expired`, `Deleted{deleted_at,deleted_by}` — **soft delete only**: records are never erased (audit requirement). `ct_eq` (`lib.rs:167-176`) is a constant-time byte compare.

#### Argon2id — from-scratch, RFC 9106 (`argon2.rs`)

This is the cryptographic heart and it is **complete and vector-verified**, not a stub.

**Blake2b (RFC 7693)** is implemented from scratch (`argon2.rs:42-144`): the 8-word IV, the 12-round SIGMA schedule, the G mixing function with the canonical 32/24/16/63 rotations, and a streaming `update`/`finalize` that correctly defers the last block so the final-block flag is applied. `blake2b(outlen, data)` supports variable 1..=64-byte digests. A host test verifies `Blake2b-512("abc")` against the RFC 7693 Appendix-A vector (`argon2.rs:485-496`).

**H′ (RFC 9106 §3.2)** (`argon2.rs:148-167`): the variable-length hash that extends Blake2b past 64 bytes by chaining 32-byte halves of successive 64-byte blocks.

**The memory-hard fill** (`argon2id`, `argon2.rs:310-391`):
1. Compute `m'` = `4·p·floor(m/(4p))` with a `8·p` minimum; derive `lane_len q = m'/lanes` and `seg_len = q/4` (4 sync points).
2. Compute `H0` = Blake2b-512 over `lanes ‖ tag_len ‖ m_cost ‖ t_cost ‖ version(0x13) ‖ type(2=id) ‖ len-prefixed pwd/salt/secret/ad`.
3. Allocate `m'` blocks of 1024 bytes (`Vec<[u64;128]>`).
4. Seed the first two blocks of each lane with `H′(1024, H0 ‖ counter ‖ lane)`.
5. For each pass × each of 4 slices × each lane, call `fill_segment`. Argon2**id** hybrid addressing: passes 0/slices 0–1 use **data-independent** addressing (pseudo-random addresses from an address block, `argon2.rs:421-430`), all later segments use **data-dependent** addressing (`rand = mem[prev][0]`). The reference-index computation (`argon2.rs:444-469`) implements the RFC's `J1²>>32` area-mapping with the correct `ref_area_size`/`start_pos` per pass/slice/lane case.
6. The compression `fill_block` (`argon2.rs:214-280`) is `out = P(R) XOR R` with `R = prev XOR ref`, 8 row-rounds then 8 column-rounds of the BLAMKA `gb` permutation (`argon2.rs:186-199`, with the `2·lo(a)·lo(b)` carry terms), and `with_xor` for passes > 0.
7. Final tag = `H′(tag_len, XOR of each lane's last block)`.

Correctness is anchored to the **official RFC 9106 §5.3 Argon2id test vector** (`argon2.rs:499-513`): `m=32,t=3,p=4` over the canonical inputs must equal the exact 32-byte expected tag. This passes.

**Credential layer** (`cred.rs`): sovereign defaults `m=65536 (64 MiB), t=3, p=4, salt=32 bytes` (`cred.rs:10-13`) — explicitly "never negotiated down." `Argon2idHash{salt,tag,m/t/p}` is self-describing (PHC-style `encode()`, `cred.rs:64-73`); `verify` recomputes and compares constant-time via `ct_eq` (`cred.rs:52-61`). `PasswordRecord` adds `changed_at`, `expires_at`, `must_change`, password `history` (no-reuse), and `locked`. `is_reused(pw, depth)` checks current + last N hashes (`cred.rs:123-133`); `set_new` rotates the old hash into bounded history (`cred.rs:136-147`).

The kernel deliberately runs **reduced** Argon2id params at boot (`BOOT_PARAMS = m=256,t=1,p=1`, `euroid.rs:31`) so the self-test is fast under TCG; the real 64 MiB params + RFC vector are proven in host tests. This is honestly labelled in the source.

#### Login flow + timing-attack prevention (`auth.rs`)

`authenticate(...)` (`auth.rs:58-180`) returns `AuthResult { outcome: Result<Session,AuthError>, events: Vec<AuditEvent> }` — the caller is *obligated* to write the events (logging cannot be skipped). The sequence:

1. Look up the user.
2. **Check state before verifying the password.** Locked/Expired/Deleted each still run a *dummy Argon2id verify* (`dummy.verify(...)`, `auth.rs:85,93,98,110`) so the wall-clock time is identical to a real wrong-password path — no user enumeration via timing.
3. Unknown user → dummy verify → generic `InvalidCredentials` (`auth.rs:107-116`). Deleted account is also reported as generic `InvalidCredentials` (`auth.rs:96-104`) — deletion is not disclosed.
4. Verify the password (`auth.rs:123`). On failure, increment `failed_logins`, emit `LoginFailed`, and at the threshold (5) call `db.lock(..., FailedLoginThreshold, SYSTEM)` + emit `UserLocked` (`auth.rs:125-143`).
5. On success: reset the counter, enforce `must_change` (`auth.rs:146-152`), derive the session caps, build the `Session{id,uid,username,caps,started_at,last_active,tty}`, emit `LoginSuccess`.

The `dummy` is a pre-computed hash with the **same params** as real accounts (`euroid.rs:140`), which is what makes the timing parity exact. A host test (`auth.rs:278-316`) actually *measures* wrong-password vs unknown-user timing and asserts the ratio is within `[0.2, 5.0]` — proving the constant-time property empirically, not just structurally.

#### Identity → capability derivation

`effective_caps(user, groupdb, europol_allowed)` (`model.rs:268-279`): start from the user's own caps, union in the primary group's caps and every supplementary group's caps, then **intersect with the EuroPol-allowed mask** (`& europol_allowed`). Policy can only take away, never add. The host test `wheel_grants_all_but_policy_can_deny` (`model.rs:322-332`) shows even wheel loses `CAP_NET` if EuroPol denies it system-wide.

#### Hash-chain audit log (`audit.rs`)

Each action is a self-describing JSON record. `AuditEntry{seq, event, timestamp, body, prev_hash:[u8;32], hash:[u8;32]}` (`audit.rs:224-232`). On `append` (`audit.rs:290-311`): `seq` = current length, build the canonical `body` JSON, set `prev_hash` = the log's `last_hash`, then `hash = SHA-256(seq_le ‖ prev_hash ‖ body)` (`compute_hash`, `audit.rs:235-241`), and advance `last_hash`. SHA-256 is from the `sha2` crate (`audit.rs:12`).

`verify_chain()` (`audit.rs:331-346`) walks the chain: each record's stored `prev_hash` must equal the running previous hash, **and** recomputing `hash` from `(seq, prev_hash, body)` must match the stored hash; the first break returns `Err(seq)`. This makes any edit to a past record invalidate all subsequent hashes. `root_hash()` is the last hash — a fingerprint of the whole log. Three host tests prove tamper-detection (`audit.rs:392-417`): editing a body breaks at that seq; even forging body+hash on record 1 breaks at record 2 because record 2 still references the old hash.

GDPR Art. 32 pseudonymisation is respected — events log UIDs as keys, with names as secondary fields (`audit.rs:129`). Event kinds (`audit.rs:88-104`) cover the full lifecycle: SystemInit, UserCreated/Modified/Deleted/Locked/Unlocked, LoginSuccess/Failed/Denied, Logout, PasswordChanged, SudoUsed, SuSwitched. The kernel persists to an append-only file (see "Two audit subsystems" below).

#### Password & username policy (`policy.rs`)

`PasswordPolicy::default()` (`policy.rs:24-39`) is the sovereign baseline: min 12 / max 128 chars, require upper+lower+digit+special, history depth 12, max age 90d, min age 1d, warn 14d, max 5 failed logins, 900 s lockout. `validate_password` (`policy.rs:67-88`) counts Unicode chars and returns a typed `PolicyError`. `validate_username` (`policy.rs:91-112`) enforces 1–32 chars from `[a-z0-9_-]` starting with `[a-z_]`. Host tests (`policy.rs:114-152`, 3) include a max-length DoS guard.

#### Boot self-tests + `eurousers` CLI

`euroid::selftest()` (`euroid.rs:146-252`, marker `[k1]`) runs the whole chain against a live store seeded with alice (users+net, own CAP_FILE) and admin bob (wheel, must-change): (1) alice logs in and gets `LOGIN|FILE|DISPLAY|NET` but not `CAP_USER_ADMIN`; (2) unknown user `mallory` fails indistinguishably; (3) 5 wrong attempts lock bob; (4) soft-deleting alice keeps the record; (5) `verify_chain()` is intact and prints the SHA-256 root. A *second* `[k1]` line then exercises the **real shell path** — `list`, `add carla ...`, `audit --verify-chain` — against the live store to prove the command path works, not just compiles.

`eurousers <subcmd>` (`euroid.rs:268-526`, `shell.rs:216`) takes `actor_uid` from the session and gates mutating commands behind `CAP_USER_ADMIN` (wheel) — `require_admin` returns `EPERM` otherwise (`euroid.rs:289-295`); uid 0 is always admin. Subcommands: `list`, `show <name>` (never shows the hash), `add <name> <pw> [groups]` (validates name+password, hashes with Argon2id, audits `UserCreated`), `passwd` (rejects reuse, forces must-change on admin reset), `lock`/`unlock`, `del` (soft delete), `groups`, and `audit [--user N | --verify-chain]`.

### EuroIDM — enterprise identity with signed tokens (`crates/euroidm`, `kernel/src/idm.rs`)

#### Purpose

Bind identity to capabilities across services without a mandatory external IdP: a login yields an Ed25519-signed, OIDC-like token (subject + groups + expiry) that any service verifies locally and from which it derives capabilities. `no_std`, `#![forbid(unsafe_code)]`, real `ed25519-dalek`.

#### Data structures & algorithm

`Idm{ key: SigningKey, users: Vec<User>, group_caps: Vec<(String,u64)> }` (`lib.rs:42-46`). Its own cap bitset (`lib.rs:24-31`): LOGIN, NET, FS_READ/WRITE, AUDIT_READ, USER_ADMIN, IMMUTABLE_ADMIN, SHUTDOWN. `Token{subject,uid,groups,issued_at,expires_at,signature:[u8;64]}` (`lib.rs:50-57`).

- `tbs(...)` (`lib.rs:68-82`): a domain-separated (`"EuroIDM-token-v1\0"`), length-prefixed canonical encoding of subject/uid/groups/iat/exp — the bytes that are signed and verified.
- `issue_token(name, now, ttl)` (`lib.rs:129-141`): look up user, sign `tbs` with the IDM key.
- `caps_for_groups(groups)` (`lib.rs:118-126`): union of each group's mask.
- `Token::verify(idm_pubkey, now)` (`lib.rs:146-158`): reconstruct `tbs`, Ed25519-verify, then check `now ∈ [iat, exp]`. Because the groups are *inside* `tbs`, **appending a group after signing invalidates the signature** — privilege escalation is cryptographically blocked.

#### Self-test + shell

`idm::selftest(seed, from_tpm, now)` (`idm.rs:30-55`, marker `[v]`) issues anke's token, verifies it, derives read+net (not write/user-admin), then pushes `"admins"` onto the token's groups and asserts `verify` now returns `BadSignature`. Host tests (`lib.rs:161-227`, 6): group→caps union, round-trip, expiry, tamper/escalation, wrong issuer, unknown user. Shell `euroidm` (`idm.rs:58-76`, `shell.rs:214`) shows the store + group→cap rules. Honest note: the shell renders a fixed-seed dry-run; the persistent IDM state is described as living in a daemon.

### EuroSandbox — capability-scoped chroot-safe containers (`crates/eurosandbox`, `kernel/src/container.rs`)

#### Purpose

Lightweight sandboxes on the capability model — **not** Linux namespaces. A container chroots to `/containers/<name>`, shrinks capabilities, and restricts network scope. The security-critical path resolution is host-tested.

#### Data structures & algorithm

`Container{name, root="/containers/<name>", caps:u64, net:NetScope}` (`lib.rs:30-37`); `NetScope` = `None | Allow(Vec<([u8;4],u16)>) | Any` (`lib.rs:18-26`).

- `effective_caps(base)` = `base & caps` (`lib.rs:47-49`) — a container can **only shrink** rights, never grant.
- `resolve(requested)` (`lib.rs:64-81`): split on `/`, ignore `""`/`.`, pop on `..` (never above the virtual root), and re-root under `self.root`. The result *always* begins with `self.root`, so `../../../etc/passwd` resolves to `/containers/web/etc/passwd` — chroot semantics with no escape.
- `contains(host_path)` (`lib.rs:85-94`): a second defence layer that correctly rejects sibling-prefix names (`/containers/webroot` is not inside `/containers/web`).
- `allow_connect(ip, port)` (`lib.rs:52-58`) enforces the net scope.

#### Self-test + shell

`container::boot_selftest(fs)` (`container.rs:77-97`, marker `[container]`) creates a `demo` container, proves `CAP_NET` is stripped from a full mask, resolves `../../../etc/passwd` and confirms containment, and writes a real file through the sandboxed path. Host tests (`lib.rs:97-152`, 4): caps-only-shrink, path-cannot-escape (many classic payloads), prefix-sibling rejection, net-scope enforcement. Shell `container`/`ctr` create/list/run (`shell.rs:401`, `container.rs:21-73`) demonstrate the chroot against the real filesystem.

### Legacy auth path — login/su/sudo (`kernel/src/auth.rs`)

This is the **older** `/etc/shadow` path that predates EuroID, kept for the desktop login/su/sudo flow. It is honestly *weaker* than EuroID and the file says so: salted, iterated **SHA-256** (`ITER = 4096`), with an in-comment note that "production upgrade = Argon2id, memory-hard" (`auth.rs:1-5,14`).

- `hash(salt, pw)` (`auth.rs:22-34`): `h0 = SHA256(salt‖pw)`, then `hᵢ = SHA256(salt‖hᵢ₋₁)` 4096×. SHA-256 comes from `eurotls::keyschedule`.
- `/etc/shadow` lines are `user:salt_hex:hash_hex`, `*` = locked.
- `verify(fs, user, pw)` (`auth.rs:67-97`) reads `/etc/shadow`, rejects locked/empty, recomputes, and compares **constant-time** (XOR-accumulate, `auth.rs:88-93`).
- Session state is three atomics + a name (`SESSION_UID/GID`, `auth.rs:16-18`); `session_uid()` feeds the vault/eurousers cap checks. `lookup_user`/`name_for_uid` parse `/etc/passwd`.

The relationship to EuroID is an intended migration target ("rewire login/desktop onto euroid::authenticate"): EuroID is the from-scratch Argon2id replacement; `auth.rs` is the legacy SHA-256 bridge still wired to the desktop session.

### Two audit subsystems (be precise about which is which)

There are **two** audit logs and they are different:

1. **EuroID's `AuditLog`** (`crates/euroid/src/audit.rs`) — the in-memory **SHA-256 hash-chain** for user-management events, verified with `verify_chain()`. Tamper-evident by chaining.
2. **The kernel P3 `audit` module** (`kernel/src/audit.rs`) — a system-wide security-event ring (`ImmutableSet/Denied`, `CapDenied`, `Login/Logout`, `Boot`) persisted to `/var/log/audit.log`, which is marked with the EuroFS `FLAG_APPEND_ONLY` flag so the filesystem physically rejects rewrites; clearing that flag requires `CAP_IMMUTABLE_ADMIN`. This is what EuroPol and EuroVault write into (`europol.rs:47`, `vault.rs:26-27`).

The two are complementary: P3 gives OS-level structural immutability; EuroID's hash-chain gives cryptographic tamper-evidence even without the FS flag.

### EuroObserve — OpenMetrics/Prometheus export (`crates/euroobserve`, `kernel/src/observe.rs`)

Lock-free in-kernel metrics with a Prometheus-scrapable OpenMetrics text renderer; zero overhead when nobody reads. `Counter(AtomicU64)`, `Gauge(AtomicI64)`, `Histogram` (`lib.rs:19-97`). The histogram has 6 fixed `le` bounds `[10,50,100,500,1000,5000]` µs + an implicit `+Inf` bucket; `observe(us)` (`lib.rs:78-85`) adds to `sum` and increments **cumulatively** (all buckets ≥ the matching one), exactly OpenMetrics histogram semantics. `render_counter/gauge/histogram` (`lib.rs:101-120`) emit `# HELP`/`# TYPE` headers and `_bucket{le=...}/_sum/_count` lines.

`observe.rs:10-14` defines live counters/gauges: `SYSCALLS`, `FS_READS`, `MSIX_IRQS`, `FREE_PAGES` (gauge), `FS_READ_US` (histogram). `render()` names them `euroos_*`. `selftest(free_frames)` (`observe.rs:29-47`, marker `[w]`) seeds representative values from real boot state and renders. Host tests (`lib.rs:122-175`, 4) cover counter/gauge math, cumulative buckets, and exact OpenMetrics format strings. Shell `metrics` (`shell.rs:188`). Honest note: this is the renderer + live counters; an actual `/metrics` HTTP endpoint on EuroNet is future ("now visible via the `metrics` command").

### EuroHealth — SMART-based system health (`crates/eurohealth`, `kernel/src/health.rs`)

Parse the NVMe SMART/Health log (log id 0x02), combine it with FS scrub integrity and memory pressure into a 0–100 health score + status. `SmartHealth` (`lib.rs:21-31`): `critical_warning` bitmap, `temperature_c` (from Kelvin), `available_spare`, `spare_threshold`, `percentage_used` (wear), `power_on_hours`, `media_errors`, `unsafe_shutdowns`. `parse(log)` (`lib.rs:44-59`) requires ≥192 bytes and reads the canonical NVMe offsets, including 128-bit little-endian counters via `r128_lo`.

- `status()` (`lib.rs:63-71`): `Failed` on any critical-warning or spare-below-threshold; `Warning` on wear ≥90%, temp ≥70 °C, or any media error; else `Passed`.
- `score()` (`lib.rs:74-90`): start 100, −50 critical, −wear/2 (max −50), −15 media errors, −10 hot, −30 spare-below-threshold, clamped.
- `HealthReport` (`lib.rs:95-101`) folds disk score with FS errors (−20), unrecoverable (−40), and memory pressure (<5% free → −15); `summary()` maps the overall score to Passed/Warning/Failed.

`health::selftest(...)` (`health.rs:29-42`, marker `[z]`) pulls a real SMART log if NVMe is present, else reports SMART n/b, and prints the combined score. Host tests (`lib.rs:135-196`, 4). Shell `eurohealth` (`shell.rs:224`).

### EuroCrash — kernel crash dumps (`crates/eurocrash`, `kernel/src/crashdump.rs`)

On a fatal `#PF`/`#DF`/`#GP`/panic, write a structured minidump (one 512-byte sector) of kernel state to a reserved disk block, and read it back on the next boot (recovery mode). `CrashDump` (`lib.rs:20-33`): version, vector, error_code, rip, rsp, rflags, cr2, cr3, `regs:[u64;16]`, build_hash, uptime_ms, seq. `encode()` (`lib.rs:66-87`) lays fields at fixed offsets behind magic `0x4555524F43525348` ("EUROCRSH"), then writes an XOR-fold checksum (`fold`, `lib.rs:118-125`) over the first 504 bytes. `decode()` (`lib.rs:90-115`) rejects wrong magic or checksum mismatch. `seq` distinguishes the newest dump.

`capture(vector, error_code, rip, rsp, rflags)` (`crashdump.rs:44-48`) snapshots CR2 and writes; `write` fills CR3, uptime, and a monotone `seq`, then writes sector `CRASH_LBA=300` via virtio-blk + flush (`crashdump.rs:31-40`). The fault handlers can dump even on stack exhaustion because `#PF`/`#DF` run on their own IST stacks. `selftest()` (`crashdump.rs:64-97`, marker `[y]`): (1) **recovery** — reads any dump from the previous boot and, if it's the `TEST_VECTOR=0xFE` sentinel, confirms cross-boot persistence; if it's a real vector, prints a warning with the decoded fault; (2) writes a fresh synthetic dump and reads it back to prove the round-trip. Host tests (`lib.rs:140-182`, 3).

### Part 4 — status summary

- **Real and verified:** Argon2id (RFC vector), Blake2b (RFC vector), the audit hash-chain (SHA-256, tamper tests), Ed25519 binary signing + tamper rejection (`verify_strict`), ChaCha20-Poly1305 vault seal/unseal, SMEP/SMAP/NX enablement, per-page W^X, capability gating on both ABIs, container path-escape resistance, EuroIDM token escalation blocking, the timing-parity login (empirically measured), OpenMetrics, SMART health, crash dumps.
- **Honest limitations / not-yet:** `CURRENT_CAPS` is a single live cap-set (userspace runs largely pre-scheduler), not a per-task capability field; W^X falls back to RWX for binaries with mixed RWE segments; the EuroObserve `/metrics` HTTP endpoint and the EuroIDM persistent daemon are future; the kernel boot uses *reduced* Argon2id params (real params proven only in host tests); the EuroID store is in-memory (EuroFS persistence is the next mile); `auth.rs` (legacy SHA-256 `/etc/shadow`) still backs the desktop session and is a migration target onto `euroid::authenticate`.
- **Naming gotcha:** "EuroGuard" the capability model is enforced in `ring3.rs`/`crypto.rs`; `euroguard.rs` is a separate network firewall/DNS-blocklist module.

Host-testable crates (`cargo test`): euroid 24, europol 5, eurovault 4, euroidm 6, eurosandbox 4, euroobserve 4, eurohealth 4, eurocrash 3. The two load-bearing cryptographic claims — Argon2id and Blake2b — are pinned to the **official RFC 9106 §5.3 and RFC 7693 Appendix-A vectors**.


---

## Part 5 — Display, Desktop, Input, Audio, Accessibility & Print

EuroOS renders its entire graphical environment from scratch: a software framebuffer with anti-aliased primitives, a desktop compositor with dirty-rect updates and drop shadows, a native modern-virtio GPU scanout driver, two Wayland-shaped protocol stacks, anti-aliased TTF text, a from-scratch design-system widget layer, PS/2 + USB-HID input, a real Intel HD-Audio driver with software mixing and earcons, an EN 301 549 accessibility tree, and driverless IPP printing. There is **no X11, no libwayland, no FreeType, no CUPS** underneath — every layer is owned code. Each kernel module fires a `[xx]` boot self-test (markers `[bb1]`..`[bb8]`, `[h2]`, `[h5]`, `[k4]`, `[p2]`, `[wm]`, `[ft]`) so the whole stack is externally verifiable over serial.

### Low-Level Framebuffer (`kernel/src/graphics.rs`)

The pixel-plotting foundation underneath everything visual. It owns the UEFI GOP framebuffer, draws to a RAM backbuffer, and blits to MMIO.

- `FrameBuffer` (`graphics.rs:118`) — fields: `base` (`*mut u8`, GOP MMIO), `buf` (`*mut u32`, RAM backbuffer in `0x00RRGGBB`), `width`/`height`, `stride` (scanline width in pixels, **may exceed `width`**), and `format` (`PixelFormat`). Two construction modes:
  - `new()` (`:144`) — *direct mode*, `buf == null`, writes straight to MMIO. Used by the panic handler so it never allocates.
  - `new_buffered()` (`:158`) — allocates and **leaks** a `width*height` `u32` backbuffer; all drawing goes there, `present()` blits once (no tearing).
- `Color` (`graphics.rs:41`) — `{r,g,b}` with `pack()`→`0x00RRGGBB` (`:55`), `lerp()` (`:66`), and the alpha-compositing `over(dst,a)` (`:77`). The whole **EDS light palette** is defined here as consts (`:89`–`108`): `BACKGROUND`/`PAPER_2` (warm sand), `SURFACE`/`CARD`, `ACCENT` `#2D6BE0` (European blue), `GOLD` (EU stars), the security colors `SUCCESS`/`YELLOW`/`RED`.

Core algorithms:
- **Pixel format detection** — `write_mmio` (`:171`) branches on `PixelFormat::Rgb` vs BGR rather than hardcoding, and respects `stride`.
- **Present / blit** — `present_rect(x,y,w,h)` (`:200`) writes one `u32` per pixel (3× faster than byte writes). Because the backbuffer holds `0x00RRGGBB`, in little-endian its bytes are `B,G,R,0` = exactly BGR, so for a BGR GOP it is a direct `u32` copy; for RGB it swaps R/B with shifts.
- **Anti-aliasing**: `blend()` (`:225`) does `over()` mixing; `fill_rounded_rect()` (`:297`) supersamples corner coverage 4×4 against the corner circle; `fill_rounded_rect_grad()` (`:356`) adds a 150° CSS-style linear gradient for the app-icon squircles; `aa_seg()` (`:412`) draws thick round-capped line segments via distance-to-segment; `aa_ring()` (`:443`) draws AA circle outlines.
- **Drop shadow** — `drop_shadow()` (`:465`) computes distance to the (offset-down) rect edge and fades opacity `70 * t²`, **skipping the interior** so only the halo is blended.
- **`sqrtf`** (`:507`) — libm-free square root: bit-trick seed + 2 Newton iterations, accurate enough for AA coverage in `no_std`.

`set_best_mode()` (`:13`) picks the GOP mode (prefers 1024×768, else largest ≤1920×1080).

### Compositor & Desktop Loop (`kernel/src/compositor.rs`, render loop in `kernel/src/main.rs`)

A software compositor that paints the "desktop.html" reference look: wallpaper, floating dock, overlapping windows with traffic-light titlebars in z-order, a right-hand status panel, and a save-under mouse cursor.

- `Window` (`compositor.rs:30`) — `x,y,w,h`, `title`, `content: Vec<String>` (monospace text lines), `ui: Vec<euroui::Widget>` (drawn instead of text when non-empty), `active`, `accent`, `sec: SecState`, `app: SuiteApp` (Writer/Calc/Browser/Settings/Agent/Installer dispatch), `visible`, and `restore: Option<(x,y,w,h)>`.
- `TitleButton` (`:52`) — `Close`/`Minimize`/`Maximize`. `title_button_at()` (`:69`) hit-tests the three 13px dots with a ±9px zone.
- `SysStats` (`:363`) — `free_mb`/`total_mb`/`uptime_s`/`cores`/`procs` for the live system card.
- Layout constants: `SIDEBAR_W=90`, `TITLEBAR_H=44`, dock geometry, `PANEL_W=284`.

Full-frame render — `render()` (`:503`): `draw_wallpaper` → `draw_sidebar` → for each index in `order` (back-to-front z-order) draw visible windows → `draw_status_panel` on top.
- `draw_wallpaper()` (`:293`) — per-row diagonal cool→warm gradient, a coarse EU-blue radial glow (`30*t²` alpha), and a 24px dotted grid, all in software pixels.
- `draw_window()` (`:118`) — drop shadow (stronger for the active window), rounded `SURFACE` body, `CARD` titlebar, traffic-light dots, title text, a green "Protected" pill with `shieldCheck` when `sec.sandboxed`, a hairline, and a white inset-glass highlight. Body delegated to `draw_window_body()`.
- `draw_window_body()` (`:172`) — dispatches by `win.app` to the rich app renderers (`calc_ui`, `webview`, `settings_ui`, `agent_ui`, `installer`, `suite_ui`); otherwise draws the `euroui` widget panel or scrolls the **last** `content` lines (so you see what the shell is doing now).
- `draw_sidebar()` (`:254`) — floating glass dock: EU mark (blue disc + 12 gold stars from fixed `STAR_RING` offsets, no trig), six colored app tiles, an active accent bar, a user avatar with initials.
- `draw_status_panel()` (`:384`) — 44px clock + date, a moon theme-toggle, a green "Your device is safe" card, and a **live system card** (memory bar, uptime, online cores, process count — real changing numbers).

#### Dirty-rect render loop (`main.rs:2643`–2714)

The performance core. Each iteration decides the *minimum* region to repaint:
- **`need_full`** (sleep / z-order change): full `compositor::render` + `fb.present()`.
- **`tick`** (every 50 ticks): redraw only the status panel (`with_shadow=false`) and the live System window *body*, then **blit only those rects** via `present_rect` — instead of ~2M px/tick.
- **Cursor-only move**: restore save-under background, redraw cursor, blit only the bounding boxes the cursor left + arrived at.
- `term_dirty`/`calc_dirty` redraw just the focused window.

The 11×16 arrow cursor (`CURSOR`, `:468`) uses **save-under**: `save_cursor_bg`/`restore_cursor_bg` snapshot the pixels beneath it. After every present, `virtio_gpu::present_frame(...)` pushes the backbuffer to the native GPU scanout if active.

`wm.rs` is the **`[wm]` self-test**: it verifies traffic-light hit zones, that maximize maps to `work_area()` (the rect between dock and panel) and restore returns the original geometry, and that close/minimize hide the window — deterministically, no mouse.

### Display Server & Surface Protocol (`kernel/src/dispserv.rs`, `crates/eurodisplay`)

A Wayland-*shaped* own protocol so an app process can open a real window over an **AF_UNIX socket** — "the window exists because another piece of code asked for it over a socket," not a mockup. The protocol + surface model is pure `no_std` and host-tested (`crates/eurodisplay`); `dispserv.rs` is thin kernel glue.

- `Request` (`eurodisplay/src/lib.rs:20`) — `CreateSurface`/`Attach{w,h}`/`Commit`/`Move`/`Destroy`, mirroring `wl_surface`/`wl_buffer`. `Event` (`:35`) — `Configure`/`Key`/`Pointer`/`FrameDone`.
- `Surface` (`:48`) — `id,x,y,width,height,mapped`. `Display` (`:60`) — z-ordered `Vec<Surface>` (back = topmost) + a `damaged` flag.
- `DispServer` (`dispserv.rs:24`) — `ServerView` + `Vec<(UnixEndpoint, Vec<u8>)>` (per-client leftover buffer for partial frames).

Core algorithms:
- `Display::handle()` (`lib.rs:75`): `Commit` with a valid buffer **removes-and-pushes** the surface to the top (raise+focus), marks damage, returns `FrameDone`. `focused()` is the topmost mapped surface; `route_pointer()` returns surface-local coordinates.
- **Wire format** (`server.rs:14`): length-prefixed frames `[op:u8][id:u32][a:i16][b:i16][len:u16][payload]` (11-byte header). `parse_frames()` (`:76`) demuxes all complete frames and **leaves a partial trailing frame** for the next call — safe for a byte stream. Extra opcodes `OP_TITLE`/`OP_LINE`/`OP_CLEAR` carry compositor-only metadata.
- `DispServer::pump()` (`dispserv.rs:47`): accept clients, drain bytes, parse frames, `ingest` into the view; returns `true` when a redraw is needed. `demo_app()` (`:85`) is an in-kernel client that opens one window through the full app→socket→server→compositor chain.

Tests: 12-byte `encode`/`decode` roundtrip, surface lifecycle, raise-on-commit, input routing, damage tracking; frame roundtrip, partial-trailing-frame, z-order.

### Native virtio-GPU Driver (`kernel/src/virtio_gpu.rs`, `kernel/src/gpu.rs`, `crates/eurogpu`)

A **sovereign** modern-virtio (virtio-1.0) transport + 2D virtio-gpu driver that presents the desktop through *our* driver rather than the OVMF GOP (`[bb2]`). The command serialization/parsing is host-tested in `crates/eurogpu`; `gpu.rs` is the protocol-only `[k4]` self-test (builds the full command stream + parses a simulated response, no hardware).

- `Vq` (`virtio_gpu.rs:62`) — one split-virtqueue: `size`, the `desc`/`avail`/`used` ring addresses, `notify` doorbell, `avail_idx`, `last_used`.
- `VirtioGpu` (`:72`) — `common` (common-cfg MMIO), the `Vq`, device `width`/`height`, and `fb`/`sw`/`sh` (the leaked DMA RAM framebuffer the desktop is copied into). The live driver lives in a `spin::Mutex<Option<VirtioGpu>>` (`VGPU`, `:87`).
- `eurogpu` command builders: `get_display_info`/`resource_create_2d`/`resource_attach_backing`/`set_scanout`/`transfer_to_host_2d`/`resource_flush`, each prefixed by the 24-byte `ctrl_hdr`. Format `FORMAT_B8G8R8A8_UNORM`.

Core algorithms:
- **Init handshake** (`VirtioGpu::init`, `:151`): enable bus-master + MMIO; locate common-cfg and notify caps; reset, then `ACK | DRIVER`; feature negotiation **requiring `VIRTIO_F_VERSION_1`**, set `FEAT_OK`; set up control-queue 0 via `dma_zeroed` (which leaks identity-mapped, so *physical = virtual* — no IOMMU mapping needed); compute the notify address; `DRIVER_OK`; then a real `GET_DISPLAY_INFO` round-trip to read the screen size.
- **Submit cycle** (`submit`, `:222`): allocate command + response DMA buffers; descriptor 0 = command (read-only, chains to 1), descriptor 1 = response (`VRING_DESC_F_WRITE`); publish into the avail ring with a `SeqCst` fence, ring the doorbell; **poll the used ring with a bounded spin** (50M, so boot can't hang); read back the response.
- **Scanout cycle** (`init_scanout` `:98` → `present_frame` `:121`): allocate a `w*h*4` B8G8R8A8 RAM framebuffer, `resource_create_2d(1)` → `resource_attach_backing(1, fb)` → `set_scanout(0,1)`. Then every frame, `present_frame` copies the desktop backbuffer (`0x00RRGGBB | 0xFF000000` forced-opaque) into the GPU framebuffer and issues `transfer_to_host_2d` + `resource_flush`.

**Honest status:** transport + scanout are real and verified against `virtio-gpu-pci` in QEMU. There is **no 3D/Virgl acceleration** — this is 2D scanout (CPU draws, GPU presents). The `[k4]` path is protocol-only.

### Wayland Protocol Layer (`kernel/src/wayland.rs`, `crates/eurowl`)

The **real Wayland wire protocol** server core (`[h5]`) — the foundation on which an *unmodified* Wayland client could eventually run via libwayland over AF_UNIX. Host-tested.

- `Obj` (`eurowl/src/lib.rs:34`) — `Display`/`Registry`/`Compositor`/`Shm`/`XdgWmBase`/`Surface`/`XdgSurface{surface}`/`XdgToplevel{xdg_surface}`.
- `Server` (`:56`) — `objects: BTreeMap<u32,Obj>` (id 1 = `wl_display`), `xdg_to_surface`, `toplevels`, committed `windows: Vec<Window>`, and a `serial`.

Core algorithms:
- `handle()` (`:97`) parses the standard 8-byte header `[obj:u32][(size<<16)|opcode:u32]` with **word-aligned** arguments, advancing `(size+3)&!3`.
- `dispatch()` (`:115`) implements the real handshake: `wl_display.get_registry` → `advertise()` emits `wl_compositor`/`xdg_wm_base`/`wl_shm` globals; `bind`; `create_surface`; `get_xdg_surface`; `get_toplevel` (which **sends back an `xdg_surface.configure(serial)`**); `set_title`; `commit` → `commit_surface()` makes a titled window only if a toplevel references that surface. `wl_display.sync` returns `callback.done` + `delete_id`.
- `rd_string()` handles Wayland's `[len][bytes incl null][pad→4]` string encoding; `write_msg()` builds correctly-sized, padded messages.

`run_handshake()` drives a full in-kernel test client through the exact sequence. Tests cover global advertisement, full handshake → titled window, no-window-before-commit, surface-without-toplevel, configure-on-get_toplevel.

### Font Rendering & Text Layout (`kernel/src/text.rs`, `kernel/src/font.rs`, `crates/eurofont`)

Anti-aliased TrueType text for all desktop chrome.

- `text.rs` — the modern renderer over **`ab_glyph`**. Two embedded fonts loaded lazily via `spin::Once`: `UI` (DM Sans, proportional) and `MONO` (DejaVu Sans Mono).
- `font.rs` — the legacy 8×8 CP437 bitmap (`FONT_DATA`, ASCII 32–126), used by the boot splash and as a fallback. `draw_char` (`:111`) notes a real bug fixed by the first QEMU screenshot: this table is **LSB-left**, so the classic `0x80 >> col` would mirror every glyph.

Text layout — `text.rs:50` `render()`: build `PxScale`, compute `baseline = y + ascent`; for each char get the `GlyphId`, **apply kerning** (`sf.kern(prev,gid)`), outline the glyph, and rasterize coverage with `outlined.draw(|gx,gy,cov| fb.blend(..., cov*255))` — soft, non-stair-stepped edges. Advance by `sf.h_advance(gid)`. `ui_px` maps legacy scale 1/2/3 → 15/25/34px; `mono_px` = `13*scale`; `mono_advance` gives the monospace column width.

EuroFont crate (`[ft]`) — a separate **sfnt/TrueType metadata parser** (no FreeType): reads the table directory and `name`/`head`/`maxp` tables to extract `FontInfo{family, subfamily, full_name, units_per_em, num_glyphs}`. The `[ft]` self-test builds a "EuroSans Bold" font and parses it back, and confirms non-fonts/truncated input are bounds-safe. This is the font *manager* metadata path, distinct from `ab_glyph` rasterization.

### EuroOS Design System — Widgets, Theme, Icons (`kernel/src/eds.rs`, `euroui.rs`, `icons.rs`, `appicons.rs`)

- **EDS tokens (`eds.rs`)** — the design vocabulary: the resolution-independent **Euro Unit** `EU=4` with `eu(n)=n*4`; the closed radius set `RADIUS_S=8`/`M=12`/`L=20`/`XL=28`; and the first-class **security color language** — `SEC_VERIFIED`/`SEC_PROTECTED`/`SEC_ATTENTION`/`SEC_COMPROMISED`/`SEC_UNKNOWN`. `SecState{sandboxed, encrypted, network}` is the per-window security status shown in titlebars ("never hide security").
- **EuroUI widgets (`euroui.rs`)** — `Widget` enum (`:17`): `Heading`, `Caption`, `Row(label,value)`, `Toggle(label,on)`, `Button(label,primary)`, `Badge(label,color)`, `Divider`, `Spacer(n)`. `draw_panel()` (`:37`) lays out a vertical stack using `eds::eu()` + radius/security tokens — apps never hand-place pixels.
- **Vector icons (`icons.rs`)** — a from-scratch **SVG-path renderer** (no bitmaps). `icon_svg()` holds the `euicons` set as mini-SVG strings; `draw()` (`:40`) scales from a 24px viewBox, parses `<circle>`/`<rect>`/`<path>`, and strokes them with `aa_seg`/`aa_ring` primitives at scaled thickness — resolution-independent.
- **App icons (`appicons.rs`)** — `draw_tile()` (`:33`) renders the colored squircle app tiles: a tinted drop shadow, a `fill_rounded_rect_grad` 150° gradient squircle (radius 28%), a glass inset-highlight, and a white icon glyph centered at ~52%.

### Input — PS/2 Keyboard & Mouse, USB-HID (`kernel/src/ps2.rs`, `kernel/src/mouse.rs`)

- **Keyboard (`ps2.rs`)** — IRQ-driven scancode-set-1 US QWERTY. The IRQ1 handler calls `push_scancode()` into a 256-entry ring buffer so no keystroke is lost while the shell isn't scheduled. `poll_key()` (`:56`) pops codes, tracks `SHIFT` (make/break), and `translate()` (`:72`) maps to chars including `\r`, backspace, tab, with `shifted_symbol()` for punctuation.
- **Mouse (`mouse.rs`)** — PS/2 auxiliary device (i8042), IRQ12. Position/buttons live in atomics so the desktop loop reads them lock-free. `init()` (`:37`) enables the aux device, sets the i8042 config byte, enables keyboard scanning (`0xF4`) and mouse reporting. `push_byte()` (`:84`) reassembles the 3-byte packet `[flags,dx,dy]`, **resyncs** on `flags & 0x08`, and updates the clamped cursor — **inverting Y** (PS/2 up = positive dy). `apply_usb()` (`:115`) feeds relative USB-HID motion into the *same* atomics (HID Y is down-positive), so the desktop works transparently on either PS/2 or xHCI HID input.

### Audio — Intel HD-Audio Driver & Mixer (`kernel/src/hda.rs`, `crates/euroaudio`)

**Real audio output** (`[bb8]` / I2). `hda.rs` is the hardware driver under the host-tested `euroaudio` software mixer.

- `Hda` (`hda.rs:85`) — `Mmio` base, `corb`/`rirb` ring addresses, `sd` (the chosen output stream-descriptor base), and `audio`/`audio_bytes` (the cyclic PCM buffer reused for earcons). All DMA structures come from the identity-mapped `FrameAllocator` (virtual = physical).

HDA stream bring-up (`init`, `:147`):
1. Find PCI class `0x04` subclass `0x03`; read BAR0 (handles 64-bit BARs); enable memory + bus-master.
2. **Controller reset**: `CRST=0`→wait, `CRST=1`→wait; read `GCAP` for stream counts and `STATESTS` for present codecs.
3. Set up **CORB/RIRB** (256-entry command/response rings); start both DMAs (poll, no interrupts).
4. **Codec enumeration** via `corb_cmd()` (`:103`): each verb is `(cad<<28)|(nid<<20)|payload`; reads `VENDOR_ID`, walks function groups to find the audio-function-group, then the first DAC and output pin.
5. **Audio buffer**: 8 contiguous frames (32 KiB), filled by `build_tone()` (`:370`) which generates 440 Hz + 660 Hz square waves and **mixes them through `euroaudio::mix`** at half-volume, then interleaves mono→stereo — proving the mixer→hardware chain.
6. **BDL** (buffer descriptor list): 2 half-buffer entries with IOC.
7. **Codec path config**: `SET_CONVERTER_FORMAT` 48kHz/16-bit/stereo, stream-channel, amp gain/unmute, pin output-enable + EAPD.
8. **Stream start**: SRST reset, set CBL/LVI/FMT/BDL pointer, write stream-tag 1 + RUN bit.
9. **Verification**: poll **LPIB** (`SDLPIB`) up to ~250ms; if it advances, the DMA is consuming the buffer = audio is really playing. `[bb8]` reports `LPIB p0→p1`.

Earcons (`earcon`, `:425`) — for accessibility cues, rewrites the cyclic audio buffer in place with a ~125ms square-wave beep at `freq_hz`. Because the DMA loops over the buffer, the new tone sounds immediately. `stream_pos()` (LPIB) and `stream_running()` (RUN bit) expose state.

euroaudio mixer (`crates/euroaudio`) — architecture-independent, host-tested. `mix()` (`:44`) sums each `(stream, Q8-volume)` in i32 and **clamps to i16** (overlapping sound distorts rather than wrap-around cracks). `scale()` applies Q8 gain with clamping; format conversions; `resample_nn()` nearest-neighbour resampling (linear/polyphase noted as later). 9 unit tests.

### Accessibility (`kernel/src/access.rs`, `crates/euroaccess`)

The EN 301 549 accessibility layer — in the EU a *procurement requirement*. `euroaccess` is the AT-SPI-equivalent: an accessibility tree, focus management, and a **multilingual screen reader** (`[p2]` deterministic + `[bb8]` live, end-to-end into audio).

- `Role` (`euroaccess/src/lib.rs:21`) — ARIA/AT-SPI subset (`Window`, `Heading`, `Button`, `TextField`, `CheckBox`, `ListItem`, …). `focusable()` defines which roles join the tab order; `label(lang)` returns the localized role word in NL/DE/FR with English fallback — **role names come from EuroLocale**, so the reader speaks the user's language.
- `AccNode` (`:73`) — `id, role, name, value, checked: Option<bool>, children`. `AccTree` (`:141`) — `root` + `focused` id.

Core algorithms:
- **Focus order** — `focus_order()` (`:153`) collects focusable nodes depth-first in reading order. `move_focus(forward)` (`:161`) cycles next/previous, wrapping.
- **Announce** — `AccNode::announce(lang)` (`:103`) builds e.g. `"knop: Aanmelden"`, `"tekstveld: Naam, leeg"`, `"selectievakje: Onthoud mij, niet aangevinkt"` — role label + name + value/checked state, all localized.
- **Live chain** — `access.rs::live_selftest()` (`[bb8]`, `:77`): tab through a real "Aanmelden" dialog; for each focus step push the NL announcement and play a **role-distinct earcon** through the real HDA DAC (TextField 440Hz, CheckBox 587Hz, Button 784Hz). It then waits ~400ms and reads LPIB + the RUN bit to prove the stream advanced — proving the chain **widget tree → focus event → announcement → audio**.

**Honest status:** focus events, multilingual announcements, role-distinct earcons, and the audio path are real and verified. **Intelligible TTS speech synthesis is explicitly not implemented** — both `access.rs:76` and the `[bb8]` output state "intelligibele spraaksynthese = volgende mijl" (next milestone). The earcons are tones, not spoken words.

### Printing (`kernel/src/print.rs`, `crates/europrint`)

Driverless, sovereign printing over **IPP-over-TCP** (IPP Everywhere, RFC 8010/8011) — "no driver, no cloud" (`[bb4]`). The binary IPP encoding is host-tested in `europrint`; `print.rs` wraps requests in HTTP/1.1 and does the real round-trip over EuroNet TCP (in QEMU, the SLIRP gateway `10.0.2.2:631`).

- `IppRequest` (`europrint/src/lib.rs:46`) — builder: `new(op,id)` auto-prepends the mandatory `attributes-charset` (utf-8) + `attributes-natural-language` (en); `.printer_uri()`, `.job_name()`, `.keyword()`, `.integer()`. `serialize(doc)` (`:93`) emits the IPP 2.0 binary form. Operations `OP_PRINT_JOB`/`OP_GET_PRINTER_ATTRIBUTES`/`OP_GET_JOBS`.
- `IppResponse` (`:115`) — `{version, status, request_id, attributes}`; `parse()` walks group/value tags bounds-safely. `europrint::http` builds the `Content-Type: application/ipp` POST and parses the reply, handling both `Content-Length` and `Transfer-Encoding: chunked` (the `dechunk()` decoder).

IPP round-trip (`print.rs:19`) — `ipp_roundtrip()` wraps the serialized IPP in an HTTP/1.1 POST, sends via `net::http_post_raw`, and parses the body as an `IppResponse`. `[bb4]` does a real two-step exchange: `Get-Printer-Attributes` then `Print-Job`; if no printer/CUPS is reachable it reports "transport ready" rather than failing. The `print` shell command prints arbitrary text. 11 host tests.

### Part 5 — status: real vs stub

- **Real + boot-verified:** software framebuffer/compositor with dirty-rects and drop shadows; AA TTF text via `ab_glyph`; vector icon renderer; EDS widgets; native modern-virtio **2D** GPU scanout (`[bb2]` against real `virtio-gpu-pci`); real Wayland wire protocol parser (`[h5]`); AF_UNIX display server (`[h2]`/`[bb3]`); PS/2 + USB-HID input; **real HDA driver** with LPIB-verified DMA + software mixer + earcons (`[bb8]`); accessibility tree + multilingual announcements + role earcons; real IPP-over-TCP printing (`[bb4]`).
- **Protocol-only / host-tested but not hardware-driven:** the `[k4]` `gpu.rs` path; `eurofont` is a metadata parser, not a rasterizer.
- **Explicit stubs / next milestones:** **no GPU 3D/Virgl acceleration** (2D scanout only); **no intelligible TTS** (earcons are tones); `resample_nn` is nearest-neighbour; an unmodified libwayland client over the socket is the stated future direction.


---

## Part 6 — Userland Runtimes, Agents, Localisation, Packaging, Web, Office & Apps

EuroOS is a from-scratch `no_std` Rust operating system — **not** Linux or BSD. Its native identity is the EuroGuard capability model, EuroIPC, EuroFS and the `Euro*` subsystems. The Linux ABI described below is a **compatibility bridge**, not the system's identity: it exists so that software compiled against musl/`x86_64-linux` can run, while every privileged operation is still routed through EuroGuard capabilities.

Each subsystem ships a host-tested core crate (`crates/euro*`) plus a kernel module that proves it *live at boot* via a serial self-test marker. Throughout, this distinguishes **engine works** (the core logic is real and verified) from **full app** (a windowed GUI program) and flags demo/mock scaffolding where the codebase itself labels it as such.

### 1. Linux ABI Compatibility — the EuroCompat Bridge

**Location:** `kernel/src/ring3.rs` (3294 lines).

`ring3.rs` lets binaries built for `x86_64-linux` (static-PIE musl) execute on EuroOS. The kernel itself performs the static-PIE self-relocation that musl's `_start` would otherwise do (`ring3.rs:1959`), and constructs the full SysV/`auxv` stack contract a musl `_start` expects — `AT_PHDR/AT_PHENT/AT_PHNUM/AT_ENTRY/AT_BASE/AT_RANDOM` (`ring3.rs:2443`, `:2480`). This is explicitly a *translation layer*: incoming Linux syscall numbers are mapped onto EuroOS's own handlers (`linux_dispatch`, `ring3.rs:2680`), and the result is reported back with Linux conventions (negative errno).

#### Capability enforcement on the Linux ABI

Least-privilege applies even to emulated Linux syscalls. `linux_required_cap` (`ring3.rs:2691`) maps syscall numbers to EuroGuard capabilities; a process lacking the right is refused with `[cap] Linux-syscall {num} GEWEIGERD` (`ring3.rs:2705`):

- `write/ioctl/writev` (1/16/20) → `CAP_CONSOLE`
- `read/open/close/fstat/stat/lseek/readv/readlink/getdents64/openat` (0/2/3/5/8/19/89/217/257/262/267) → `CAP_FILE`
- `socket/connect/sendto/recvfrom` (41/42/44/45) → `CAP_NET`
- `getpid` (39) → `CAP_PROC_INFO`

#### Emulated Linux syscalls

Two dispatchers exist. The **per-process preemptive** PCB dispatcher (`process_syscall`, `ring3.rs:1051`) and the **foreground Linux** dispatcher (`linux_dispatch`, `ring3.rs:2708`). The combined emulated set:

| nr | syscall | behaviour (file:line) |
|---|---|---|
| 0 | read | VFS read / pipe read / EOF (`:2806`, `:1086`) |
| 1 | write/writev | console line-buffer or pipe FIFO (`:2709`, `:1052`) |
| 2 | open | VFS open via `vfs_open` (`:2908`) |
| 3 | close | (`:2840`) |
| 5/262 | fstat/newfstatat | stat fill (`:2920`) |
| 8 | lseek | `vfs_lseek` (`:2882`) |
| 9 | mmap | bump-allocate from per-process heap window, page-aligned (`:2747`, `:1100`) |
| 10 | mprotect | no-op success (musl RELRO) (`:2985`) |
| 11 | munmap | silent success (bump allocator doesn't free) (`:2759`) |
| 12 | brk | sets/returns new break (`:2738`, `:1110`) |
| 13/14 | rt_sigaction/rt_sigprocmask | pretend success; no signals (`:2986`) |
| 16 | ioctl | success (isatty/TCGETS — stdout is a tty) (`:2984`) |
| 21/269 | access/faccessat | (`:3054`) |
| 22/293 | pipe/pipe2 | `pipe_create` (`:1087`) |
| 24 | sched_yield | no-op (`:3038`) |
| 32/33 | dup/dup2 | pipe-end copy (`:1088`) |
| 35 | nanosleep | no-op (`:3111`) |
| 39 | getpid | (`:2729`, `:1129`) |
| 41/42/44/45 | socket/connect/sendto/recvfrom | POSIX sockets onto EuroNet (`:2848`–`:2876`) |
| 56 | clone | real **threads** sharing the address space (`CLONE_VM`) with own stack/TLS/kernel-stack — basis for pthreads; CLONE_SETTLS/PARENT_SETTID/CHILD_CLEARTID honoured; **no fork** (`:1138`) |
| 59 | execve | image replace `do_execve` (`:1182`) |
| 60/231 | exit/exit_group | thread-exit vs process-exit (zombie + reaper) (`:2730`, `:1201`) |
| 63 | uname | (`:3018`) |
| 72 | fcntl | pretend success (`:3039`) |
| 96/228 | gettimeofday/clock_gettime (`:3008`, `:2991`) |
| 102/107 | getuid/geteuid → session uid (`:3036`) |
| 158 | arch_prctl — `ARCH_SET_FS` writes FS_BASE (musl TLS) (`:2760`, `:1119`) |
| 186/202 | gettid/futex — real block + wake (`:1183`, `:2990`) |
| 217 | getdents64 — fills `linux_dirent64` records (`:1345`, `:2983`) |
| 257/262 | openat/newfstatat (`:2883`) |
| 318 | getrandom — deterministic fill (`:3103`, `:1131`) |
| 332/334 | statx (`:3076`) / rseq → `-ENOSYS` so glibc falls back cleanly (`:3053`) |
| 500–502 | **EuroIPC** — native message-bus syscalls in their own number range (`:1192`) |

Unhandled syscalls return `-ENOSYS` with `[linux-abi] ENOSYS Linux-syscall {num}` (`ring3.rs:3113`). A synthetic `/proc` (`version/cpuinfo/meminfo/self/maps/stat/cmdline`) is injected into the VFS so Linux programs that probe `/proc` work (`ring3.rs:293`); `/proc/version` reports `Linux version 6.6.0-euroos … EuroToolchain`. Background musl processes are scheduled preemptively, each with its own 2 MiB arena + PML4 (`spawn_bg_musl`, `ring3.rs:1241`), visible via `ps` and killable via `kill <pid>`. Bundled musl test binaries are embedded ELFs with `.sig` signatures (e.g. `/bin/mcat`, `/bin/mwrite`, `/bin/msock`, `/bin/mdns`, `/bin/mpthread`).

### 2. EuroCoreutils — GNU-compatible userland

**Crate:** `crates/eurocoreutils` (1465 lines). **Self-test marker:** `[cu]`. **Shell wiring:** `kernel/src/shell.rs:787`+.

A from-scratch, GNU-shaped coreutils library. Each command is a pure function over `(&[&str] args, &[u8] stdin) -> Vec<u8>` (some return `(Vec<u8>, i32)` to carry an exit code).

#### Command set
- **text.rs** (filters; take stdin): `head`, `tail`, `wc`, `tac`, `rev`, `nl`, `fold`, `cat`, `sort`, `uniq`, `cut`, `tr`, `grep`
- **checksum.rs**: `sha256sum`, `sha512sum`, `sha224sum`, `sha384sum`
- **encoding.rs**: `base64`, `base32` (both with `-d`), `cksum`
- **compute.rs**: `printf`, `expr` (with exit code), `test`/`[`, `numfmt`, `factor`
- **lib.rs**: `echo`, `seq`, `basename`, `dirname`; the shell adds `true`, `false`, `yes`, `arch`, `nproc`, `pwd`
- **find.rs**: `glob_match` + `FindOpts::{parse, start_path, matches}` (`-name GLOB`, `-type f|d`, `-maxdepth N`); the actual VFS tree-walk is `shell.rs:934` (`find_walk`).

#### How pipes / stdin work
The shell distinguishes three modes (`kernel/src/shell.rs`):
1. **`coreutils(cmd, line, fs)`** (`:787`) — single command. It scans the argument tokens *from the back* for the first one that names a readable VFS file and uses that file's bytes as stdin; the remaining tokens are options. Arg-only compute commands are handled first so a numeric arg isn't mistaken for a filename.
2. **`coreutils_filter(cmd, args, input)`** (`:869`) — a filter applied to upstream stdin bytes (the pipeline role).
3. **`run_pipeline(ctx, line)`** (`:903`) — splits `A | B | C` on `|`. Stage 0 runs via the normal shell `exec` (may read a file, `echo`, `ls`); its stdout becomes the byte stream. Each later stage is a `coreutils_filter` over the previous stage's bytes. A `tee FILE` stage writes the stream to the VFS and passes it through unchanged.

### 3. EuroWASM + WASI Runtime — the agent sandbox

**Crate:** `crates/eurowasm` (1336 lines). **Kernel:** `kernel/src/wasm.rs` (markers `[h4]`/`[h4-ctr]`); `kernel/src/wagent.rs`.

A no-JIT, `no_std` WebAssembly **interpreter** that runs WASM modules directly in the kernel, with WASI-style host imports mapped onto EuroGuard capabilities. The execution sandbox for EuroAgent.

- `Module::parse(bytes)` (`lib.rs:176`) parses the binary `\0asm` format: type/import/function/memory/export/code sections, with `uleb`/`sleb` decoding and structured-control bytecode rewriting (block/loop/if/else End targets pre-resolved during parse).
- Opcode set decoded into an `Op` enum (consts, locals/globals, `Block/Loop/If/Else/End/Br/BrIf/Return/Call`, `Drop/Select`, `I32Load/Store/MemorySize/MemoryGrow`, numeric `Op::Num`).
- `Instance::{new, write_mem, mem, invoke}` (`lib.rs:496`) runs an operand stack + control stack interpreter with linear memory.
- `HostImports::call(...)` is the import boundary; `WasmError` includes `Trap`, `HostError`, and crucially **`CapabilityDenied(String)`**.

Capability gating (the sandbox boundary): the kernel self-test (`wasm.rs:121`) builds a module computing `1..=10 = 55` and calling `euro.fd_write`. With `CAP_CONSOLE` granted, the write succeeds; **without** it, the host returns `CapabilityDenied` and the WASM trap propagates. `container_selftest` (`wasm.rs:241`) binds WASI imports to a real `eurosandbox::Container`: each host call is checked against `effective_caps = base ∩ container.caps`, and `sock_connect` against the container's `NetScope`.

WASM agents (`wagent.rs`) — closes the agent chain end-to-end: real WASM **agent code** (`agent.fs_write` import, `run()` export) executes in the interpreter; its host import is routed through the cap-gated MCP gateway to real EuroFS. Without the capability the tool call is refused — proven with actual WASM-compiled agent code, not a flag.

### 4. EuroAgent — sovereign agent-first runtime

**Crate:** `crates/euroagent` (2181 lines). **Kernel:** `agent.rs`, `agent_ui.rs`, `wagent.rs`, `mcpd.rs`. **Markers:** `[aa]`, `[aa-fs]`, `[bb1]`, `[p3]`.

Agents are **WASM modules + a declarative capability manifest**; the trust boundary lives in the kernel (EuroGuard), not in a cloud. Positioned against MS "Project Solara": EuroAgent runs fully offline with EU data residency.

#### The `.euroa` manifest (TOML)
`AgentManifest::from_toml` (`manifest.rs:223`) parses a hand-written TOML subset into `AgentManifest` (`manifest.rs:15`): `name, version, description, author, wasm, lang`; `required`/`optional` capabilities; `triggers_intent`/`triggers_event`; `tools_allowed`/`tools_denied`; `max_memory_mb`, `max_runtime_ms`, `network_domains`, `log_tool_calls`, `log_inputs`. Errors are typed `ManifestError::{MissingField, UnknownCap, Syntax(line)}`.

#### Capabilities & policy
`caps.rs` defines a `u64` bitset `AgentCaps`: `FS_READ/FS_WRITE/FS_READ_GLOBAL`, `VAULT_READ/WRITE`, `NET_GET/POST/LISTEN`, `MIC/CAMERA/SPEAKER`, `DISPLAY/CALENDAR/EXEC/AGENT_SPAWN/IPC_SEND`. `ELEVATED = EXEC|VAULT_WRITE|FS_READ_GLOBAL|AGENT_SPAWN|NET_LISTEN`. The effective set is derived by `policy::derive(manifest, granted, user_caps, policy_denied)` (`policy.rs:36`) in three stages: (1) `required ∪ (optional ∩ granted)`; (2) clamp to `user_caps` (hard upper bound); (3) subtract EuroPol `policy_denied`. The result `CapDecision` records `effective`, `dropped_by_user`, `dropped_by_policy`, and `needs_confirmation` (true if elevated).

#### MCP gateway (`mcp.rs`)
JSON-RPC 2.0 `McpGateway` over the open Model-Context-Protocol. `builtin_tools()` (`mcp.rs:36`) defines `ToolDef { name, description, required_cap }`: `fs_read`, `fs_write`, `net_get`, `net_post`, `vault_get`, `display_notify`, `calendar_read`, `mic_record`, `agent_spawn`, `exec`. `handle(name, caps, json_rpc, backend)` (`mcp.rs:109`) parses the request, looks up the tool's required cap, and **refuses with `ERR_CAP_DENIED = -32001`** if the caps lack it. Every call appends an `AuditRecord` (P3 audit). `list_for(caps)` exposes only the tools the cap-set may call.

The kernel ships two backends: `KernelBackend` (echo stub) and the **real** `FsToolBackend` (`agent.rs:66`) that maps `fs_write`/`fs_read`/`display_notify` onto EuroFS — but **only inside the agent's sandbox dir `/agents/<name>/`**. `sandbox_path` (`agent.rs:75`) strips `.`/`..`/empty segments so an agent can never escape its root. `net_get`/`vault_get`/`exec` are explicitly **not yet wired** in-kernel (return an error string) — the real wiring is the userspace daemon.

#### MCP daemon (`mcpd.rs`)
The gateway served over a **real AF_UNIX socket** `/run/euroagent/mcp.sock`. The boot self-test binds the socket, has a client connect, sends a JSON-RPC tool-call over the socket, serves it through the cap-gated gateway onto EuroFS, and verifies the round-trip plus that the file was actually written.

#### The agent loop (`agentloop.rs`)
`run(name, caps, llm, gateway, tools, messages, max_steps)` (`agentloop.rs:41`) drives **model → tool → result → model → final answer**: it asks `LlmBackend::step` for the next move; on `LlmResponse::Text` it returns the `AgentRun` (`answer, tool_calls, denied, truncated, log`); on `LlmResponse::ToolCall` it builds a JSON-RPC call, runs it through the gateway (cap-gate + audit), records allowed/denied in a transcript `log`, and feeds the result back as a `Role::Tool` message. Bounded by `max_steps`.

#### LLM backend — Ollama HTTP (`llm.rs`)
`Message`/`Role`, `LlmResponse::{Text, ToolCall}`. `ollama_http_request(host, model, messages, tools)` (`llm.rs:117`) builds a raw HTTP/1.1 `POST /api/chat`; `parse_http_response(raw)` (`llm.rs:135`) parses the real HTTP reply. In the kernel, `NetOllama` (`agent.rs:160`) sends this over EuroNet-TCP to the **local, sovereign** endpoint `10.0.2.2:11434` (QEMU SLIRP gateway), default model `mistral:7b-instruct`. The `[bb1]` self-test (`agent.rs:203`) proves the transport end-to-end (or reports "transport ready, no endpoint" if Ollama isn't running). **Honest status:** the *transport, request builder and response parser are real*; the boot/dispatch demos drive the loop with a *scripted mock model* (`ScriptedLlm`, `agent.rs:129`) when no live endpoint is reachable — labelled as such in code.

#### Bundle & registry (`.euroa` bundle, Ed25519)
`bundle.rs`: `AgentBundle { manifest_toml, wasm, signature: [u8;64] }`. `signing_message(manifest, wasm)` (`bundle.rs:40`) is a domain-separated message: `DOMAIN ‖ len(manifest) ‖ manifest ‖ wasm`. `verify(pubkey)` (`bundle.rs:53`) checks the Ed25519 signature **before** parsing the manifest (no trust before verification); a tampered WASM under the same signature fails. `registry.rs`: `AgentRegistry::install(bundle, trusted_pubkey)` (`registry.rs:75`) verifies, then records `InstalledAgent`. **Anti-hijack:** if an agent name already exists under a different publisher key, install returns `RegistryError::PublisherMismatch` — a second publisher cannot overwrite "facilitator" (`registry.rs:83`). The `[aa]` self-test proves valid-bundle-accepted, tampered-rejected, install-OK, and hijack-blocked.

#### Shell & self-tests
`euroagent` subcommands (`agent.rs:468`): `list`, `caps`, `mcp list`, `inspect`, `llm [prompt]`, `dispatch test <intent>`. Self-tests: `[aa]` (full chain manifest→caps→MCP→intent→loop→Ed25519 bundle→registry), `[aa-fs]` real FS tools (write+read on EuroFS, path-escape blocked, no-cap denied), `[bb1]` LLM transport. Intent routing: `intent::route(text, routes)` scores by `agent_score`.

### 5. EuroLocale — 24 EU languages

**Crate:** `crates/eurolocale`. **Kernel:** `locale.rs`.

Full localisation for the 24 official EU languages. `Lang` enum (`lang.rs:10`) covers `Bg, Hr, Cs, Da, Nl, En, Et, Fi, Fr, De, El, Hu, Ga, It, Lv, Lt, Mt, Pl, Pt, Ro, Sk, Sl, Es, Sv`, with `Lang::ALL: [Lang;24]` and `code()` (ISO 639-1). `Locale::{new, parse(tag), int, money, date, date_long, plural, sort, currency_code}` (`lib.rs:34`). Underneath:
- **Number** (`number.rs`): per-language grouping + decimal separators.
- **Currency** (`currency.rs`): `symbol`, `iso_code`, `format_minor`/`format_amount`.
- **Date** (`datefmt.rs`): `format_short`, `format_long`, localized `month_name`.
- **Plural** (`plural.rs`): `Plural::{One, Few, Many, Other}` selected by a per-language `PluralSystem` — `OneOther`, `FrenchOneOther`, `WestSlavic`, `Polish`, `Croatian`, `Latvian`, `Lithuanian`, `Slovenian`, `Irish`, `Maltese`, `Romanian` — encoding the actual CLDR-style plural rules.
- **Collation** (`collation.rs`): `collate(lang, a, b)` / `sort(lang, items)` — language-aware ordering.

### 6. EuroInstall — guided installer / live-image planner

**Crate:** `crates/euroinstall`. **Kernel:** `installer.rs` (planner + GUI window, marker `[q1]`), `instexec.rs` (execution, marker `[q1x]`).

A pure-logic installer **planner**: validate a config, then emit an ordered, executable install plan. `validate(cfg)` (`lib.rs:118`) rejects bad configs with typed `PlanError`. `partition_layout(disk)` (`lib.rs:143`) computes a GPT layout with 1 MiB alignment: **ESP · EuroOS-A · EuroOS-B · EuroVar** (two equal A/B system slots, EuroVar gets the remainder). `plan(cfg)` (`lib.rs:168`) returns an ordered `Vec<Step>`: `Partition`, `FormatEsp`, `FormatSystem` (EuroFS on slot A, copied to B), `FormatVar`, write kernel image to A+B, install the two-stage loader, enable FDE (ChaCha20, key sealed to the TPM), `ConfigureLocale`, `ConfigureKeymap`, `SetHostname`, create first user, `ProvisionEuroCa`, write A/B boot-config + activate slot A. Live-boot mode emits only RAM-resident config steps. The `[q1x]` self-test actually *runs* a dry-run against a 4 MiB RAM-disk, then **remounts** (simulating reboot) and verifies hostname/user/locale/EuroCA persisted.

### 7. EuroPkg — package manager (.eupkg)

**Crate:** `crates/europkg`. **Kernel:** `pkg.rs` (marker `[m2]`).

`.eupkg` = **ZIP + manifest + SHA-256 + Ed25519 signature**; `eupkg` verifies the signature *before* installation. The resolver: `Version{major, minor, patch}` with ordering; `Constraint::{Any, Exact, AtLeast, Caret}` where `Caret(^1.2.0)` = `>=1.2.0 && <2.0.0`. `Repo::resolve(root)` (`lib.rs:155`) does a depth-first walk producing a **topologically ordered** install list (dependencies before dependents), picks the **highest** version satisfying each constraint, detects cycles, and detects version conflicts (`ResolveError::{NotFound, NoMatchingVersion, Conflict, Cycle}`). The `[m2]` self-test resolves a sample `eurosuite` graph and asserts a single shared `libc`, correct topo ordering, plus missing-dep and cycle detection.

### 8. EuroRepro — reproducible-build attestation & consensus

**Crate:** `crates/eurorepro`. **Kernel:** `repro.rs`.

`BuildSpec` (`lib.rs:36`) holds inputs + sorted env-vars; `id()` is the SHA-256 of the canonically-encoded inputs (order-independent). `volatile_inputs()` flags env keys (time/randomness/paths) that would break reproducibility. `attest(builder_key, spec_id, output_hash)` (`lib.rs:121`) produces an Ed25519-signed `Attestation`; `reproduce(rebuilt_output)` compares a locally rebuilt binary against the attested hash (`Reproduction::{Reproducible, Mismatch}`). **`consensus(spec_id, attestations, quorum)`** (`lib.rs:149`) tallies, per `output_hash`, the number of *unique, validly-signed* builders, and returns the hash that **≥ quorum distinct builders** agree on — independent-reproduction trust without a central authority.

### 9. EuroWeb + EuroJS — sovereign browser engine

**Crates:** `crates/euroweb` (2941 lines), `crates/eurojs` (1662 lines). **Kernel:** `web.rs` (marker `[ab]`), `webview.rs`, `jsapp.rs` (marker `[js]`).

#### HTML pipeline (engine works)
1. **Tokenizer** (`tokenizer.rs:127`): a proper HTML5 state-machine (`State` enum — `Data, TagOpen, TagName, AttrName, AttrValue{...}, Comment*, Doctype*`, etc.) emitting `Token::{Doctype, StartTag, EndTag, Char, Eof}`. It handles RCDATA (`<title>`) and RAWTEXT (`<script>` keeps `a<b` as text).
2. **Entities** (`entities.rs:71`) — named + numeric refs.
3. **Tree construction** (`parser.rs:185`, `parse(html) -> Dom`).
4. **DOM** (`dom.rs`): arena `Dom` of `Node { kind, … }`; `append`, `tag`, `attr`, `text_content`.
5. **CSS** (`css.rs`): `parse_stylesheet`, `Selector` with `Specificity(id,class,type)` and `matches(dom,node)`; `compute(dom, sheets)` runs the **cascade with specificity + inheritance** producing per-node `ComputedStyle`.
6. **Layout** (`layout.rs`): block box model — `Rect`, `EdgeSizes`, `Dimensions`, `LayoutBox`, `BoxType`; `layout(dom, styles, viewport_width)`. **Flexbox** (`flex.rs:47`): grow/shrink/basis with `Justify` and gap.
7. **Paint** (`paint.rs:27`): `paint(...) -> Vec<DisplayItem>`; `parse_color`.

The `[ab]` self-test parses a realistic page and asserts node counts, RCDATA/RAWTEXT correctness, a CSS cascade (`h1.hero` 0,1,1 beats `.hero` and UA `h1`), inheritance, block stacking (two 50px divs → y=0, y=50), and flex (two grow-1 in 300px → 150px each).

#### EuroJS (engine works)
`eurojs::eval(src)` / `run_capture(src)`. `lex` → `Tok`, `Parser::parse_program` → AST (`Expr::{Num, Str, Bool, Array, Object, Bin(BinOp), And, Or, Assign, Cond, Call, Member, Index, Func, Update}`; `Stmt::{Let, Expr, If, While, For, Return, FuncDecl, Block}`). The tree-walking `Interp` (`interp.rs:80`) supports scopes/closures and built-ins `console.log` (captured) and `Math.*`. The `[js]` self-test verifies recursive `factorial(6)=720`, a closure `adder(40)(2)=42`, an array+loop+object program (`sumEven=30`), and `Math.pow(2,10) → 1024`.

#### The browser window (`webview.rs`)
A usable browser with tabs and an editable address bar. Pages are **actually fetched** over HTTP/HTTPS (via `net::fetch_full` with EuroTLS for `https://`) and rendered by the engine. **Honest scope:** it parses+lays-out+paints real fetched HTML, but large sites render only partially (~150 KB cap), and the engine is a from-scratch subset, not a standards-complete browser.

### 10. EuroSuite — office suite

**Crates:** `eurodoc` (model), `eurodocio` (IO: OOXML/ODF/HTML/XML), `eurocalc` (spreadsheet formulas), `euroreken` (calculator). **Kernel:** `suite.rs` (marker `[es]`), `suite_ui.rs`, `calc_ui.rs`, `reken.rs`.

One **Universal Document Model** (`eurodoc`) serves all three apps. `Document::{writer, sheet, deck}`. Structure: `Block::{Paragraph, Table, …}`, `Paragraph{props, runs}`, `Run{text, props}` with `RunProperties` (bold/italic/color); `StyleRegistry` with style inheritance. Spreadsheet `SheetBody{cells}` with `Cell::{Empty, Text, Number{scaled,scale}, Formula}`. Presentation `Slide{title, blocks}`.

- **Document IO (`eurodocio`)** — a small XML engine underpins **OOXML** (`.docx`-style round-trip preserving bold), **ODF**, and **HTML export**.
- **Spreadsheet formulas (`eurocalc`)** — `eval(formula, sheet)` (`lib.rs:258`) tokenises + parses A1-style cell references, ranges, arithmetic, and functions `SUM, AVERAGE, MIN, MAX, COUNT, ABS, ROUND, IF`, with **cycle detection** (`CalcError::Cycle`). The `[es]` self-test verifies `=A4*2+MAX(A1:A3)=150`, `=AVERAGE(A1:A3)=20`, and a cycle is caught.
- **Calculator (`euroreken`)** — `eval(expr)` (scientific: `+ - * / ^`, `sqrt`, `sin`, `pi`), `eval_programmer` (bitwise + hex), `format_base`, `convert` (units). The `reken` self-test checks `1+2*3=7`, `2^10=1024`, `sqrt(2)+sin(pi/2)≈2.414`, `0xF0|0x0F=255`, `1 mi=1.609 km`, `100°C=212°F`.

**Honest status:** the document/spreadsheet/IO/calculator **engines** are real and round-trip-verified at boot. `suite_ui.rs`/`calc_ui.rs` provide windowed Writer/Calc/Impress + a calculator UI that *render* and accept input, but these are GUI front-ends over the engines rather than full-featured office applications.

### 11. EuroApps — engines (mostly headless cores, verified at boot)

Twelve app crates. **Important honesty note:** their kernel modules (`notes.rs`, `archive.rs`, `safe.rs`, `files.rs`, `media.rs`, `clip.rs`, `clockapp.rs`, `shot.rs`, `contacts.rs`, `calapp.rs`, `musicapp.rs`, `mailapp.rs`) contain **only `selftest()` functions — none define a `render()` window**. These are *verified engines/libraries*, not windowed desktop apps. (By contrast `webview.rs`, `suite_ui.rs`, `calc_ui.rs`, `settings_ui.rs`, `installer.rs` do have real GUI render paths.)

| App | Crate | Marker | Engine — what's real |
|---|---|---|---|
| **EuroNotes** | `euronotes` | `[an]` | Markdown `parse(md) -> Note` |
| **EuroArchive** | `euroarchive` | `[az]` | Real **tar** `write_tar`/`read_tar` with checksum validation + `verify_manifest` signature hook |
| **EuroSafe** | `eurosafe` | `[sf]` | App-permission **risk model**: `Capability::{weight,label}`, `raw_score`/`effective_score`/`risk`, sandboxed ×0.6, unverified ×1.5, flags vault+net+exec exfil combos |
| **EuroFiles** | `eurofiles` | `[fl]` | File browser model: `DirEntry`, `FileKind`, `Badge` (signed/encrypted), `SortKey`, hidden/extension detection |
| **EuroMedia** | `euromedia` | `[mv]` | Real **QOI** image codec `encode`/`decode`, `Image` with `flip_vertical`/`crop`; lossless round-trip + >50× compression on solids |
| **EuroClip** | `euroclip` | `[cl]` | Clipboard `copy_text`/`copy_image`/`history`/`set_pinned`/`search`/`expire` with retention + pin |
| **EuroClock** | `euroclock` | `[ck]` | `time_of_day`, `format_time`, `format_duration`, `WorldClock` (EU defaults), `Timer` |
| **EuroShot** | `euroshot` | `[st]` | Screenshot region capture, `Region::{clamp_to, area}`, annotations `Annotation::{arrow, boxed, label}` |
| **EuroContacts** | `eurocontacts` | `[ct]` | **vCard** `parse`/`to_vcard`, `AddressBook::{from_vcards, sort, search, in_group}` |
| **EuroCalendar** | `eurocalendar` | `[cal]` | Civil-date math `days_from_civil`/`civil_from_days`, `weekday_mon0`; recurrence `Recurrence`/`Freq`/`Event` |
| **EuroMusic** | `euromusic` | `[mu]` | `Track`, `Library::{add, search, album, artists, total_duration}`, `Player`/`Repeat` |
| **EuroMail** | `euromail` | `[ma]` | **MIME/email** parser: `base64_decode`, `quoted_printable_decode`, RFC 2047 `decode_header`, `parse_headers`, `parse_addresses`, `parse(raw)` |

Each `selftest()` runs the engine on representative input and prints a `[xx] EuroXxx … ✓/FOUT` line.

### Part 6 — summary of honesty boundaries

- **Real & boot-verified engines:** all coreutils; the WASM interpreter + capability gating; the EuroAgent manifest/caps/policy/MCP-gateway/agent-loop/Ed25519-bundle/registry/anti-hijack and the AF_UNIX MCP daemon; the Ollama HTTP transport (request builder + response parser); EuroLocale's 24-language rules; the EuroPkg semver resolver; EuroRepro attestation/consensus; the EuroWeb HTML→DOM→CSS→layout→flex→paint pipeline and the EuroJS interpreter; the EuroSuite document model + OOXML/ODF/HTML IO + spreadsheet formula engine + calculator; and every EuroApp engine.
- **Compat bridge, not identity:** the Linux ABI in `ring3.rs` is a deliberate emulation layer, capability-gated, defaulting to `-ENOSYS` for the unimplemented tail; it reports a synthetic `/proc/version` for compatibility only.
- **Engine ≠ full app:** EuroWeb/Writer/Calc/Impress/calculator have real GUI windows but are from-scratch subsets (the browser renders large pages only partially). The twelve EuroApps in §11 are **verified engines with no GUI window yet** — their kernel modules are self-test-only.
- **Mock clearly labelled:** the agent boot/dispatch demos use a `ScriptedLlm` mock when no local Ollama endpoint is reachable; the in-kernel MCP backend wires `fs_*`/`display_notify` to real EuroFS but leaves `net/vault/exec` as explicit "not yet wired in the kernel" stubs pending the userspace daemon.


---

## Appendix A — Boot self-test marker index

Every subsystem proves itself at boot by printing one labelled line over the COM1 serial console. Booting headless with `-serial stdio` and grepping for `[` gives a single, externally-auditable record that the whole system works. The markers, grouped by Part:

#### Part 1 — boot & core kernel
| Marker | Proves |
|---|---|
| `[loader]` | Two-stage A/B UEFI loader picked + loaded the kernel image |
| `[euro]` | `int3` breakpoint handled (IDT live); Ed25519 verify-before-execute + tamper rejection |
| `[a2]` | Guarded boot stack (write/read magic, guard page absent) |
| `[g1]` | Kernel-stack-overflow recovery on the page-fault IST stack |
| `[mm]` | 64 MiB process frame pool reserved |
| `[apic]` | LAPIC software-enabled + PIT-calibrated periodic timer |
| `[hpet]` | HPET counter advancing (1M spin measurement) |
| `[j2]` / `[j2-blk]` | MSI-X programmed (xHCI event ring + virtio-blk completion) |
| `[smp]` | AP bring-up, parallel sum across cores, IPIs, TLB shootdown, per-CPU schedulers |
| `[S2]` | Scheduler priority (nice) + real timed sleep counters |
| `[sec]` / `[cap]` | SMEP/SMAP/NX enabled; a denied syscall logged |
| `[isolatie]` | A ring-3 page fault kills only that process |
| `[exec]` / `[h3]` / `[h3-fs]` | ELF exec by name; dynamic linker resolving `.so` relocations |
| `[init]` | EuroInit supervisor restarts a dead service |
| `[j1]` / `[j1-cache]` | Lock-free klog; concurrent RwLock FS cache |
| `[y]` | Crash dump written + read back across boots |
| `[pci]` / `[acpi]` / `[r]` / `[i3-aml]` / `[xhci]` | PCI scan · MADT/FADT parse · EuroDevice tree · DSDT AML interpretation · USB enumeration |

#### Part 2 — storage & filesystem
| Marker | Proves |
|---|---|
| `[blk]` / `[nvme]` | virtio-blk round-trip · NVMe write/read/verify + SMART |
| `[gpt]` / `[g2]` | GPT A/B partition install · EuroFS mounts (root, /var, 2nd disk, NVMe) |
| `[g4]` / `[euroupdate]` | A/B slot raw-block state survives reboot; signed image write to inactive slot |
| `[g5]` | Filesystem integrity scrub |
| `[k3]` | Full-disk encryption (ChaCha20) read-after-write on an encrypted volume |
| `[s]` | CoW snapshot create → modify → rollback, byte-identical |
| `[j3]` / `[j3-fault]` | Fault-driven swap-out then transparent swap-in |
| `[l1]` | Immutable + append-only flags enforced by the filesystem |
| `[p3]` | Append-only audit log: tamper refused, genuine append grows the on-disk trail |

#### Part 3 — networking, crypto, PKI
| Marker | Proves |
|---|---|
| `[g3]` / `[h1]` | poll/select · AF_UNIX round-trip with EOF |
| live `ping`/`fetch`/`https` | ARP/ICMP/DNS/HTTP and a real TLS 1.3 handshake to a public host |
| `[n3]` / `[n2]` / `[n1]` / `[bb3]` | Firewall block/allow · VPN handshake + encrypted round-trip · WiFi PTK derivation · honest "no radio in QEMU" |
| `[o1]` / `[o2]` / `[ca]` | TPM measured boot (PCR extend) · attestation quote/replay-reject · CA issue/verify/revoke |

#### Part 4 — security & identity
| Marker | Proves |
|---|---|
| `[x]` / `[u]` | EuroPol policy → caps (deny wins) · EuroVault seal/unseal, cap-gated read |
| `[k1]` (×2) | EuroID full chain (Argon2id login, lockout, soft-delete, hash-chain) + live `eurousers` CLI path |
| `[v]` | EuroIDM signed token, escalation blocked |
| `[container]` | EuroSandbox chroot-escape resistance + cap shrink |
| `[w]` / `[z]` | OpenMetrics render · SMART health score |

#### Part 5 — display, input, audio, a11y
| Marker | Proves |
|---|---|
| `[bb2]` / `[k4]` | Native virtio-GPU 2D scanout (real device) · GPU command stream (protocol-only) |
| `[h2]` / `[bb3]` / `[h5]` | AF_UNIX display server window · Wayland wire-protocol handshake → titled window |
| `[wm]` / `[ft]` | Window-management hit-testing/maximise/restore · TrueType font metadata parse |
| `[bb8]` / `[p2]` | Live accessibility focus → multilingual announcement → role earcon through the HDA DAC · deterministic a11y tree |
| `[bb4]` | IPP-over-TCP printing (Get-Printer-Attributes + Print-Job) |

#### Part 6 — userland, agents, apps
| Marker | Proves |
|---|---|
| `[cu]` | GNU-compatible coreutils + pipelines |
| `[h4]` / `[h4-ctr]` | WASM interpreter + capability-gated WASI host imports (per-container) |
| `[aa]` / `[aa-fs]` / `[bb1]` | EuroAgent full chain (manifest→caps→MCP→loop→Ed25519 bundle→registry, anti-hijack) · real EuroFS tools · Ollama HTTP transport |
| `[ab]` / `[js]` | EuroWeb HTML→DOM→CSS→layout→flex→paint · EuroJS interpreter |
| `[es]` / `reken` | EuroSuite document/spreadsheet/IO round-trip + formula engine · calculator |
| `[q1]` / `[q1x]` / `[m2]` | Installer plan · installer dry-run executes + persists · package dependency resolution |
| `[an] [az] [sf] [fl] [mv] [cl] [ck] [st] [ct] [cal] [mu] [ma]` | The twelve EuroApp engines (Markdown, tar, risk-model, file model, QOI, clipboard, clock, screenshot, vCard, calendar, music library, MIME/email) |

---

## Appendix B — Global honesty matrix

EuroOS's hard rule is to never present a demo as a finished product. Consolidated across all Parts:

#### Real and verified (host-tested + boot-verified)
The core kernel (paging/W^X, scheduler, SMP, IDT/APIC, drivers); EuroFS (CoW, A/B superblock, snapshots, scrub, immutability) with crash-consistency proven; virtio-blk FLUSH barrier; ChaCha20 full-disk encryption; A/B atomic updates with Ed25519-verified apply; the complete TCP/IP path (handshake, reliable send/retransmit, teardown) and DNS anti-spoofing; **TLS 1.3 with real X25519 ECDHE, ChaCha20-Poly1305, the SHA-256 key schedule pinned to RFC vectors, the Finished MAC, and certificate-chain validation against 30 EU-first roots** (including hand-rolled RSA-PKCS1/PSS verified against OpenSSL output); the sovereign VPN (quadruple-DH + HKDF + anti-replay); TPM measured boot; the CA and attestation; the **EuroGuard** capability gate on both ABIs; **Argon2id and Blake2b pinned to the official RFC 9106 / RFC 7693 test vectors**; the SHA-256 audit hash-chain with tamper tests; Ed25519 binary signing with `verify_strict` + a boot tamper test; the WASM interpreter + capability-gated host imports; the EuroAgent manifest/caps/policy/MCP-gateway/agent-loop/Ed25519-bundle/registry + AF_UNIX MCP daemon; EuroLocale's 24-language rules; the EuroPkg semver resolver; EuroRepro consensus; the EuroWeb HTML→paint pipeline and the EuroJS interpreter; the EuroSuite document/spreadsheet/IO/calculator engines; every EuroApp engine; native virtio-GPU 2D scanout; the real HDA audio driver with LPIB-verified DMA + mixer + earcons; the accessibility tree with multilingual announcements; and IPP printing.

#### Simplified / stub / planned next step
- **TLS**: no OCSP/CRL revocation, no CT/SCT, no name-constraint/path-length/key-usage enforcement, no session resumption.
- **Firewall**: stateless (no connection-tracking table); stricter default-deny is an EuroPol policy choice.
- **TCP**: the RFC 6298 RTO + RFC 5681 Reno estimator is host-tested as a library but not yet driving the live send loop (which uses fixed retransmit rounds).
- **FDE**: the key is TPM-*sourced* (hardware RNG) but not yet PCR-*sealed*; the sealed-key superblock slots are reserved/zeroed.
- **EuroGuard**: `CURRENT_CAPS` is a single live cap-set (userspace runs largely pre-scheduler), not yet a per-task capability field; W^X falls back to RWX for binaries with a mixed RWE segment.
- **EuroID**: the kernel boot self-test uses *reduced* Argon2id parameters for TCG speed (the full 64 MiB / t=3 / p=4 params + the RFC vector are proven in host tests); the store is in-memory and rebuilt each boot (EuroFS persistence of `/etc/euro/*.db` with IMMUTABLE flags is the next mile); the desktop login still uses the legacy SHA-256 `auth.rs` path, to be rewired onto `euroid::authenticate`.
- **EuroIPC**: functional message bus + audit, but the permission check is an open allow-all hook pending EuroGuard-policy coupling.
- **EuroAML**: a deliberate subset — no OperationRegion/Field side-effects, no control flow.
- **Kernel heap**: the linked-list allocator, not yet the planned EuroMM slab allocator. APs load no TSS (timer-only).
- **GPU**: 2D scanout only — no 3D/Virgl acceleration.
- **Accessibility**: focus events + multilingual announcements + role earcons are real; **intelligible speech synthesis (TTS) is explicitly not implemented** — earcons are tones, not words.
- **EuroAgent**: the transport/request/response code is real; boot/dispatch demos use a scripted mock model when no local Ollama endpoint is reachable; the in-kernel MCP backend wires `fs_*`/`display_notify` to real EuroFS but leaves `net`/`vault`/`exec` as explicit "not yet wired in-kernel" stubs pending the userspace daemon.
- **EuroWeb browser**: parses/lays-out/paints real fetched pages but is a from-scratch subset; large sites render only partially (~150 KB cap).
- **EuroApps (the twelve in Part 6 §11)**: verified *engines* with no GUI window yet (their kernel modules are self-test-only); EuroSuite Writer/Calc/Impress and the browser/settings/installer do have real GUI render paths but are subsets, not full applications.

#### Hardware-attended (honestly not a false checkmark)
- **WiFi radio**: QEMU emulates no 802.11 radio, so only the protocol core (frame parsing + WPA2/3 PTK derivation) runs in software; the Intel-radio bring-up needs real hardware.

---

## Appendix C — Build, run & verify

```bash
# 1. Build the kernel UEFI binary (release, ~18 s)
cargo kbuild-release

# 2. Build the bootable FAT32 image (two-stage loader + A/B kernel slots)
./scripts/build.sh release        # → eurokernel.img

# 3. Boot headless in QEMU with serial self-tests
qemu-system-x86_64 -machine q35 -m 256M -cpu qemu64,+smep,+smap \
  -bios /usr/share/ovmf/OVMF.fd -drive format=raw,file=eurokernel.img \
  -display none -serial stdio -no-reboot
#   → grep the serial output for the [xx] markers in Appendix A.
#   (No KVM in the build sandbox → TCG is ~60× slower; on real hardware/KVM it boots in ~1–2 s.)

# 4. Run the 793 host tests (no VM; the sans-IO crate cores under std)
cargo test

# 5. Screenshot the desktop (QMP screendump)
python3 scripts/screenshot.py eurokernel.img boot.png

# 6. Multi-disk / NVMe / SMP harnesses
python3 scripts/run-multidisk.py ; python3 scripts/run-nvme.py
```

Downloadable preview images (raw `.img`, `qcow2`, `vmdk`, and a one-click QEMU bundle) are published at <https://euro-os.eu/try/>; `scripts/release-web.sh` regenerates them deterministically from `eurokernel.img`.

---

## Appendix D — Glossary of Euro* subsystems

| Name | Crate(s) / module | One line |
|---|---|---|
| **EuroFS** | `eurofs` | Copy-on-write filesystem: A/B superblock, inodes/extents, XXH3 checksums, snapshots, immutability, self-healing scrub |
| **EuroFDE** | `eurofde` | Transparent full-disk encryption (ChaCha20, per-block nonce, TPM-sourced key) |
| **EuroUpdate** | `euroupdate` | Atomic A/B system updates with bounded-try auto-rollback |
| **EuroMM** | `euromm` | Bitmap frame allocator + CLOCK swap victim selection |
| **EuroNet** | `euronet` | From-scratch TCP/IP stack (Ethernet→TCP, DNS, sockets, HTTP, AF_UNIX) |
| **EuroTLS** | `eurotls` | TLS 1.3 client (X25519, ChaCha20-Poly1305, X.509 chain validation, EU-first roots) |
| **EuroFW** | `eurofw` | 5-tuple stateless packet filter |
| **EuroVPN** | `eurovpn` | Sovereign WireGuard-style tunnel (quadruple-DH, HKDF, anti-replay) |
| **EuroWiFi** | `eurowifi` | 802.11 frame parsing + WPA2/3 key derivation (protocol core) |
| **EuroTPM** | `eurotpm` | TPM 2.0 command codec + TIS transport (measured boot, RNG) |
| **EuroCA** | `euroca` | Sovereign local certificate authority (Ed25519, own compact format) |
| **EuroAttest** | `euroattest` | Remote attestation (signed PCR quotes + nonce) |
| **EuroSign** | `eurosign` | Document-signing envelope (`.eurosig`) |
| **EuroGuard** | `ring3.rs`, `crypto.rs` | The capability security model: syscall gating, W^X/SMEP/SMAP, Ed25519 code authenticity |
| **EuroPol** | `europol` | Declarative policy → capability mask (deny wins) |
| **EuroVault** | `eurovault` | Capability-gated encrypted secrets store (ChaCha20-Poly1305) |
| **EuroID** | `euroid` | Sprint K1 user management: from-scratch Argon2id, login, lockout, hash-chain audit |
| **EuroIDM** | `euroidm` | Enterprise identity: Ed25519-signed OIDC-like tokens, group→capability |
| **EuroSandbox** | `eurosandbox` | Capability-scoped, chroot-safe containers |
| **EuroObserve** | `euroobserve` | Lock-free OpenMetrics/Prometheus metrics |
| **EuroHealth** | `eurohealth` | SMART-based system-health scoring |
| **EuroCrash** | `eurocrash` | Structured kernel crash minidumps with cross-boot recovery |
| **EuroDisplay / Wayland** | `eurodisplay`, `eurowl` | AF_UNIX surface protocol + the real Wayland wire protocol |
| **EuroGPU** | `eurogpu` | virtio-gpu 2D command model (native scanout driver) |
| **EuroFont** | `eurofont` | TrueType/sfnt metadata parser |
| **EuroAudio** | `euroaudio` | Software audio mixer (under the HDA driver) |
| **EuroAccess** | `euroaccess` | EN 301 549 accessibility tree + multilingual screen reader |
| **EuroPrint** | `europrint` | Driverless IPP-over-TCP printing |
| **EuroCoreutils** | `eurocoreutils` | GNU-compatible userland (grep/sort/find/…) + pipelines |
| **EuroWASM** | `eurowasm` | `no_std` WebAssembly interpreter + capability-gated WASI |
| **EuroAgent** | `euroagent` | Sovereign agent runtime: `.euroa` bundles, MCP gateway, agent loop |
| **EuroLocale** | `eurolocale` | Localisation for all 24 official EU languages |
| **EuroInstall** | `euroinstall` | Guided installer / live-image planner |
| **EuroPkg** | `europkg` | `.eupkg` package format + semver dependency resolver |
| **EuroRepro** | `eurorepro` | Reproducible-build attestation + multi-builder consensus |
| **EuroWeb / EuroJS** | `euroweb`, `eurojs` | From-scratch HTML/CSS engine + JavaScript interpreter |
| **EuroSuite** | `eurodoc`, `eurodocio`, `eurocalc`, `euroreken` | Office engines: document model, OOXML/ODF IO, spreadsheet formulas, calculator |
| **EuroApps** | `euronotes`, `euroarchive`, `eurosafe`, `eurofiles`, `euromedia`, `euroclip`, `euroclock`, `euroshot`, `eurocontacts`, `eurocalendar`, `euromusic`, `euromail` | Twelve app engines (Markdown, tar, risk model, file model, QOI, clipboard, clock, screenshot, vCard, calendar, music, MIME/email) |

---

*End of reference. Generated from the EuroOS source tree at build `2026.06.08`; every claim is traceable to a `file:line` citation or a `[xx]` boot marker. When in doubt, the code and the serial log are authoritative.*
