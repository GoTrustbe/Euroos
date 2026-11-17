# ASLR for Linux-ABI processes: a virtual arena per process (BUG-088)

Status: S2 and S3 implemented (behaviour-neutral, branch `feature/11f-aslr-s23`, section 7); S0 and S1
implemented, `aslr=on` the default (branch `feature/11f-aslr-s01`, section 8); S4 implemented behind the boot option
`aslr.arena=fixed|identity`, `fixed` the default (branch `feature/11f-aslr-s4`, section 9); S5 implemented,
`aslr.arena=random` the default, BUG-088 closed (branch `feature/11f-aslr-s5`, section 10); S6 implemented, image, heap
and stack segments at bases of their own, `aslr.split=on` the default (branch `feature/11f-aslr-s6`, section 11); S7 and
S8 are design only. Written against `feature/app-control` @ `e4b8ba4`;
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

## 7. Verified in S2/S3

Branch `feature/11f-aslr-s23` (from `feature/11f-integ` `00a6459`). Proof: `docs/proof/11f-aslr-s23/`. Every
process still gets the addresses it got before: nothing is placed in the arena zone yet, every caller passes
equal numbers.

**S2 (paging.rs, interrupts.rs).** New: `ARENA_ZONE_BASE`/`ARENA_ZONE_END` (8..12 TiB, PML4 16..23),
`in_arena_zone(addr)` (PML4[0] or the zone) and `arena_pdpt(pml4, virt)` (the PDPT under the slot of the
address). `lazy_pte_ptr`, `arena_pte_ptr`, `arena_pt`, `arena_split`, `share_arena_cow`, `clone_arena_lazy`,
`fill_remap_tables_lazy`, `map_user_arena` (now takes the PML4, not a PDPT) work in any of those slots; above
PML4[0] a missing PDPT or PD is created empty, not as identity. `free_fork_tables`, `free_address_space` and
`lazy_arena_frames` walk PML4[0] and the zone slots. The page-fault handler sends a fault to
`arena_cow_fault`/`lazy_arena_fault` when `in_arena_zone(addr)` instead of `addr >> 39 == 0`. `[arena-hi]`
covers it (lazy arena across PML4 16/17, faults, copy-on-write between two spaces, census, teardown).

Corrections to section 1.3 found in the code:

- A6: `lazy_pte_ptr` did not only answer None for a high arena: the old `fill_remap_tables_lazy` wrote the page
  directories of a high arena into the PDPT of PML4[0] (index `virt >> 30 & 511`), over the kernel's identity
  map of that address space. `[arena-hi]` before S2: "the arena pages are not lazy entries above PML4[0]".
- A6, not changed (not needed for S2, needed in S4): `arena_slots` and the identity builders
  (`try_build_address_space*`, `fill_remap_tables`, `fill_remap_tables_wx`) still build PML4[0] only;
  `fill_remap_tables*` without `_lazy` are the native (musl) fork, A12. `table_frames` returns the PD under
  PML4[0] only; its one caller is the reaper of pooled native processes (`ring3.rs` ~5014), also A12.
- A7: the two `addr >> 39 == 0` tests in `page_fault_handler` were the only fault dispatch by slot; the demand
  path (`handle_demand_fault`) goes by its own range.
- A8: `arena_cow_fault` Evict looks children up with `lazy_pte_ptr`, so it follows the zone with no change.
- Kernel code that touches a page of a scratch or foreign arena on its CR3 needs STAC/CLAC (SMAP), as
  `copy_user_page` does; S4 helpers (rule 2 of 3.2) go through the frame and need none.

