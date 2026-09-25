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

Built (455b38e): kernel/src/alsa.rs answers the hw plugin's ioctls on
/dev/snd/pcmC0D0p (PVERSION, INFO, HW_REFINE/HW_PARAMS, SW_PARAMS, PREPARE,
START, DROP, DRAIN, PAUSE, WRITEI_FRAMES, STATUS, DELAY, HWSYNC, SYNC_PTR,
CHANNEL_INFO) and a control node (card info, one playback device, no mixer)
over the HDA ring at 48 kHz/16-bit/stereo: hw_ptr from the link position,
appl_ptr from the client, consumed frames zeroed behind the cursor.
RW_INTERLEAVED only; libasound falls back to SYNC_PTR without mmap. Chrome
gets --alsa-output-device=plughw:0,0. The runbook attaches intel-hda with
hda-duplex and captures the codec's output to $LOG.wav on the NUC (56f8bd3).
Run 70 plays a video with it; the WAV is the proof of sound.

Run 70 (455b38e, audio codec attached): the HDA controller and codec come
up, the stream runs, chrome logs "Falling back to ALSA for audio output", and
the captured WAV (116 MB for the run) carries the kernel's boot self-test
tone at the same level at 0 s and at 300 s: the capture path is proven end
to end, and the tone loops in the ring until something overwrites it (zeroed
after the test since 8ae0f25). No [alsa] line: chrome opened no output
stream, since the video never played (the consent sequence of run 69). The
run also refused a fork: eleven forks, three recycled, eight children alive
against seven arenas; 8ae0f25 puts a 2176 MiB pool first on a 6144M guest.
Runs 71 to 73 carry the cookie-based consent (cdp:socs) and the watch page.

### Run 68: the never-delivered bodies were not stuck

The census at heartbeat 12 (86d93b2): zero AF_UNIX queues with unread bytes,
every IO thread parked in epoll_wait, load event fired, 38 requests with 35
done, no fault. The three long-lived requests are long-lived by design (the
sign-in check, a preload); nothing waits in the kernel. Closed.

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
QUIC to Google); 6844617 restricts the walker to TCP.

Runs 54 and 55 (QUIC): the page sits in its loading skeleton after ~19
requests. Run 56, 57 (`--disable-quic`, 18a83ca): eleven and twelve
subresources fail with ERR_SSL_PROTOCOL_ERROR after a complete server flight,
walker clean both ways. Run 58: chrome writes no log line for it. Run 59 with
`--log-net-log`: BoringSSL WRONG_VERSION_NUMBER in tls_record.cc after a
completed handshake, then PROTOCOL_IS_SHUTDOWN (the NetLog also silenced the
DevTools channel: one heartbeat answered of 21). Run 60, walker with
BoringSSL's own rule (0x0303 after the first record): still clean, 12
failures: the kernel's stream was intact and chrome's was not.

