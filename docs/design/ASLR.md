# ASLR for Linux-ABI processes: a virtual arena per process (BUG-088)

Status: design only, nothing implemented. Written against `feature/app-control` @ `e4b8ba4`;
the sibling branches `feature/11f-keylat`, `feature/11f-mem` and `feature/11f-integ` (`7253cb1`)
were read for what touches this (section 1.5). Line numbers are those of `e4b8ba4`.
Earlier work: `docs/proof/kbugs-2026-11e/README.md` (commits `aa1a096`, `8c3b146`), tracker
entry BUG-088. Things that were read but not proven are marked **(not verified)**.

## 0. Summary

A glibc process has two kinds of memory today. The *demand region* (PML4 slot 2) is already
virtual: its pages are mapped one by one and the kernel reaches them only through the page
tables of the process. The *arena* of the launched process (executable or ld.so, brk heap,
small mmaps, stack) is one contiguous piece of RAM that is mapped at its own physical address
inside the kernel's identity map. That is why the PIE base and ld.so have 0 random bits and why
the other slides are bounded by the arena size.

The main design choice: **the launched process gets what its fork children already have.** A
fork child runs on an arena whose virtual address is not the address of its frames
(`fill_remap_tables_lazy`, BUG-156), and Chromium runs every renderer, GPU and utility process
that way. The launched process is the only one that is still identity-mapped. We map its arena
at a random virtual base above the kernel's identity map, widen the demand region from one PML4
slot to many, and give every region its own draw from the kernel DRBG. The kernel keeps its
identity map for *frames* (it stays the way to reach physical memory); only the equality "user
address = physical address" goes.

No new memory manager, no higher-half kernel, no VMA tree. Nine small steps (section 5); the
first two ship without touching the arena and already bring libc, large mmaps and the browser
binary to the Linux level.

## 1. Current state

### 1.1 Address space of a process today

| PML4 slot | Range | Contents |
|---|---|---|
| 0 | 0 .. 512 GiB | kernel, identity, supervisor, 1 GiB and 2 MiB pages. The **user arena is punched into this slot at virtual = physical** with the USER bit (`paging::try_build_address_space*`, `rwx_big_inner`). Userspace-driver DMA pages too (`map_user_identity_4k`). |
| 1 | 512 GiB .. 1 TiB | kernel, identity, supervisor; shared PDPT (`HIGH_PDPT`): high MMIO window, guarded kernel stacks |
| 2 | 1 TiB .. 1.5 TiB | demand region, `DEMAND_BASE` = `0x100_0000_0000`, `DEMAND_SIZE` 480 GiB (`ring3.rs:11407`) |
| 3 | 1.5 TiB .. 2 TiB | vDSO, fixed `VDSO_BASE` = `0x1F0_0000_0000` (`ring3.rs:9532`) |
| 4 .. 255 | 2 TiB .. 128 TiB | unused |

The kernel has no higher half: slots 256..511 are empty. User space can therefore never use
addresses below 1 TiB; 127 TiB above it are free.

### 1.2 What is randomised today (`[aslr]`, 64 starts of `userland/glibc/gaslr`)

`kernel/src/aslrtest.rs` counts per region the address bits that differ between starts and
wants mmap >= 28, stack >= 22, PIE >= 28 (the Linux defaults). Source of the slides:
`ring3::aslr_slides` (`ring3.rs:5346`), one 8-byte draw from `entropy::draw_wait`.

| Region | Where it lives | Bits today | Why not more |
|---|---|---|---|
| PIE base, embedded program (`run_glibc`) | arena + 0 | **0** | `exe_base = arena` (`ring3.rs:14298`), arena = physical |
| PIE base, program from disk (`glibc_disk_launch`, Chromium) | demand region | **0** | `exe_base = DEMAND_BASE`, a constant (`ring3.rs:12943`, `11095`) |
| ld.so | arena + 8 MiB | **0** | `ldso_base = arena + 0x80_0000` |
| brk | arena + 32 MiB + slide | 8 | slide 0..1 MiB in pages; arena is 64 MiB..1 GiB |
| small mmap (< 64 KiB) | arena window + slide | 9 (seen 9..17) | slide 0..2 MiB; the extra bits are the physical place of the arena, which is not random, only RAM-dependent |
| stack | arena top - slide | 8 | slide 0..1 MiB |
| large mmap, libc and other libraries | demand region, floor `DEMAND_BASE + 192 MiB + slide` | 20 | slide 0..2 GiB in pages; one PML4 slot, Chromium reserves > 256 GiB of it |
| vDSO | fixed | 0 | constant |
| fork child | same as parent | (by design) | |
| exec in a fork child (`do_child_execve`) | new demand slide and in-arena slides; **arena base and exe base stay** | as above | the child keeps the parent's virtual arena |

