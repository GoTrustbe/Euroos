# Sprint: multi-process Chromium — the execve refactor

Goal: a forked chrome child re-execs /proc/self/exe as a renderer/GPU process,
so the browser's real multi-process Mojo pipeline runs (not --single-process).

## The one blocker (from the trace, 2026-08-29)
The child's whole post-fork setup now succeeds and it calls
execve("/proc/self/exe", "--type=...") = ENOSYS. The glibc launcher can't do
execve because its address-space state is a set of PROCESS-GLOBAL singletons:
GLIBC_PML4, ARENA_BASE, ARENA_SPAN_DYN, DEMAND_NEXT, DEMAND_FILE_MAPS, the
disk-exe segment registrations, SHARED_MAPS. One process at a time was fine;
two live processes (browser + child) each need their own.

## Design: per-process demand context, keyed by PML4
A `ProcCtx { pml4, arena, arena_frames, demand_next, file_maps, disk_exe_segs,
shared_maps, ... }`. A registry Vec<ProcCtx>. The demand fault handler and the
mmap router already read Cr3 (per-process); they now look up the ctx for the
current Cr3 instead of the globals. The globals become the "current process"
convenience that fork()/launch set, but the authoritative store is per-ctx.

## Phases (each ends buildable + a run or host test)
1. Introduce ProcCtx + registry; fork() registers a child ctx cloned from the
   parent's (same disk-exe segs + file maps, own demand_next continuing from
   the parent's — the child inherits the parent's mappings). Fault handler +
   mmap router resolve ctx by Cr3. NO behaviour change for single-process
   (one ctx). Verify: baseline single-process chrome still renders.
2. execve(path, argv, envp) on the glibc path, fork-child only: tear down the
   child's arena + demand pages, allocate a FRESH ctx (new arena, empty demand
   region), load ld.so + the disk exe at DEMAND_BASE, build a SysV stack with
   the NEW argv/envp, retarget the child task to the new entry. The parent's
   ctx is untouched. Verify: child reaches --type=renderer main, threads spawn.
3. Wire the renderer's Mojo channel: the child's fd5 (aliased socketpair) must
   carry real bytes to the browser's end. Verify: browser stops logging
   "GPU process launch failed"; a renderer paints.
4. Full multi-process screenshot on the desktop. Restore --single-process as
   default only if 3 doesn't land; otherwise flip the default.

## Honest scope
Phase 1 is a real refactor (touches every demand-state access). Phases 2-3 are
where multi-process either works or reveals the next wall (cross-process shared
memory for the compositor). Runs are 15-20 min each under TCG. --single-process
stays the shipping default until phase 4 proves a rendered page.
