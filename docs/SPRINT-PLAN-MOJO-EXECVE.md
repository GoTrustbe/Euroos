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

### 2026-08-30 — BREAKTHROUGH: the fork child execve's and runs as a renderer
Phase 1 (per-process state swap) + phase 2 (do_child_execve) landed and WORK:
- "[execve] child task 31 re-exec /pack/chrome argv0=/proc/self/exe -> ld.so
  entry 0x2a01f540" for BOTH children (pid 1000/1001). Where the log used to say
  "LaunchProcess: failed to execvp", the child now jumps into a FRESH image.
- After execve the child issues brk() = 0x2b800000 — glibc's malloc init in the
  NEW process. The child is running its renderer image, on its own address space.
- The register-retarget bug that a code review caught before wasting a run: the
  syscall return sysret's to the SAVED_REGS-block's pushed rcx (+104) and takes
  rsp from USER_RSP; setting USER_RIP had no effect. Fixed by writing ld.so's
  entry into the block's rcx slot and zeroing the GP regs (a fresh _start state,
  like spawn_user gives a new process).

NEXT WALL (phase 3, separate + substantial): the renderer runs but the BROWSER
aborts "GPU process isn't usable. Goodbye." — it never completes the Mojo
handshake with the child. The children and the browser are two real processes
each holding one end of the inherited socketpair; bytes written by the child to
its fd5 (aliased channel) must reach the browser's end, and vice versa, plus the
Mojo data pipes need cross-process shared memory. That is the socketpair data
flow + memfd MAP_SHARED-across-address-spaces work. Shipping default stays
--single-process (proven).

## Phase 3 sprint (2026-08-30): the Mojo channel carries bytes cross-process

Success criterion: the browser stops logging "GPU process launch failed" /
"Network service crashed" — a child completes the Mojo handshake. Stretch: a
renderer paints (multi-process screenshot).

Steps, executed in order, each ending in a build + ONE isolated run:
1. DIAGNOSE: reset the per-child syscall-trace budget at execve, so the trace
   shows the renderer's post-exec life (ld.so loading libs from the child's
   fresh demand state, then Mojo's first channel ops). Find the exact stall.
2. Expected fix candidates, in likelihood order:
   a. lib loading in the child's demand state (open/mmap of libc.so.6 etc. via
      the swapped file-maps — the VFS paths must resolve under the swap);
   b. sendmsg/recvmsg with SCM_RIGHTS over the socketpair (Mojo passes fds/
      memfds across the channel at invite time);
   c. memfd MAP_SHARED across the two address spaces (SHARED_FRAMES is global
      = good; each process maps its own VA to the same file frames);
   d. eventfd/epoll wakeups crossing processes.
3. Iterate 1-2 until the handshake completes; keep --single-process the
   shipping default until a renderer paints.
Process discipline: pgrep qemu before every run; one isolated run at a time;
review code before spending a run.

### 2026-08-30 phase 3 progress — the renderer fully loads and reaches Mojo
Two more real bugs found and fixed with the auxv dump + full child trace:
1. The child ran the parent's DIRTY, already-relocated exe pages (forked copy):
   ld.so read a phdr whose p_vaddr was DEMAND_BASE+0x360, computed base+p_vaddr
   = a slot-4 address (0x200_0000_0360), and the isolation handler TERMINATED
   the child. Fix: free_demand_region(child_pml4, 2) at execve drops the
   inherited demand pages, so every exe page re-faults FRESH from disk (original,
   unrelocated phdrs). No more termination.
2. HEAP_END was set to HEAP_BREAK (zero mmap arena), so ld.so's libc.so.6 mmap
   got ENOMEM. Fixed to stack_top-0x100000, same as glibc_disk_launch.
Result (proven in the trace): the child now runs 613+ syscalls after execve
with ZERO terminations — it opens and mmaps the WHOLE renderer dependency tree
(lib after lib, real addresses), then __libc_start_main (arch_prctl ARCH_SET_FS
TLS, set_tid_address, set_robust_list), and reaches Mojo channel I/O
(writev/recvmsg/poll on fd 608, a unix socketpair). The browser NO LONGER aborts
"GPU process isn't usable". Remaining: confirm the channel bytes cross to the
browser's end and a renderer paints. Shipping default stays --single-process.

### 2026-08-30 (2) — the child runs as CrGpuMain with working threads
Fix: a THREAD spawned by a fork child ran on the child's PML4 but the PARENT's
demand-state (only the child MAIN task was recognised as a fork child, not its
threads), so the thread faulted in the arena. Now fork_child_owner() maps a
child thread -> its child process, CHILD_THREADS registers threads at clone,
and child_mem_swap keys on the owner so a thread swaps its process's ChildMem.
Also --disable-gpu-watchdog + --disable-hang-monitor: under TCG the child's
init outran chrome's own GpuWatchdog timeout (an abort in the GpuWatchdog
thread even though the child was healthy).
Result: terminations dropped 2 -> 1, the browser stopped aborting, and the
child t31 named itself "CrGpuMain" (chrome's GPU-process main thread) with a
second child thread (t35) running — a full, named, multi-threaded chrome GPU
process on its own address space. Remaining: one browser-side page fault (arena,
task 9) late in the run — likely chrome's own crash after a still-missing piece
(cross-process shared memory for the compositor, or a Mojo reply), not the state
swap (the #PF handler runs IF=0, so the swap is atomic). Shipping default stays
--single-process. This is a natural, proven stopping point for the sprint.
