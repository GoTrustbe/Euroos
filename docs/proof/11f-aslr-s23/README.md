# ASLR steps S2 and S3 (docs/design/ASLR.md), behaviour-neutral preparation

Branch `feature/11f-aslr-s23` from `feature/11f-integ` `00a6459`, merged `feature/11f-aslr-design` (`4200fad`, the
design). Worktree `/opt/euroos/wt-11f-aslr23` on .16. Not pushed. R630 run directory `/srv/euroos/aslr23-runs`,
CPUs 18-19 and their siblings 38-39 only.

| commit | step |
|---|---|
| `c7aa31f` | S2: arena functions and page-fault dispatch for any PML4 slot, self-test `[arena-hi]` |
| `215534d` | S3: `load_elf64` and `setup_user_stack_glibc` with a virtual and a kernel-side address, self-test `[elf-bias]` |

What changed and what the design got wrong: `docs/design/ASLR.md` section 7. No process gets another address: nothing
is placed in the arena zone, every caller passes one number twice (`addresses-unchanged.txt`: the same load bases and
the same `[aslr]` bits as `00a6459`).

## Tests first (`tests-first.txt`)

| image | `[arena-hi]` | `[elf-bias]` |
|---|---|---|
| tests added, code unchanged (`ff`) | FAILED: "the arena pages are not lazy entries above PML4[0]" | FAILED (first version) |
| S2 + loader/stack again with one number (`ff2`) | OK | FAILED: "the program headers are not at the kernel side of the PHDR address" |
| S2 + S3 (`fin`, `215534d`) | OK | OK |

The first `[elf-bias]` loaded ld.so only; with S3 done it still failed ("no R_X86_64_RELATIVE relocation to check"):
ld.so's relative relocations are packed `DT_RELR`, the kernel applies none of them. The test now loads `seq`
(10 relocations) and ld.so; `ff2` shows that version failing on the one-number loader.

## Results (`215534d`)

| check | result | file |
|---|---|---|
| cargo test (default-members) | 188 suites, 1381 passed, 0 failed (as `00a6459`) | `cargo-test-summary.txt` |
| KVM self-test boots 1/2/4/8 GiB, 2 vCPU, HD-Audio | 4/4 to the end, 0 crash lines, FAILED lines 3/1/1/1 as `00a6459`; markers-compare **0 worse** at all four sizes, new: `[arena-hi]` OK, `[elf-bias]` OK | `kvm-markers-compare.txt` |
| 30 TCG boots combined-smp1-iommu-1g, restrict=on | **30/30 desktop**, 0 watchdog, 0 NMI, 0 crash logs, `[arena-hi]` and `[elf-bias]` OK 30/30, 0 `[fuzz]`/`[churn*]` FAILED; markers-compare against a TCG boot of `00a6459` (same script): 0 worse in 30/30 | `tcg30.txt` |
| gui-smoke, public image, KVM 4 vCPU 4 GiB | **67/67** | `gui-smoke.txt` |
| Chromium 10 starts, smp 2 | **10/10**, 0 FROZEN | `chrome-starts10.txt` |
| sites, 3 rounds, smp 2 | **36/36** (6 sites x 3 rounds, consent banners answered), 0 aborts, 0 oom, 0 ring-0 faults | `chrome-sites.txt` |

All runs on 4 logical CPUs shared between guests (no timing claims). The FAILED lines of the TCG boots (`[udrv-net]`,
`[udrv-crash]`, `[udrv-linux]`: no DNS behind restrict=on; GBIG at 1 GiB; `[aslr]`) are the same in the `00a6459` boot.
gui-smoke on the browser image (`ui.tour=off`) gives 66/66: the first-run tour step only exists with the tour on.

## New bugs

None (BUG-350..359 not used).

## Not done

S0, S1, S4 and later (not approved). What S4 needs: design section 7, "For S4".
