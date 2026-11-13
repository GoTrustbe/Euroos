# Chromium on EuroOS, W23 follow-up: sound, the link click, more tasks (2026-11-13)

Branch `feature/chrome-w23` (after `eacc361`, with `feature/chrome-harness` `6ac5e85` merged in at `acc46e2`).
Not merged, not pushed. Bugs: `../../LOADTEST-TRACKER.md` (BUG-160, BUG-162, BUG-147 closed; BUG-170, BUG-171 new).

| Commit | What |
|---|---|
| `acc46e2` | merge of `feature/chrome-harness` (its scripts: `--only localvideo`, `mk-test-video.sh`, consent banners) |
| `83d87de` | test first: `[syncsock]`, the audio stream between two processes. FAILS on the tree before |
| `d4d06ed` | BUG-170 (O_NONBLOCK of a closed socket), BUG-171 (a blocking socket read that spun): BUG-160 |
| `e5990d5` | test first: `[many-threads]`, 320 threads alive at once. FAILS on the tree before |
| `95a3bf2` | BUG-147: 512 task slots, thread kernel stacks past 224 from the heap |
| (last) | this proof, the tracker |

Images: `w23g-pub` = `7b0629c` (before); `s3b`/`s3` = `83d87de` / `+ BUG-170`; `s4` = `d4d06ed`; `s5` = `d4d06ed` + BUG-147;
`m0`/`m1` = `e5990d5` / `95a3bf2`. R630, KVM, 2 vCPU, 6 GiB for the browser, the clips of the harness team (VP9+Opus,
VP9 alone, Opus alone, H.264+AAC; `scripts/mk-test-video.sh`) served from the host to `http://10.0.2.2:PORT/`,
`-device intel-hda -device hda-duplex` with `-audiodev wav`.

## 1. BUG-160: no sound, clips with audio stuck at 0.00 (fixed)

**What the kernel saw** (the audio syscall trace of the harness run lv2): on the audio service's end of a fresh
socketpair, `fcntl(F_GETFL) = 0x802` (O_NONBLOCK already set) and the first 4-byte write `= -32` (EPIPE); the renderer's
audio thread never made a call of its own. In the renderer the blocking `Receive` of that socket answered EAGAIN at once,
its audio device gave up and closed its end.

**Cause (BUG-170).** O_NONBLOCK is kept per object number and nothing removed it when the object was closed;
`socketpair()` set it only when asked for. Object numbers are handed out again, so the audio sync socket was born
non-blocking where a Mojo socket had been. **Fix:** the flag goes with the last close of an object; `socketpair()` sets
it either way.

**BUG-171, found with it.** With BUG-170 fixed the renderer's audio thread blocked for real, the first program to do so on
a socket, and the wait was a loop of yields that never charged the waiter: the machine stopped answering a few seconds
into the first clip (run v1: every NMI in `unix_recv_blocking` / `yield_switch` of `AudioOutputDevice`; its wav already
had 109 seconds of sound). **Fix:** two yields, then a tick of sleep per look, as poll does.

