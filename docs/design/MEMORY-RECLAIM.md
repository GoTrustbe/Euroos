# Memory that comes back: frames for memory files, page-cache eviction, out of memory

Status: implemented on `feature/chrome-mem` (BUG-150 to BUG-155); section 7 on
`feature/chrome-mem2` (BUG-156); section 8, named RAM files in frames and sysinfo on
`feature/mem-2026-11e` (proof `docs/proof/mem-2026-11e/`). Measurements and
proof: `docs/proof/chrome-mem/README.md`. Code: `crates/euromm` (`ramfile`,
`pagecache`, `oom`, host-tested against models) and `kernel/src/ring3.rs`,
`kernel/src/paging.rs`.

## 1. Where the memory of a browser session is

A glibc process tree (the browser and its children) runs on three stores:

| Store | Sized | Holds |
|---|---|---|
| demand pool | at launch: the RAM that is left, minus a margin | every 4 KiB page that is committed on a fault: anonymous memory, private copies of file pages, page tables of the demand region, the page cache of the packs, the frames of memory files |
| process pool | at boot: up to half of the RAM | the identity arena of the launched process (128 MiB for the browser, one piece); per fork child its page tables and the arena pages it uses, as single frames (section 7) |
| kernel heap | at boot: a quarter of the RAM, 64 to 512 MiB | kernel objects; the bytes of RAM files that have a name outside a demand-paged run (section 3) |

The census (`memstat`, `[mem]`/`[memstat]` every 30 s and after `ramfiles`) names
the owner of every frame of the demand pool and prints what it cannot name as
`unattributed`. That figure is the test of the ownership model: it must be 0.

## 2. Rules

1. **Nothing is held that nobody can reach.** A fork that is refused gives back
   what it took; `madvise(MADV_DONTNEED/MADV_FREE)` gives the frames back and the
   next touch reads zeros; a process that ends gives its task slots back.
2. **One copy.** A memory file is its frames. Mappings, `read` and `write` use the
   same frames; there is no second copy in the kernel heap.
3. **Limits follow the machine.** The budget of the memory files is derived from
   the demand pool, which is derived from the RAM of the guest.
4. **Reclaim before refuse, refuse before kill, never panic.** Under pressure the
   kernel first takes what costs nothing to take back, then says no to the caller
   that asks for more (`ENOSPC`, `ENOMEM`, `EAGAIN`), and only then ends a process;
   the process it ends is not the browser while a child can be ended instead.

## 3. Memory files made of frames (`euromm::ramfile`)

`PageFile` = a length and one frame per page, 0 for a hole. `read`, `write`,
`set_len` (ftruncate: shrinking frees the frames past the end and zeroes the tail
of the last page) and `ensure_frame` (the page fault of a `MAP_SHARED` mapping).
A hole reads as zeros and costs nothing, so the 2 MiB regions Chromium sizes and
then touches in part cost what is touched.

Which files: a file **without a name**, i.e. a memfd from its creation and a file
that is unlinked while a descriptor or a mapping still holds it (the two ways
Chromium makes shared memory). Its bytes move out of the heap at that moment.
A file **with a name** (the profile, caches, `/tmp`) that a program writes during a
demand-paged run is made of frames too, from its first `write`, `ftruncate` or
`fallocate` (`paged_named_adopt`); the kernel's own files (`/proc`, `/dev`) stay byte
vectors. Both kinds share the one budget below. At the end of the run, before the pool
goes, every paged file that still has a name gets its bytes back in the kernel heap
(`paged_named_release`), so files outlive the run as before; outside a run they are byte
vectors under the old rule (never into the last 32 MiB of the heap, `ENOSPC`).

`Budget::for_pool(pool)`: the files together may hold **half of the demand pool**,
and stop while **a sixteenth of the pool (at most 256 MiB)** is still free for the
page faults of programs. The pool is what the guest has left after the kernel
heap and the process pool, so the budget follows the RAM; the measured pools and
budgets at 4, 8 and 16 GiB are in the proof README.

At the limit: `write` returns what fitted, then `ENOSPC`; `ftruncate` and
`fallocate` only set the length (frames come on touch) and refuse more than 1 TiB
(`EFBIG`); a fault in a mapping that cannot get a frame fails like a fault for
anonymous memory (section 5). The last close or unmap of a file without a name
frees its frames (the sweep of BUG-132, now on `PageFile::release`).

