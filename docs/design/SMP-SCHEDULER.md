# SMP scheduler design: ring 3 on every CPU

Status: DESIGN (no implementation). Base: `feature/app-control` @ `3ecde9e`. The build is for the Opus team.
Bugs: BUG-146 (key to screen), BUG-192 (churn starved), BUG-182 (`[timer-probe] FROZEN`).

## 1. What the code does today (read from `3ecde9e`)

- **One scheduler, one CPU.** `sched::schedule_core` (kernel/src/sched.rs) runs on the BSP only, under
  `SCHED: Mutex<Scheduler>` (class `Sched`, `try_lock` from the tick, BUG-007). All 512 task slots, every ring-3
  thread, the desktop loop (task 0, also the idle task) and the kernel tasks share that CPU. The APs run a toy
  queue (`AP_SCHED`, four slots) and then `kwork::ap_idle_loop`: kernel closures only.
- **The Linux ABI layer assumes one CPU with interrupts off.** `syscall_entry` (ring3.rs) saves into the globals
  `USER_RSP`/`USER_RIP`/`SAVED_REGS`/`CURRENT_SC_STACK`; a syscall runs with IF=0 and is atomic against everything
  else; per-process memory state lives in *swapped globals* (`GLOBALS_OWNER`, `ensure_globals_for_current`,
  `child_mem_swap`); the demand-fault handler uses the same globals. `gdt::set_rsp0` writes the BSP TSS only.
  `percpu::cpu()` finds the CPU from the stack range and calls every unregistered stack "BSP".
- **vruntime counts turns, not time.** `vstep(nice)` is added once per switch-out, and only if the task is still
  `Ready`: a task that blocks is never charged, a task that yields after 50 us pays as much as one that ran 10 ms.
- **A yield gives away the rest of the tick.** There is no preemption between ticks: after `yield_now` the next
  task runs until the 100 Hz tick. The desktop loop gets the CPU back at every tick end (`T0_WANTS`, BUG-127).
- **Waits spin.** `virtio_blk::kick_and_wait_inner` spins until the device answers (limit 60 s, a line only above
  2 s), in whatever interrupt state the caller has.

### Reading of the three bugs

- **BUG-146 / why `wakeyield` failed.** With the hand-off on, woken tasks ran within 0.1 ms (17 809 of 19 670
  wakes) and the kernel chain got shorter (113 -> 52 ms median), but the number of wake-ups went from ~7 000 to
  ~19 700 for fewer keys, the maxima grew to 1.3-2 s and the harness median doubled. Three causes, all in the
  code above: (a) the waker yields *inside* its system call, so it must come back to the one CPU just to return
  to user mode; (b) it is charged a full `vstep` for a 50 us turn and queues behind thirty ready threads with
  10 ms slices; (c) every `futex_wake` of a pool worker became a forced switch with a CR3 reload, also for
  workers that find nothing to do. What helps is not a faster hand-off on one CPU. It is a second CPU for the
  woken thread, and a waker that keeps running.
- **BUG-192 (reading, to be confirmed in S0).** In the 42 s round the ledger adds up: 5 tasks x 647 ticks = 3 235
  of 3 979 ticks, task 0 got ~100. Task 0 was `Ready` all the time and did real work (`proc_churn_n` ->
  `run_glibc`: `memset`, disk reads). It gets one short burst per tick (the `T0_WANTS` turn), hands the CPU on,
  and the next CPU-bound task then owns the CPU until the following tick. Five ring-3 hogs share the one CPU with
  the task that runs the console command: 2.5 % for task 0 where an equal share is 17 %.
