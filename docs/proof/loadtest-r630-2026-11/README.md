# LOADTEST-R630 (2026-11-02/03): summary logs

Report: [`../../LOADTEST-R630-2026-11.md`](../../LOADTEST-R630-2026-11.md). Bug entries:
[`../../LOADTEST-TRACKER.md`](../../LOADTEST-TRACKER.md) (BUG-012 … BUG-026).
Dell R630 (2 × E5-2630 v4), QEMU 10.2.1 with KVM (`-cpu host`), OVMF 2025.11. The full serial
logs (tens of MB) stayed on the test server and were deleted with the VM images; what is here
are the harness CSVs and the lines quoted in the report.

| File | What |
|---|---|
| `lockbench-kvm.csv` | every `[lk-bench]` line of every lockbench boot: config (`smpN`, `node0`, `2nodes`, `-rerun`, `node0-2`, `node0-4`), run, workload, J1/GLOBAL, 1-core and N-core ops/s, scaling, host load |
| `procs-before-fixes-ltstat.csv` | `ltstat` samples, procs scenario on 813e627 + hooks: tasks to 256, live processes that never run |
| `procs-after-fixes-ltstat.csv`, `procs-after-fixes-results.csv` | the same scenario with all fixes (3 waves, 300 burst, 32 pthread processes) |
| `churn-scale8-verdicts.txt` | every `[churn-*]` verdict of the churn scenario at scale 8 (`-m 16G`) |
| `fsio-{virtio,nvme,ahci}.csv`, `persist-*.csv` | fsio per command (status, ms) on an 8 GiB disk and the check after the reboot |
| `net-{kernel,user,user-iommu}.csv` | net scenario per command; `net-kernel-before-branch-fixes.csv` on an image without BUG-018 |
| `net-400-fetches.txt` | the 4 slow or failed out of 400 sequential connections (BUG-026) |
| `accept-kernel.txt` | inbound accept test after BUG-018 |
| `selftest-markers.txt` | full selftest boot `-smp 4`: 813e627 vs this branch, OK kinds and FAILED lines |
| `soak*.csv`, `soak-summary.txt` | the soak: every 10th `ltstat` sample, per-command latency summary |
| `excerpts.txt` | the serial lines behind each finding, before and after its fix |
