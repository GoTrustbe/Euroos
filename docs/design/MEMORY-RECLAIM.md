# Memory that comes back: frames for memory files, page-cache eviction, out of memory

Status: implemented on `feature/chrome-mem` (BUG-150 to BUG-155). Measurements and
proof: `docs/proof/chrome-mem/README.md`. Code: `crates/euromm` (`ramfile`,
`pagecache`, `oom`, host-tested against models) and `kernel/src/ring3.rs`,
`kernel/src/paging.rs`.

## 1. Where the memory of a browser session is

A glibc process tree (the browser and its children) runs on three stores:

| Store | Sized | Holds |
|---|---|---|
| demand pool | at launch: the RAM that is left, minus a margin | every 4 KiB page that is committed on a fault: anonymous memory, private copies of file pages, page tables of the demand region, the page cache of the packs, the frames of memory files |
| process pool | at boot: up to half of the RAM | one identity arena per process (128 MiB for the browser), fork page tables |
| kernel heap | at boot: a quarter of the RAM, 64 to 512 MiB | kernel objects; the bytes of RAM files that have a name |

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
Files with a name (the profile, caches, `/tmp`) stay byte vectors in the kernel
heap under the existing rule (never into the last 32 MiB of it, `ENOSPC`).

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

## 6. Not in this design

- One pool instead of two: the process pool keeps up to half of the RAM for
  arenas whatever the browser needs (section 1). Measured in the proof README.
- Copy-on-write fork: a fork still copies every committed private page.
- Named RAM files in frames; a page cache for writable disk files (the writeback
  half of section 4 has no caller yet).
- `sysinfo` and `/proc/meminfo` still report a fixed 256 MiB machine to programs.