Not random at all and not in the test: all regions of the arena move together with at most
9 bits between them (one leaked stack address gives ld.so and the heap).

### 1.3 Places that rely on virtual = physical for user memory

This is the list of what has to change. Class A is the real dependency. Class B looks like it
in a grep ("identity-mapped" in a comment) but is the kernel reaching a *frame* by its physical
address, which stays valid. Class C are constants that assume the slot layout of 1.1.

**A. User address used as a physical address, or user memory reached without the process's
page tables**

| # | File / function | What it does |
|---|---|---|
| A1 | `ring3.rs` `run_glibc` (14209), `spawn_glibc_persistent` (12258), `glibc_disk_launch` (12878), `spawn_glibc_disk_persistent` (12682) | allocate the arena, zero it (`write_bytes(arena as *mut u8, ..)`), then use the same number as load address, `ARENA_BASE`, stack top and brk base. All of it on the launcher's CR3 (the boot address space), which sees the arena only because it is identity |
| A2 | `ring3.rs` `load_elf64(elf, base, pages)` (9938) | `base` is both where the bytes are written and the load bias of the relocations and of `LoadInfo` (entry, phdr) |
| A3 | `ring3.rs` `setup_user_stack_glibc(stack_top, ..)` (14535) | writes argv, envp, auxv at `stack_top` and stores pointers computed from the same number (`AT_PHDR`, `AT_BASE`, `AT_ENTRY`, `AT_RANDOM`, argv pointers) |
| A4 | `ring3.rs` `glibc_arena_layout` (5333), `euromm::arena::glibc_layout` | one base for "address in the process" and "address of the RAM" |
| A5 | `paging.rs` `try_build_address_space`, `_big`, `_rwx`, `_rwx_big`, `big_inner`, `rwx_big_inner`, `map_user_arena` | map the arena at `arena_phys` inside PML4[0] |
| A6 | `paging.rs` `arena_slots` (121), `arena_pt` (676), `lazy_pte_ptr` (1093: `virt >> 39 != 0 -> None`), `arena_pte_ptr` (1236), `arena_split` (1261), `lazy_arena_frames` (1611), `table_frames` (741), `free_address_space` (2064), `free_fork_tables` (2114), `fill_remap_tables*` | walk from PML4[0] only: an arena can only lie below 512 GiB. Not virt = phys in itself (fork children use these with virt != phys) but it pins the arena into the kernel's slot |
| A7 | `interrupts.rs` `page_fault_handler` (733, 752) | arena copy-on-write and lazy-arena faults are recognised by `addr >> 39 == 0` |
| A8 | `euromm::cow` "pinned" frames, `paging.rs` `share_arena_cow` (1340..1360), `arena_cow_fault` | the frames of the launched process's arena may not move "because the kernel reaches it by address"; when that process writes a shared page the *children* get the copies (`evict_plan`). Stays correct with a virtual arena over the same contiguous frames; the reason for it goes away (section 3.4) |
| A9 | `ring3.rs` `copy_to_user`, `copy_from_user`, `zero_user`, `write_user`, `read_user`, `user_cstr` and ~10 `from_raw_parts` in `linux_dispatch_inner_raw` | dereference the user address directly. Correct on the process's own CR3 and already used that way for demand-region addresses and by fork children; the SAFETY comments say "identity-mapped", which is only true for the launched process. **Each call is fine as long as it runs on the CR3 of the process that owns the pointer**; the risk is a call on another CR3 (A10) |
| A10 | anything that touches memory of the launched process while another address space is loaded | known: A1..A3 (launch). Known to be safe already: `read_glibc_u32` (5581, goes through `paging::translate_in`), `sync_shared_region` (4196, by frame). **Not verified exhaustively**: thread exit (`GLIBC_CTIDS` clear + wake), signal delivery to another task, the kill at the launcher's deadline, tombstones, `gdbstub.rs`. Step S4 finds them mechanically (fixed non-identity base: every such access is a ring-0 page fault with a RIP) |
| A11 | device I/O straight into a user buffer | **not verified**: no block or network path was found that hands a user address to a device as a DMA address (a grep of the block-driver calls in `ring3.rs` shows none with a syscall argument as buffer), but the audit of S4 must confirm it; this is the one class that would fail silently |
| A12 | native (Euro-profile, musl) processes: `spawn_bg_musl` (7236), `do_fork` (5991, copies the whole 2 MiB arena by address), `do_execve` (6119), `bg_dispatch`/`bg_syscall` (`ARENA_BASE.swap(arena_virt)`), `spawn_daemon`, `spawn_counter_task_in` + `read_counter` (9166, reads by physical address on the boot CR3), `run_dynamic`, `run_interp`, `run_args`, `phase3g.rs`, userspace drivers (`map_user_identity_4k`: DMA pages are identity on purpose) | the 2 MiB arena world. Out of scope of BUG-088 (section 6); userspace-driver DMA pages must stay identity |
| A13 | diagnostics: `demand_addr_origin` (15333), `mm_fail` (15643), `mem_census` / `ram_files_census_inner` ("identity arena"), `reap_orphan_fork_children` (10881) | print or compare `ARENA_BASE` as if it were the frame address |