- **BUG-182 (hypothesis).** The LAPIC is fine: ISR is clean, the timer is pending in IRR, and the NIC vector
  (0x4B, priority class 4) is simply served before the timer (0x20, class 2) once interrupts are deliverable again.
  The probe compares two NIC interrupts, so for that second the vCPU took *no* interrupt at all and then resumed
  in user mode with IF=1, which is what the first instruction after a `sysret` looks like. H1 (most likely): a
  system call or demand fault sat in `kick_and_wait_inner` with IF=0 while the host disk was slow (seen at browser
  start and at a page that loads new media; requests under 2 s are not logged). H2: the vCPU thread was not
  scheduled by the loaded R630 (the guest TSC runs on). H3, a lost timer interrupt, does not fit the clean ISR.
  The `[tick-late]` lines already in `docs/proof` show the same class: windows of 0.35-0.55 s with task 0 in ring 0.
  S0 adds the attribution that separates H1 from H2.

## 2. Design in one paragraph

**Asymmetric SMP first: the kernel stays on the BSP, user mode runs everywhere.** A ring-3 thread whose saved
context is *user mode* may run on any AP. Whenever it needs the kernel (system call, page fault, any exception)
the AP saves its user context and sends the thread back to the BSP, where it re-executes the same instruction and
takes the existing, unchanged path. The whole Linux ABI layer, the swapped globals, the drivers and all interrupt
handlers keep their invariant "kernel code runs on one CPU". The scheduler itself is made properly multi-CPU
(per-CPU run queues, time-based fairness, IPIs), so that later steps can move kernel work to the APs one system
call at a time (futex first) without a redesign.

Why not a big kernel lock: IF=0 would no longer exclude the BSP's interrupt handlers or the preemptible desktop
loop from a system call running on an AP, and the swapped globals would have to follow two CPUs. That is an audit
of 19 000 lines, not a step.

## 3. Thread placement, run queues, migration, IPIs

### 3.1 Data

```
// sched.rs (new, outside the SCHED mutex: atomics, readable by census_nolock)
static RUNCTX: [RunCtx; MAX_TASKS];   // rsp, fs_base, vruntime, exec_start_tsc, cpu (u8), flags
  flags: USER_CTX   saved frame has CS ring 3 (frame word at rsp+128, as schedule_tick reads it)
         MIGRATABLE spawn_user/spawn_thread task with cr3 != 0 and kstack != 0, glibc path
         NEEDS_BSP  bounced, kicked or signalled: must be resumed on the BSP once
static RQ: [eurolock::sync::Mutex<ApRq>; MAX_CPU];  // class RunQueue, Mode::Irq, APs only
  ApRq { ready: [u64; 8] (bitmap of task slots), nr: u32, current: u16 (or IDLE), min_vr: u64 }
static CUR_ON: [AtomicU16; MAX_CPU]; static CUR_CR3: [AtomicU64; MAX_CPU]; static IDLE_MASK: AtomicU8;
static INBOX: [[AtomicU64; 8]; MAX_CPU];  // AP -> BSP hand-back, lock-free bitmap of task slots
```

`Task.state` and everything else stay inside `SCHED`. **Ownership rule:** while `RUNCTX[i].cpu == k != 0`, CPU k
owns `rsp`, `fs_base`, `vruntime` and the FPU area of task i; the BSP sees the task as `Ready` and touches none of
them. An AP never changes `Task.state`: blocking, sleeping and exiting are kernel operations and happen on the BSP.

The BSP run queue *is* `SCHED` (the same scan, now over tasks with `cpu == 0`). It holds every kernel-context
task and the user-context tasks that no AP has room for.

### 3.2 What may leave the BSP

Only tasks with `USER_CTX && MIGRATABLE && !NEEDS_BSP`. Never: task 0, kernel tasks (`spawn_kernel`), the musl
background path (`bg_dispatch` under `BG.lock`), a foreground excursion (`SAVED_KERNEL_RSP`/`EXITED`), a task that
blocked or yielded inside a system call (its saved frame is ring 0).

### 3.3 AP side (replaces `ap_schedule_tick` once the demo workers are parked)

- **Own entry stack.** `AP_TSS[cpu].rsp0` points at a per-CPU stack registered with `percpu::register_stack`
  (so `percpu::cpu()` and the `[lk]` order contexts stay right), not at the task's kernel stack. The task's
  kernel stack holds only its saved 20-word frame at the top.