**S3 (ring3.rs).** `load_elf64(elf, virt_base, kern_base, pages)`: segments are written at `kern_base`,
`R_X86_64_RELATIVE` tables are read and written at `kern_base`, the words, `entry`, `phdr` and `base` carry
`virt_base`. `setup_user_stack_glibc(stack_top, stack_kern, ..)`: every pointer (argv, envp, `AT_RANDOM`,
`AT_PLATFORM`, `AT_EXECFN`, the returned rsp) is virtual, every byte (including the 16 `AT_RANDOM` bytes) is
written at the kernel-side address. All ten `load_elf64` calls and the four stack calls pass equal values.
`[elf-bias]` covers it (exe `seq` + ld.so + stack, virtual next to the kernel side and at 8 TiB).

Corrections:

- A2: ld.so has **no** `R_X86_64_RELATIVE` in `DT_RELA` (its relative relocations are packed `DT_RELR`,
  which the kernel does not apply; ld.so relocates itself). The kernel's relocation pass matters for the
  executable (and native programs), so `[elf-bias]` loads `seq` (10 relocations) next to ld.so; the design's
  test ("load ld.so ... relocated words") would have checked nothing.
- A4: `glibc_layout` / `glibc_arena_layout` need no second number: every result is the base plus an offset, so
  the kernel-side address of any of them is `kern + (x - virt)`. Unchanged; S4 computes the kernel-side stack
  top that way.
- A3: `VDSO_EHDR_NEXT` is consumed by `setup_user_stack_glibc` (a process-global hand-off from the launcher);
  S1/S4 must keep it per launch when the vDSO base becomes per process.

**For S4 (noticed, not done):** the launchers still zero and load the arena through its identity address
(A1) and pass that number twice; `ARENA_BASE`, `in_user_arena`, `glibc_launch_abort`, the census and the
reaper use it as both. `try_build_address_space_rwx_big` + `arena_split` must build the arena under the zone
slot (`map_user_arena` already can) and leave PML4[0] without a USER leaf; `free_address_space` already frees
such a chain. `table_frames` and the native reaper (A12) stay on PML4[0].

## 8. Verified in S0/S1

Branch `feature/11f-aslr-s01` (from `feature/11f-aslr-s23` `3a51bc6`). Proof: `docs/proof/11f-aslr-s01/`.

| commit | step |
|---|---|
| `6504c66` | S0: `euromm::layout` (host tests), boot option `aslr=`, probe `gaslrx`, the lines `[aslr]` (+vDSO), `[aslr-phys]`, `[aslr-ind]`, `[aslr-fork]`, `[aslr-exec]`, `[demand-slots]` |
| `deaba4d` | S1: demand zone 32..96 TiB, one draw per launch and per exec (exe base, mmap floor, vDSO), per-process layout in `ChildMem`, test `[vdso-ro]` (tests first) |
| `0073feb` | BUG-350 (vDSO in fork and exec children), BUG-353 (vDSO read-only) |
| `277b4dc` | `aslr=on` is the default |
| `1d95d9b` | BUG-355: no vDSO after exec again (the network service of Chromium aborted with one) |

**What is where now (`aslr=on`).** `euromm::layout::draw(32 bytes, mode, exe_span, exe_align)` is the one cut of
a draw (section 4); `ring3::image_layout` calls it once per launch (`run_glibc`, `spawn_glibc_persistent`,
`glibc_disk_launch`) and once per exec (`do_child_execve`). The demand region is `zone(mode)`: 32..96 TiB with
`on`/`off`, slot 2 with `legacy` (`demand_base()`, `demand_size()`, `demand_slots()` replace the constants of
class C; the walks of class C are the `dz_*` helpers, one loop over the slots). Per process, in `ChildMem`:
`EXE_BASE`/`EXE_END` (the exe window), `DEMAND_FLOOR`, `VDSO_CUR` (0 after an exec). The stack builder takes the vDSO base as a
parameter (`VDSO_EHDR_NEXT` is gone, correction A3). `aslr=off` is the same code with every draw 0: exe 32 TiB,
floor 40 TiB, vDSO 100 TiB, arena slides 0.

