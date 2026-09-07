# Night sprint: close the frame gap, and harden what the hunt exposed

Written 2026-09-01, to be worked through unattended. Two tracks run in parallel:
track A is the open research question, track B is work with guaranteed value so
the night pays off even if A stalls. Real hardware (NUC, KVM) makes a full
multi-process chrome run cost about ninety seconds, and the reference oracle on
the same machine answers a hypothesis in seconds.

## Discipline (the rules that keep this from wasting hours)

1. Every chrome run carries ONE hypothesis and ONE decisive measurement.
2. Test in the oracle FIRST whenever the question can be asked there: seconds
   instead of minutes, and it separates "our kernel" from "chrome behaves so".
3. Kill a run the moment it shows a known failure signature. Never wait out a
   run that has already answered.
4. Three consecutive dead hypotheses on one theory: stop, switch track, write
   down what was excluded.
5. While a run executes, do track B. Never sit idle waiting.
6. Anything found gets a test or a measurement, not a claim.

## Track A: the threaded-compositor completion gap

The single measured difference: with threaded compositing native completes a
driven frame and this kernel does not. Everything else on that path is healthy
(submit+ack, copy results, completion eventfd written and consumed, Mojo round
trip returns, no lost futex wakes).

- A1. Differential syscall trace. Capture the oracle's syscalls in the beginFrame
  window with threaded compositing (it succeeds) and EuroOS's systrace in the
  same window (it hangs). Diff the sequences; the first divergence is the lead.
- A2. Whatever A1 names: form a hypothesis, ask the oracle if possible, then one
  EuroOS run to confirm.
- A3. If A1 is inconclusive: bisect the thread that stalls. The completion in
  threaded mode crosses the renderer's compositor thread; dump what each named
  chrome thread waits on at the moment the frame is pending (safe path only:
  chrome's own trace and the systrace, never kernel spinlocks from the pump).
- A4. Fallback with real value: if the gap resists, characterise it precisely
  enough that the next session starts from a named function rather than a symptom.

## Track B: harden what the hunt exposed

- B1. Clippy: the only red CI check that is ours to fix. Work through the
  warnings, keep behaviour identical, verify with the full test suite.
- B2. Syscall-surface audit against the oracle. The oracle's strace lists every
  syscall real chrome uses; compare against what this kernel implements and fix
  the gaps that matter (CLOEXEC came out of exactly this kind of comparison).
- B3. Re-validate on hardware after each batch: boot self-tests green, then the
  multi-process session still clean.
- B4. Publish refreshed images if anything user-visible improved.

## Done means

- Every finding committed with its measurement in the message.
- Host tests green (1008) at each commit point.
- Memory updated so the next session starts where this one ends.

## Results (worked through the same night)

### Track A: the gap is now a named divergence, not a symptom
Differential syscall trace against the oracle in the same threaded configuration.
Both runs match step for step: the compositor finishes, wakes the main thread over
its self-pipe, the futex wakes land, the Mojo round trip returns, and both run the
same mprotect block. There they part. The oracle's main thread writes eight bytes
to the completion eventfd and the DevTools thread answers; ours runs the same
mprotects and returns to polling, and the DevTools writer is never woken once.
So every syscall-level mechanism is healthy and what is missing is chrome's own
decision that the frame is finished. A syscall diff cannot see further; the next
attempt starts there rather than at a symptom.

Also established on the way: driven frames DO complete here with
--disable-threaded-compositing (and answer hasDamage=true once the page is dirtied
by a resize rather than a style change), but that mode cannot produce a picture on
ANY kernel - the oracle behaves identically in it. So the threaded compositor is
the only real target.

### Track B: banked
- B1. The red Clippy CI check is green: 'cargo clippy -p eurofs -p euronet --
  -D warnings' passes clean. Workspace warnings 43 to 13, two answered with a
  justification rather than a change (the JPEG cosine table keeps its literals,
  BLAKE2b keeps its indices).
- B2. Syscall audit against the oracle: ppoll, chdir, getpriority and
  sched_setaffinity implemented; pkeys, landlock and inotify correctly still
  refused. Honest finding: ppoll was a glibc-version artefact, not a gap.
- B3. Re-validated on hardware: 161 boot self-tests green under KVM, zero
  failures, zero panics.
- B4. Published: lean image sanity clean, download page and live-VNC base
  refreshed (2026.09.01).

1008 host tests green at every commit point.
