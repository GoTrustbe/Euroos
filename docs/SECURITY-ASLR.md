# Memory protection for programs: random addresses (ASLR)

This page is for people who run EuroOS: what the protection does, what you will notice, what to do if a program
misbehaves, and where it stops. The options are listed in [BOOT-OPTIONS.md](BOOT-OPTIONS.md); the design and the
measurements are in [design/ASLR.md](design/ASLR.md) (section 12) and in the proof directories named below.

## What it does

A program that has a bug can be tricked into running an attacker's code, but the attacker first has to know where the
program's code and data lie in memory. Since 2026.11.19 EuroOS places the memory of every Linux program
(the browser and every other program built for Linux) at addresses that differ from start to start, and the browser's
child processes (renderers, the GPU process, the network service) get new addresses of their own when the browser starts
them. An address that leaks from one place says little about another place:

- the program itself, its libraries, its large memory areas, its heap and its stack each have their own random place;
- the program's own block of memory is cut into three parts (the program with the dynamic loader, the small heap with the
  small allocations, the stack), and each part has a place of its own;
- two parts differ in about 31 to 32 random address bits, so an address of one part does not tell where another lies.

The clock page (vDSO) that every Linux program maps is read-only, so one program cannot rewrite what the others run (this
was a security flaw until BUG-353 was fixed).

Measured numbers (random address bits per region, per start) are in [design/ASLR.md](design/ASLR.md) section 12.2; the
measurements themselves are in `docs/proof/11f-aslr-s6/` (final state) and `docs/proof/11f-aslr-s5/`, `docs/proof/11f-aslr-s01/`.
In the browser tests none of 185 child processes kept an address of the browser that started it (`11f-aslr-s6/chrome-entropy.txt`).

## What you notice

Nothing. There is no setting to turn on and no prompt. Programs start and run as before; in the tests the desktop
(67 of 67 checks of the GUI smoke test) and the browser (20 of 20 starts, sites, interaction, one-hour sessions) ran with it
on (`docs/proof/11f-aslr-s6/README.md`).

The one visible difference is in logs: the addresses in the serial console, in crash reports and in the boot log differ
from run to run. If you compare two logs of the same run, set `aslr=off` (below).

## If a program misbehaves

Only change this if a program fails with random addresses and works without them. Put one line in `\EFI\BOOT\euroos-boot.conf`
(see [BOOT-OPTIONS.md](BOOT-OPTIONS.md) for how), restart, and try the steps in this order; stop at the first one that helps
and tell us which one it was:

| Step | Line | What it changes |
|---|---|---|
| 1 | `aslr.split=off` | the program's own block is in one piece again (one random place) |
| 2 | `aslr.arena=fixed` | that block is always at the same address (8 TiB); the other regions stay random |
| 3 | `aslr.arena=identity` | that block is at the address of the memory it uses, as in 2026.11.17 and earlier |
| 4 | `aslr=legacy` | the whole layout of 2026.11.17 and earlier: few random bits, the browser always at the same address |

`aslr=off` is not a way back: it keeps the new layout but draws nothing, so addresses repeat from run to run. It is for
debugging and for comparing logs, not for daily use. With `off` and with `legacy` the protection above is largely gone.
`aslr=legacy` may be removed in a later release; report a program that needs it.

The log line `[bootopt] aslr=`, `[bootopt] aslr.arena=` and `[bootopt] aslr.split=` on the serial console show what is in use.

## Known limits

- **The stack part is 4 MiB.** The stack has at least 2 MiB of room in it. A stack that grows past its part ends the program
  (before, it could run on into unused memory). Programs that need a very large stack are the ones to watch.
- **A smaller window for small memory areas.** Allocations of 64 KiB and more are placed in the large area and are not
  affected. The window for the small ones is 1 to 2 MiB smaller than before. When it is full the kernel logs `mmap window full`;
  none of the Chromium test runs logged it (`docs/proof/11f-aslr-s6/README.md`).
- **Within one part only a page offset separates two regions**: the dynamic loader and the program differ in 12 bits, the small
  allocations and the heap in 14. Leaking one of them tells about the other; it does not tell about the other parts.
- **Not randomised:**
  - the kernel itself (no KASLR; it is identity-mapped, design section 6.3);
  - native EuroOS programs and the drivers that run as programs (they keep their own, older memory layout);
  - a program that forks without starting another program gets its parent's layout (the browser's child processes do start
    another program, so they get a new one);
  - addresses a program asks for in `mmap` (hints) are not honoured.
- **The serial console shows addresses.** It is the operator's console, a program cannot read it.
- The proof directories record runs in virtual machines (QEMU with KVM, and QEMU software emulation); none on bare metal.

## Where to read more

- [BOOT-OPTIONS.md](BOOT-OPTIONS.md): the three options and their values.
- [design/ASLR.md](design/ASLR.md): why it is built this way, what each step measured, section 12 as built.
- [LOADTEST-TRACKER.md](LOADTEST-TRACKER.md): BUG-088 (the weak slides) and the bugs found on the way (BUG-350 to 355, 371, 380).
