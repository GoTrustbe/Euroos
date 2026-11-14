# Chromium on EuroOS: audio with more than two channels (BUG-181), 2026-11-14

Branch `feature/chrome-audio` from `fd72e2c` (`feature/chrome-integ`), worktree `/opt/euroos/wt-audio`. Not pushed.
The memory and scheduler code is not touched (only `alsa.rs`, the new self-test in `churn.rs`/`main.rs` and the
registration of `gpcm` in `ring3.rs`).

| Commit | What |
|---|---|
| `1a50fba` | test first: `[pcmch]` (`userland/glibc/gpcm.c`), the ALSA device with 6, 1 and 2 channels. FAILS on the tree before |
| `bf92f3c` | BUG-181: the device takes 1-8 channels and downmixes them to the stereo ring |
| (last) | this proof, the tracker (BUG-181 fixed, BUG-182 new) |

R630, KVM, CPUs 34-39, run directory `/srv/euroos/integ3-runs` (`audio/`, `runs/awm1`, `runs/av1`, `runs/av2`, `runs/ags1`).

## Choice

Linux has two ways: a `default`/`plughw` PCM in libasound that converts, or a device that takes the count (an HDA codec
with a channel map). The plug plugin over this device never settled its rate/format refine (runs 78-83: that is why the
kernel starts Chromium on `hw:0,0`), and Chromium does not downmix a named device. So the device does it: `hw_params`
offers CHANNELS 1..8 (bounded by the client's CHANNELS, FRAME_BITS, PERIOD_BYTES and BUFFER_BYTES; frame bits and byte
sizes follow the count; HW_PARAMS with a range left takes stereo), and `writei` downmixes each frame into the stereo ring
(ALSA default channel order per count; fronts 1, centre, rears and sides 0.7071, rear centre 0.5, LFE dropped; each side
divided by the sum of its weights so nothing clips). Ring, DMA and pacing are unchanged; stereo is copied as before.

## Results

| Check | Result |
|---|---|
| `[pcmch]` (`pcmch.txt`, KVM 4 GiB with `intel-hda`) | before (`1a50fba`): **5 of 11**, the whole space offered channels [2,2], HW_PARAMS -22 for 6 and 1 channels, nothing in the ring → FAILED. After (`bf92f3c`): **15 of 15**, ring 6 ch (7899, -5556), mono (5000, 5000), stereo (1234, -4321) → OK. Without HD-Audio: "not run" (exit 78) → OK |
| Wikimedia 480p video, VP9 + 6-channel Opus (`wikimedia-video.txt`) | before (`af6f415`): picture moves, **0 of 18** windows with sound, `Channels count (6) not available`, wav all zeros. After: `hw_params: ... 6 channels (downmixed to stereo)`, picture 22/22, **17 of 18** windows with sound in both rounds (the first window is the quiet opening of the film), `[wmvideo] PASS` (`screenshots/wm-480p-with-sound.png`) |
| local VP9+Opus clip (`localvideo.txt`, run av2) | **no regression**: 2.36 → 60.86 s, 28/28 screendumps differ, 12/12 windows (RMS 1158); Opus alone 12/12; VP9 alone plays; H.264/AAC fails as before (BUG-161). Run av1 stopped at the H.264 step on `[timer-probe] FROZEN` before any audio was opened: BUG-182, also seen in the Fable team's runs on another branch |
| `cargo test` on the host (`cargo-test.txt`) | 187 test binaries, **1 363 passed, 0 failed**, 1 ignored |
| KVM self-test boot + markers-compare (`comparison-kvm.txt`) | 4 GiB, desktop, 0 FAILED lines, 0 crash lines. Against the integration image (`af6f415` code, same options): **0 worse**, new `[pcmch]` OK. With HD-Audio, against `1a50fba`: 0 worse, `[pcmch]` FAILED → OK |
| `gui-smoke.py`, plain public image, `restrict=on`, 4 GiB (`gui-smoke-public.txt`) | **PASS 67/67** |

## Not done, open

- BUG-182 (a second of no timer ticks, `[timer-probe] FROZEN`): recorded, not investigated further (scheduler/interrupt
  area of the Fable team).
- The channel order is ALSA's default; a source whose order differs (Chromium hands the channels in its own layout to
  `hw:0,0`) only moves a channel between the weights, it does not silence it. A map ioctl (`SNDRV_PCM_IOCTL_CHMAP`) is not
  served.
