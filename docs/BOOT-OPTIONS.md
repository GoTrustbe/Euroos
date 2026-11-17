# Boot options and run-time switches

EuroOS starts well with no settings at all. This page is for the cases where you want
to change one thing: a test machine that should not look for updates, a guest that
should not show the first-run tour, a host where the network driver gets too much
processor time. Every option has a default; leave it out and you get the default.

## How to set an option

There is no kernel command line. Options live in a small text file on the boot
partition (the FAT partition that holds the EuroOS kernel):

```
\EFI\BOOT\euroos-boot.conf
```

One option per line, written as `name=value`. A line that starts with `#` is a
comment. The file is read once, early in the boot. A missing file means every
default. A value that is not allowed is ignored and the boot log says so
(`[bootopt] ... ignored`); the machine still starts.

On an image you build yourself, `EUROOS_BOOTOPTS` puts the file on the image for you,
with options separated by `;`:

```
EUROOS_BOOTOPTS="ui.tour=off;update.check=off" ./scripts/build.sh release
```

The file is not signed. It only chooses between code paths that are already part of the
signed kernel; it cannot add code.

Two options can also be changed while the system runs, from the Terminal
(`smpuser` and `drvbudget`, below). A change made that way lasts until the next
restart; put it in the file to keep it.

## Using more than one processor core: `smpuser`

| Value | What it does |
|---|---|
| `off` (default) | Programs run on the first core only. The other cores are used by the kernel itself (devices, timers), as before. |
| `test` | Only the kernel's own tests may place a program on another core. For people who work on the kernel. |
| `on` | Programs that use several threads, such as the browser, may have their threads run on the other cores. |

```
smpuser=on
```

In the Terminal: `smpuser` shows the current value, `smpuser on` (or `off`, `test`)
changes it, `smpstat` shows what has been moved and how often. It only has an effect on a
machine or virtual machine with more than one core. If something misbehaves with
`on`, `smpuser off` brings every program back to the first core at its next timer tick.

## How much processor time a driver may take: `sched.drvbudget`, `sched.drvwindow`

Some drivers run as ordinary programs, for example the network card driver
and the NVMe data-disk driver. They get a high priority so that network traffic and
disk requests are answered quickly. When a flood of traffic keeps such a driver busy
all the time, it can leave the desktop and the Terminal too little time.

The budget limits that. In every time window a driver keeps its high priority for
the budget share of the window. After that it only runs when nobody else wants the
processor, until the window ends; then the next window starts with the high priority again.
A driver that does a short piece of work per request never reaches the budget and does not notice.

| Option | Meaning | Allowed | Default |
|---|---|---|---|
| `sched.drvbudget=PERCENT` | Share of each window a driver keeps its high priority | 1 to 100 | 10 |
| `sched.drvwindow=TICKS` | Length of a window in timer ticks of 10 ms | 5 to 1000 | 50 (500 ms) |

`sched.drvbudget=100` switches the limit off: the driver always has its high priority, which is how
EuroOS behaved before this option existed. A value outside the range is ignored.

In the Terminal, `drvbudget` shows the budget, the window, which programs count as
drivers and how often one went over its budget; `drvbudget 30` sets the budget to 30 %.

```
sched.drvbudget=30
sched.drvwindow=100
```

## Where Linux programs get their memory: `aslr`

Linux programs (the browser, and every other program built for Linux) get their memory at addresses the
kernel picks. With address-space layout randomisation the addresses differ from start to start, so an
attacker who finds a bug in a program cannot know in advance where its code and data are.

| Value | What it does |
|---|---|
| `on` (default) | Every start of a program, and every new program a browser process starts, gets its own random addresses for the program read from disk (the browser), its libraries and large memory areas. A started program also gets a random place for its small clock page (vDSO). |
| `off` | The same layout as `on`, but always at the same addresses. For people who compare two logs of the same run. Not for daily use. |
| `legacy` | The layout of EuroOS 2026.11.16 and earlier: fewer random bits and the browser program always at the same address. A way back if a program misbehaves with `on`; it will be removed later. |

