# Isolation levels on EuroOS, named honestly

"An agent cannot be its own security authority. It must run within a boundary
defined by the developer or organization and enforced independently of the
agent itself." (Microsoft, Windows Developer Blog, 7 October 2026, introducing
Microsoft Execution Containers.) That sentence is the EuroGuard principle;
EuroOS enforces it in the kernel's syscall path, and names three levels. There
is no fourth: EuroOS has no hypervisor, so no microVM level, and says so.

| Level | What draws the boundary | Proof (boot self-test) |
|---|---|---|
| Process | Syscall profile per task (utility, renderer, euro), seccomp strict and cBPF filters, rlimits, ASLR, uid drop; the per-app policy from the outside (`/etc/euroapps/<app>.policy.toml`: paths read/write/deny, domains) | [sbx], [sbx2], [app-policy] |
| Session | A separate uid and EuroID session per user; a process dropped to `nobody`; files scoped by owner | [sbx], [ae], [ag-lock] |
| Container | EuroContainer: a signed image (tampering refused), constrained capabilities, an enforced memory limit, overlay with copy-up and whiteout | [3f1] |

Agents add their own boundary on top of the process level: a declarative
manifest (required and optional capabilities, tools allowed and denied,
network domains, memory and runtime limits), derived to an effective set that
can never exceed what the user has, with elevated capabilities only through a
JIT grant that lasts one action, every tool call audited, and the audit trail
feeding anomaly detection. The person approves the boundary: learning mode
proposes it, `approve` installs it, and only the owner can ([policy-learn],
[aa], [af-jit], [af-anom]).

Drivers get a boundary of their own, so far for two drivers. The e1000 NIC
driver is a ring-3 process (`e1000d`) under the `driver` syscall profile, and
that is the default (`net.driver=kernel` brings back the in-kernel driver).
It holds a device grant and nothing else: the pages of one BAR, a DMA window,
and its own EuroIPC ports. Under VT-d the device itself is confined too: its
IOMMU domain holds exactly the DMA window, so a driver that points the device
at kernel memory gets a fault record, not a corrupted kernel. A driver crash
is a process exit; the kernel quiesces the device and restarts the driver
([udrv-grant], [udrv-iommu], [udrv-dma], [udrv-crash]). Processes reach the
network through it: a blocking socket call gives the CPU away and wakes when
the driver delivers, and a background process needs the network capability
in its own capability set to get a socket at all ([net-late], [udrv-linux],
[bgnet-cap]). The NVMe driver (`nvmed`) follows the same model for a data
disk, opt-in with `storage.nvme=user`, never for the root: its grant leaves
out the MSI-X table, its DMA is confined the same way, and a crash in the
middle of a write is an I/O error after which the filesystem mounts
consistent ([udrv-blk-*]). QEMU only so far, see
docs/SPRINT-PLAN-USERSPACE-DRIVERS.md and docs/SPRINT-PLAN-USERSPACE-STORAGE.md.
Every other driver still runs in the kernel, and without an IOMMU device DMA
is not confined.

What EuroOS does not claim: hardware-enforced isolation below the kernel (the
IOMMU confines device DMA, it does not isolate software), fleet
management of these policies, and an on-device model good enough to act as an
assistant (the engine runs, see docs/LLM-LOCAL.md; a usable model is a
hardware question).
