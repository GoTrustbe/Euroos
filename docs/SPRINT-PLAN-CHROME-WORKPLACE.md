# Sprint plan: Chromium as a workplace browser on EuroOS

Started 2026-09-25, after the desktop sprint (docs/SPRINT-PLAN-DESKTOP-CHROME.md)
met its three exit criteria: euro.html and the live site render on the NUC's
desktop unattended, nine PASS runs in a row.

## Goal

Chromium on the EuroOS desktop works the way a person expects a browser to
work: any site loads and behaves (YouTube included: video and sound), the
keyboard and mouse do what they do everywhere (URL bar, links, scrolling, tabs,
window size, clipboard), it stays up for hours, and it is as fast as the
hardware allows. Tested to the point where a fresh run of the matrix passes
three times in a row without a single retry. No new blog post until then: the
existing ones already tell the story.

## Method (unchanged)

One measurement per run, the binary names its own blocker, never a zero without
checking what may print, commit as we go, nothing pushed. Runbook
`scripts/nuc-desktop-run.sh` on the NUC; `CHROME_CMD="chrome URL..."` visits
the URLs one every two minutes from the fourth heartbeat; the verdict at the
end of `runNN.out` checks every navigation committed, no browser fault, no
refused fork, no exhausted pool, no wedge.

## Test matrix (the sites; each gets a screendump and a verdict)

1. https://www.youtube.com/ : the front page, then a video: picture, sound.
2. https://euro-os.eu/ : reference (passes since run 34).
3. https://nl.wikipedia.org/ : text, fonts, images, search box.
4. https://github.com/GoTrustbe/Euroos : heavy JS, fonts, scrolling.
5. A Nextcloud instance (canvos.eu when available): login form, file list,
   open a document (the workplace case).
6. https://www.openstreetmap.org/ : tiles, WebGL absent (software GL), drag.
7. A PDF: https://euro-os.eu/ (a document there) in the built-in viewer.

## Interaction matrix (through real input, no DevTools)

- Ctrl+L, type a URL, Enter: navigation from the omnibox.
- Click a link; back and forward; reload.
- Scroll with the wheel and by dragging the scrollbar.
- Ctrl+T new tab, Ctrl+W close, switch tabs.
- Type into a form field; select text; copy; paste in another field.
- Maximise the window; resize; fullscreen (F11) and back.
- Sound: a YouTube video is audible on the host's audio device (needs a guest
  audio device: intel-hda + hda-duplex in the runbook, and the kernel's HDA
  driver on the desktop path).

## Walls, filled in as they are found

### W10. site matrix: youtube.com (run 50 onwards)

Run 50 is the first: `chrome https://www.youtube.com/ https://euro-os.eu/`.
The guest read `chrome httpswww:youtube:com httpseuro=os:eu`: see W11.

### W11. found by typing a URL: the Belgian AZERTY table was wrong

eurokeymap gave the AZERTY punctuation keys their US shifted forms, so a
Belgian keyboard could not type '.', '/' or '-' in the guest: shift+';' was
':', ':' had no shifted form, the keys right of the digits were '-' and '='.
Commit dfcf3c2: , ; : = shift to ? . / + on the bottom row, ) - right of the
digits with ° _ shifted (French: ) = and ! §). A real-user bug, not only a
harness one. Run 51 typed `chrome httpswww.youtube.com httpseuro-os.eu`: dot
and hyphen right, colon and slash still missing, because QEMU names the key
after comma "dot" and the harness asked for "period" (8201b79). Run 52 is the
first with both fixes.

Run 50 also showed two fork children dying on glibc's abort() hlt: chrome
runs `xdg-settings` for its default-browser check, the child re-executed
/pack/chrome under that name, setsid was ENOSYS, and tgkill returned. Commit
69e9c84: execve of a program that is not here is ENOENT, a fatal tgkill ends
the process, setsid answers.

### W12. sound: chrome's audio path needs /dev/snd

The pack carries libasound.so.2 (and libpulse, libpipewire); chrome dlopens
libasound and opens /dev/snd/pcmC0D0p through the ALSA hw plugin's ioctls
(HW_PARAMS, PREPARE, WRITEI/mmap, STATUS). EuroOS has an HDA driver
(kernel/src/hda.rs, "[hda] no HD-Audio controller found" on the NUC guest
until the runbook adds `-device intel-hda -device hda-duplex`). Sound on
EuroOS therefore means an ALSA-compatible /dev/snd on top of that driver:
a bounded protocol, done after the video path renders (W10).

## Exit criteria

1. Every site of the matrix loads and renders (screendump), three runs in a
   row, no retry.
2. Every interaction of the interaction matrix works through real input
   (keyboard/tablet over QMP), verified by screendump or CDP state.
3. A YouTube video plays with picture and sound for two minutes.
4. A two-hour run with the matrix repeated stays up: no fault, no wedge, no
   memory growth beyond the pools (the [mm] lines).
5. Page load of the reference site under 3 s after navigate; the interaction
   latency (key to screen) under 100 ms measured on the screendump clock.
