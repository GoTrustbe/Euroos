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