**B. Kernel reaches a frame by its physical address (stays; listed so nobody "fixes" it)**

`handle_demand_fault_inner` (zero/fill a fresh frame), `paging::copy_user_page`, `page_is_zero`,
all page-table walks, `swapmgr.rs` (walks tables with physical pointers, "< 512 GiB"), the page
cache and RAM files (`euromm::pagecache`, `euromm::ramfile`: keyed by file page and frame, no
user address), `euromm::cow::Refs` (one counter per frame), `vdso_tick`, `alsa::ring_phys`
mapped into a process. None of these depend on where a page is mapped.

**C. Constants and tests that assume the slot layout**

| Item | Where |
|---|---|
| `DEMAND_PML4_IDX = 2`, one slot: `scan_demand_ro`, `demand_phys_sorted`, `walk_demand_region`, `free_demand_region`, `free_demand_region_except`, `clone_demand_region`, `free_demand_table_chain` take one `idx` | `paging.rs`; callers `pcache_reclaim`, `child_keep_list`, `fork_child_teardown`, `do_child_execve`, `do_glibc_fork`, `fork_in_flight_reclaim`, `oom_kill`, `mem_census`, exit (16489) |
| "exe window" = `DEMAND_BASE .. +192 MiB`, floor = `DEMAND_BASE + 0x0C00_0000 + slide` | `ring3.rs` 11076, 13037, 14365, 16518 |
| one-sided tests `ptr >= DEMAND_BASE` meaning "not the arena" | `in_user_arena` (217), `demand_file_backed` (177), `demand_addr_origin`, mremap (17211) |
| `VDSO_BASE` constant; freed by slot number | `vdso_map_into`, `free_process_space` (9714), `VDSO_EHDR_NEXT` |
| `map_user_4k_at`: `va < 512 GiB` | userspace drivers only |
| process-global layout state swapped per fork child (`ChildMem`: `demand_next`, `arena_base`, `arena_span`, brk, heap, maps) | `ring3.rs` 10654; every new per-process layout value (exe base, vDSO base) must be added here or a child runs on the parent's value |

### 1.4 What already works with virtual != physical