ROOT CAUSE (1fb5901): recvfrom and recvmsg ignored MSG_PEEK. Chrome's
IsConnectedAndIdle peeks one byte on a pooled connection before reusing it;
the peek consumed the byte, the next record began one byte late (type 0x03,
version 0x03LL), WRONG_VERSION_NUMBER on exactly the reused connections
(youtube's scripts and stylesheets, tracera.eu now and then), never a fresh
connection's first request. The walker never saw it because it is fed at
consumption. Run 61: 0 SSL failures (from 12). Run 62: the 2.7 MB document
downloads in 5 s, the load event fires, 38 requests. QUIC returns after the
matrix passes on TCP.

### W16. the renderer's main thread never stops running

Run 62's [cpu] ledger (ticks per task per heartbeat, c1f1fb6): after the
youtube load, CrRendererMain takes about 1950 of every 3000 ticks for the
rest of the run, and during the load it held the CPU for 165 s while the
desktop loop got 1.3 s (one heartbeat slipped by 148 s). Either the thread
spins (a lock or a yield loop) or JavaScript runs without a JIT at a hundred
times the cost. The tick sampler now profiles the busiest task of each
interval (9c95636): [prof] names its code pages (exe or library offset, or
anon = JIT), its last syscall and its syscall count.

Run 63 ended in a kernel panic: "memory allocation of 2097152 bytes failed"
in a read syscall, the 384 MiB kernel heap full. Since the ENOENT fix chrome's
Simple Cache really writes, and /tmp/cr lives in FILES, in that heap.
Commit 72c6bc8: caches capped at 16 MiB each, heap used/free on every [cpu]
line. Run 64: no panic, heap 219 MiB before youtube, 308 MiB after and steady;
the renderer this time idle (CPU 95% idle) and the page still without a load
event: 19 requests, 3 pending for eight minutes (the sign-in check at
accounts.google.com, a Google font, the web manifest), no connection ever made
for them. EuroGuard answers a blocked name with silence, a resource that
never fails. Commit 0a31615: a DNS ledger (every query and answer with tick),
NXDOMAIN synthesized for a blocked name, connect log 200 lines with ticks.
Run 66 measures. The renderer's 165 s of CPU in run 62 did not recur in 64;
kept as W16 until the profile names it.

ROOT CAUSE (run 71, a5c447b): the pick is by smallest vruntime, task 0 was
charged a step on every tick since boot, halted or not, and a thread created
later starts at its creator's vruntime, which for chrome's mostly sleeping
threads is far below task 0's. The watch page's renderer main took 38090 of
39664 ticks in one heartbeat interval; the desktop loop, the heartbeats, the
visit list and every other thread waited for it to catch up. Now: a tick that
finds task 0 halted charges nothing, the pick records the minimum runnable
vruntime, and new or woken tasks are placed at that minimum less a small
grace. Run 74 measures; runs 72 and 73 (before the fix) show the hog once
more with the consent cookie and eight arenas.

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

ROOT CAUSE FOUND (run 73, e31a251). Run 73's fault was the same instruction
at the same offset: chrome+0x6f132e2 is
v8::internal::UnifiedHeapMarkingVisitorBase::Visit (the pack's chrome binary
carries symbols; extracted from /opt/euroos/packs/chrome-pack2.img at pack
offset 20480), reading the HeapObjectHeader four bytes before a traced Oilpan
pointer, and the pointer was garbage (0x389ca3a2c, which lands in the kernel's
identity map: pdpt[14] = a 1 GiB kernel page, hence the protection fault).
The same run had a ThreadPoolForegr thread spinning at 0 syscalls in
v8::base::TemplateHashMapImpl<AstRawString>::InsertNew (chrome+0x6d743a0),
the linear probe that ends only at an empty slot: a table whose every slot
reads occupied. Both are stale memory. The kernel's MAP_FIXED overlay inside
the demand region only recorded a zero-fill shadow and kept the old frames
mapped, while V8's OS::DecommitPages and PartitionAlloc's
DecommitAndZeroSystemPages are exactly mmap(addr, len, PROT_NONE,
MAP_FIXED|MAP_ANONYMOUS|MAP_PRIVATE) over live heap pages, and both count on
fresh zero pages when they recommit (the madvise zeroing contract of W9, one
call further). Commit e31a251: paging::unmap_demand_range unmaps the range
(invlpg per page) and frees the frames the process owns (writable, not in a
MAP_SHARED window or alias); the overlay tracks PROT_NONE like mprotect; the
zero shadow is recorded only over a file mapping (V8's decommits were growing
the map list, searched on every fault); munmap, a no-op since the bump
allocator days, now frees demand-region pages the same way. Run 75 measures.

### W15. signal delivery

Chrome's renderers and V8 use signals: SIGSEGV handlers for WebAssembly bounds
traps and crash reporting, SIGCHLD, SIGPIPE, SIGALRM, SIGTERM to children. The
kernel delivers none (a fault terminates the process; tgkill ends it). The
workplace bar ("chrome works") will need real delivery of at least SIGSEGV to
a registered handler, with siginfo and ucontext, before the rest of the
matrix can be trusted. Planned after W13.

### YOUTUBE RENDERS (run 67, build 35b973c)

Run 67: PASS, no panic (512 MiB heap, 384 MiB used and steady), youtube's
load event fires, 43 requests including the `youtubei` API and a
`videoplayback` preload from googlevideo.com; the screendump at 480 s shows
YouTube's page (sidebar, header) under the EU consent dialog "Before you
continue to YouTube". Proof:
`docs/proof/2026-09-25-desktop-chromium-youtube-consent-run67.png`. One
renderer (task 117, the sign-in iframe's) died at exe offset 0x6f132e2 on
0x1116111c4, the same offset and address shape as run 54 (W14); the page
survived it. Next: accept the consent (a `js:` step in the visit list) and
open a video: picture first, sound after W12.

### W17. the kernel heap fragments under the browser's profile

Run 65 panicked like run 63, this time on a 64 KiB allocation with 169 MiB
nominally free: the first-fit list heap (linked_list_allocator, blocking lock
under interrupts-off, so never a null on contention) had no hole of that size.
Both panics sat in the child exit/exec path, where the exit guard walked the
parent's demand region into two 1.3 MiB lists and shared_phys_sorted collected
the whole disk page cache, all grown by doubling. Commit 35b973c: the guard is
off by default (it measured zero in every run since 37), the shared list is
allocated once at its exact size, the heap is 512 MiB. The structural answer
is a buddy or slab heap, or keeping the big buffers (VFS files, the page
cache index) out of the general heap; after the site matrix.

### Run 74 (a2aa8bc: scheduler fix, full census, tone and state steps)

FAIL on fork-refused: nine children alive against seven arenas, two forks
refused at "pool has 127 MiB"; the 6144M guest yields one 1920 MiB run and
the eight-arena candidate found none. js:tone answered "tone running 48000
latency 0.0427": the Web Audio context runs, but no [alsa] line: chrome's
audio manager fell back to ALSA, asked for plughw:0,0, and libasound said
"Unknown PCM" because /usr/share/alsa/alsa.conf did not exist. The heartbeat
ledger accounted 1530 of 3000 ticks after the youtube navigation: the run
forked task 132 and every per-task table (caps, ticks, last syscall, syscall
count) stopped at 128 slots, so a task above that ran on the global
capabilities and its CPU time vanished from the line; the state, play and
video evaluations were never answered, which fits a renderer above slot 128
spinning as in run 73. Commit caef3f6: a minimal alsa.conf (hw, plughw,
defaults on card 0), tables sized by sched::MAX_TASKS (256), and the fork
pool assembled from the largest run plus further 258 MiB runs up to the cap.
Run 76 measures all three with the W14 fix.

### Run 75 (e31a251: MAP_FIXED/munmap fix): kernel panic, W17 again

The unmap path ran (512 MAP_FIXED overlays, 6012 frames freed by the time
the log stopped; munmaps of thread stacks and transfer buffers) and the run
reached the watch page, then the kernel panicked on "memory allocation of
4195744 bytes failed": a socket receive queue doubling under youtube's 4.7 MB
document, with 223 MiB of the list heap free but no hole that size. Same
failure as runs 63 and 65, so W17 gets its structural answer now (e067bab):
allocations of 128 KiB and up come from a page-granular bitmap pool of 256
MiB of frames handed over at boot (allocator::install_big_pool), the list heap
only sees small blocks, and the [cpu] line reports both pools and the demand
pool. The fork cap goes to half of usable RAM so the extra 258 MiB runs fit
(run 76 showed the eighth run 1.4 MiB over the old cap). Run 77 measures.

### THE WATCH PAGE RENDERS WITH ITS PLAYER (run 77, e067bab)

PASS, no panic, no refused fork (ten arenas), 25 heartbeats. The screendump at
660 s shows youtube.com/watch?v=jNQXAC9IVRw with the title "Me at the zoo",
the player with its poster frame, the play button and the controls at
0:00 / 0:19; a videoplayback request went out. The video did not start: the
js:state, js:play and js:video evaluations after the navigation were never
answered, and from the moment js:state was sent the page session emitted no
event for 400 s while the browser answered every heartbeat (the renderer for
youtube was a fresh process, task 132, above the old 128-slot tables). The
big-block pool filled its 256 MiB at the load and fell back to the list heap.
Commit 0a0666d: [rmain] line per heartbeat (each CrRendererMain's state and
last syscall), census at heartbeats 12 and 20, js:state without innerText,
the VFS files total on the [cpu] line, big pool 512 MiB. Run 79 sends play
before state to separate the step from the silence.

### W12 continued: the device opens (run 78), the plug layer wants MMAP

Run 78 (de57e86, alsa.conf on the desktop path): PASS; chrome's audio manager
opened /dev/snd/pcmC0D0p twice ([alsa] open at the tone and at the watch
page) and libasound closed it at once with "Rate 48000Hz not available for
playback: Invalid argument" from snd_pcm_set_params, before a single refine
reached the kernel. alsa-lib's pcm_plug.c maps the client's RW_INTERLEAVED
access to MMAP_INTERLEAVED on its slave (line 658), so a slave that offers RW
alone fails in software. Commit 32aabbc: the device offers MMAP_INTERLEAVED
and RW_INTERLEAVED, reports MMAP|MMAP_VALID, and an mmap of the PCM fd at
offset 0 maps the HDA ring's eight frames into the process (an alias entry
keeps munmap and madvise off them); status and control pages stay unmapped,
so the pointers keep travelling through SYNC_PTR. Every refine request is
logged with its masks and intervals. Run 80 measures.

### W19. CDP Runtime.evaluate is not routed to the youtube renderer

Runs 77 and 79: js:tone (a Runtime.evaluate) on file:///tmp/euro.html returns
its value; every evaluate sent after the cross-process navigation to
https://www.youtube.com (ids 63-65: play, video, state) gets no reply, while
the browser keeps answering the heartbeat Target.getTargets. The youtube main
renderer is not stalled: task 131 ran 73027 syscalls and painted the player
(run 77 screendump). So the browser is not forwarding the page session's
renderer-level command to the swapped-in renderer process (the DevTools mojo
channel to a cross-process navigated renderer is not carried), or the reply is
not routed back. Real Chrome forwards it over --remote-debugging-pipe; the gap
is ours. The proper fix is Target.setAutoAttach{flatten:true} with per-frame
sessions, or wiring the devtools channel through the process swap; both are
their own work.

Playback does not need it: commit 1bdacb4 adds
--autoplay-policy=no-user-gesture-required, so the page starts the video
itself, with sound. js:video stays as a best-effort read. Run 81 measures
picture (screendump progress) and sound (the WAV) with autoplay.

### W12 continued: the refine loop (run 80), then the tmpfs (run 81)

Run 80 (32aabbc): the device opened and refine ran at last, with the client
offering exactly the ring's config (access 0x9 = MMAP|RW, S16_LE, 2 ch, 48000
Hz, period 1024, buffer 8192). It then refined the same params ~100 times and
set_params failed "Rate 48000Hz not available", a wedge. Two faults, both in
hw_params (f33ba12): the access mask was forced to MMAP alone, so chrome's
set_access(RW_INTERLEAVED) left an empty space the rate refine read as
unavailable; and cmask was hardcoded to everything-changed, so libasound never
converged. Now the access mask is the client's request intersected with
{MMAP, RW} (both kept), and cmask names only the params this call narrowed.