## 4. Page-cache eviction (`euromm::pagecache`)

The page cache maps (file, page) of a pack to a frame that every process maps
read-only. `PageCache` adds a logical clock per page (the last fault that asked
for it) and a write state: `Clean`, `Dirty`, `Writeback`.

`pcache_reclaim(want)`, called from the fault path when fewer than 4 MiB are free:

1. walk the page tables of every process once: which cache frames are mapped,
   which were referenced since the last pass (the accessed bit, cleared as it is read);
2. `PageCache::evict`: **clean pages only**; first the ones nobody maps, oldest
   first; then mapped ones that were not referenced, oldest first; a referenced
   page stays;
3. unmap the mapped victims from every process, reload CR3, free the frames. The
   next access faults and reads the page from the pack again.

**Dirty pages** are never evicted. They go through `begin_writeback` (oldest dirty
first) and `end_writeback`; a page written again during its writeback, or whose
write failed, is dirty again. The cache of the packs has no dirty pages (packs
are immutable), so the kernel never calls the writeback half today; it is in the
structure and in its host tests so that a writable disk file can be cached later
without a new eviction rule. Anonymous pages and the frames of memory files have
no backing store: they leave memory only to a swap partition (the existing
`swapmgr`, when one is present) or when their owner gives them up.

## 5. When memory runs out

A page fault that finds fewer than 4 MiB free climbs this ladder (`memory_pressure`):

1. the page cache gives an eighth of itself (at least 8 MiB) back;
2. the sweep of memory files nobody holds is asked for;
3. with a swap partition: anonymous pages of the faulting process go to it;
4. below 1 MiB free and for a fault from ring 3: **one process is ended**
   (`euromm::oom::pick_victim`): the child that holds the most private memory. If
   that is another process it is torn down on the spot (wait status SIGKILL, the
   browser shows the tab as crashed) and the fault is served from what it held. If
   it is the faulting process itself it is ended and torn down by the next task
   that runs. The launched process (the browser) is ended only when it has no
   child that holds memory.

What a caller is told instead of being killed:

| Request | Answer |
|---|---|
| `write`/`pwrite` to a memory file past the budget | short write, then `ENOSPC` |
| `ftruncate`/`fallocate` past 1 TiB | `EFBIG` / `ENOSPC` |
| grow of a named RAM file into the last 32 MiB of the heap | `ENOSPC` (BUG-132) |
| `mmap` when the address space of the demand region is used up | `ENOMEM` |
| `fork` without an arena, page tables, a copy of the demand pages | `ENOMEM`, and everything taken so far is given back |
| `fork`/`clone` without a task slot or kernel stack | `EAGAIN`, likewise |