- Fork children of the launched process: lazy arena at the parent's virtual address, frames
  one by one from the process pool (`fill_remap_tables_lazy`, `lazy_arena_fault`,
  `share_arena_cow`, `clone_arena_lazy`), exec inside it (`do_child_execve` loads ld.so and
  builds the stack through virtual addresses on the child's CR3).
- The whole demand region for every process: demand faults, file mappings, copy-on-write
  (`demand_cow_fault`, `clone_demand_region`), swap, page cache.
- SMP: threads on application processors, `apuser::shootdown`, `sched::HoldMm` during fork.

### 1.5 Sibling branches (`feature/11f-integ`)

- `544d5a2` (keylat, BUG-307): a private futex is keyed `uaddr | (cr3 tag << 48)`. Needs
  user addresses below 2^48: holds. A *shared* futex and the clear-child-tid wake are keyed by
  the bare virtual address, so two processes only meet on a shared futex if both map the page
  at the same address. True for mappings inherited over fork, already false today for a memfd
  mapped after exec (bump pointers differ). ASLR makes "by accident the same address"
  practically impossible, which is good for false wakes and exposes any code that relied on
  it. Recommended follow-up, independent of this design: key shared futexes by frame + offset.
- `11f-mem` (BUG-281): `mem_census` walks the PTEs of `DEMAND_PML4_IDX` and counts the ranges of
  a fork in progress; both become loops over the demand slots (class C).
- `11f-keylat` X server and serial changes, `11f-small`: no user-memory access. Nothing else
  assumes virt = phys.
- All three change `ring3.rs` (+481/-158 in integ). Implementation starts from the merged
  integ branch, not from `e4b8ba4`.

## 2. Target

### 2.1 Layout (4-level paging, user half below 2^47)

| Zone | Range | Slots | Contents |
|---|---|---|---|
| kernel | 0 .. 1 TiB | 0..1 | unchanged, supervisor. No user page here any more (except userspace-driver DMA pages) |
| legacy | 1 .. 2 TiB | 2..3 | the old demand region and vDSO, only with `aslr=legacy` |
| arena | 8 .. 12 TiB | 16..23 | the arena of the process at a random 2 MiB-aligned base; from S6 three windows of 4 TiB: image 8..12, heap 12..16, stack 16..20 TiB |
| demand | 32 .. 96 TiB | 64..191 | `DEMAND_BASE` = 32 TiB, `DEMAND_SIZE` = 64 TiB. Executable from disk at 32 TiB + draw; mmap floor at 40 TiB + draw, bump upward with `FreeRanges` as today |
| vDSO | 100 .. 100.5 TiB | 200 | two pages at a random page of the slot |
| unused | 100.5 .. 128 TiB | | nothing is ever mapped in the last slot: no user page next to the non-canonical hole (SYSRET) |

Order kept on purpose: kernel < arena < `DEMAND_BASE` <= demand < vDSO. Every one-sided test
of class C stays true.

### 2.2 Entropy per region

All windows are powers of two, so a draw is a mask, no modulo bias.

| Region | Step | Window | Random bits | `[aslr]` varying bits | Linux default |
|---|---|---|---|---|---|
| arena base (ld.so, brk, small mmaps, stack; embedded exe) | 2 MiB | 4 TiB | 21 | 21 + in-arena page slide | |
| embedded PIE in the arena | + 4 KiB slide, 9 bits (new) | | 30 | 30 | 28 |
| ld.so | + 4 KiB slide, 9 bits (new) | | 30 | 30 | 28 |
| brk | + 4 KiB slide, 8 bits (exists) | | 29 | 29 | 13 relative to the exe |
| small mmap | + 4 KiB slide, 9 bits (exists) | | 30 | 30 | 28 |
| stack | + 4 KiB slide, 8 bits (exists) | | 29 | 29 | 22 |
| PIE from disk (Chromium) | max(4 KiB, largest `p_align`) | 4 TiB | 30 (21 if the binary asks 2 MiB) | 30 | 28 |
| mmap floor (libraries, large mmaps, thread stacks) | 4 KiB | 4 TiB | 30 | 30 | 28 |
| vDSO | 4 KiB | 512 GiB | 27 | 27 | (with mmap) |

Until S6 the regions inside the arena share the 21 bits of the base and differ by 8..9 bits
from each other. S6 gives image, heap and stack their own base. The executable from disk, the
mmap floor and the vDSO are independent draws from S1 on.

Alignment: arena base 2 MiB (the arena code works per 2 MiB block and per PT); executable to
its largest `p_align`; a reservation of a power of two >= 1 GiB stays aligned to
max(size, 4 GiB) as today (`ring3.rs:16925`).

### 2.3 What glibc, Chromium and V8 need

| Need | Answer |
|---|---|
| Large reservations: > 256 GiB seen (135 + 69 GiB + GigaCage pools); V8 with the sandbox asks up to 1 TiB and falls back | 52 TiB of bump room above the floor. **Behaviour change (not verified):** a 1 TiB reservation that failed until now may succeed; it costs no frames, but V8 then takes another path. S1 measures the reservations of one browser start before and after |
| Cage base aligned to 4 GiB, pools aligned to their size | rule kept (2.2) |
| `MAP_NORESERVE`, `PROT_NONE` reservations | unchanged: a reservation is only a bump of `DEMAND_NEXT`; frames on first touch |
| `MAP_FIXED` | unchanged: honoured inside a span the process reserved (`ring3.rs:16981`), which is how ld.so and the allocators use it. Not extended |
| Hint addresses (PartitionAlloc and V8 pass random hints) | ignored, as today. Entropy comes from the kernel. Honouring hints is out of scope (section 6) |
| 47-bit limit (NaN-boxing, tagged pointers) | every zone ends below 100.5 TiB < 2^47. No LA57 |
| `MAP_32BIT`, addresses below 4 GiB, non-PIE `ET_EXEC` | not possible (the kernel lives there), as today |
| brk grows contiguously | the brk window stays one piece of the arena |
| glibc: `AT_BASE`, `AT_PHDR`, `AT_ENTRY`, `AT_SYSINFO_EHDR`, `AT_RANDOM` | all computed from the virtual bases (A3). The vDSO is position-independent (`__ehdr_start`-relative, no relocation) |
| `/proc/self/maps` | prints only `[heap]` today (`ring3.rs:3507`). Addresses must be the virtual ones. Whether glibc/V8 need a `[stack]` line for the main thread (`pthread_getattr_np`) is **not verified**; it works today without one and the design does not change that |

### 2.4 fork and exec

- **fork / clone:** the child has the parent's layout bit for bit (it is a copy of running
  code with absolute pointers). `ChildMem` carries every layout value. Unchanged in effect.