**Entropy measured** (`[aslr]`, 64 starts of `gaslrx`, KVM 1/2/4/8 GiB, and the Chromium launch/exec lines):

| region | before (`3a51bc6`) | `aslr=on` | how |
|---|---|---|---|
| program from disk (Chromium) | 0 | 30 (its `p_align` is 4 KiB) | 4 TiB window above 32 TiB |
| mmap floor: libc, 1 MiB mmap, thread stacks, heaps of Chromium | 19 | 30 | 4 TiB window above 40 TiB |
| vDSO (launched process and its fork children) | 0 | 27 | a page of PML4 200; none after exec (BUG-355) |
| embedded PIE, ld.so | 0 | 0 | arena, identity (S4, S5) |
| stack, brk, small mmap | 8, 8, 9..10 | 8, 8, 9..10 | arena slides (S5, S6) |

The varying bits of the 64 starts equal the bits of the draw (30, 30, 27): every draw bit reaches the address.

**Corrections found in S0/S1:**

- Question 6 (section 6.2, item 7): the probe is rebuilt with the gcc 15.2.0 of the R630 (the compiler of the
  existing `gaslr`, read from its `.comment`); nothing installed. It is a new binary `gaslrx` with its own signature;
  `gaslr` and its signature are unchanged. Symbols needed: up to `GLIBC_2.34` (the image has 2.39).
- `[aslr-exec]` cannot be measured with the probe: `do_child_execve` re-executes only the persistent program from
  disk (`CHILD_EXE_PATH`); an embedded program gets ENOEXEC. The line measures the exec placement kernel side
  (`ring3::place`, the function the exec path uses) and says so; the real execs are the `[aslr-layout] exec` lines
  of Chromium (every child gets a new exe base, floor and vDSO).
- 1.2, "fork child: same as parent": not for the vDSO. A fork child had **no** vDSO mapping and died on its first
  clock call through it (BUG-350); an exec'd child got `AT_SYSINFO_EHDR` 0. The fork child now maps it at the parent's
  address (tables from the process pool, `paging::VDSO_SLOT`). Giving the exec'd image a vDSO at its new draw (design
  2.4) made Chromium's network service abort (BUG-355), so an exec'd image still has none: the vDSO is random per
  launch, not per exec, and `[aslr-exec]` says so.
- The vDSO was mapped user-writable in every process (BUG-353), now read-only.
- `DEMAND_FLOOR` was global: an exec in a child moved the swap range of the browser main (BUG-354). Now per process.
- Class C also has `interrupts.rs` (`[pf-diag]` range of plausible code pointers) and the `exit_group` diagnostics
  (the exe window); both follow the zone now.
- Assumption 4 (section 6.2): `swapmgr` takes `[base, end)` from the caller and skips 2 MiB per missing table; it
  assumes no slot. Holds. With a floor..next range of tens of GiB its scan is as long as before (Chromium's
  reservations did not grow, below).
- Found, not fixed: getrandom into a copy-on-write page of a fork child returns EFAULT (BUG-351); a fork child killed
  by isolation is never reported to `wait4`, the parent waits 60 s (BUG-352).

**What S4 needs (updated list):** design 1.3 A1..A13 as before, plus: `phys_probe_note` and `[aslr-phys]` compare an
address with its frame (`translate_in`), so S4 sees the line turn OK without a change to the test; `place()` takes
the arena base, so S5 only has to draw it; `free_fork_tables` frees the vDSO chain of `VDSO_SLOT`, a moved arena
must not land in that slot (zone 8..12 TiB, slot 200: no overlap); `[aslr-exec]` reports ld.so as kept until S5
  draws the arena base in `do_child_execve`, and the vDSO until BUG-355 is solved.

## 9. Verified in S4

Branch `feature/11f-aslr-s4` (from `feature/11f-aslr-s01` `c71cce8`). Proof: `docs/proof/11f-aslr-s4/`.

