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
