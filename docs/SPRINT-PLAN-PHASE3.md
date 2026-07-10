# EuroOS — Phase 3 Sprint Plan (everything still open)

> **Purpose.** A single reviewable backlog of *all* remaining work, grouped into phases by
> theme and value. Status labels are honest: **✅ done · 🟢 core done (host-tested + boot
> marker), real remainder · 🟡 partial · ⬜ not started · 🔒 needs real hardware (can't be
> fully proven in QEMU/TCG)**.
>
> Items marked **[NEW]** were added in the 2026-06-14 revision (the strategic/compliance pass);
> everything else is unchanged from the first Phase-3 draft.
>
> Two hard rules carry over: **verify by running** (host tests green **and** a `[xx]` boot
> marker) and **never present mock as real** (label demo vs real; under-claim).
>
> **Progress (2026-06-14):** Scheduled **Sprint 1** (3A-1, 3A-2, 3A-7) and **Sprint 2**
> (3A-3, 3C-1, 3C-7, 3C-8, 3C-9) are ✅ **done** — host-tested + boot-verified, and the new
> code was audited ([`CODE-AUDIT-2026-06-14.md`](CODE-AUDIT-2026-06-14.md): 1 CRITICAL + 3
> HIGH + lower fixed with regression tests). 793 host tests. Items are marked ✅ below.
>
> Per-item detail + done-work log: [`NEXT-SPRINTS.md`](../NEXT-SPRINTS.md) (G–Z board).

---

## Where we are (done, boot-verified)

Core OS, EuroFS, storage interop (FAT32 r/w · exFAT/ext r · SMB · NFS · multi-disk + stress),
network/firewall/VPN, EuroDesktop, EuroID, EuroAgent, TPM+FDE+attestation+EuroCA, A/B
self-update, R–Z sovereign-service crates. **793 host tests.**

Recurring pattern: a `🟢 core done` item has a host-tested crate + boot self-test; its
**REMAINING** is the real-world wiring (hardware / live userspace / persistence / interop)
that turns "engine works" into "feature works end-to-end".

---

## Phase 3A — Storage, finished *(no hardware gate, highest practical value)*

| ID | Task | Status | Scope & verification |
|----|------|--------|----------------------|
| 3A-1 | exFAT write | ✅ | create/write/delete/rename in `euroexfat`. Verify: write → `fsck.exfat` clean + Linux reads it. |
| 3A-2 | ext2/3/4 write | ✅ (ext2 w) | block/inode bitmap alloc + extent/indirect write + dir link/unlink. Verify: `e2fsck -f` clean. Start ext2, gate ext4-journal. |
| 3A-3 | Real partition + USB-disk mount harness | ✅ | Mount a *partition* inside a GPT/MBR disk + a removable **USB** volume with **auto-mount** on hotplug. "Plug in a stick and it appears." Verify: extended `run-usb.py` + `[io-usb]`. |
| 3A-4 | NTFS read | ✅ **done + host-verified vs mkntfs** | New crate [`eurontfs`](../crates/eurontfs): a from-scratch read-only NTFS driver — boot sector → **$MFT** → FILE records with **USA fixup** → attributes ($FILE_NAME/$DATA) → **runlist** decode → resident + non-resident file read + root listing. Reads a known file **verbatim from a real `mkntfs` image** (4 host tests). REMAINING: live NTFS disk-mount in the VFS + directory index-tree walk (today a linear MFT scan) + write. |
| 3A-5 | btrfs / xfs read | 🟡 **identify done; full read honestly deferred** | New crate [`eurofsid`](../crates/eurofsid): superblock **identification** of btrfs/xfs (+ ntfs/exfat/fat32/ext) for VFS **mount auto-detect**, with label/UUID, verified vs real `mkfs.btrfs`/`mkfs.xfs` superblocks (`[3a5]`, 4 host tests). btrfs/xfs are now *recognised* (and honestly reported as not-yet-readable). **REMAINING (large, as the plan flagged):** full btrfs (chunk/root/fs/extent B-trees) and XFS (AG + B+ trees) file read — each a multi-week driver. |
| 3A-6 | SMB3 encryption/signing + NFSv4 | 🟡 **SMB2 signing done; SMB3/NFSv4 deferred** | New [`eurosmb::signing`](../crates/eurosmb/src/signing.rs): **SMB 2.1 message signing (HMAC-SHA256)** — sign + constant-time verify; a tampered message or wrong key is rejected (`[3a6]`, 3 host tests). Authenticates the SMB session without AES. **REMAINING:** SMB3 **encryption** + AES-CMAC/GMAC signing (needs an AES primitive the stack deliberately avoids) and **NFSv4** (a large protocol addition). |
| 3A-7 | **SSD health & durability** **[NEW]** | ✅ | TRIM/discard + write barriers + `fsync` durability guarantees. Underpins data integrity for everything above; cheap, no hardware gate. Verify: discard reaches the device; barrier ordering survives a fault-injection power-cut. |