Run 81 (1bdacb4, autoplay): FAIL after four heartbeats, "No space left on
device". The SystemRescue root is a ~1.9 GB tmpfs and a dozen ~150 MB run WAVs
had filled it. The runbook now keeps only the newest previous WAV and the last
three logs (0cda5dc). Run 82 carries the ALSA fix and autoplay together.

### W12 continued: plughw will not settle, so open hw:0,0 (run 83)

Run 83 (tone only, refine output logged): the kernel's refine converges. The
client asks with everything wide, the kernel returns access 0x9 (MMAP|RW),
S16, rate [48000,48000], period [1024,1024], buffer [8192,8192], cmask 0xfff07
on the first call and 0x0 (nothing changed) on every call after. So the device
correctly advertises exactly one config. Yet libasound's plug plugin
(plughw:0,0) still fails set_rate_near with "Rate 48000Hz not available"
before any commit: its rate/format convergence over a single-config slave does
not settle. Rather than keep fighting the plug, commit d28f64f points chrome
at hw:0,0 (no plug, no resample): chrome writes with snd_pcm_writei straight
into the ring, which the kernel already serves, and the AudioContext runs at
48 kHz so nothing needs resampling. The info field also advertised
BLOCK_TRANSFER with bit 0x10 (that is BATCH); the real bit is 0x10000. Run 84
tests the tone through hw:0,0 and reads the WAV.

