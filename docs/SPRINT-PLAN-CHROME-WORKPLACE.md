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

### W13. youtube.com: the document loads, its scripts and stylesheets fail TLS

Run 52 (URLs intact): Page.frameNavigated to youtube.com, loadEventFired at
124 s, and twelve subresources (scripts, a stylesheet) with
net::ERR_SSL_PROTOCOL_ERROR; the screendump is YouTube's skeleton (menu icon,
three grey circles). The same error tracera.eu showed now and then. euro-os.eu
(nginx, no MLKEM) never fails; Google's CDNs and Caddy (X25519MLKEM768, bigger
ServerHello) do. Commit 84ebcd1: a TLS record walker on every port-443 TCP
socket, both directions, prints the first bad record header per fd with the
stream offset: a stream corrupted by this stack versus one the peer rejects.
Run 53's first walker lines were QUIC long headers on UDP 443 (chrome speaks
QUIC to Google); 6844617 restricts the walker to TCP. Run 54/55 measure.

### W6b. run 53's wedge: PIPES taken from the desktop loop

The NMI probe, symbolized against the exact build (a worktree link of
84ebcd1 with the main tree's userland artifacts): chrome's sandbox_ipc_thread
in ring3::epoll_fd_ready+0x215 spinning on ring3::PIPES with interrupts off.
The DevTools pipes are driven from task 0 with interrupts enabled (cdp_send,
cdp_next_msg), and a preempted holder blocks every pipe syscall forever.
Commit 09b59c1: all acquisitions go through pipes_lock() (interrupts off
first). Rule, now enforced for PIPES as it was for SOCKETS: a ring3 lock that
the supervising loop touches is taken under IfOffGuard everywhere.

### W14. the YouTube renderer dies on a wild pointer

Run 54 (youtube over QUIC): the page sat in its loading skeleton and the
renderer (task 112) was terminated on a read at 0x111d1131c, an address in the
kernel's identity map (4.5 GiB, protection violation), `movzwl -4(%rdx)` at
exe offset 0x6f132e2; no syscall in the run ever handed out an address near
it. Whether this is chrome's own bug, a consequence of a kernel answer, or a
missing signal (V8 relies on SIGSEGV for some traps) is open. The isolation
line now carries the task's last syscall; the [fsdiag]/[tls] instruments say
nothing about it yet.

### W15. signal delivery

Chrome's renderers and V8 use signals: SIGSEGV handlers for WebAssembly bounds
traps and crash reporting, SIGCHLD, SIGPIPE, SIGALRM, SIGTERM to children. The
kernel delivers none (a fault terminates the process; tgkill ends it). The
workplace bar ("chrome works") will need real delivery of at least SIGSEGV to
a registered handler, with siginfo and ucontext, before the rest of the
matrix can be trusted. Planned after W13.

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
