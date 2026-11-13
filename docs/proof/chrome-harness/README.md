# Chrome test harness: consent banners, YouTube, local video, q35 under restrict=on, baseline (2026-11-13)

Branch `feature/chrome-harness`, from `40f853a`. Only `scripts/`, tests and docs changed. Images: public image
(`ui.tour=off;update.check=off`) and release image built from `40f853a` on .16 (the kernel is the one of `8346efc`).
R630, KVM, `-cpu host`, 2 vCPU 6 GiB for browser runs, CPUs 28-39. Browser runs: user-net without `restrict=on`, no
hostfwd to the outside. Raw logs and screenshots in this directory.

## 1. Consent banners: found and clicked generically (`chrome-smoke.py`)

No DOM access, no OCR. The harness opens Chromium's find bar (Ctrl+F, typed through the X server), types a label
(`reject all`, `alles weigeren`, `tout refuser`, `alle ablehnen`, ..., then `accept all`, `alles accepteren`, ...), looks
for the orange "active match" highlight in a screendump, steps through the matches with Enter (find scrolls each one
into view) and takes the first one that stands alone like a button label (`isolated()`: the 8 pixels left and right
of the highlight are one flat colour; "Reject" inside "rejected" or "Accept all" inside a sentence is not a button).
Then Esc, a real click with the USB tablet, and the check: the label is no longer found as a button and the pixels
around the button changed. Every click is logged (`consent_clicks` in `report.json`: site, button, position, gone).
A banner that is known to come (YouTube) is waited for, up to 45 s, before clicking (`expect=`).

| Site | Result |
|---|---|
| local banners (EN "Reject all", NL "Alles weigeren", accept-only "I agree") | 3 of 3 clicked, page title confirms the button, banner gone (`consent-selftest-report.txt`, `consent-consent-nl-*.png`) |
| youtube.com ("Before you continue to YouTube") | the click on "Reject all" works and the dialog is gone (`consent-youtube-reject-*.png`); in several runs the harness ended on "Accept all" because the first letters typed in a fresh find bar were lost (a harness flake, fallback allowed by the rules, logged) |
| github.com, openstreetmap.org, euro-os.eu, nl.wikipedia.org | no banner shown to this guest (no button found by 17 labels) |
| pdf | not searched |

Harness bugs found on the way, all fixed: "reject" matched the word "rejected" in the GitHub README (false click);
"Accept all" matched the sentence above the buttons; typing right after Ctrl+F lost letters; a gone-check that compared
against the wrong picture. No EuroOS input bug: a real click on the banner button works every time it is made on the button.

BUG-145 (from the kernel team): after Ctrl+L the harness now waits until the omnibox shows its selection (a run of
selection blue in the omnibox, 0.0-0.3 s in these runs) before typing, retries Ctrl+L if it does not, and judges a
navigation by the host name of the URL the tab shows (`url_is`), not by "a page loaded".

## 2. YouTube, guest, consent answered (`youtube-report.txt`, `youtube-botcheck-1.png`)

Consent dialog answered with a real click, the watch page of Big Buck Bunny loads. Then the player says, literally:
**"Sign in to confirm you're not a bot"** with a "Sign in" button (screenshot `youtube-botcheck-1.png`, the same page
for the three further public videos: YE7VzlLtp-4, eRsGyueVLvQ, R6MlUcmOul8). The consent was handled and verified gone
before the player was looked at; the harness re-ran the consent check on the watch page (nothing left to click). Nothing
was done about the page (no login, no workaround). So the step "picture, time and sound for 2 minutes" is **not
measurable here**. The measurement code is in place (frames, red progress bar, audio from QEMU's wav) and runs as soon
as a video plays.

## 3. Local video (`localvideo-report.txt`; clips by `scripts/mk-test-video.sh`, served from this host to `http://10.0.2.2:PORT/`)

| Clip | canPlayType | picture | currentTime | sound (QEMU wav) |
|---|---|---|---|---|
| H.264 + AAC (mp4) | no (`DEMUXER_ERROR_NO_SUPPORTED_STREAMS`) | no | 0 | silence |
| VP9 + Opus (webm) | probably | frozen, `error 3` (decode) | stays 0.00 | silence (RMS 0) |
| VP9 only | probably | 18 of 18 dumps differ | 3.04 -> 39.54 s in 40.1 s | - |
| Opus only | probably | - | stays 0.00, `error 3` | silence |

The video decoder works. The audio stream does not: `AUDIO_RENDERER_ERROR`, `sync_reader.cc ... Broken pipe`
(**BUG-160**), the kernel's ALSA trace shows the stream opened, three writes of silence and a close. H.264/AAC is not in
this Chromium build (**BUG-161**, a build choice). This is also the YouTube "no sound" of `chrome-2026-11b`.

## 4. q35 matrix under `restrict=on`: 13 of 13

`q35-restrict-kvm.txt` and `q35-restrict-tcg.txt` (13 legs, every NIC `restrict=on`; KVM and TCG both 13/13).
`EK_RESTRICT=1` and `EK_ACCEL=kvm` in `scripts/run-metal-matrix.py`. Why the previous attempt hung:

1. QEMU refuses a `guestfwd` on the host address itself: `Conflicting/invalid host:port in guest forwarding rule
   'tcp:10.0.2.2:...'`. The scan and printer legs talk to 10.0.2.2, the slirp host address.
2. `guestfwd=tcp:ADDR:PORT-tcp:127.0.0.1:PORT` opens ONE host connection at start-up (and fails to start when the mock
   is not yet listening) and runs every guest connection over it in turn; the mocks answer one request per connection,
   so the second connection of the guest is never answered (the hang).

Fix: the slirp host moves to 10.0.2.4 for these two legs (`host=10.0.2.4`), and `guestfwd=tcp:10.0.2.2:PORT-cmd:
scripts/slirp-guestfwd-bridge.py PORT` starts one bridge per guest connection to the mock on 127.0.0.1. The mocks are
started before QEMU. Scan and printer pass, the other 11 legs are unchanged.

## 5. Baseline on 40f853a

| Measurement | Result |
|---|---|
| site matrix, 3 rounds, one session (`sites-3-rounds.log`) | round 1: **6/6** (euro-os, wikipedia, github, youtube with "Reject all", openstreetmap, pdf). Rounds 2 and 3: **every site except the pdf failed** — after the PDF the typed URLs no longer navigate (W23, BUG-134/135). Recorded, not fixed. The run was stopped after round 3 started (the remaining steps would run in the same wedged browser) |
| interaction matrix, fresh boot (`interact-perf-report.txt`) | 18 of 20; `link` and `history` failed in the first run because of a harness regression (host `euro-os.eu/en`, fixed); in the rerun (`interact-rerun-report.txt`) 16 of 18: `link` did not navigate and `history` followed from it (**BUG-162**: one click on the page header link was not followed, once in two runs) |
| reference site load | median 1.32 s (1.09-2.83), first change 0.62 s |
| key to screen | median 336 ms, p90 864 ms, max 1796 ms over 30 keys: **not met** (goal 100 ms) |
| 20 starts (`starts20.txt`) | 20/20 PASS |
| YouTube | blocked by the bot page, see 2 |
| local video | see 3 (BUG-160) |
| BUG-137 (demand pool) | not reached in this run (a 2-hour soak is needed); unchanged |
