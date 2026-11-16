# D2, step B: BUG-231 (inbound HTTP hammer 11 times slower after S1/S2)

## Where the time went

Short reproductions on the R630 (KVM, 4 vCPU, e1000e, `--hammer 4`), the images of the sched-d1 proof:

| Run | Image | Desktop loop | Hammer p50 | Requests |
|---|---|---|---|---|
| `h-s0` / `h-s1` / `h-d1` | S0 / S1 / S2+S8, self-test | the soak scenario, 600 s | **5.0 / 60.5 / 61.7 ms** | 72 422 / 34 689 / 34 366 |
| `i-s0` / `i-s1` / `i-d1` | public | idle (`sleepwait 60`) | 4.2 / 4.1 / 7.4 ms | |
| `b-s0st` / `b-d1st` | S0 / S2+S8, self-test | 15 x (`spawn worker 32`, `churn proc 8`) | **5.6 / 78.4 ms** | 8 659 in 80 s / 1 238 in 29 s |

S1 alone makes the difference, and only with a busy desktop. `sched-stat` over the busy window names the path: the
inbound frames reach the netstack through **e1000d** (`net.driver=user`, task 27, a ring-3 driver at nice 0) and the
**net worker** (task 22, nice -5). Under turn-based vruntime (S0) a task that slept when it left the CPU was never
charged, so the driver, which sleeps after every pass, always ran first when its interrupt woke it. S1 charges real run
time: at nice 0 the woken driver now sits behind the desktop loop (nice -10, placed at the wake floor at every switch)
and the five nice-0 counter tasks of the self-test image until the next tick, at every frame of every connection.

## Variants (same image `x1`, a `renice` command and a wake-margin switch, `hammer-runs.txt`)

| Variant | Hammer p50 | Requests | `churn proc 8` |
|---|---|---|---|
| unchanged (`x-base`) | 79.5 ms | 1 287 in 31 s | 1.4 s |
| wake margin scaled by the woken task's weight (Linux) | 70.0 ms | 1 504 | 1.4 s |
| net worker and e1000d at nice -5, margin scaled | 29.3 ms | 4 006 | 1.65 s |
| both at nice -10 | 13.4 ms | 12 454 in 50 s | 2.6 s |
| both at nice -15 | 7.8 ms | 38 980 in 99 s | 5.4 s |
| **e1000d at nice -15, worker unchanged at -5** (`x-drv`) | **4.7 ms** | 24 353 in 54 s | 2.9 s |

The weight of the woken task's wake margin changes little; the driver's weight is what counts. With only the driver at
-15 the worker batches more frames per pass (299 switches/s against 1 360 at -10) and the latency is below S0's.

## Fix and result

Every userspace driver (`Profile::Driver`: e1000d, nvmed, megaraidd, bcm5720d) runs at nice -15
(`sched::DRIVER_NICE`). `renice TASK [NICE]` stays as a diagnostic command; the wake-margin switch is not kept.

| Measurement (`b1`, the fix) | Before (`x-base` / sched-d1) | After |
|---|---|---|
| hammer p50 under 15 rounds of churn (`b1-hb`) | 79.5 ms | **4.7 ms** (S0: 5.6 ms), 27 040 requests in 57 s, p99 49.8 ms |
| `churn proc 8` under the hammer | 1.4 s | 2.7-3.4 s (the network is served now: e1000d 45 % of the CPU) |
| `[fair]` 30 rounds smp 4 / smp 1 (`d1-criteria.txt`) | worst 1.6 s | **worst 1.5 s / 1.6 s** (limit 12.6 s); share 43-44 % as in sched-d1 on the self-test image |
| `[wakelat]` smp 4 / smp 1 | p50 0.05 ms | **p50 0.05 ms**, p99 0.07 / 0.10 ms, hogs keep 89 % |

The 2 h soak of the final commit has the whole-soak numbers (`docs/proof/d2/README.md`).