Anonymous `mmap` itself never fails for want of frames (the region is reserved,
not committed, as Chromium's 100+ GiB reservations require); the fault is where
memory is asked for.

**Never a kernel panic.** The last 64 frames of the pool are for faults from
kernel mode (a system call that copies into a page of its caller that was never
touched); a fault from ring 3 is refused before them. If a system call still
finds none, its process is ended (`[oom] ... inside a system call`), where the
kernel used to stop with `[idt] PAGE FAULT`. Every list the reclaim path needs is
allocated with `try_reserve`; when that fails, nothing is evicted.

## 7. The arena of a fork child (BUG-156, `feature/chrome-mem2`)

The launched process keeps its arena in one piece, identity mapped with 2 MiB
pages: the kernel reaches it by address, and it is one process. A fork child
runs at the same addresses on memory of its own. That memory used to be a
second piece of the same size (128 MiB), copied whole at every fork and zeroed
whole at every execve, whatever the child used of it.

Now a child has page tables over the arena addresses (one per 2 MiB, the PD
entry carries `LAZY_PT`) and a frame of the process pool for a page that holds
something (the PTE carries `OWNED`):

| Event | What happens to the arena |
|---|---|
| `fork` | every page of the parent that is not all zeros is copied into a frame of the child (the parent is the launched process, or a lazy child); 8 MiB of the pool are left alone, else `ENOMEM` and everything goes back |
| first touch of a page nobody wrote | page fault, a zeroed frame, from ring 3 or from a system call that copies into its caller (`paging::lazy_arena_fault`; it takes the process-pool lock and no other) |
| `execve` | every frame goes back to the pool; the new image gets frames for what it touches |
| end of the process, by whatever path | `free_fork_tables` frees the frames with the tables |
| the pool has no frame for a touch | ring 3: the process is ended (`[oom] no frame in the process pool for an arena page`), 1 MiB before the pool is empty; the kernel may use that last 1 MiB |

Copy-on-write replaced the copy at fork in `feature/mem-2026-11e`: section 8.
Copying what is not zero keeps the semantics of the copy that was there and
removes the cost that mattered: the browser has 23 MiB of 128 that are not
zero when it forks, and after its execve a child holds 17 MiB.

Measured (KVM, 4 GiB): an idle fork child of the 96 MiB test arena costs
4.6 MiB of the process pool instead of 96; 40 at the same time instead of 16.

## 6. Not in this design

- One pool instead of two: the process pool keeps up to half of the RAM for
  arenas whatever the browser needs (section 1). Measured in the proof README.
- Reclaim for the process pool: its pages have no backing store and the pool
  is not counted in the census of the demand pool; a child's arena frames are
  what the census prints as `arena=`.
- A page cache for writable disk files (the writeback half of section 4 has no
  caller yet).
- A virtual arena: the arena of the launched process is still identity mapped
  (virt = phys), which is why its frames are pinned in section 8 and why its base
  cannot be randomised. Its 4 KiB page tables (section 8) are the first step; the
  kernel's accesses to it by address are what is left.

`sysinfo` and `/proc/meminfo` report the machine since `feature/mem-2026-11e`: total =
the RAM the frame allocator manages, free = what the demand pool and the process pool
have free, shared = the frames of the memory files, buffers/Cached = the page cache,
MemAvailable = free + page cache.

## 8. Copy-on-write fork (`feature/mem-2026-11e`, `euromm::cow`)

Section 7 copied, at every fork, the arena pages of the parent that were not
all zeros and every committed page of the demand region (23 MiB and ~100 MiB
for the browser, 56 + 33 ms). Now the two sides share the frames:

| Event | What happens |
|---|---|
| `fork` | a private page of the parent (writable, or copy-on-write already) becomes read-only with the software bit `COW` (bit 11) on both sides, and its frame gains a holder in `Refs` (one `u16` per frame of RAM, in frames of their own: 3 MiB at 6 GiB of address space). Windows of `MAP_SHARED` files and the ALSA ring are copied as before; page-cache pages stay read-only and shared as before; a page of the launched process that is all zeros is left out (the child reads zeros) |
| write to a `COW` page, ring 3 or a system call that writes into its caller | the writer is the last holder: the entry becomes writable, same frame. Otherwise the writer gets a copy and the old frame loses a holder (`paging::demand_cow_fault`, `paging::arena_cow_fault`). The fault handler reads the user page under STAC (SMAP) |
| the launched process writes a page children share | its frames never move (identity mapped: the kernel reaches that arena by address), so the CHILDREN get the copies (`Evict`, through the registry of child address spaces), then the parent's entry becomes writable |
| `munmap`, `MADV_DONTNEED`/`MADV_FREE`, `execve`, exit, a refused fork | the entry goes; its frame loses a holder and is freed by the last one (`cow::on_unmap`) |
| census | a shared frame counts for its part in each holder, so once over the processes; a count left over after the children of a run ended is printed (`[cow]`) and cleared |

The arena of a launched process that runs demand-paged (the browser, the
`[memt]` runs) gets 4 KiB pages before it starts (`paging::arena_split`: a
page table per 2 MiB, `SPLIT_PT`, from the frame allocator that built its
directories, freed with them) so that single pages can be write-protected.
Any other process keeps 2 MiB pages, and a fork copies its arena pages that
are not all zeros as in section 7. A page that was
zero at a fork has its dirty bit cleared and carries `KNOWN_ZERO`; the next
fork does not read it again while the dirty bit stays clear.

The decisions on an entry (share, write, unmap, evict) are `euromm::cow`,
host-tested against a model of whole address spaces (bytes per page, random
forks, writes, reads, munmap, execve and exit on all sides, the launched
process with pinned frames): every read sees the bytes of the model, every
frame's count equals its holders, no frame is free while mapped, every frame
comes back.

What it rests on, and is not proven by a test: ring 3 runs on one core (no
TLB shootdown); the kernel writes into the memory of a process only through
that process's own page tables (a write through the identity map of another
address space would reach a shared frame unseen). `swapmgr` does not swap out
a `COW` page.