**What changed.** Boot option `aslr.arena=identity|fixed` (`ring3::arena_fixed`; `aslr=legacy` always keeps
`identity`, its demand region at 1 TiB lies below the arena zone). With `fixed` the three launchers (`run_glibc`,
`spawn_glibc_persistent`, `glibc_disk_launch`; `run_glibc_disk` and `spawn_glibc_disk_persistent` go through the last)
take the arena RAM as before, zero and load it through the identity map (the kernel side) and give the process
`ARENA_ZONE_BASE` (8 TiB) as its virtual base: `load_elf64(elf, virt, kern, ..)`, `setup_user_stack_glibc(top, kern(top), ..)`,
`glibc_arena_layout(virt, ..)`. `ARENA_BASE` is the virtual base everywhere; `LAUNCH_ARENA_PHYS` is the RAM of the
launched process (frees and the reaper). `paging::try_build_address_space_arena(virt, phys, n)` builds PML4[0] as a PDPT of
1 GiB supervisor pages (no USER bit at any level) and hangs the arena under its zone slot with `map_user_arena`;
`arena_split` takes the virtual base. `free_address_space` already frees that chain (section 7). Fork, copy-on-write,
lazy arenas and exec in a child needed no change: they always worked on the parent's virtual base (S2).

Corrections and findings:

- Design 3.2 says `ARENA_PHYS` per process in `ChildMem` (0 for a child). Not done: nothing reaches a child's arena by a
  RAM base (its arena is single frames, BUG-156), and a swapped 0 would make the reaper skip the sweep when a child owns the
  globals at the end of a run. One global for the launched process (`LAUNCH_ARENA_PHYS`) is enough.
- Miss found by reading (not by a fault): `reap_orphan_fork_children` cleared the copy-on-write counts of
  `ARENA_BASE .. +span`; the counts are per frame, so with `fixed` the range (8 TiB) is clamped to the end of the table and
  the sweep does nothing. It is a safety net (children release their counts when they end), so the miss is silent; no
  failing test was possible without injecting a leaked count. Now `LAUNCH_ARENA_PHYS`.
- `[pf-diag]` took a code pointer for plausible only in the identity range or the demand zone; the arena zone is added
  (diagnostics only).
- `translate_in` does not understand a 1 GiB leaf; with `fixed` PML4[0] consists of them. It is only used for user addresses
  (arena zone, demand zone, vDSO), where there is none. Noted, not changed.
- A10 (another CR3 touching memory of the launched process): no such access was found by the sweep and none showed up as a
  ring-0 fault in the workloads (self-test boots, TCG matrix, Chromium starts, sites, interaction, video, 1 h soaks). The
  places that look at a process from outside (`read_glibc_u32`, `phys_probe_note`, `[futex]`/`[abort]` dumps) already go
  through `translate_in`; there is no signal delivery (`rt_sigaction` is a stub), thread exit clears `clear_child_tid` in
  its own `exit` call. The list is in the proof README.
- A11 (a device given a user address): none. Every block, audio and network path copies through a kernel buffer
  (`pack_read`, `disk_read_bytes`, `virtio_blk::read_io_dev` with its own DMA buffer, ALSA `writei` -> `hda::pcm_write`,
  sockets). Checked again by the TCG legs with the IOMMU on: only the deliberate fault records of the malicious-driver
  self-tests.
- Assumption 1 of 6.2 (no exec without fork) still holds: only `do_child_execve` execs.

**`[aslr-phys]`**: FAILED with `identity` (exe ld.so stack brk mmap at their own physical address), OK with `fixed`
in every boot. `[aslr]` itself is unchanged (PIE and ld.so 0 bits: the base is fixed, S5 draws it).