- **exec:** every draw is new: arena base (the lazy arena tables at the old base are discarded
  and built at the new one; the frames go back to the pool as today), executable base, mmap
  floor, vDSO base (unmap, map at the new page), in-arena slides, `AT_RANDOM`. One TLB flush
  and shootdown at the end, as today. Chromium children are fork + exec here (no zygote
  reuse), so every renderer gets a layout of its own, which is better than Linux with a zygote.
- **Order in exec (matters for copy-on-write):** first drop the arena pages at the *old*
  virtual addresses (`lazy_arena_discard` releases the COW references the parent looks up by
  address), then free the old table chain, then build at the new base.
- Exec by the launched process itself: only fork children exec today (`do_child_execve`);
  **not verified** that no program execs without fork. If it happens, the same code applies
  once the launched process is a virtual arena.

## 3. Mechanism

### 3.1 Page tables

Per process, as today: a PML4 of its own; slots 0 and 1 are the kernel. New: the arena hangs
under the PML4 slot of its virtual base, `PML4[i4] -> PDPT -> PD -> PT`, all USER, the PT
entries name the frames of the arena (`phys_arena + offset`, flags W^X as today; 2 MiB pages
for a program without demand paging, 4 KiB after `arena_split`). The functions of A6 take the
slot from the address instead of assuming 0; the fault handler asks "is this address in the
arena zone" instead of `addr >> 39 == 0`. The demand functions of class C loop over the slots
of the demand zone and skip absent PML4 entries (128 entries to look at: nothing).

The arena of the launched process stays one contiguous piece of RAM in this design (same
allocation, same accounting, same "pinned" rule). Only its mapping moves. A later, optional
step makes it lazy like a child's (S8): then no contiguous 64..128 MiB is needed any more and
the 64 MiB fallback on fragmented RAM (BUG-136) disappears.

Cost: per process 1 PDPT + 1 PD more for the arena, and one more chain (PDPT, PD, PT) because
the executable and the mmap floor are far apart: about 5 frames (20 KiB). Once no user page
lives in slot 0, `PML4[0]` of every process can point to the boot PDPT (S8): that gives back
2 or more frames per process.

### 3.2 How the kernel reaches user memory

Three rules replace "the arena is identity-mapped":

1. **On the CR3 of the owner: dereference the user address**, after `in_user_arena`. This is
   what `copy_to_user` and friends do; unchanged. `in_user_arena` checks the arena range(s)
   first, then the demand zone.
2. **On any other CR3: never dereference a user address.** Go through the tables:
   `paging::translate_in(pml4, va)` gives the frame, the kernel reads or writes it through its
   identity map, page by page. Two helpers, `read_user_in(pml4, va, &mut buf)` and
   `write_user_in(pml4, va, &buf)`, return an error for a page that is not present (no demand
   fault from a foreign context). `read_glibc_u32` is the existing example.
3. **At launch: two numbers.** `load_elf64(elf, virt_base, kern_base, pages)` writes at
   `kern_base + off` and relocates with `virt_base`; `setup_user_stack_glibc` gets the same
   pair. For the launched process `kern_base` is the physical arena (reached through the
   identity map); for exec in a child both are equal (it runs on its own CR3 and faults its
   frames in). No CR3 switch in the launchers.

