# BUG-270 fix: the shutdown lines reach the serial port again

Base: `06e913e` (validate-2026-11e). Self-test images (`scripts/build.sh release`), KVM on the R630, pinned to CPUs 0-3 + 20-23.

Cause: since BUG-251 (`0cf75e6`) prints wait in a deferred queue for the sender task once the desktop loop runs. `power::shutdown()` and
`power::reboot()` wrote the ACPI/reset registers without `serial::emergency()`, so the guest was off before the sender had a turn.
Fix: `shutdown()` and `reboot()` call `serial::emergency()` first (drain + synchronous prints from then on) and again right before the
register writes; `emergency()` now retries the UART lock (bounded, 200 000 spins) instead of losing the queue when another cpu is appending.
All callers go through these two functions (ACPI power button SCI handler, shell `shutdown`/`poweroff`/`reboot`, installer, update, lockbench).
Panic (`main.rs`) and the fatal fault handlers (`interrupts.rs`) already called `emergency()`; there is no other register-write reset path in the kernel (grep of 0x604/0xB004/0xCF9/0x64 outside power.rs: none). A triple fault cannot print by definition.

| Check | Before (`06e913e`) | After |
|---|---|---|
| `power` leg alone, KVM, restrict=on (`power-leg.txt`) | FAIL 3/3 (no lines) | PASS 5/5 (both lines in the serial log) |
| q35 matrix, 13 legs, KVM, restrict=on (`q35-matrix.txt`; tpm/tpmcrb not run, no swtpm) | 12/13 (validate-2026-11e) | 13/13 |
| `cargo test` (`cargo-test-summary.txt`) | 188 binaries, 1381 passed | 188 binaries, 1381 passed, 0 failed, 1 ignored |
| KVM self-test boot 1 GiB smp 4, markers-compare (`kb/compare.txt`) | - | 0 worse, 0 changed (both: 3 known FAILED lines, 0 crash lines) |

BUG-251 benefit kept: the deferred queue is unchanged in normal operation; `emergency()` is only reached on the way out.
Drivers: `drivers/pw.sh`, `drivers/kboot-hda.sh`.