- **Own IDT for the APs** (`AP_IDT`; the BSP IDT is not touched). Vectors 0-31 point at `ap_trap_N` stubs: if the
  interrupted CS is ring 0, jump to the existing handler; if ring 3, drop the error code, push the 15 registers
  and call `ap_bounce(rsp, vec)`.
- **System calls bounce the same way.** The APs run with `EFER.SCE = 0`: `syscall` in ring 3 raises #UD with the
  rip *at* the instruction. `ap_bounce` treats it like any fault. No `swapgs`, no second syscall stub, no rip
  arithmetic. (`int3`: rewind rip by one. #DB from a user TF: bounced, the step is retaken on the BSP.)
- **`ap_bounce(rsp, vec)`**: copy the frame to the top of the task's kernel stack, store `RUNCTX.rsp`, save
  `fs_base` and the FPU state, set `NEEDS_BSP`, set the task's bit in `INBOX[cpu]`, send `IPI_RESCHED` to the BSP,
  pick the next task of `RQ[cpu]` (or the idle context). It takes `RQ[cpu]` only; it never takes `SCHED`, never
  allocates, never calls `sched::current()` (use `CUR_ON[cpu]`).
- **`ap_tick` (vector 0x41)**: charge the running task (section 4), preempt when its slice is over and `nr > 1`.
- **Switch-in on an AP**: CR3 (also into `CUR_CR3[cpu]`), `IA32_FS_BASE`, FPU restore, then pop and `iretq` from
  the task's kernel stack.
- **Parity.** `smp::ap_cpu_features` copies CR0/CR4/XCR0/NXE today; extend it to every bit ring 3 depends on
  (SMEP/SMAP, FSGSBASE, UMIP, PAT) and print `[ap-parity] OK` after comparing each AP with the BSP. `EFER.SCE` is
  the one deliberate difference.

### 3.4 BSP side (in `schedule_core`, under `SCHED`)

1. **Drain `INBOX`**: for each bit, `cpu = 0`, renormalise vruntime (`v - src.min_vr + s.min_vr`).
2. **Pick** (section 4). Kernel-context tasks first, within a bound.
3. **Offload**: every `Ready` task on the BSP with `USER_CTX && MIGRATABLE && !NEEDS_BSP` that was not picked goes
   to an AP: its last AP if that one is idle, else an idle AP, else the AP with the smallest `nr` if that is below
   the BSP's user load. Lock `RQ[k]`, set the bit, unlock; if bit k of `IDLE_MASK` is set, send `IPI_RESCHED`.
   The outgoing task may be offloaded in the same call: the BSP is still on that task's kernel stack, but below
   the saved frame, and never reads the frame again; the AP only reads those 20 words. Keep that as a stated
   invariant with a debug assert.
4. **Balance** once every 10 ticks: if two APs differ by 2 or more in `nr`, move one queued (not running) task:
   lock the source, take it out, unlock, then lock the target. Two run-queue locks are never held together.

Only the BSP moves tasks between queues. An AP that goes idle sets its bit in `IDLE_MASK` and, when the BSP
advertises queued user work (`BSP_USER_READY > 0`), sends `IPI_RESCHED` to the BSP; the offload step feeds it.

### 3.5 IPIs

- `IPI_RESCHED_VECTOR` (0x42, free today; add it to the `fixed` table in interrupts.rs). On the BSP a new stub
  `resched_switch` (a copy of `yield_switch` plus EOI) calls `schedule_core(rsp, false)` without the timer
  prelude. On an AP it enters `ap_resched`. A pending IPI is taken at the first instruction with IF=1, so a
  system call in progress on the BSP ends first: preemption stays at the user boundary.
- `IPI_TLB_VECTOR` (0x45, exists): section 3.6. `IPI_PING_VECTOR` stays the kwork wake-up.

### 3.6 What must be true before a real process leaves the BSP

- **TLB shootdown.** One choke point `paging::flush_user(cr3, range)`: local `invlpg`, then `IPI_TLB` to every AP
  with `CUR_CR3[cpu] == cr3`, and wait for the acknowledgement. Convert the 22 flush sites (ring3.rs 12,
  paging.rs 5, swapmgr.rs 4, sched.rs 1): munmap, mprotect, MADV_DONTNEED, CoW break, swap-out. A fault that only
  *adds* a mapping needs none.
- **`sched::kick_to_bsp(idx, wait)`**: take a task off its AP (dequeue under `RQ[k]`, or set `evict`, send
  `IPI_RESCHED` and wait until `CUR_ON[k] != idx`). Mandatory before `mark_dead`/`retire_task`, exit_group of
  sibling threads, exec, freeing a PML4, and for a signal to a thread that is in user mode on an AP.
- **`sched::mm_quiesce(cr3)`**: `kick_to_bsp` for every thread of an address space; they stay queued on the BSP
  until the operation is over. For fork (the eager copy of the demand pages) and exec.

## 4. Fairness

- **Time, not turns.** At every switch-out and every tick: `delta = rdtsc - exec_start_tsc`;
  `vruntime += delta_us * 1024 / weight(nice)`, with the usual 40-entry weight table (1024 at nice 0, x1.25 per
  step). Charged whether the task stays `Ready`, blocks or yields. `vstep` goes.
- **Slice.** 10 ms (one tick) while more than one task is ready on that CPU. HZ stays 100, because `TICKS` is the
  time base of every sleep. Finer latency comes from wake preemption (section 5), not from a faster tick.
- **Placement after a wait.** `vruntime = max(vruntime, min_vr - 5 ms)`: a sleeper gets ahead by half a slice,
  never by more (today: `min_vr - 64` in turn units, and `sched::wake` forgets the floor).
- **Yield.** `yield_now` of a `Ready` task ends its slice and re-runs the pick over everybody, itself included.
  A task that is behind keeps the CPU; a task that is level or ahead hands over. This is what ends BUG-192 on one
  CPU: a short burst no longer costs a whole tick of somebody else's.
- **Desktop.** Task 0 gets the weight of nice -10 (9548 against 1024) and keeps `T0_IDLE` around its hlt (idle
  time is not charged, BUG-143) and `DESK_KICK`. `T0_WANTS`/`T0_TURN_TICK` go once the `[fair]` test passes
  without them. Against five hogs on one CPU task 0 is then entitled to 65 % instead of 2.5 %; with APs the hogs
  are not on the BSP at all.
- **Desktop, hosted client and foreground programs (fix-d2).** While a hosted X window exists the desktop task is charged at
  nice 0: its passes are then work for that client, and at nine times the client's weight it took the CPU chrome needed (BUG-274).
  A glibc program the desktop task runs in the foreground and waits for (`run_glibc`) gets `DESKTOP_NICE` (BUG-273).
- **Kernel-entry boost on the BSP, bounded.** A user task in kernel context (bounced, or woken inside a system
  call) is picked before user-context tasks and before kernel tasks while its vruntime is within 10 ms of
  `min_vr`. Beyond that plain vruntime order applies, so a thread that floods system calls cannot starve task 0.
- **Batch work is not starved**: every runnable task is in vruntime order on its CPU, and the balancer keeps the
  AP queues within one task of each other.

## 5. Wake-up latency (futex, poll, socket)

All wakes happen on the BSP (they are kernel code). The path of one hand-off A -> B:

1. A (user mode, AP 1) executes `syscall` -> #UD -> `ap_bounce` -> `INBOX`, `IPI_RESCHED` to the BSP.
2. The BSP takes the IPI at the user boundary and runs A's `futex_wake`; B becomes `Ready` in kernel context.
   `woken()` sets `RESCHED` (as today) but **nobody yields inside the system call**: the dispatch epilogue
   (`take_resched` in `linux_dispatch`) sends `IPI_RESCHED` to the BSP itself instead of `yield_reacquire`.
3. `sysret`; the self-IPI fires on A's first user instruction. A now has a user-mode frame, is offloaded to its
   AP and continues there. B finishes its `futex_wait` on the BSP and is offloaded at the next entry.

Wake preemption on the BSP applies when the woken task's vruntime is at least 1 ms below the current task's.
Cost per kernel round trip from an AP: two IPIs and two switches, tens of microseconds under KVM, against the
1-10 ms a woken thread waits today (`[keylat] woken tasks waited`: 4 000 of 7 000 above 1 ms).

Two refinements, each behind its own measurement:
- **Stay while it is free.** A thread that returns to user mode on the BSP stays there until the BSP is wanted
  (IPI, wake, tick). A thread with a high bounce rate (more than ~2 per ms of user time) is left on the BSP.
- **Idle polling.** An idle AP spins up to 200 us on its queue before `hlt` (KVM and metal; off under TCG, where
  a spinning vCPU costs a host core): waking a halted vCPU costs more than the IPI itself.

`WAKE_YIELD` and the `wakeyield` command are removed in the step that adds the self-IPI.

## 6. Locking

| Lock | Class (rank) | Taken by | Rule |
|---|---|---|---|
| `SCHED` | Sched (10) | BSP only | as today; `try_lock` from tick/IPI (BUG-007); also the BSP run queue |
| `RQ[k]` | RunQueue (15), Mode::Irq | AP k; the BSP | never two at once; never held across an IPI wait |
| `INBOX`, `RUNCTX`, `CUR_ON`, `CUR_CR3`, `IDLE_MASK` | atomics | everyone | no lock |

- Order: `SCHED` -> `RQ[k]` -> (leaf classes). The ranks already say so; no new class.
- **An AP never takes `SCHED`**, not even with `try_lock`: its only way to the BSP is `INBOX` plus an IPI. So the
  one inversion this design could create (RunQueue held, Sched wanted) cannot be written.
- The `[lk]` checker stays valid because (a) AP kernel code runs on registered per-CPU stacks, so
  `percpu::cpu()` and the per-context `OrderTracker` see the right CPU; (b) same-class nesting stays forbidden and
  the balancer is written without it; (c) `futex_wake` holding `FUTEX_QUEUE` while taking `SCHED` is unchanged
  (`SCHED` is `Mode::Plain`). Add to `lockstress.rs`: BSP offload against AP bounce on the same task, 10^6 rounds,
  0 inversions.
- **A lock that system calls or faults take (IF=0) is never held with interrupts on.** Ring-0 code runs on the
  BSP only: a holder the timer preempted cannot run again while a task with interrupts off spins on its lock,
  and the machine stops (BUG-007 family; BUG-390: the desktop loop held `DEMAND_FILE_MAPS` in its heartbeat
  census, the browser spun in `child_mem_swap`). The locks of the swapped demand state (`CHILD_MEM`, the five
  lists `child_mem_swap` swaps, `GLIBC_FORK_CHILDREN`, `CHILD_THREADS`) are `irqlock::IrqLock`s: interrupts off
  while held, whoever takes them; a wait of 2 s prints an `[irqlock]` line with the last holder and its place.
- `census_nolock`, `current_lockfree` (BUG-144/157): keep reading through the init-time pointer; add the CPU
  column from `RUNCTX.cpu` and `CUR_ON`. BUG-202 (scan bound of the NMI probe) is independent and should be fixed
  first, because the probe will now also land on AP entry stacks.
- The global lock is *not* removed in this plan. It stops being the bottleneck because user time leaves the BSP;
  `lockstat` on `sched.SCHED` after S5 decides whether a later split (task table against BSP queue) is worth it.

## 7. Timer and preemption per CPU

- **BSP, vector 0x20, periodic 100 Hz**: unchanged owner of `TICKS`, the late-tick repair, `xhci::poll`,
  `virtio_input::poll`, `vdso_tick`, the watchdog and the sleeper scan (once per tick, not per switch: keep a
  `next_deadline` and skip the 512-slot scan on IPI and yield entries).
- **AP, vector 0x41, periodic**: local accounting and preemption only. No global time, no device polling.
- **AP idle is tickless**: mask the LVT timer before `hlt` when `nr == 0` and no kwork is pending, unmask on the
  first enqueue. Eight vCPUs ticking under TCG is what would make the matrix slower.
- **Preemption points**: tick (both), `IPI_RESCHED` (both), the system-call epilogue self-IPI (BSP). Kernel code
  with IF=0 is never preempted, as today.
- **BUG-182 in this design**: user threads on the APs keep running through a frozen BSP; the freeze itself is
  attacked at its source (S0 attribution, S8 blocking disk wait).

## 8. Step plan for the build team

Every step: marker first (fails before, passes after), default off behind `smpuser=off|test|on` (boot option and
shell command) until its criterion is met, targeted tests per step, the full matrix once per step that changes
default behaviour (S1, S2, S6).

| Step | Change | Test-first marker | Criterion |
|---|---|---|---|
| **S0** | Numbers only. `[sched-stat]`: per task user/kernel tick split (from `sample_user_rip`/`sample_kernel_rip`), syscalls/s, switches/s, `[t0-yield]` by call site. `[if0-window] ms cause=blk/syscall N/fault/steal` in the late-tick branch (longest `kick_and_wait` since the last tick, KVM steal-time MSR). Shell command `hogtest N` (N ring-3 hogs) | `[fair] hogtest 5 + churn proc 8: T s, task 0 share P %` FAILS (P < 10); `[if0-window]` appears with a throttled disk (`throttling.iops-total`) | BUG-192 reproduced in < 3 min; BUG-182 reproduced on demand or H1 refuted; **gate:** kernel-context share of the browser's CPU during keylat. Above ~40 %: pull S7 before S5 |
| **S1** | Time-based vruntime, weights, placement floor, yield rule, desktop weight (section 4). One CPU, no SMP code | `[fair]` from S0; `[fair-nice]`: nice 0 against nice 5 gets 3:1 +-15 % | `[fair]` share >= 50 %, churn worst <= 12.6 s (smp 1 and smp 4, 30 rounds with hogs); keylat not worse; matrix 0 worse |
| **S2** | `IPI_RESCHED` + `resched_switch`; self-IPI in the dispatch epilogue; wake preemption (1 ms); remove `WAKE_YIELD` | `[wakelat]`: two threads ping-pong on a futex next to 4 hogs, round trip p50/p99 | p50 < 1 ms, p99 < 12 ms; hogs keep >= 85 % of their throughput; keylat `woken tasks waited` >= 80 % under 1 ms; harness median not worse than off |
| **S3** | AP can run ring 3: `RUNCTX` split, `ApRq`, AP entry stacks, `AP_IDT` + `ap_trap_N`, `EFER.SCE=0`, `ap_bounce`, parity check. Only a test process is placed (`smpuser=test`) | `[ap-parity]`; `[ap-user]`: a glibc process pinned to each AP computes a checksum with SSE/AVX, makes 10^4 syscalls and 10^3 demand faults, result equal to the BSP run, `bounces` counted | PASS on smp 2/4/8, KVM and TCG; 0 `[lk]` inversions in `lockstress`; `[smp]`, `[ap-tss]`, `[kwork]` unchanged; smp 1 boots identically |
| **S4** | `paging::flush_user` + shootdown at all 22 sites; `kick_to_bsp`, `mm_quiesce`; kill/exit/exec/fork/signal paths | `[tlb-smp]`: a thread on an AP reads a page while the BSP unmaps / protects / CoW-breaks it: fault or new data within 1 ms, never stale. `[kill-smp]`: exit_group and SIGKILL with 8 threads spinning on APs, 1 000 rounds, census back to the start | both PASS x 1 000; `[many-threads]` 320/320 with `smpuser=test`; shootdowns/s reported |
| **S5** | Offload, balance, idle `IPI_RESCHED`, stay-while-free; `smpuser=on` for glibc threads in the browser images | `[spread]`: 8 hog threads of one process on smp 4 finish in <= 0.4 x the smp 1 time; the ledger shows every AP used | keylat A/B off/on in one session (`perfab`), smp 4: **harness median < 100 ms** (kernel `total` median < 60 ms); smp 2 not worse than off; site matrix 3 rounds 0 worse |
| **S6** | Tickless AP idle, idle polling, `smpuser=on` by default | `[ap-idle]`: an idle AP takes < 5 timer interrupts in 10 s | TCG/KVM matrix 0 worse (boot time and host CPU of an idle guest within 5 %); 2 h soak: churn worst <= 12.6 s, 0 `[lk]` inversions, no FROZEN that `[if0-window]` does not explain |
| **S7** (if the S0/S5 numbers ask for it) | `futex_wait`/`futex_wake` on the AP: `FUTEX_QUEUE` to `Mode::Irq`, wake through an `INBOX`-style hand-off. First kernel code for user threads on an AP | `[futex-ap]` stress: 16 threads x 10^6 lock/unlock on 4 CPUs, no lost wake (`lost_wake_check` silent) | bounces/s down >= 50 %; keylat better than S5 |
| **S8** | BUG-182 at the source: task-context callers of `kick_and_wait` block on the MSI-X completion (0x47) instead of spinning; `[blk] slow` from 200 ms | `[if0-window]` with the throttled disk FAILS (window > 200 ms) | no window > 50 ms with the throttled disk; soak as S6 |

Acceptance for the whole plan: keylat median < 100 ms (harness, smp 4), BUG-192 churn worst case <= 12.6 s,
0 `[lk]` inversions, TCG/KVM matrix 0 worse. Note for S5: the harness number also contains one screendump
(23-43 ms) and the 100 Hz input poll. If the kernel `total` is under 60 ms and the harness is not under 100 ms,
the rest is outside the scheduler and is reported as such, not tuned away.

## 9. Risks and what NOT to do

Risks:
- **Nothing here is measured yet.** The design rests on reading the code and the existing proof logs; no new run
  was made. S0 produces the numbers before anything is built.
- **The BSP is the kernel bottleneck.** A browser that spends most of its time in system calls gains little
  (the S0 gate); S7 is the answer.
- **A long IF=0 system call on the BSP stalls every thread that needs the kernel** (not the ones in user mode).
  Fork's eager copy and the spinning disk wait are the known ones (S8).
- **Stale TLB entries** are silent memory corruption. S4 is not optional and comes before S5.
- **Bounce cost under KVM with halted vCPUs** (host wake-up latency): idle polling in S6; measure on the R630
  with the guest pinned.
- **Two-vCPU guests** (the current browser runs) have one AP: expect a gain, not the target. Run the browser
  harness with `--smp 4`.
- **Semantics**: #DB single-step and `int3` cross a CPU hop; fork with sibling threads running would copy a
  moving address space, hence `mm_quiesce`.

Do NOT:
- run system calls, demand faults or signal delivery on an AP before S7, and then only futex; no big kernel lock;
- take `SCHED` from an AP, hold two `RQ` locks, or allocate / print under a lock in `ap_bounce`;
- migrate task 0, kernel tasks, the musl `bg_dispatch` path or a foreground excursion;
- call `sched::current()`, `gdt::set_rsp0` or `ensure_globals_for_current` on an AP;
- touch the BSP stubs `timer_switch`/`yield_switch`/`syscall_entry` or the BSP IDT;
- change HZ, or bring back a yield inside the waking system call (`wakeyield`);
- remove `T0_IDLE` (BUG-143) or the `try_lock` in `schedule_core` (BUG-007);
- weaken `[fair]`, `[wakelat]` or the keylat criterion to make a step pass: a step that misses its number stays
  off by default and is reported with the number.