With `on`, every part of a program gets about 30 random bits (Linux gives 28): the browser's main program, its
libraries and large areas, and also the program's own block (the dynamic loader, the small heap, small
allocations and the stack; see `aslr.arena` below). Each new process the browser starts gets new addresses for
all of them. The program's own block is cut into three parts that each get a place of their own: the program and
the dynamic loader, the small heap with the small allocations, and the stack (see `aslr.split` below). Who learns
an address in one part cannot work out the others.
An unknown value is ignored and the kernel log says so; the log line `[bootopt] aslr=...` shows the mode in use.

### Where the program's own memory lies: `aslr.arena`

Besides its libraries and large areas, a Linux program started from the desktop or the shell has one block of
memory of its own: the dynamic loader, the small heap, small allocations and the stack. This option decides at
which address the program sees that block.

| Value | What it does |
|---|---|
| `random` (default) | The block is seen at random places, new for every start and for every new process the browser starts: in three parts between 8 and 20 TiB (see `aslr.split`), or in one piece between 8 and 12 TiB. Inside it, the program itself and the dynamic loader get a random page offset too. With `aslr=off` the places are always the same (8, 12 and 16 TiB; 8 TiB in one piece). |
| `fixed` | The block is always seen at 8 TiB, whatever memory it uses. A program cannot learn where its memory physically lies, but the addresses repeat. A way back if a program misbehaves with `random`. |
| `identity` | The block is seen at the address of the memory chips it uses, as in EuroOS 2026.11.16 and earlier. The way back if a program misbehaves with `random` and with `fixed`. |

With `aslr=legacy` the block always stays at `identity`. The log line `[bootopt] aslr.arena=...` shows the
placement in use.

### Three parts or one: `aslr.split`

With `aslr.arena=random` the program's own block is seen in three parts, each at a random place of its own: the
program and the dynamic loader (8 to 12 TiB), the small heap and small allocations (12 to 16 TiB), and the stack
(16 to 20 TiB). The distance between two parts is different at every start, so one address the program gives away
does not tell where the others are. The parts still use the same memory as before; only where the program sees
them changes. The stack part is 4 MiB: the stack has at least 2 MiB of room, and a stack that grows past its
part ends the program instead of running into the heap.

| Value | What it does |
|---|---|
| `on` (default) | Three parts at random places of their own. |
| `off` | The block in one piece at one random place, as before this option existed. A way back if a program misbehaves with three parts. |

With `aslr.arena=fixed` or `identity` the block is always one piece. The log line `[bootopt] aslr.split=...` shows
what is in use.

## The other options

| Option | Values | Default | What it does |
|---|---|---|---|
| `ui.tour` | `on`, `off` | `on` | The first-run tour. `off` for unattended images. |
| `update.check` | `on`, `off` | `on` | The automatic update check. With `off` nothing is fetched or installed unless you type `euroupdate check`. |
| `net.driver` | `user`, `kernel` | `user` | The network card driver as a program, or built into the kernel. |
| `storage.nvme` | `kernel`, `user` | `kernel` | With `user` an NVMe disk is handed to the driver program as a data disk at `/data`; the root is never on it. |
| `display.mode` | `<width>x<height>` | largest up to 1920x1080 | The screen size, when the firmware offers it. |
| `chrome.input` | `x11`, `cdp` | `x11` | How clicks and typing reach the browser. |
| `virtio.transport` | `modern`, `legacy` | `modern` | How virtio devices are driven. |
| `serial.chatter` | `on`, `off` | `off` | Detailed diagnostic lines on the serial console. |
| `aslr` | `on`, `off`, `legacy` | `on` | Random addresses for the memory of Linux programs (see above). |
| `aslr.arena` | `random`, `fixed`, `identity` | `random` | Where a Linux program sees its own memory block (see above). |
| `aslr.split` | `on`, `off` | `on` | Whether that block is seen in three parts at places of their own (see above). |
| `debug.schedwatch` | `on`, `off` | `off` | A debugging aid for kernel developers. |

## Video and sound in the browser

The browser plays open formats only: VP9, VP8 and AV1 video, Opus and Vorbis sound.
It does not play H.264, HEVC or AAC, because those formats are covered by patents
and EuroOS does not license them. A page that offers only such a file (typically an
`.mp4`) shows an error instead of playing. Large video sites offer an open format
to a browser that does not support the others, so they play. This is a decision, not a
missing feature, and it applies to every image.