### W12 RESOLVED (mostly): the ALSA path works end to end (runs 84-93)

The device now opens as hw:0,0, refines, sets params, prepares and takes
snd_pcm_writei, and the state machine is correct. The bugs cleared in order:
alsa.conf registered on the desktop path; hw:0,0 instead of plughw (the plug
would not settle over a single-config slave); RW-only access, no MMAP in info
(MMAP sent libasound to the XRUN-ioctl avail path); audio moved in-process so
the SyncWriter socketpair send does not cross a process boundary (it returned
EPERM as "No room in socket buffer"); unix send returns EPIPE not EPERM; and
the decisive one, the PCM state constants were each one too high, so chrome
read PREPARED as RUNNING and looped XRUN/PREPARE 77 times without writing.
With that fixed chrome writes audio and the WebAudio tone's clock reaches
7.77 s.

One gap remains on the WebAudio tone: chrome writes three buffers (about
0.18 s), all silence (peak 0, the pre-roll), then its audio output thread
stops reposting, so no tone reaches the ring. hw_ptr was first paced to the
real HDA LPIB and then to a guest-time clock; neither changed it, so the stall
is inside chrome's audio scheduling, not ALSA availability. The tone is
WebAudio; a video uses the separate media-audio path. Run 94 plays a YouTube
video with autoplay and measures whether media audio flows (writei count and
the WAV) and the picture advances.

