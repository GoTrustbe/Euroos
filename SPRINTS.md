# EuroOS — Sprint Task Board

*Pick a sprint/task ID and tell me to work on it. Details: [IMPLEMENTATION-PLAN.md](IMPLEMENTATION-PLAN.md) · What's built: [STATUS.md](STATUS.md).*

**Now:** 162 host tests green · clippy clean · boots to desktop, 0 faults · HTTPS validated end-to-end · multi-disk + NVMe verified.
**Legend:** ✅ done · 🟡 core done (remainder noted) · 🔒 attended/large remainder.

After a long marathon, **every sprint A–F has been advanced**; the cores are built + verified, with large integrations remaining where noted.

---

## Sprint A — Security Foundation
| ID | Task | Status |
|----|------|--------|
| A1 | X.509 certificate-chain validation | ✅ full DER parser + 6 sig schemes + chain + 30-root EU store + CertificateVerify; MITM cert refused, verified vs live HTTPS |
| A2 | Kernel-stack guard pages | 🟡 mechanism done (shared high region + guarded stack + #PF `KERNEL STACK OVERFLOW`); ⬜ migrate all kernel stacks to guarded ones |
| A3 | Randomised DNS source port | ✅ RDRAND-seeded txid + ephemeral port |

## Sprint B — Storage Depth ✅
| ID | Task | Status |
|----|------|--------|
| B1 | EuroFS data-path scrub & repair | ✅ per-file XXH3, loud reads, unrecoverable reporting, mirror interface |
| B2 | NVMe driver + SMART | ✅ PCI→init→Identify→I/O queues→read/write (PRP)→SMART; verified on `-device nvme`. ⬜ EuroFS-on-NVMe mount |
| B3 | Multiple disks & mountpoints | ✅ multi-device virtio-blk + 2nd EuroFS mount + `df`; verified on 2-disk harness. ⬜ full VFS path-routing |

## Sprint C — Network Depth ✅
| ID | Task | Status |
|----|------|--------|
| C1 | TCP retransmit / congestion / keepalive | ✅ RFC 6298 RTO + Reno (host-tested) + SYN & data retransmission. ⬜ cwnd-pacing/keepalive (need long-lived conns) |
| C2 | POSIX server sockets (listen/accept) | ✅ bind/listen/accept + passive open + service routing + `tcpserve`. ⬜ non-fragile e2e test, select/poll |
| C3 | ICMP errors & rate-limit | ✅ port-unreachable/echo-reply/RST/fast-fail + token-bucket rate limit |

## Sprint D — SMP Scalability
| ID | Task | Status |
|----|------|--------|
| D1 | Finer-grained kernel locking | 🟡 D1a profiling done (`syscallprofile`). ⬜🔒 D1b/c per-subsystem spinlocks + lock-free hot paths (concurrency refactor) |

## Sprint E — Compatibility / Display
| ID | Task | Status |
|----|------|--------|
| E1 | glibc-compat syscalls | ✅ ~50 syscalls (mmap/brk/mprotect/…); static musl runs. ⬜ ld.so dynamic linker (large) |
| E2 | Display server API | 🟡 protocol + surface model done (`eurodisplay`, host-tested). ⬜🔒 Unix-domain-socket transport + live compositor wiring |

## Sprint F — Platform Maturity
| ID | Task | Status |
|----|------|--------|
| F1 | EuroUpdate: atomic A/B slots | ✅ anti-brick state machine + boot integration + Ed25519-signed `apply`. ⬜ real A/B GPT partitions + loader slot-switch |
| F2 | Container / WASM sandbox | ✅ EuroGuard-native container (caps-shrink + chroot-safe paths), host-tested + boot-verified. ⬜🔒 WASM interpreter |

---

## What's genuinely left (all large / attended)
- **A2** migrate every kernel stack onto guarded stacks + destructive overflow test.
- **D1b/c** per-subsystem locking refactor (concurrency correctness).
- **F2** WASM/WASI interpreter · **E1** ld.so dynamic linker · **E2** Unix-domain sockets + live wiring.
- Each is a focused multi-component build; the host-tested cores + kernel hooks are in place.

*Tell me e.g. "do A2 migration" / "D1b locking" / "WASM for F2" and I'll run it with the usual tests + boot-verify + docs.*