**What S5 needs.** Draw the arena base (21 bits, 2 MiB steps in 8..12 TiB) instead of `ARENA_ZONE_BASE` in
`arena_virt_for` and in `do_child_execve` (order of 2.4: `lazy_arena_discard` at the old base, free the chain, build at the
new one); page slides for exe and ld.so; `place()` already takes the arena base. Then `fixed` becomes `on` and `identity`
remains only for `aslr=legacy`.

## 10. Verified in S5

Branch `feature/11f-aslr-s5` (from `feature/11f-aslr-s4` `ce6ca19`, with `feature/11f-bugs-35x` `9c4d420` merged in
`417f6b9`). Proof: `docs/proof/11f-aslr-s5/`.

| commit | step |
|---|---|
| `417f6b9` | merge of BUG-351/352/355 (conflict only in the tracker); checked before S5 began (proof README) |
| `35d7bab` | tests first: host tests of the arena draw, self-test `[arena-move]` (both fail) |
| `e9b488d` | S5: arena base and page slides drawn per launch and per exec, `aslr.arena=random` the default |

**What changed.** `euromm::layout::draw` cuts three more fields from the same 32 bytes, from bits the S1 draws left
unused: `arena` (21 bits of 2 MiB steps over the 4 TiB zone, word 1 bits 42..62), `exe_slide` and `ldso_slide` (9 bits of
pages each, word 2 bits 42..59). `arena_base(lay, span)` puts the arena at 8 TiB + offset and moves it down just far enough
to end inside the zone. `off` and `legacy` draw none of them (`off` = the S4 layout at 8 TiB). In the kernel,
`ring3::arena_place` gives the base and the two slides for the mode of `aslr.arena=random|fixed|identity` (`random` is the
default; `fixed` is the S4 layout, `identity` the one before S4). The three launchers draw first and place the arena,
the embedded executable (`ArenaPlace::exe`: the slide only as far as the program still ends below ld.so) and ld.so
(+8 MiB + slide). `place()` and `[aslr-exec]` use the same functions.

**Exec in a fork child (2.4).** `do_child_execve` discards the pages at the old base (`lazy_arena_discard`, as before),
then, after the new draw, `paging::lazy_arena_rebase` gives the old table chain back to the process pool (page tables, and
a page directory or PDPT that is empty after that, with its entry cleared) and builds empty lazy tables at the new base;
`ARENA_BASE` of the child (in `ChildMem`) follows, ld.so, brk, small mmaps and the stack are placed in the new arena.
One TLB flush after the move. Only a lazy arena in the zone moves (every Chromium child); a contiguous or identity arena
keeps its base. A failed rebuild returns ENOMEM (the image is wiped by then, as with the other late failures of exec).