### THE YOUTUBE VIDEO PLAYS WITH PICTURE (run 94, autoplay)

Run 94 (autoplay, a2de9d9): PASS, and the screendump at 600 s shows the video
mid-playback, the "Me at the zoo" frame with no play button and no controls
overlay, unlike run 77's paused poster. So --autoplay-policy started the video
and the picture advances. Proof
docs/proof/2026-09-25-desktop-chromium-youtube-playing-run94.png.

Sound is still silent, and run 94 says why: youtube fetched ONE stream (one
videoplayback, 3.58 MB of video), no separate audio stream, and chrome wrote
25 s of audio buffers that were all silence (the writei peak is 0). A muted
autoplay is exactly this: youtube mutes the video and does not fetch the audio
track. Unmuting needs a real user gesture, and Runtime.evaluate (the js:play
that sets v.muted=false) does not reach the youtube renderer (W19). Commit
021c7e2 adds click: and key: walker steps that dispatch Input.dispatchMouseEvent
and Input.dispatchKeyEvent, which chrome routes to the current renderer at the
browser level. Run 95 clicks the player and presses m to unmute, and measures
whether the audio stream is then fetched and the WAV carries sound.

### W19 narrowed: the session works on the initial renderer, a navigate breaks it

Run 99 loaded youtube as chrome's initial page (chrome_init_url puts the first
http URL of the visit list into the argv). The viewport evaluate (id 60) sent
right after the attach got its reply from youtube's renderer, so a CDP command
DOES reach it. The js:play and js:video evaluates that followed did not,
because between them the visit list navigated (a reload of the same youtube
URL) and every navigation, even same-site, swaps the renderer process here and
strands the session on the old one. So W19 is precisely: the DevTools session
does not follow a navigation to a new renderer process, and neither a
re-attach (run 98) nor the browser-level Input path reaches the swapped-in
renderer. Getting past youtube's consent needs a navigation (the SOCS cookie
is set after the first load), which is the same navigation that breaks the
session, a catch-22. Run 100 tries a re-attach after the same-site reload.

### W18. the watch page stops loading with nothing pending (run 72)

Run 72 (0b69065, the ALSA gaps closed, consent cookie set at heartbeat 4,
watch page at 8): PASS on the desktop checks, but the page stayed blank with
the toolbar spinner on until the end. The network side finished: 21 requests
sent and 21 done (the document, base.js, the kevlar bundle, the css, two
generate_204 probes), 3.4 MB received from www.youtube.com, no request failed,
no connection left with unacked data, no unix socket with unread bytes at the
census. The renderer answered every js:video evaluation ("no video"), so its
main thread was alive and idle; no load event fired for the watch page; the
CPU ledger shows the machine 85% idle and the renderer absent from the top
six. So the HTML parser waited for bytes the network service had already
delivered, or a mojo data pipe signal was lost between the two, and neither
side ever moved again. Run 71 on the build before got past this point (the
renderer hogged the CPU and requested videoplayback), so it is a race.

The census printed nothing useful for the renderer: it listed the main
process's threads only, and the last-syscall table had 64 slots while the
run used 103 tasks. Commit c4abe4f: the census walks every task with cr3,
name, state and last syscall; js:state reports readyState, script and
resource counts, the pending resources and the start of the body text. Run
74 (scheduler fix plus this) runs js:state at the twelfth heartbeat, with
the census.

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