`ARENA_BASE` becomes the virtual base everywhere; a new `ARENA_PHYS` (per process, in
`ChildMem` it is 0 for a lazy arena) is used only for freeing and for the census.

SMAP/SMEP: `enable_smep_smap` turns them on when the CPU has them. The identity alias of the
arena frames becomes supervisor-only, as every other frame: ring 3 loses nothing, and a user
address no longer tells where the RAM is.

### 3.3 Demand paging, RAM files, page cache, swap

Keyed by (address space, virtual address) for mappings and by frame or (file, page) for
content; none of it knows the arena. Changes: the slot loops of class C; the per-process
executable window (`EXE_BASE`, `EXE_END` instead of `DEMAND_BASE .. +192 MiB`), added to
`ChildMem`. `swapmgr.rs` keeps a registry of user virtual addresses and walks all four levels:
**not verified** that nothing in it assumes slot 2.

### 3.4 Copy-on-write

`euromm::cow::Refs` counts per frame: independent of addresses. `share_arena_cow` and
`arena_cow_fault` look the same virtual address up in parent and children; fork keeps the
layout, so that holds. The "pinned" rule (the parent keeps its frame, the children are given
copies) stays while the launched arena is contiguous RAM. With S8 it can go: then the parent
is a holder like any other.

### 3.5 SMP and TLB

No new kind of mapping change:

| Event | Today | With this design |
|---|---|---|
| launch | tables built before the first instruction | same: no flush |
| fork | `HoldMm` + write-protect + shootdown | same |
| exec | unmap demand region, `flush_tlb_all` (reload CR3 + `apuser::shootdown`) | plus arena and vDSO move; the same single flush at the end. Exec runs when the task is alone in its address space |
| mmap/munmap/mprotect in the demand zone | `flush_user_page` / shootdown | same, any slot |

No PCID is used, so a CR3 reload flushes everything of that space.

## 4. Entropy source and the boot option

- Source: `entropy::draw_wait::<N>()`, the kernel HMAC-DRBG (`crates/euroentropy`), seeded at
  boot from RDSEED/RDRAND, TPM and jitter with health tests, reseeded on a VM generation
  change, a clock jump and every 5 minutes (BUG-089, BUG-082).
- Per launch and per exec: one draw of 32 bytes, cut into the fields of section 2.2 by one
  pure function `euromm::layout::draw(bytes, mode) -> Layout` (host-tested: every region
  inside its zone, aligned, disjoint, below 2^47, for random and extreme inputs). Today
  `aslr_slides` is called from four places with separate draws; it becomes one call per image.
- Nothing is drawn at boot for user ASLR: there is no per-boot secret, every image has its own.
- Boot option `aslr=` (`kernel/src/bootopt.rs`) and a shell command `aslr` that shows and sets
  the mode for the next launches:

| Value | Meaning |
|---|---|
| `aslr=on` | default after S5 |
| `aslr=off` | the same mechanism with every draw 0: arena at 8 TiB, exe at 32 TiB, floor at 40 TiB, vDSO at 100 TiB. Addresses repeat from run to run (debugging, comparing two logs). `[aslr]` prints "off (boot option)" and is neither OK nor FAILED |
| `aslr=legacy` | the layout of today (identity arena, slot 2). Default until S5, removed in S8. The A/B switch for every step |

- The serial console prints addresses (`[glibc] spawned ... rsp@`, tombstones). That is the
  operator's console, not a channel a process can read; left as it is.

## 5. Step plan

Every step: test or marker first (it fails), then the change, 0 markers worse against the base
in the KVM self-test boots at 1/2/4/8 GiB. "Ships alone" = can be merged and released without
the later steps. Size is relative (S = half a day of one agent, M = 1 to 2 days, L = 3 or more).