---

## Phase 3B — Bare-metal readiness *(🔒 protocol cores done, last mile needs metal)*

| ID | Task | Status | The remaining (hardware) part |
|----|------|--------|-------------------------------|
| 3B-1 | Modern virtio transport (unlocks GPU) | 🟢→🔒 | virtio-1.0 transport (PCI caps, common/notify/device cfg) + scanout binding. Verify: `virtio-gpu-pci` + a real scanout screenshot. |
| 3B-2 | WiFi radio driver | 🟢→🔒 | Intel AX200/210 PHY/MAC + iwlwifi firmware + full SAE handshake. Real radio. |
| 3B-3 | Printer live (IPP→CUPS) | 🟢→🔒 | Wire `europrint` to a live TCP connect; verify a real job vs CUPS. + SANE scanning. |
| 3B-4 | ACPI S3 suspend/resume | 🟡→🔒 | CPU + device save/restore across firmware sleep. Not headless-verifiable. |
| 3B-5 | USB hubs + USB EuroFS writeback | 🟡 | Multi-tier hubs; write + mount a USB-resident EuroFS volume. |
| 3B-6 | NVMe MSI-X completion wait-queue | 🟢 | IRQ-blocking wait-queue (vs poll); NVMe MSI-X; live-FS auto-remap. Mostly in-QEMU. |
| 3B-7 | **Audio device driver** **[NEW]** | 🔒 | HDA + USB-audio codec driver (virtio-audio testable in VM first, then metal). Engine half of audio; desktop/routing half is 3F-6. |
| 3B-8 | **Bluetooth** **[NEW]** | 🔒 | HCI stack + pairing; peripherals + BT audio on real hardware. |
| 3B-9 | **Power & thermal** **[NEW]** | 🔒 | CPU freq scaling/governors, ACPI battery, thermal throttling. Laptop usability + eco-argument. |
| 3B-10 | **Multi-monitor / HiDPI / display-hotplug** **[NEW]** | 🔒 | Extends 3B-1 (scanout) into a usable desktop on real panels. |

---

## Phase 3C — Finish the cores *(the 🟢/🟡 REMAINING tails)*

| ID | Task | From | The remainder |
|----|------|------|---------------|
| 3C-1 | Live-FS through the block cache | ✅ | `EuroFs<RootBlk>` → `EuroFs<BlockCache<RootBlk>>` (ripples through `fs`/`fs2`/`vfs`). |
| 3C-2 | Per-subsystem locking | J1 | Per-mount FS / per-conn net / per-CPU run-queue / per-channel IPC; prove no inversions with `eurolock`. |
| 3C-3 | ld.so libc breadth | H3 | 🟡 **PT_INTERP + userspace ld.so path done + boot-verified** — the missing structural piece landed: a dynamically-linked exe that names `PT_INTERP=/lib/ld-euro.so` is now launched via its **interpreter**, and a **from-scratch userspace** `ld-euro.so` (`userland/ldeuro.c`) does the JUMP_SLOT/GLOB_DAT/RELATIVE relocations itself against `libc-euro.so` — not the in-kernel linker. Kernel `run_interp` (`kernel/src/ring3.rs`) loads exe+libc+interp, sets the auxv (`AT_BASE` + EuroOS exe/libc-base entries) and jumps to the interpreter; marker `[3c3]` = `3C3: 42` exit 42. This is the real Linux dynamic-linking mechanism, in userspace, with a controlled in-tree ld.so/libc. **REMAINING for unmodified busybox/curl/sqlite:** file-backed `mmap`/`MAP_FIXED` so ld.so maps the libs itself (today the kernel pre-loads them into the 2 MiB arena — the big structural blocker), general-dynamic TLS (`__tls_get_addr`), extra reloc types (IRELATIVE/COPY), and glibc-required auxv/syscalls (`AT_SYSINFO_EHDR`/vDSO, real `futex`). |
| 3C-4 | WASI preview1 complete | H4 | Real `fd_read`/`path_open`/`sock_*`; `wasm <file>` shell command. |
| 3C-5 | Real Wayland clients | H5 | Unmodified libwayland clients over AF_UNIX: fd-passing, `wl_shm`, damage/frame callbacks. |
| 3C-6 | Display-server input/SHM | H2 | Input events back to apps; per-tick live updates; pixel/SHM buffers. |
| 3C-7 | Swap auto-evict under memory pressure | ✅ | Wire CLOCK into the live frame allocator (swap core + fault path already ✅). |
| 3C-8 | Coreutils long-tail + security flags | ✅ | `ln/readlink/realpath/mktemp`, `shuf/comm/join/split`, `sha1/md5/b2sum`, `env`, `xargs`; `--audit`/`--sign`. |
| 3C-9 | FS symlinks | ✅ | Symlink inodes in EuroFS + VFS resolution. Prerequisite for several coreutils. |

