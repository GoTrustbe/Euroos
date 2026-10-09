# Sprint plan, night of 2026-10-06: EuroApps SDK, stability gate, footprint

Status: ACTIVE. Written as the extended plan for the next sprints; the night
executes N1 first, then N3 and N2 as far as they get. Everything is measured in
QEMU; hardware runs wait for the lab.

## Where we stand (2026-10-05)

Landed this week, all QEMU-verified and committed on `feature/app-control`:
the guided installer with a real EuroID account; thread-group kill on faults;
syscall profiles, seccomp cBPF filters, resource limits, ASLR and a renderer
file scope for Linux-ABI processes; VT-d DMA isolation with one domain per
driver. Open from those: W^X stays log-only (V8 JIT), the PIE base is fixed,
legacy virtio bypasses the IOMMU in QEMU, interrupt remapping, gpango's NULL
write, gscm3's small process pool, gvec needs AVX state save/restore.

## Progress, night of 2026-10-06 (all QEMU-measured)

- **N1 built and proven.** `[app]` self-test in the dev boot: a package signed
  with an unknown key is refused (BadSignature), the key is enrolled, the
  package installs (caps `console,proc`, profile utility), a tampered copy is
  refused, the app is listed, and it runs to exit 7 with its output. Trust
  store is one file, `/etc/euroapps/trusted.keys` (name, hex key per line),
  not a directory of `.pub` files as first planned. Shell: `app installed |
  install <file> | trust <name> <hex> | trusted | remove <name> | run <name>`;
  installed apps appear in the launcher. Found and fixed on the way: a process
  without the `file` capability could not even load libc (ld.so's opens were
  refused), so read-only access to the system library paths is now exempt from
  CAP_FILE.
