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
| `debug.schedwatch` | `on`, `off` | `off` | A debugging aid for kernel developers. |

## Video and sound in the browser

The browser plays open formats only: VP9, VP8 and AV1 video, Opus and Vorbis sound.
It does not play H.264, HEVC or AAC, because those formats are covered by patents
and EuroOS does not license them. A page that offers only such a file (typically an
`.mp4`) shows an error instead of playing. Large video sites offer an open format
to a browser that does not support the others, so they play. This is a decision, not a
missing feature, and it applies to every image.