| Step | What | Flag | Failing-first test | Exit criterion | Risk | Ships alone | Size | Model |
|---|---|---|---|---|---|---|---|---|
| S0 | Tests and plumbing. `[aslr]` gets: vDSO address; `[aslr-phys]` (an address of the process equals its frame address); `[aslr-ind]` (bits that vary in stack - mmap, exe - stack, exe - mmap: independence); `[aslr-fork]` (child = parent); `[aslr-exec]` (exec'd child differs from parent in every region). `euromm::layout` with host tests. `aslr=` parsed; `off` zeroes today's slides | `aslr=off` | all new lines FAILED except `[aslr-fork]`; host tests red until the function exists | lines print real numbers in 4 boots; `aslr=off` gives 0 bits everywhere | low | yes | S | Sonnet |
| S1 | Demand zone over many slots: `DEMAND_BASE` 32 TiB, 64 TiB wide; slot loops (class C); per-process `EXE_BASE` (random, for programs from disk); mmap floor with 30 bits; vDSO at a random page; exec redraws all three | `aslr=legacy` keeps slot 2 | `[aslr]` mmap 1 MiB 20, libc 20, vDSO 0; `[aslr-exec]`; new `[demand-slots]` self-test (map, fork-share, free in two far slots; frame counts back to the start) | mmap 1 MiB, libc, vDSO >= 27; Chromium smoke + one soak on the R630: pages load, `unattributed` = 0, reservations logged before/after | medium | **yes** (largest gain for Chromium: binary, libraries, heaps) | M | Opus |
| S2 | `paging.rs`: arena functions for any PML4 slot (A6), fault dispatch by zone (A7) | none (no behaviour change: the arena is still in slot 0) | `[arena-hi]`: scratch address space with a lazy arena in slot 16, touch, share copy-on-write with a second space, write, free; today `lazy_pte_ptr` answers None | `[arena-hi]` OK; fork/COW self-tests (`[memt]`) unchanged | low | yes | M | Opus |
| S3 | Loader with two numbers (A2, A3, A4): `load_elf64`, `setup_user_stack_glibc`, `glibc_layout` take virtual base and kernel-side base; all callers pass equal values | none | `[elf-bias]`: load ld.so into a scratch buffer with a different virtual base; relocated words and `LoadInfo` must carry the virtual base | `[elf-bias]` OK; every glibc self-test as before | low | yes | S | Opus |
| S4 | **The launched process at a fixed non-identity base** (8 TiB): the four launchers (A1), builders (A5), `ARENA_PHYS`, diagnostics (A13), then the audit of A10/A11: every ring-0 fault that shows up is one identity assumption, fixed with rule 2 of 3.2 | `aslr=off` selects it; default stays `legacy` | `[aslr-phys]` FAILED | with `aslr=off`: `[aslr-phys]` OK; self-test boots 1/2/4/8 GiB, TCG matrix, GUI smoke, Chromium smoke and a 30-minute soak with **0 ring-0 page faults** and 0 markers worse than `legacy` | **high** (the core step; a missed access halts the machine, a missed DMA use corrupts silently) | dark only (flag off by default) | L | Opus; Haiku for the sweep of call sites |
| S5 | Randomise: arena base 21 bits, page slides for exe and ld.so inside the arena, exec moves the child's arena (order of 2.4). Default becomes `aslr=on` | `aslr=on/off/legacy` | `[aslr]` PIE 0, ld.so 0, stack 8; `[aslr-exec]` for the arena | **`[aslr]` OK in every boot** (PIE, mmap >= 28, stack >= 22), `[aslr-exec]` OK; full regression once; Chromium soak with `on`. BUG-088 can be closed here | medium | yes (needs S0..S4) | M | Opus |
| S6 | Independent bases for image, heap and stack: the arena becomes up to three virtual segments over the same RAM; arena functions and `in_user_arena` take a segment list; `ChildMem` carries it | `aslr.split=on/off` | `[aslr-ind]` FAILED (8..9 bits between stack and ld.so) | `[aslr-ind]` >= 20 bits for stack - mmap, exe - stack, stack - ld.so | medium-high (touches fork, COW and lazy-arena paths again) | yes, optional hardening | M-L | Opus |
| S7 | Docs and tracker: BUG-088 entry, `docs/` boot options, release notes, website EN/FR/NL if the feature is mentioned there; proof directory | | | proof README with the numbers of every step | low | with S5 | S | Sonnet |
| S8 | Cleanup, each part separately: remove `aslr=legacy` and the identity builders for glibc; `PML4[0]` shared with the boot PDPT; launched process with a lazy arena (no contiguous RAM, "pinned" rule removed) | | `[memt]` census: process pool use of a browser start before/after | no identity user page in any glibc process (`[aslr-phys]` checks the tables, not one address); pool use not higher | medium | yes, later sprint | M | Opus |

Order and dependencies: S0 first. S1 and S2+S3 are independent of each other and can run in
parallel by two agents (S1 touches the demand paths, S2/S3 the arena and loader). S4 needs S2
and S3. S5 needs S4. S6 and S8 are optional and independent of each other.

Minimum to close BUG-088 as the test defines it: S0, S1, S2, S3, S4, S5 (+ S7).
Cheapest useful stop: S0 + S1 (Chromium's binary 0 -> 30 bits, libraries and heaps 20 -> 30;
`[aslr]` stays FAILED on PIE of embedded programs and on the stack).

## 6. Risks, unknowns, out of scope

### 6.1 Risks

| Risk | Effect | Mitigation |
|---|---|---|
| A kernel path uses an address of the launched process on a foreign CR3 (A10) | ring-0 page fault, machine halts | S4 runs with a fixed base first, so the fault is reproducible and names the RIP; `legacy` stays the default until the soak is clean |
| A device is given a user address as DMA address (A11) | silent corruption or an IOMMU fault | explicit audit item of S4; run S4 once with the IOMMU on (the TCG `iommu` legs) so a wrong address faults instead of writing |
| A layout value missing from `ChildMem` | a fork child uses the parent's executable window or vDSO base after exec | `[aslr-fork]` and `[aslr-exec]` compare every region; the list in 1.3 C |
| Chromium behaves differently with 52 TiB of address room (V8 sandbox size, more and larger reservations, more page-table frames for sparse touches) | memory or start-up changes | S1 logs reservations and page-table frames of one browser start before/after; cap `DEMAND_SIZE` if needed (it is one constant) |
| Copy-on-write lookups by address during exec of a child | parent gives a copy to a child that already moved | order of 2.4; `[memt]` cowfork with an exec in the middle |
| Addresses differ per run: harder to compare logs and tombstones | slower debugging | `aslr=off`; `demand_addr_origin` prints region + offset |
| `[aslr]` costs about 50 s per self-test boot (64 starts) and the new lines add starts | longer boots in the matrix | reuse the same 64 starts for all lines; fork/exec lines use 8 starts |
| Conflicts in `ring3.rs` with the 11f merge | rework | start from `feature/11f-integ` after it lands |

### 6.2 Assumptions that were not verified in the code

1. No program execs without a fork (2.4).
2. No I/O path uses a user address as a DMA address (A11).
3. Thread exit, signal delivery, kill and tombstone paths touch user memory only on the
   owner's CR3 (A10).
4. `swapmgr.rs` and the CLOCK registry do not assume PML4 slot 2 (3.3).
5. The lazy arena (S8) works for a program that runs without demand paging (frames come from
   the process pool either way, but only demand-paged runs use it today).
6. Which V8/PartitionAlloc reservation sizes the packed Chromium asks for when more room is
   available (2.3).
7. The gaslr probe has to print one more address (vDSO) and a fork/exec mode; `.16` has no
   glibc cross toolchain, so where the probe is rebuilt has to be decided.
8. The inventory of 1.3 comes from reading the launch, fork, exec, fault and paging code and
   from greps (`ARENA_BASE`, `arena_virt`, `>> 39`, `DEMAND_PML4_IDX`, "identity"); `ring3.rs`
   is 19 700 lines and was not read end to end. S4 is designed to find what the list misses.
9. No measurement was taken for this document; the numbers of 1.2 are those of
   `docs/proof/kbugs-2026-11e/`.

### 6.3 Out of scope

- **KASLR for the kernel.** The kernel is identity-mapped at its load address, has no higher
  half, and uses physical addresses as pointers throughout (page tables, frames, MMIO). Moving
  it means a relocatable kernel image and a separate direct map: a rewrite of the memory
  foundation, not a sprint next to this one. Not recommended now: SMEP/SMAP are on, ring 3 has
  no kernel pointer to learn from once the arena leaves slot 0, and the cheaper kernel
  hardening (W^X for kernel pages, no kernel addresses in anything a process can read) comes
  first. Worth a design sprint of its own only together with a move to a higher-half kernel.
- Native Euro-profile programs and userspace drivers (A12): 2 MiB arena at its physical
  address. They can take the same remap later (their fork already does); driver DMA pages stay
  identity or get a translate call.
- Honouring mmap hint addresses; `MAP_32BIT`; non-PIE executables; `MAP_FIXED` outside a
  reservation.
- 5-level paging (57-bit addresses).
- Re-randomising a fork child that never execs; `personality(ADDR_NO_RANDOMIZE)`; sub-page
  stack offset; randomised kernel-stack offsets per syscall.
- Shared futexes keyed by frame (1.5): a bug of its own.