- **N1 proof 6b done:** `scripts/app-x11-desktop.py` boots the dev image,
  types `app run hello-x11` into the Terminal over USB HID (QMP) and
  screendumps the desktop: `docs/proof/2026-10-06-third-party-x11-app.png`
  shows "Hello (window) - hello-x11" as a framed, titled, focused window with
  the app's white background, blue panel and three lines of text. Three real
  defects stood between the launch and that picture, each named by a bounded
  syscall trace: (1) the X display connection is an AF_UNIX client socket and
  was filed under `net`; AF_UNIX client traffic is now capability-free (bind/
  listen/accept keep `net`); (2) `poll(fd, 1, -1)` returned 0 after the
  kernel's 64-try "give the CPU back" guard, which modern xcb treats as a
  dead connection ("X connection to :0 broken"); an infinite poll now
  returns -EINTR on that path, which glib, libevent and xcb all retry; (3)
  the in-kernel X server had no core text at all: PolyText8, ImageText8,
  ClearArea, the CWBackPixel window background and GCBackground are
  implemented, QueryFont reports the real metrics of the one server font
  (the kernel's 8x8 bitmap font). Plus the shell path (`app run`) now gives
  the hosted window the app's title and frame like the launcher path.
- **N3.1 gate script** `scripts/gate.sh` written; first full run tonight.
- **N3.2 fuzzer** `gfuzz` built: 20000 random Linux syscalls with a pool of
  hostile arguments, deterministic seed, replayable on the host
  (`replay.c`). First run found a kernel panic: `fallocate(fd, mode, 2,
  0x100000000)` resized a RAM file to 4 GiB and the allocator aborted. Fixed:
  every RAM-file grow (write past EOF, ftruncate, fallocate) now goes through
  a fallible `grow_ram_file` capped at half the kernel heap, ENOSPC beyond.
  Second run: `[fuzz] 20000 syscalls, kernel-survived=true`.
- **N2.1/N2.2 footprint.** The 512 MiB kernel heap was a static array, which
  made the kernel image 610 MB of virtual size and the public image fail at
  LoadImage below about 1 GiB. The heap is now allocated at boot from UEFI
  (a quarter of conventional RAM, 64 to 512 MiB, halving on failure, 32 MiB
  static fallback). Measured: the public image boots to the desktop in a
  512 MiB guest with a 77 MiB heap; the dev image gets 512 MiB at 3 GiB.
- **First gate run** (`scripts/gate.sh`): host tests PASS, dev build PASS,
  dev boot FAIL on three pre-existing FAILED markers, none from tonight's
  code. `[aa-mcp]`/`[aa-wasm]`: the session and must-change self-tests leave
  the uid-context on a logged-in user and rwx is enforced on the live root,
  so the gateway's write under root-owned /agents failed; both now run under
  `sysctx::as_system`. `[trash]`: only with a blank target disk attached (the
  gate's configuration): [q1x3] installs it, the live root moves to the
  fresh disk, and /var does not exist there yet; the self-test now creates
  its scratch directory first. The gate found real self-test defects on its
  first run, which is what it is for.

## N1. EuroApps SDK: let others build and ship apps (this night)

**Goal:** a developer who is not us can build an app on their Linux machine,
package and sign it with their own key, and a EuroOS user can install and
launch it from the launcher, under the kernel's capability and syscall
policy. Nothing of that is possible today: the launcher list is a constant in
the kernel, and `eupkg` verifies against our developer key only.

**Pieces**
1. Package format: the existing signed `.eupkg` (STORED zip: `MANIFEST.toml`,
   the binary, `signature.ed25519` over the manifest, sha256 of the binary in
   the manifest). New manifest fields: `kind = "app"`, `title`, `caps`
   (console,file,net,proc), `profile` (full|utility|renderer|strict),
   `display` (none|x11), `args`. Old packages stay valid.
2. Trust store: `/etc/euroapps/trusted/<name>.pub`. A package verifies against
   our developer key or any enrolled key. Only the owner may enrol a key:
   `eupkg trust <file.pub>`; `eupkg trusted` lists them.
3. Direct install: `eupkg install-file <path.eupkg>`; the registry keeps the
   app fields; `app list` shows installed apps.
4. Launch: installed apps appear in the launcher. An `x11` app is hosted in
   the X client window (like Chromium); a `none` app runs from the shell with
   `app run <name>` and its output is shown. Capabilities and the syscall
   profile come from the manifest and are applied at spawn.
5. SDK on the host: `sdk/euroapp.py` (`keygen`, `build`, `verify`),
   `sdk/examples/hello-cli` and `sdk/examples/hello-x11` with a build script,
   `docs/APP-DEVELOPER-GUIDE.md`.
6. Proof: `[app]` self-test in the dev build: the example package signed with
   the example developer key is refused before the key is enrolled, installed
   after, a tampered copy is refused, and the app runs with its exit code and
   output. Plus a QEMU run where the X11 example is launched from the launcher.

**Limits (stated in the guide):** glibc programs against the library set
EuroOS serves (the chrome pack's libs), one hosted X11 app at a time, package
size bounded by RAM (the whole binary is loaded, not demand-paged), no
dependencies between app packages yet.

## N2. Memory footprint and swap (WP3)

1. Measure: the kernel image is 610 MB of virtual size; list the static arrays
   (thread kernel stacks, FPU areas, pools, embedded test binaries) and the
   boot RAM floor of the public image (today: fails below 1 GiB).
2. Reduce: move the largest statics to runtime allocation sized from RAM;
   keep embedded self-test binaries out of the public profile.
3. Swap (J3): anonymous-page swap of the demand pool to the EuroVar
   partition under pressure; page-out picks least-recently-faulted frames.
- **Verify:** the public image boots in 512 MiB; the installed system in
  2 GiB; Chromium renders euro-os.eu in a 4 GiB guest (needs the NUC for
  speed, QEMU for correctness).

## N3. Stability gate (WP4)

1. `scripts/gate.sh`: host tests, dev build, QEMU dev boot with marker checks
   (every `→ FAILED` is a failure, `[desktop]` must appear), public image
   build, wizard harness. One command, one verdict.
2. Syscall fuzzer `gfuzz` (glibc): random syscalls with random arguments for
   N rounds under a profile that forbids the destructive ones; the kernel must
   never panic or fault in ring 0. `[fuzz]` marker with the round count.
3. Soak: a 2-hour QEMU run of the desktop with the harness typing and the
   self-tests looping; verdict = still responsive, no leak in the frame
   allocator high-water mark.

## N4. RDP client (WP5)

A native client for the one legacy Windows application: RDP 10 core
(X.224, MCS, TLS, CredSSP optional), keyboard, mouse, bitmap updates, clipboard
text. Host-tested protocol crate `eurordp`, a desktop window renderer.
- **Verify:** connect to a Windows host in QEMU, the remote desktop paints,
  typing arrives. Needs a Windows VM in the lab.

## N5. Isolation remainders

PIE base randomisation (slide the exe image inside the demand pool); W^X for
Chromium with V8 write-protected code; modern virtio in the drivers so the
IOMMU covers virtio in QEMU; interrupt remapping; `dma_add` for USB hot-plug
allocations; one IOMMU domain per device instead of per driver.

## N6. Bugs with owners

gpango worker writes to address 0x10 (SIGSEGV, previously masked); gscm3
fails on the 55 MiB process pool at 3 GiB; gvec needs AVX enabled with XSAVE
context switching (a real feature: AVX-using programs crash today); gnss needs
the NSS pack attached in the QEMU harness.

## N7. App store and developer onboarding

A signed package index on euro-os.eu, `eupkg install <name>` over the update
channel's HTTPS path, a store section in the launcher, a published developer
key policy (who signs what, revocation through the index).

## Order for the night

N1 complete with proof, then N3.1 (the gate script) and N3.2 (fuzzer), then
N2.1 and N2.2 (measure and reduce), then whatever N6 item fits.

## Progress 2026-10-07: audit remainders, app store (N7), swap (N2)

- **Audit remainders closed.** WASM `file_read`/`file_write` refuse any path
  with a symlink component (`euroapi::path_has_symlink` over
  `FileSystem::read_link`); `[euro-fscope]` proves a link in the home to
  `/etc/euroguard/system.conf` is refused and a plain file round-trips.
  Ctrl+Alt+Esc always belongs to the desktop: it sends Close to the focused
  native or WASM app and terminates it if it ignores that.
- **N7 app store built.** `kernel/src/appstore.rs`: the index at
  `https://euro-os.eu/apps/index.json` is signed by the EuroOS key; every
  entry names the developer key that signed its package, so the signed index
  vouches for it (a package signed by any other key is refused even when
  listed); packages are verified as always and re-hashed at launch. Shell
  `app store [refresh|install <name>]`; the launcher lists store apps with an
  "install" tag and installs on selection; the verified index is cached under
  `/pkg` (system data, written as the system from the owner's shell).
  Published: `scripts/publish-apps.sh` builds the index with
  `toolchain/eupkg/make-app-index.py` and serves 5 packages live.
  `[appstore]` self-test: forged signature refused before any fetch, wrong
  developer key refused, vouched package installs without enrolment. Live
  proof: `app store refresh` over EuroTLS from a QEMU desktop ("index from
  https://euro-os.eu/apps/ verified (EuroOS key): 5 app(s), cached").
  Found on the way: the desktop shell runs with the session user's
  uid-context, so `/pkg` writes (store cache, installs) failed as the user;
  store and `app install`/`remove` now run as the system for the owner.
- **N2 swap on the demand pool.** New `EuroSwap` GPT partition (8% of the
  disk, at least 64 MiB) on every new install; `swapmgr` gained
  explicit-page-table variants, CLOCK eviction over a process's demand region
  (`evict_demand`, Accessed bit as the reference bit), USER-marked swapped
  PTEs so swap-in restores a user page from the demand pool, and slot
  release when a demand region is torn down. The demand-fault handler evicts
  from the faulting process when the pool is dry instead of ending it.
  `free` shows the swap line. `[j3-demand]` (gate boot with a disk): a glibc
  process allocates more than the pool holds and verifies its pattern twice.
  Limits: virtio-blk only (the swap I/O busy-polls that driver), eviction
  from the faulting process's own region only, no swap on disks installed
  before this layout.
- **N4 RDP client, first rung.** `crates/eurordp` (no_std, forbid(unsafe),
  14 host tests): TPKT/X.224 with the RDP negotiation (TLS requested), MCS
  Connect-Initial/Response (BER + GCC in PER: client core/security/network
  data, server data blocks), erect domain, attach user, channel joins, send
  data; Client Info with credentials, licensing (STATUS_VALID_CLIENT accepted,
  a licensing exchange is refused), Demand Active parsing, Confirm Active with
  16 capability sets (no drawing orders, so the server sends bitmaps;
  uncompressed at 16 bpp), synchronize/control/font list, slow-path bitmap
  updates to XRGB (15/16/24/32 bpp, bottom-up), input events (scancode,
  unicode, pointer, buttons, wheel). `Session` is a state machine over bytes
  in/out. `kernel/src/rdpclient.rs`: TCP through the kernel stack, EuroTLS 1.3
  after X.224, frame in a Euro API surface shown in the hosted window, input
  from that window; `rdp <host>[:port] <user> [password]` in the Terminal.
  Test target: xrdp in docker on the build server (`docker run --name
  xrdp-test -p 127.0.0.1:3389:3389 -e USER=euro -e PASSWORD=euro-test-1
  danielguerra/ubuntu-xrdp`), reached from QEMU user-net as 10.0.2.2.
  Limits stated: TLS 1.3 only (older Windows servers offer 1.2: the EuroTLS
  1.2 client is the next rung), no NLA/CredSSP, the server certificate is
  not validated in v1 (self-signed RDP certificates; TOFU with the settings
  UI next), no RLE bitmaps, no clipboard/drive redirection, no licensing
  exchange (Windows Server asks for one; xrdp does not).

### N2 swap, status correction (2026-10-07, later)
Swap-OUT under demand-pool pressure is proven: [j3-demand] shows thousands of
anonymous pages written to the EuroSwap partition with no kernel fault and the
desktop still up. Transparent swap-IN (and sustained eviction) is still WIP:
eviction stops at a deterministic point, so demand-pool eviction is now OFF by
default (SWAP_DEMAND_EVICT, flipped on only by the test). Production keeps the
prior clean-OOM behaviour. The EuroSwap partition and the [j3] basic swap-cycle
(victim selection + disk round-trip) stand.

## Stability backlog (started 2026-10-07, afternoon)

Jeroen: "een OS-ontwikkeling kan nooit zomaar klaar zijn; we missen vast nog
heel wat zaken." Standing order: keep improving stability, features and
stress coverage, autonomously. Rules for this track: stress runs are SHORT
and SHARP (minutes, in the gate on every build), never hours-long idles; a
test that finds a bug is followed by the fix and stays as a regression test.

Done today
- [churn-proc] 16 glibc processes run + reaped, every counter back to baseline
  (frames, demand pool, task slots, thread kernel stacks, file table, heap).
- [churn-fs] 3 rounds x 120 files write/rename/read/remove on the live root,
  integrity + empty dir + sentinel + heap stable.
- [churn-net] 64 sockets open/close (half UDP to the loopback DNS responder),
  socket table back to baseline.
- [churn-surface] 160 Euro API surfaces create/present/remove, absurd size
  refused, list + heap back to baseline.

Next (in order; each one is a gate test when it lands)
1. Public image boot at 512 MiB after today's growth (guards the download).
2. Swap-in root cause: evict_demand returns None at a deterministic point
   (~2576 pages) with the correct window and a generous budget; instrument the
   None return (present pages seen, hand) and fix; then SWAP_DEMAND_EVICT on.
3. [churn-win] window manager: open/close/move/resize/focus 200 windows from
   the compositor side, Alt-Tab cycling, workspace switch; no stale entries.
4. [churn-thread] glibc threads: 200 pthread create/join cycles in one process
   (gthread), kstack + futex + TLS back to baseline.
5. [churn-fork] fork/exec/wait 100x; zombie reaping; pid reuse safe.
6. [churn-pkg] app install/remove 30x (signed packages), /pkg store size
   stable, launcher entries stable.
7. Timer/IRQ storm: 10 s of PIT+USB+virtio interrupts with a busy ring-3 task;
   tick-late count bounded, no lost wakeups.
8. gpango SIGSEGV at 0x10 (open since 2026-10-05).
9. RDP licensing (client new-license / platform challenge), then TLS 1.2
   client for Windows 10 / Server 2019 hosts.
10. Dark theme + screen recording (the two conveniences still missing).
   Note on 3 ([churn-win]): the window list lives inside the desktop's
   interactive loop (main.rs), not behind an API, so a boot-time churn cannot
   reach it; the Euro API surface churn covers the hosted-window path. Moving
   the window list behind a small API is the prerequisite (a refactor, later).

### Stability track, later on 2026-10-07
- [churn-proc] found a real leak: 4 frames per glibc process. Root cause:
  paging::free_address_space freed only the PML4[0] chain and stopped at the
  first 4 KiB page table; the vDSO lives at its own PML4 slot (62) with
  pdpt/pd/pt from the frame allocator, never freed. Fix: free_user_table_chain
  + ring3::free_process_space at all six teardown sites. gtiny exits 42 by
  design (the test's "clean" criterion was wrong at first).
- Added [churn-thread] (gthreadchurn, 60×4 pthreads), [churn-fork] (gforkchurn,
  40 fork/wait), [churn-pkg] (12 install/remove), [storm-io] (300 reads + 300
  presents, lost ticks ≤ 10%).
- gpango SIGSEGV narrowed: mmap(NULL) → ENOMEM from the arena mmap window,
  then an unchecked NULL (write to 0x18). A bounded diagnostic now prints the
  size and the window state at every such ENOMEM.
- Swap: evict_demand now explains a "no victim" return (present pages seen,
  Accessed bits cleared, swap-out failures, slots used/capacity).
- RDP licensing (MS-RDPELE) implemented in crates/eurordp/src/license.rs:
  MD5, SHA-1, RC4 from scratch (RFC/FIPS vectors), key schedule, MAC, raw RSA
  via crypto-bigint, proprietary server certificate; wired into the Session
  with kernel entropy (21 host tests green). Live proof against xrdp pending
  the next QEMU slot.

### Results, churn3 (2026-10-07 17:04)
- gpango exit=0 with demand paging: the SIGSEGV open since 2026-10-05 is gone.
- [j3-demand] swap-outs=20856, swap-ins=8524, 64 MiB verified twice, exit 0:
  transparent swap on the demand pool WORKS (the pool top-up threshold was
  the last bug: a 2 MiB-aligned page needs a page-table frame as well).
- [churn-proc] leak down from 4 to 1 frame per glibc run (vDSO chain freed on
  the run_glibc paths; the last frame is still to be found).
- [churn-thread] FAILED, tasks 39->256: a glibc worker thread's slot was never
  offered for reuse while its process ran (reclaim only happened in reap_dead
  after the run), so 240 create/join cycles exhausted the 256-slot table.
  Fix: sched::RECLAIMABLE flag set at thread exit (registries cleaned first),
  swept lazily by alloc_slot before the table grows.
- [churn-fork] FAILED: every fork copies the 384 MiB arena (40 cycles do not
  fit the run's timeout under TCG) and each child leaked 3 demand-pool frames
  (the demand region's page tables). Fix: free_demand_table_chain at child
  exit; the test runs 6 cycles.
- fs, net, surface, pkg churn and the I/O storm: OK (lost ticks 0 in 61).

### Results, churn4 (2026-10-07 17:30)
- GTHREADCHURN 60×4 pthreads: bad=0, exit 77: the lazy slot reclaim works
  (the table grows two slots as a high-water mark before the first thread
  exits, then every slot is reused). Criterion relaxed to "at most a few
  slots, never per cycle".
- GFORKCHURN 6 cycles: bad=0, exit 66: every child reaped with its status.
  Still 3 demand-pool frames per child, with demand paging OFF, so not the
  demand region's tables; the fork child's own page tables under PML4[0]
  (pdpt, pd, PTs from the process pool) were also never freed. Both: free_fork_tables at
  child exit + bounded [fork-acct] pool accounting at fork start and child
  exit to pin the demand-pool frames.
- [churn-proc] 1 frame per glibc run remains; bounded [glibc] frames: start/end
  accounting added to run_glibc.

### Results, churn8 (2026-10-07 18:36): ALL GREEN, 117 OK, 0 FAILED
- [churn-proc] leak-free: frames 411854 → 411854 over 16 glibc runs. The last
  frame per run was a page directory: build_address_space_rwx_big adds one
  under the PDPT index where the big arena physically lies, and
  free_address_space only freed pdpt[0]. It now frees every real PD + PTs.
- [churn-thread], [churn-fork], [churn-fs], [churn-net], [churn-surface],
  [churn-pkg], [storm-io] OK; [j3-demand] transparent swap OK (on by default);
  gpango exit 0.
- Found on the way: (a) a fork child's exit path runs on the child's own page
  tables, so teardown there may only mark frames free, never clear entries
  (churn6 hung silently when it did); (b) the frame allocator's aligned /
  contiguous search re-scanned up to `count` bits per candidate window, which
  froze NET=1 boots for minutes on a 96 / 384 MiB arena request once the NIC
  buffers fragmented the bitmap; rewritten as a linear word-skipping scan
  (euromm, 15 host tests).

## Policy from the outside (2026-10-09, after the MXC comparison)

Microsoft Execution Containers (GA 2026-10-07) ship a learning mode and an
exportable policy schema; EuroOS had the boundary in the kernel but no way to
learn or move it. Built, both with enforcement ON while learning:

1. Agents: `euroagent learn [<agent>] <intent>` records what the MCP gateway
   denied; `euroagent propose` shows the manifest that would allow it (an
   elevated capability only ever as optional, an explicit denial never undone,
   nothing removed); `euroagent approve` installs it at
   /etc/euroagent/<name>.toml (owner, as the system, audited POLICY_APPROVED).
   Manifests carry `schema = 1`, serialise exactly (to_toml), a newer schema is
   refused; `euroagent export|import`. [policy-learn] green, 54 host tests.
2. Programs and apps: a per-app policy file /etc/euroapps/<app>.policy.toml
   (paths read/write/deny, net domains), enforced in the Linux-ABI syscall path
   (EACCES, never a kill) and in the DNS path, for any profile. No file = the
   profile's rules only. `app policy learn <app>` arms a session: the program
   runs under an empty policy, every refusal is a want; `app policy propose`
   (hard-denied system paths such as /etc/euroid are never proposed),
   `app policy approve` (owner), `show|export|import`. [app-policy] self-test:
   gpolicy (a plain glibc program) reads /etc/hostname, writes a home file and
   tries the identity store: without a policy 3 (system refuses the store), in
   learning 0 with both wants recorded, after approval 3 again with the store
   still refused.