**Test first** (`syncsock.txt`, KVM 4 GiB): `[syncsock]` (`gsyncsock.c`: a socketpair and a memfd made in the parent,
one end and the region handed to a fork child with SCM_RIGHTS, the parent's copies closed, 4-byte signals both ways,
data through the region; a closed non-blocking pair before it). Before (`s3b`): **9 of 17 FAILED** ("a new socketpair is
blocking" 0x802, the child's blocking recvmsg EAGAIN, no signal answered). After (`s3`): **17 of 17 OK**. Linux 6.x 17/17.

**In the browser** (`localvideo.txt`, `audio-wav.txt`):

| Run | Image | VP9+Opus | Opus alone | VP9 alone | QEMU wav, seconds with sound |
|---|---|---|---|---|---|
| v0 | before (`w23g-pub`) | t 0.0 -> 0.0, error 3, RMS 0 | t 0.0, error 3, RMS 0 | 3.2 -> 41.2 s | 1 of 234 |
| v1 | BUG-170 only (`s2-pub`) | sound, then the desktop stopped answering (BUG-171) | — | — | 109 of 199 |
| v2 | `d4d06ed` (`s4-pub`) | **2.2 -> 61.2 s, 12/12 windows RMS 1158**, no error | 2.9 -> 61.4 s, 12/12 | 2.8 -> 62.2 s | 140 of 320 |
| v3 | `s5-pub` | 1.9 -> 60.4 s, 12/12 | 3.1 -> 61.5 s, 12/12 | ok | 139 of 312 |
| v4 | `95a3bf2` (`m1-pub`) | **2.2 -> 60.7 s, 12/12 windows RMS 1158**, no error | 2.8 -> 61.3 s, 12/12 | ok | — |

H.264+AAC still fails (`error 4`, `canPlayType` empty): BUG-161, this Chromium build has no such codecs (not kernel).

## 2. BUG-162: a link click not followed (not a EuroOS input bug)

10 fresh boots × 3 clicks on euro-os.eu's Download link at the coordinate of `chrome-smoke --only interact`
(`drivers/linkclick.py`, `link-clicks.txt`): **27 followed in 0.7-1.1 s, 1 not**, and the 2 after it in that boot
clicked on the page the test had gone back to. Every click reached the X client (press and release). The one that was not
followed came while euro-os.eu was still loading: the screendump before the click shows the previous page under the new
tab title with the stop button (`screenshots/bug162-click-came-before-the-page-painted.png`); 15 s later the page is there
with the pointer on its Download button (`screenshots/bug162-same-page-15s-later.png`). The harness waits for 1.5 s of
unchanged picture, which a page that has not painted yet also gives. Harness: wait for the load to finish
(`wait_loaded`) before clicking a link. (These boots did not answer the site's cookie notice: it covers no part of the
header link; the harness's consent handling does it in its own runs.)

## 3. BUG-147: 512 task slots (fixed)

`[many-threads]` (`gmanythreads.c`, 320 threads of one process alive at once): before (`m0`) **223 of 320, pthread_create:
Resource temporarily unavailable → FAILED**; after (`m1`) **320 of 320 → OK**, thread kernel stacks back to 0. MAX_TASKS
512 (+4 MiB static: one 16 KiB kernel-task stack per slot); thread kernel stacks 224 static, the rest from the heap on
first use. The minimal session of five rounds (a3, `s5-pub`): 28/28 steps, 210 live tasks of 512, 0 held by ended tasks.

## 4. Regression

| Check | Result |
|---|---|
| `cargo test` on the host, `d4d06ed` | 187 binaries, **1 350 passed**, 0 failed; `eurofdspace` again on `95a3bf2` (MAX_SPACES 512): 20/20 |
| KVM self-test boot, `s5` against `s4` (BUG-147 alone) | 0 markers worse; `[churn-forkthreads]`, `[syncsock]` OK in both |
| TCG matrix of 10, `s5`, against `w23g` under the same conditions (`comparison-tcg.txt`) | 10/10 to the desktop, 0 watchdog trips, longest gap 1.7-2.3 s; 0 worse in 6; in the four 1 GiB configurations `fork FAILx9 → FAILx10`: the fork of the new `[syncsock]` that 1 GiB refuses (its verdict "fork refused: not run", OK) |
| `gui-smoke.py`, plain public image of `95a3bf2` (`restrict=on`, 4 GiB) | **PASS 67/67** |
| site matrix, `95a3bf2` (`m1-pub`), 3 rounds in one session (`site-matrix.txt`) | **PASS 36/36**, 0 aborts, at most 193 live tasks of 512 |

## 5. Not done

Key to screen (out of scope); BUG-161 (codecs of the build); the harness changes of BUG-145 and BUG-162.

## Deviations

- The local clips were copied from the harness team's run directory (`/srv/euroos/harness-runs/vid`, read only); the R630
  has no ffmpeg and nothing may be installed there.
- Test drivers outside the tree (`/srv/euroos/w23-runs/drivers/linkclick.py`); the harness is used as merged.
- Merge conflict in `LOADTEST-TRACKER.md` (both sides added entries after BUG-147): both kept.

## Files

`syncsock.txt` · `localvideo.txt` · `audio-wav.txt` · `link-clicks.txt` · `many-threads.txt` · `comparison-tcg.txt` ·
`gui-smoke-public.txt` · `site-matrix.txt` · `screenshots/`. Serial logs and wavs stay on the R630 (`/srv/euroos/w23-runs/`).