> **3C-7 note (reconciled):** J3 swap itself is ✅ done in the G–Z board — only *auto-evict
> under memory pressure* remains, captured here as 3C-7. The older "S5: J3 swap pressure"
> pending task has been **retired** to remove the drift between the task list and the board.

---

## Phase 3D — Sovereign trust, end-to-end *(the European USP)*

| ID | Task | From | The remainder |
|----|------|------|---------------|
| 3D-1 | TPM key sealing → FDE/Vault unseal only on untampered boot | O1+K3 | ✅ **core done + boot-verified (swtpm)** — real `TPM2_CreatePrimary/StartAuthSession(trial)/PolicyPCR/PolicyGetDigest/Create/Load/Unseal/FlushContext` in `eurotpm` (12 host tests) + kernel orchestration (`tpm::seal_to_pcr`/`unseal_from_pcr`). The FDE key (K3) + vault master (U) are sealed **inside the TPM** under a PolicyPCR over PCR16; released only on a matching boot. Marker `[3d1]`: same-boot-unseal=OK, and a **tamper (extra PCR extend) is REFUSED by the TPM itself** (`TPM_RC_POLICY_FAIL`) — fail-closed in hardware. Replaces the old software-KDF `[af-seal]`. Verified with `scripts/run-swtpm.sh`. **Headline.** **REMAINING:** durable on-disk persistence of the sealed blob across a *physical* reboot (the crypto/binding is proven in-boot; the parent is deterministic so it re-loads) + a real loader→kernel measured-boot chain into PCR0-7 (today PCR16 carries the O1 measurement). |
| 3D-2 | Verity immutable system partition | L3 | 🟢 **core done + boot-verified** — new crate [`euroverity`](../crates/euroverity): a **SHA-256 Merkle tree** over the system image's blocks + an **Ed25519-signed manifest** binding the root. `verify_block` checks any block against the signed root with an O(log n) proof; a tampered block or a forged manifest is refused. Marker `[3d2]`: signed-root verify + tampered-block DETECTED + forged-manifest REFUSED (4 host tests). **REMAINING:** wire it onto the live read-only slot read path (`eurofs` read → per-block verify) + the loader falling back to the good slot on a verity failure. |
| 3D-3 | Attestation + CA over network | O2+O3 | 🟢 **core done + boot-verified** — [`euroca`](../crates/euroca) gained certificate **serialization**, multi-level **`verify_chain`** and a persistable **`CertStore`** (root + issued + CRL); [`euroattest`](../crates/euroattest) gained a JSON **`Report`** a verifier consumes. Marker `[3d3]`: a 3-level **root→intermediate→leaf** chain verifies against the root only, the store round-trips **on disk**, and a JSON attestation report over the real boot PCRs is accepted while a **replay** and a **tampered PCR state** are refused. **REMAINING:** a live HTTPS attestation endpoint (over `eurotls`) and a **hardware-resident TPM2_Quote** (AK inside the TPM — needs a TPM-signature verifier, e.g. ECDSA-P256, the stack doesn't have yet; today the AK is a software key bound to the boot PCRs). |
| 3D-4 | Signed policy bundles | X | ✅ **done + boot-verified** — [`europol::bundle`](../crates/europol/src/bundle.rs): canonical byte encoding of a policy set + **Ed25519 sign / verify-before-load**. A policy can only change capabilities if it is signed by the release key. Marker `[3d4]`: valid bundle loads, a tampered bundle + a wrong signer are refused (4 host tests). **REMAINING:** load real signed bundles from `/etc/europol/*.bundle` + per-binary attach + syscall-path cache. |
| 3D-5 | User-scoped immutability + euroattr | L4 | ✅ **done + boot-verified** — new [`kernel/src/euroattr.rs`](../kernel/src/euroattr.rs): a user may set/clear `IMMUTABLE`/`APPEND_ONLY` on files **under their own home** *without* `CAP_IMMUTABLE_ADMIN` (gated by ownership), while system paths still require the admin cap. `euroattr +i/-i/+a/-a/status` shell command. Marker `[3d5]`: own-file lock + write-then-blocked + system-path/other-user denied + owner-can-clear. **REMAINING:** file-manager lock badge; per-inode uid ownership in EuroFS (today ownership is the home-path prefix). |
| 3D-6 | Vault/GDPR audit persistence | U+P3 | 🟢 **core done + boot-verified** — new crate [`euroaudit`](../crates/euroaudit): a **hash-chained** (`hash_i = SHA-256(hash_{i-1} ‖ entry_i)`) tamper-evident audit log with `execve`/`connection`/`vault_access`/… event kinds, **JSON export**, **query/filter**, and **rotation that carries the chain across files**. And the **TPM-sealed vault blob is now persisted to disk** and reloaded+unsealed (was RAM-only). Marker `[3d6]`: chain verifies, tamper detected, JSON/query/rotation, sealed-vault survives on disk (6 host tests). **REMAINING:** migrate the live `kernel/src/audit.rs` onto `euroaudit`, wire real `execve`/connection call sites, and persist the boot vault to `/var/lib/vault.seal` at shutdown. |
| 3D-7 | **Secure time (NTS, RFC 8915)** **[NEW]** | 🟢 **core done + boot-verified** | New crate [`euronts`](../crates/euronts): the NTPv4 + **NTS extension-field** protocol (Unique Identifier / Cookie / Authenticator-and-Encrypted EFs) + the **RFC 8446 §7.5 TLS-exporter key schedule** (HKDF-Expand-Label; C2S/S2C keys match a real endpoint given the same exporter secret). AEAD = ChaCha20-Poly1305 (IANA id 29). Marker `[3d7]` (nonces from the 3D-8 CSPRNG): an authenticated server time is accepted, a **tampered timestamp is rejected** (AEAD), and an **off-path reply with a wrong Unique Identifier is rejected**. 5 host tests (roundtrip + tamper + off-path + wrong-key + key-schedule). **REMAINING:** live **NTS-KE over TLS** (eurotls) + real-server sync, and the mandatory **AEAD_AES_SIV_CMAC_256** (id 15, needs an AES/CMAC impl the stack currently avoids). |
| 3D-8 | **Early-boot entropy / RNG quality** **[NEW]** | ✅ **done + boot-verified** | New crate [`euroentropy`](../crates/euroentropy): **HMAC-DRBG SHA-256 (NIST SP 800-90A), verified byte-for-byte against the NIST ACVP KAT** (`tests/kat.rs`) + own HMAC-SHA256 (RFC 4231 tested). An `EntropyPool` gathers CPU-timing **jitter** (RDTSC deltas, conservative min-entropy estimate) + the TPM RNG, and **refuses output until a 256-bit real-entropy threshold is met** (hard `getrandom`-blocking). Kernel `entropy::{init,getrandom,ready}` + `[3d8]` marker: empty-pool refused, **jitter alone reaches readiness with no TPM**, seeded pool yields non-zero distinct output. **REMAINING:** migrate the existing FDE/PQC/VPN/CA call sites off raw `tpm::get_random` onto `entropy::getrandom`. |
| 3D-9 | **Post-quantum crypto** **[NEW]** | 🟢 **KEM done + boot-verified** | New from-scratch crate [`europq`](../crates/europq): Keccak/SHA3/SHAKE (FIPS 202) + **ML-KEM-768 (FIPS 203)**, verified **byte-for-byte against the NIST ACVP known-answer vectors** (`crates/europq/tests/kat.rs`: keyGen/encaps/decaps). **Hybrid X25519 + ML-KEM-768** wired into `eurovpn` (`initiate_hybrid`/`respond_hybrid`/`finish`); the ML-KEM secret is mixed into the HKDF so the tunnel key is secret if **either** primitive stands (harvest-now-decrypt-later resistant). Boot marker `[3d9]` proves the round-trip **and** that a tampered KEM ciphertext breaks the tunnel (PQ secret is load-bearing). 9 KAT + roundtrip + rejection tests; hybrid VPN handshake tests. **REMAINING:** ML-DSA (FIPS 204) for EuroCA + update-signing, hybrid ML-KEM in `eurotls` (TLS), and side-channel hardening/constant-time review. |
| 3D-10 | **eIDAS 2.0 / EUDI-wallet + Belgian eID into EuroID** **[NEW]** | 🟢 **software half done + boot-verified** | New crate [`eurowallet`](../crates/eurowallet): **SD-JWT VC** (IETF selective-disclosure JWT for verifiable credentials, the EUDI-wallet format) with EdDSA — issue / present / verify + **holder key binding** (anti-replay). The disclosure/digest encoding is **cross-checked against the IETF SD-JWT reference worked example** (`given_name`/`John` → the spec's exact base64url + `_sd` digest). EuroID acts as PID issuer **and** relying party: boot marker `[3d10]` issues a PID, discloses ONLY `nationality` (name/birthdate stay hidden), and proves a replayed nonce + a wrong issuer are refused. 10 host tests incl. forged/tampered-disclosure rejection. **REMAINING (hardware-gated):** Belgian eID **card-reader** middleware + itsme, and OpenID4VCI/OpenID4VP transport. **Hard clock: each member state must offer a wallet by Dec 2026.** |

---

## Phase 3E — Distribution & governance *(ship it)*

| ID | Task | From | Scope |
|----|------|------|-------|
| 3E-1 | Installer GUI + FDE enrol | Q1 | 🟢 **done + boot-verified** — the installer now **executes** `Step::EnrollFde` for real (`instexec::enroll_fde`): the FDE key comes from the TPM RNG, is **TPM2-sealed to PCR16** (3D-1 path), only the sealed blob is written to the target (`/etc/fde/root.seal`), unseal round-trips and neither blob leaks the key (`[3e1]`, fail-closed with no TPM — no plaintext fallback). The guided GUI **Install** button is wired to a real install to the first blank virtio disk (`installer::gui_install`, `button_at`). **REMAINING:** loader-side unseal-at-boot to auto-open the FDE root; cross-machine key-escrow/recovery enrolment. |
| 3E-2 | EuroUpdate delivery server | K2 | 🟢 **done + boot-verified LIVE** — signed **release channels**: `update::check_channel` fetches an Ed25519-signed channel manifest, **refuses a forged manifest before fetching anything**, compares versions, then fetches an image whose sha256 is **pinned by the signed manifest** and Ed25519-verified before A/B staging. Host server: `toolchain/update-server/{make-repo,serve}.py`. `[3e2]` runs it **live over EuroNet TCP** against the server on the SLIRP gateway (stable→staged, old→up-to-date, evil→refused). Shell: `euroupdate check`. **REMAINING:** HTTPS transport with a kernel-trusted server cert; delta images. |
| 3E-3 | Full multi-user / session model | K1 | 🟢 **done + boot-verified** — a real **session lifecycle** on EuroID (`kernel/src/session.rs`): open/close with single-seat switching, **auto-created `/home/<user>` OWNED by the user**, and a per-session **FS uid-context** so files a user creates are theirs. Wired into shell `login`/`su`/`logout` (+ `sessions`) and the desktop lockscreen. Rests on new **uid-on-inode** in EuroFS (`OFF_UID`, `chown`/`owner`). `[3e3]`. **REMAINING:** concurrent (SSH-style) sessions with per-process binding; per-user EuroPol policy files. |
| 3E-4 | Reproducible release pipeline (CI) | Q2 | 🟢 **done + verified** — **reproducible** kernel build (`scripts/repro-build.sh`: `--remap-path-prefix` + lld `/Brepro` to zero the PE COFF timestamp → **byte-identical `eurokernel.efi`** across rebuilds, verified locally) + a **signed release manifest** (`toolchain/release/make-release-manifest.py`: source commit → binary sha256s, Ed25519). CI gains a `repro-check` job (double-build + compare) and a tag-triggered `release` job (build → signed manifest + SBOM → attach to the GitHub release). **REMAINING:** reproducibility across toolchain minor versions; a public rebuilder. |
| 3E-5 | EuroToolchain self-hosting | M1 | 🟢 **GDB stub done + boot-verified; native std deferred** — new crate [`eurogdb`](../crates/eurogdb): a from-scratch **GDB Remote Serial Protocol** stub (packet framing/checksum, amd64 `g`-packet register layout, `?`/`g`/`G`/`m`/`M`/`p`/`P`/`c`/`s`/`qSupported` dispatch), 8 host tests. Kernel `gdbstub.rs` serves it over **COM2** against live register/memory state; `[3e5]` drives real RSP packets against a live [`KernelTarget`] (real RIP/RSP, `m`-read == direct pointer read, guarded `M` write). Attach recipe: `-serial tcp:...,server` + `target remote` (`scripts/run-gdbstub.py`). **REMAINING (large):** the native `x86_64-unknown-euroos` Rust std target + `eurolibc` + breakpoints/watchpoints/`vCont`. |
| 3E-6 | Package-manager execution | M2 | 🟢 **done + boot-verified** — `europkg::store` executes **install/remove/upgrade** on a **content-addressed store** (`/pkg/store/<hash>` two-level split for the 48-char FS name cap) driven by an **Ed25519-signed repository index** (forged index refused before any fetch), verifying each `.eupkg` (STORED-zip CRC via new `europkg::zipread` → Ed25519 over the manifest → sha256-pinned binary). `/bin/<name>` links to the CAS blob; remove refuses while a dependant needs it; `gc` reclaims orphans. 15 host tests + `[3e6]` on the live FS with the committed dev.key fixtures. Shell: `eupkg list/install/remove/upgrade`. **REMAINING:** a live repo index over HTTPS; per-package sandbox-policy enforcement at launch. |
| 3E-7 | OSS governance | Q3 | 🟢 **done** — DCO (no CLA), CoC and a full CVD process (`SECURITY.md`) already existed; this adds the two missing pieces: a **hardware-compatibility list** with an evidence-based reporting process ([`docs/HARDWARE-COMPAT.md`](HARDWARE-COMPAT.md)) and a stated **support & release policy** ([`SUPPORT-POLICY.md`](../SUPPORT-POLICY.md), release channels + support period + signed-update delivery), both cross-referenced from `CONTRIBUTING.md` and `CRA-CONFORMANCE.md`. **REMAINING:** fuller governance model (maintainer roles/voting) + trademark policy at 1.0. |
| 3E-8 | **CRA conformance** **[NEW]** | 🟢 **SBOM + CVD done; conformance is a roadmap** | From-scratch deterministic **CycloneDX 1.5 SBOM** generator [`toolchain/sbom/gen-sbom.py`](../toolchain/sbom/gen-sbom.py) (from the pinned `Cargo.lock`; 148 components, 70 first-party vs 78 upstream, reproducible hash) + a CI job that generates, **self-validates**, uploads and attaches it to releases. [`SECURITY.md`](../SECURITY.md) rewritten into a full **coordinated-disclosure + vulnerability-handling process** (reporting channels, triage SLAs, 90-day disclosure, CVE/advisory, signed-update remediation, safe harbour). New [`docs/CRA-CONFORMANCE.md`](CRA-CONFORMANCE.md): honest self-assessment mapping EuroOS to CRA Annex I Part I/II + Annex II + the **11 Sep 2026 / 11 Dec 2027** dates — explicitly **not** a Declaration of Conformity (alpha; no CE marking). **REMAINING:** dependency-advisory scanning + fuzzing in CI (3G-4), fixed support period + Annex VII technical documentation at 1.0. |
| 3E-9 | **Disk quota** **[NEW]** | 🟢 **done + boot-verified** | Per-user disk quota enforced in EuroFS on the live root FS: a per-uid block limit persisted in a reserved quota block, usage rebuilt at mount, charged/credited in the CoW write + delete path (uid 0 exempt), enforced BEFORE allocation (`FsError::QuotaExceeded`, no partial file). `chown` transfers usage and re-checks the target's limit. `[3e9]` + 6 host tests; shell `quota [set <uid> <blocks>]`. **REMAINING:** soft limits + grace period; quota on foreign-FS mounts. |

---

## Phase 3F — Apps, desktop & accessibility

| ID | Task | From | Scope |
|----|------|------|-------|
| 3F-1 | EuroContainer runtime | T | 🟢 **runtime pieces done + boot-verified; process exec remains** — the chroot+caps sandbox ([`eurosandbox::Container`]) gained the three missing runtime pieces: **`ResourceLimits`** (mem/pids/cpu/wall accounting that refuses the allocation crossing a ceiling), a **CoW `Overlay`** (read-only lower image + writable upper, copy-up on write, whiteout on delete), and a **signed `ImageManifest`** (Ed25519 verify-before-run: a tampered manifest — elevated caps, swapped rootfs — is refused). `[3f1]` proves all three on the live kernel against a committed dev.key-signed image fixture; +13 host tests (eurosandbox 4→17). **REMAINING:** actually launching a *containerised process* under the overlay+limits (bind the WASI/ELF exec path to the accounting), and a signed image *registry* (the manifest format is here; the fetch/store is not). |
| 3F-2 | EuroSuite GUIs | ES-* | 🟢 **real Office containers done + boot-verified; interactive GUIs remain** — the keystone blocker is closed: new crate [`euroflate`](../crates/euroflate) is a from-scratch `no_std` **DEFLATE/INFLATE** (RFC 1951) whose INFLATE decodes real `zlib`-level-9 streams (stored/fixed/**dynamic** Huffman) byte-for-byte and whose DEFLATE output is read back by real `zlib` (host-proven), plus zlib/gzip wrappers with Adler-32/CRC-32. On top, [`eurodocio::zip`] + [`eurodocio::docx`] give a **real ZIP+DEFLATE container**, so EuroSuite **opens a real `.docx`** (a python-`zipfile` fixture, real deflate) and **saves a `.docx` real tools read back** — proven both on the host (interop tests) and on the live kernel (`[3f2]`). **REMAINING:** interactive Writer/Calc GUIs (today static renders), `.xlsx`/`.pptx` open/save (same container, sheet/slide XML), and PDF export. |
| 3F-3 | **Accessibility (EAA-aligned)** **[NEW — broadened]** | 🟢 **core done + boot-verified** | Broadened [`euroaccess`](../crates/euroaccess) across the full EAA surface (16 host tests): **(a)** richer accessibility tree — new roles (slider/radio/tab/progressbar/dialog/panel/toolbar), states (disabled/selected/expanded/range), bounds, and an **activation API** (`activate_focused`/`adjust_focused`) with multilingual state announcements; **(b)** complete **keyboard navigation** (`keynav`: Tab/Shift-Tab, arrows, Enter/Space activate, Escape cancel, Home/End, slider adjust) returning the screen-reader speech; **(c)** a runtime **high-contrast theme** with **exact WCAG 2.x contrast math** (`theme`: sRGB-linear LUT, `contrast_ratio`/`meets_aa`/`meets_aaa`) — the high-contrast palette is *proven* to clear AAA (21:1); **(d)** follow-focus **magnification** (`magnify`: centred source-rect + nearest-neighbour scaled blit). Boot marker `[3f3]` exercises all four on real data. **REMAINING:** the deep live-compositor wiring — bridge live `Window`/`euroui::Widget`→tree (needs ids/roles/bounds on widgets), route `ps2` Tab/arrows into `keynav`, apply the theme palette through a runtime indirection in `graphics.rs`, and a live framebuffer magnifier overlay in `present_rect`; plus a11y labels for the 21 EU languages currently English-fallback. **EAA in force since 28 Jun 2025.** |
| 3F-4 | Locale completeness | P1 | 🟢 **done + boot-verified** — **month + weekday names now complete for all 24 EU languages** (CLDR-sourced via babel, previously 8/24 months and 0 weekdays), plus a Sakamoto `day_of_week` so long dates name the weekday (`eurolocale::datefmt`, +3 tests → 31). **Keyboard layouts**: new crate [`eurokeymap`](../crates/eurokeymap) — scancode-set-1 → char for **US-QWERTY, BE/FR-AZERTY, DE-QWERTZ** (letter transpositions + the AZERTY shifted-digit row), 6 host tests; the kernel PS/2 driver decodes under a **selectable active layout** (`ps2::set_layout`), the installer keymap step applies it live, and a `keymap` shell command switches it. `[loc]` (now checks all-24 names) + `[3f4]`. **REMAINING:** full AltGr/dead-key composition; more layouts (QWERTZ variants, Nordic, Baltic). |
| 3F-5 | EuroSuite ↔ OS integration | ES-Int | CoW version history in-UI (= S snapshots), document sandboxing (macros never auto-run), MIME registration, default-app settings. |
| 3F-6 | **Audio routing & desktop integration** **[NEW]** | ⬜ | Mixer, per-app routing, device hotplug, default-device policy. End-to-end half of audio (driver half is 3B-7). Relevant to the conferencing path. |
| 3F-7 | **App permission / portal model** **[NEW]** | 🟢 **core done + boot-verified; live dialog wiring remains** — new crate [`europortal`](../crates/europortal): a capability-scoped **permission broker**. An app requests a sensitive [`Resource`] (camera/mic/location/screen/files/network/…); the broker resolves it to **allow / deny / ask** (the `Decision::Ask` seam EuroGuard left open), and on consent records a **scoped grant** — `Once` (auto-revoked after one use, the JIT model generalized from agents to GUI apps), `Session` (dropped at logout), or `Persistent` (remembered on disk). Grants are **scoped to the exact detail** (a grant for one host is not the whole network); a persisted refusal stops re-prompting; every decision is audited. `[3f7]` + 10 host tests; kernel broker + a `render_dialog` (Allow once / This session / Deny) + `portal` shell command; session-scoped grants end via [`crate::session`]. **REMAINING:** insert the modal into the live compositor event loop (`render_dialog` + hit-rects are built; the last mile is showing it per-frame and routing clicks), and route real app resource-access through `portal::check`. |

---

## Phase 3G — Operations, hardening & network completeness **[NEW phase]**

| ID | Task | Status | Scope |
|----|------|--------|-------|
| 3G-1 | Crash-capture + persistent structured journal | 🟢 **done + boot-verified** | New crate [`eurojournal`](../crates/eurojournal): a **structured, severity+facility-tagged, queryable** journal (journald-equivalent) with JSON export + a bounded ring (drops-oldest, counts drops). The **panic path now persists a minidump** (vector 0xFF), not only #GP/#PF/#DF. Marker `[3g1]` (3 host tests). REMAINING: persist the journal ring to disk + a ring of crash slots (history, not one LBA). |
| 3G-2 | Watchdog timer | 🟢 **core done + boot-verified** | New crate [`eurowatchdog`](../crates/eurowatchdog): a **deadman** liveness deadline — pet before it expires or it trips. `[3g2]`: alive-within-grace, trips-on-hang, recovers-after-pet (3 host tests). REMAINING: wire pet into the 100 Hz scheduler tick + a real reset (or QEMU i6300esb) on trip. |
| 3G-3 | Kernel-hardening baseline | 🟢 **done + boot-verified** | `[3g3]` reads the live CPU posture: **CR0.WP, CR4.SMEP, CR4.SMAP, EFER.NX all on**, plus W^X-per-page (build_address_space) + the sched stack canary — the CRA secure-by-design evidence, now boot-checked and cross-referenced from [`CRA-CONFORMANCE.md`](CRA-CONFORMANCE.md). REMAINING: KASLR of the kernel image + microcode-load + Spectre-class MSR mitigations. |
| 3G-4 | Security-fuzzing CI + sanitizers | ✅ **done** | New crate [`eurofuzz`](../crates/eurofuzz): a deterministic (seeded, replayable) fuzz harness feeding **200k inputs/parser** into every untrusted-input parser (policy bundles, certs, CA store, DHCP/DHCPv6/DNS, TPM responses, base64/JSON/SD-JWT) — proving none panic. **It already found + fixed a real panic** (empty SD-JWT presentation → `segs[1..]` out-of-bounds). A CI `fuzz` job runs it. REMAINING: sanitizer (ASan/MSan) builds + a persistent corpus. |
| 3G-5 | IPv6 completeness | 🟢 **core done + boot-verified** | New `euronet::dhcpv6` (RFC 8415): Solicit/Advertise/Request/Reply + IA_NA address option build/parse. `[3g5]`: solicit carries client-DUID+IA_NA, request confirms an assigned 2001:db8::5 (3 host tests). Complements the existing SLAAC/NDP/ICMPv6. REMAINING: the live DHCPv6 lease loop + a stateful IPv6 address/neighbour table + AAAA in the resolver + dual-stack routing. |
| 3G-6 | mDNS / zeroconf | 🟢 **core done + boot-verified** | New crate [`euromdns`](../crates/euromdns) (RFC 6762/6763): `.local` detection, mDNS query/response, and a **Responder** that answers A/AAAA for its own name and **DNS-SD** PTR/SRV/TXT for advertised services (and stays silent for others). `[3g6]` (4 host tests). REMAINING: the live 5353 UDP-multicast socket + a discovery cache. |
| 3G-7 | Sovereign DNS resolver | 🟢 **core done + boot-verified** | New crate [`eurodns`](../crates/eurodns): a DNS message model (A/AAAA) + **DNSSEC validation** — RRSIG verify for **Ed25519 (alg 15, RFC 8080)** over the RFC 4034 canonical form + DS (SHA-256) chain link. `[3g7]`: a valid RRSIG verifies, a spoofed record and a wrong key are rejected (5 host tests). REMAINING: **DoT/DoH** transport (reuse `eurotls`), RSA/ECDSA DNSSEC algorithms, and full chain-to-root-anchor validation. |

---

## Suggested order

1. **3A** storage writes + partition/USB harness + SSD durability (3A-7)
2. **3C** quick wins (block-cache live FS, swap auto-evict, symlinks + coreutils)
3. **3D fundamentals first** — NTS (3D-7) + early-boot entropy (3D-8), *then* TPM sealing → FDE/Vault-to-PCR + verity. PQ crypto (3D-9) folds in alongside update-signing.
4. **Compliance-driven 2026 track (parallel, deadline-pressured):**
   - SBOM + CVD in CI (3E-8) — must land before **11 Sep 2026**
   - eIDAS/EUDI into EuroID (3D-10) — target the **Dec 2026** wallet window
   - EAA accessibility (3F-3) — already legally in force
5. **3E** installer GUI + update server + governance + quota
6. **3G** ops & hardening (crash-capture, watchdog, kernel-hardening, fuzzing CI) + network completeness (IPv6/mDNS/DNS) — partly VM-testable, run in parallel
7. **3B** bare-metal batch (modern virtio/GPU first; then audio driver, BT, power/thermal, multi-monitor)
8. **3F** EuroSuite GUIs + audio routing + container runtime + portals (long parallel track)

**Deferred:** hypervisor, distributed storage, native backup, AI runtime, sovereign-cloud.

> **Backup note:** given the GDPR/compliance positioning, a sovereign backup/restore +
> key-escrow story (currently Deferred as "native backup") may need to be pulled forward
> before a real customer deployment — flag for re-scoping rather than leaving fully parked.

---

*Revised 2026-06-14. Companion to `NEXT-SPRINTS.md` (per-item detail + done-work log),
`docs/SPRINT-PLAN-INTEROP.md` (storage), `docs/PHASE2-PLAN.md` (K–Z ordering), and
`docs/EUROSUITE-PLAN.md` / `docs/EUROCOREUTILS-PLAN.md` (product tracks).*