**Entropy measured** (`[aslr]`: 64 starts of the embedded probe `gaslrx` per boot, 20 KVM boots of each image; Chromium:
the `[aslr-layout]` lines of 20 starts per image, the launched browser and every exec'd child). "Over boots" = the bits
that vary in the first address of each boot; "per boot" = the `[aslr]` figure of every one of the 20 boots.

| region | before (`417f6b9`, S4) over boots / per boot | after (`e9b488d`) over boots / per boot | Linux x86-64 |
|---|---|---|---|
| embedded program (PIE base) | 0 / 0 | 30 / 30 | 28 |
| ld.so (embedded launch) | 0 / 0 | 30 / 30 | 28 |
| stack | 8 / 8 | 29 / 29 | 22 |
| brk heap | 8 / 8 | 29 / 29 | 13 relative to the exe |
| small mmap (4 KiB, in the arena) | 9 / 9..10 | 30 / 30 | 28 |
| mmap 1 MiB, libc (mmap floor) | 30 / 30 | 30 / 30 | 28 |
| vDSO | 27 / 27 | 27 / 27 | (with mmap) |
| Chromium: exe from disk (launch and exec) | 30 | 30 | 28 |
| Chromium: ld.so, brk, stack (launch / exec) | 0, 8, 8 / 0, 8, 9 | 30, 29, 29 / 30, 29, 29 | |
| Chromium: arena base (launch / exec) | fixed | 21 / 21 | |

`[aslr]` OK in 20 of 20 boots (before: FAILED in 20 of 20). Of 199 exec'd Chromium children none kept an address of
the launched browser in any region (exe, ld.so, arena, brk, stack, floor, vDSO). `aslr=off`: 0 bits everywhere (20
starts, 190 execs). `aslr=legacy`: the layout of before S1 (exe, ld.so, vDSO 0, floor 20, brk and stack 8).

**Corrections and findings:**

- The design's order for exec (discard at the old base, free the chain, build at the new one) holds as written; the move
  needs no copy-on-write work of its own, because the discard already released every shared frame by the old address.
  `[arena-move]` checks that the parent's frames hold no child after the move, also when old and new range overlap.
- 2.2 lists the stack at 29 bits; the `[aslr]` statistic can count 30 for "base + span - slide" (the subtraction
  borrows into the base bits). It measured 29 in every boot; the host test asks >= 29.
- An embedded executable can only slide as far as it still ends below ld.so (+8 MiB); every embedded program is far
  smaller than 6 MiB, so all of them get the full 9 bits. A launcher computes it per program (`ArenaPlace::exe`).
- `aslr.arena=fixed` is kept as a value (the S4 layout, at 8 TiB) next to `identity`: a way back that keeps the arena out
  of the identity map. The design (4) foresaw only `on/off/legacy`.
- `[aslr-ind]` stays FAILED by design: stack - mmap 10, exe - stack 10, exe - mmap 15 bits (the regions of the arena still
  share one base); that is step S6.
- Found, not fixed (not caused by S5): the vDSO clock still steps back by up to tens of microseconds under TCG and on a
  heavily loaded KVM host, also with one vCPU (`[vdso-mono]`, BUG-380).

**What S6 needs.** The arena functions take one base; S6 gives image (exe + ld.so), heap (brk + small mmaps) and stack
their own virtual segments over the same RAM (design 3.1): `arena_place` becomes three draws, `in_user_arena`,
`lazy_arena_rebase`, `fill_remap_tables_lazy`, `share_arena_cow`, `clone_arena_lazy` and `lazy_arena_discard` take a
segment list, `ChildMem` carries it, and `[aslr-ind]` is the test (>= 20 bits per distance). The bits for three bases
need two more offsets of 21 bits: S5 leaves 50 bits of the 32-byte draw unused (word 0 bits 44..63, word 1 bit 63,
word 2 bits 60..63, word 3 bits 39..63), so either they are cut from those or `DRAW_BYTES` grows.

## 11. Verified in S6

Branch `feature/11f-aslr-s6` (from `feature/11f-aslr-s5` `7b2eea6`). Proof: `docs/proof/11f-aslr-s6/`.

| commit | step |
|---|---|
| `fcb4736` | tests first: `euromm::layout` segments (host tests fail), the arena functions of `paging` take a segment list (one segment everywhere, no change in behaviour), self-test `[arena-segs]` and the `[robust-paging]` case `arena_segs` (fail: the zone ended at 12 TiB), `[aslr-ind]` judges every pair of segments |
| `831894f` | S6: image, heap and stack segments at bases of their own, boot option `aslr.split=on|off` |

**What changed.** An arena is a list of segments over one span of arena offsets (`euromm::layout::Segs`): one piece
(`identity`, `fixed`, `aslr.split=off`), or three: image (offset 0 up to the brk heap: 32 MiB, a quarter of a smaller
arena), heap (from there to 4 MiB before the end: brk and the small-mmap window) and stack (the last 4 MiB). Each lies at
a random 2 MiB step of a 4 TiB window of its own: image 8..12 TiB (the S5 field `arena`), heap 12..16, stack 16..20 TiB
(two new 21-bit fields from a fifth word of the draw: `DRAW_BYTES` 32 -> 40; S5 left 50 bits unused, but no two 21-bit
runs among them, so the draw grew instead). `aslr=off` puts the three at the start of their windows. The arena zone of
`paging` is 8..20 TiB (PML4 16..39). The RAM of the launched process stays one contiguous piece: segment `s` maps
`phys + s.off` at `s.virt`, so the reaper, the frees and the census, which go by RAM, are unchanged.

Every function that walks an arena by its virtual addresses takes the list (`try_build_address_space_arena`,
`arena_split`, `fill_remap_tables_lazy`, `share_arena_cow`, `clone_arena_lazy`, `lazy_arena_discard`,
`lazy_arena_rebase`); fork gives the child the list of its parent, exec unbuilds every old segment and then builds every
new one (design 2.4: discard first). `in_user_arena` accepts a range inside one segment. In the kernel the list is
`ARENA_BASE` (image) plus the heap and stack segment, keyed to the image base (the native paths that swap `ARENA_BASE`
see one piece), and is part of `ChildMem`. `euromm::arena::glibc_layout_segs` gives every region at the offset of before,
seen through its segment; the small-mmap window now ends with the heap segment (before: 1 MiB below the stack top).

**Measured** (21 KVM boots per image, 64 starts of `gaslrx` each; Chromium 20 starts per image, launched browser and
every exec'd child):

| distance (varying bits) | before (`7b2eea6`) | after (`831894f`) |
|---|---|---|
| exe - brk (image - heap) | 14 | 31 |
| brk - stack (heap - stack) | 10 | 31 |
| exe - stack | 10 | 32 |
| exe - small mmap | 15 | 31 |
| stack - small mmap | 10 | 31 |
| stack - ld.so | 10 | 32 |
| exe - mmap 1 MiB, exe - vDSO | 34, 30 | 34, 31 |
| Chromium ld.so - brk, brk - stack, ld.so - stack (launch and exec) | 12, 10, 10 | 31, 31, 32 |

`[aslr-ind]` FAILED in 21 of 21 boots before, OK in 21 of 21 after. The regions themselves keep the bits of S5 (exe,
ld.so, small mmap 30; brk, stack 29; vDSO 27); Chromium arena, heap and stack segment bases 21 bits each; none of 185
exec'd children kept an address of the launched browser. Inside one segment only the page slides differ (ld.so - exe 12,
small mmap - brk 14 bits), as designed.

**Corrections and findings:**

- Design 2.1 gave the three windows (8..12, 12..16, 16..20 TiB) and 3.1 the segment list; both hold as written. The
  design said `aslr.split=on/off`: done, default `on`; `fixed` and `identity` are always one piece.
- The cut: the image segment must hold ld.so with its largest slide (8 + 2 + 1 MiB) and ends where the brk heap begins;
  the stack segment is 4 MiB, so the stack keeps at least 2 MiB (before S6 1 MiB was reserved below the stack top, and the
  stack could silently run on into an unused mmap window). A stack that grows past its segment now faults in ring 3 (a
  guard), and the small-mmap window is 1..2 MiB smaller than before.
- Two places would have done nothing for the new segments without failing (S4 rule: a silent place is a fault): the
  zone end (12 TiB) under every walk that frees or counts arena tables, and the copy-on-write page scan of
  `[fork-cow-write]` (`base .. base + span`). A third, the non-lazy wipe in `do_child_execve`, would have written into the
  gap between segments. All three per segment now (audit: `docs/proof/11f-aslr-s6/audit.md`).
- `clone_arena_lazy` has no caller since BUG-156; converted, not removed.
- Two test corrections after the tests-first commit: the `[robust-paging]` totals had to count the new case (31 refusals,
  7 builds), and a host-test bound on the pairs inside one segment (`<= 11` varying bits) was wrong for a difference of
  two slides (it counts up to 14 bits); it now checks the distance range. Neither touches a >= 20-bit criterion.
