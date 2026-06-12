# EuroCompat — Applicatie Compatibiliteitslaag
## Track 8 van het EuroKernel Project
## Linux ABI · X11 Bridge · Flatpak Runtime · WASM Sandbox
## Technische Specificatie v0.1 & Claude Code Build Prompt

> **Dit is de strategisch meest impactvolle track na de kernel zelf.**
> EuroCompat bepaalt of EuroOS een speeltuin voor developers blijft
> of een bruikbaar OS wordt voor gewone gebruikers.
> Zonder app-adoptie heeft het beste OS ter wereld geen toekomst.
>
> **Afhankelijkheden:**
> - Track 3 (syscalls) — basis syscall infrastructuur vereist
> - Track 6 (EuroToolchain + POSIX) — libc en sysroot vereist
> - Track 5 (EuroDesktop) — voor GUI app ondersteuning
>
> **Start: Na Run 6 (VFS compleet) — parallel aan Run 7-9**

---

## 1. Visie & Strategie

### Het Ecosysteem Probleem

Elk nieuw OS sterft aan de kip-en-ei situatie: geen apps omdat geen
gebruikers, geen gebruikers omdat geen apps. De enige succesvolle
doorbraken in OS geschiedenis losten dit op via compatibiliteit:

- **macOS** → Classic environment (OS 9 apps), later Rosetta (PowerPC → x86)
- **Windows** → DOS compatibiliteit, later Win16 → Win32 → Win64
- **Android** → NDK voor native code, later Play Games voor PC
- **Linux desktop** → WINE voor Windows apps, later Proton voor gaming

EuroOS kiest een vierde weg: **Linux compatibiliteit**, omdat de open
source Linux app catalogus de grootste vrij beschikbare softwarebibliotheek
ter wereld is — zonder licentiekosten, zonder vendor lock-in.

### Vier Lagen van Compatibiliteit

```
┌─────────────────────────────────────────────────────────────┐
│  Laag 4: WASM Runtime                                       │
│  Toekomstbestendige sandbox voor nieuwe generatie apps      │
├─────────────────────────────────────────────────────────────┤
│  Laag 3: Flatpak/OCI Container Runtime                      │
│  Zelfbevattende app bundles — geen dependency hell          │
├─────────────────────────────────────────────────────────────┤
│  Laag 2: X11/Wayland Bridge                                 │
│  Linux GUI apps in EuroDesktop vensters                     │
├─────────────────────────────────────────────────────────────┤
│  Laag 1: Linux ABI Compatibiliteitslaag                     │
│  Linux syscalls → EuroKernel syscalls                       │
│  Bestaande Linux ELF binaries draaien zonder aanpassing     │
└─────────────────────────────────────────────────────────────┘
         ↑
   EuroKernel + EuroFS + EuroNet (bestaand)
```

### Wat dit Geeft — Dag 1 na Implementatie

Met alleen Laag 1 + Laag 2 zijn de volgende apps beschikbaar
**zonder enige aanpassing of hercompilatie:**

```
Browsers:       Firefox, Chromium, Brave, Vivaldi
Office:         LibreOffice, Euro-Office Desktop, OnlyOffice
Code editors:   VS Code, Cursor, Zed, Helix, Neovim, Vim
Terminal:       Alacritty, kitty, WezTerm, foot
Communicatie:   Signal, Telegram, Discord, Slack, Element
Media:          VLC, mpv, Spotify, Rhythmbox
Development:    git, python3, nodejs, rustc, gcc, docker
Utilities:      bash, curl, wget, htop, tmux, screen
Productivity:   Obsidian, Joplin, Standard Notes
```

Dit is de volledige productiviteitsstack van een moderne developer
of kenniswerker — beschikbaar op dag 1.

---

## 2. Laag 1 — Linux ABI Compatibiliteitslaag

### 2.1 Architectuur

De Linux ABI compat laag zit **in de kernel** — niet als userspace wrapper.
Wanneer EuroOS een Linux ELF binary laadt, detecteert het dit en
activeert de syscall vertaallaag voor dat specifieke proces.

```
Linux ELF binary start
  → EuroOS ELF loader detecteert Linux ABI (via ELF interpreter path)
  → Proces krijgt flag: LINUX_COMPAT_MODE = true
  → Bij elke SYSCALL instructie:
      → Normale pad: EuroKernel syscall tabel
      → Linux compat pad: Linux → EuroKernel vertaling
  → Resultaten worden terugvertaald naar Linux verwachtingen
```

### 2.2 ELF Binary Detectie

```rust
// kernel/src/compat/linux/loader.rs

/// Detecteer of een ELF binary een Linux binary is
/// Kijkt naar de ELF interpreter string en OS ABI veld
pub fn is_linux_binary(elf: &Elf64) -> bool {
    // Methode 1: OS/ABI veld in ELF header
    // 0x00 = System V (ook Linux), 0x03 = Linux specifiek
    if elf.header.os_abi == ElfOsAbi::Linux {
        return true;
    }

    // Methode 2: Interpreter path
    // Linux binaries gebruiken /lib64/ld-linux-x86-64.so.2
    // EuroOS binaries gebruiken /lib/ld-eurokernel.so
    if let Some(interp) = elf.interpreter() {
        if interp.contains("ld-linux") || interp.contains("ld-musl") {
            return true;
        }
    }

    // Methode 3: .note.ABI-tag sectie
    if let Some(note) = elf.find_section(".note.ABI-tag") {
        return note.contains_linux_abi_tag();
    }

    false
}

/// Laad een Linux binary in een EuroOS proces
pub fn load_linux_binary(path: &str) -> Result<Process, LoadError> {
    let elf = Elf64::parse(path)?;

    if !is_linux_binary(&elf) {
        return Err(LoadError::NotLinuxBinary);
    }

    let mut process = Process::new();

    // Activeer Linux compat modus voor dit proces
    process.flags |= ProcessFlags::LINUX_COMPAT;

    // Stel Linux-compatibele geheugenindeling in
    // Linux verwacht specifieke adressen voor stack, vdso, etc.
    setup_linux_memory_layout(&mut process, &elf)?;

    // Laad Linux vDSO (virtual Dynamic Shared Object)
    // Bevat snelle implementaties van clock_gettime, gettimeofday etc.
    load_linux_vdso(&mut process)?;

    // Laad EuroCompat glibc-compatibele libc
    load_compat_libc(&mut process)?;

    kinfo!("compat", &alloc::format!(
        "Linux binary geladen: {} (compat modus)", path
    ));

    Ok(process)
}
```

### 2.3 Syscall Vertaaltabel

Linux gebruikt andere syscall nummers dan EuroOS. De volledige
Linux x86-64 syscall tabel bevat 335+ entries. We implementeren
de meest gebruikte ~100 voor 95% app compatibiliteit.

```rust
// kernel/src/compat/linux/syscalls.rs

/// Linux x86-64 syscall nummers (uit linux/arch/x86/entry/syscalls/syscall_64.tbl)
mod linux_nr {
    pub const READ:         u64 = 0;
    pub const WRITE:        u64 = 1;
    pub const OPEN:         u64 = 2;
    pub const CLOSE:        u64 = 3;
    pub const STAT:         u64 = 4;
    pub const FSTAT:        u64 = 5;
    pub const LSTAT:        u64 = 6;
    pub const POLL:         u64 = 7;
    pub const LSEEK:        u64 = 8;
    pub const MMAP:         u64 = 9;
    pub const MPROTECT:     u64 = 10;
    pub const MUNMAP:       u64 = 11;
    pub const BRK:          u64 = 12;
    pub const RT_SIGACTION: u64 = 13;
    pub const RT_SIGPROCMASK: u64 = 14;
    pub const IOCTL:        u64 = 16;
    pub const PREAD64:      u64 = 17;
    pub const PWRITE64:     u64 = 18;
    pub const READV:        u64 = 19;
    pub const WRITEV:       u64 = 20;
    pub const ACCESS:       u64 = 21;
    pub const PIPE:         u64 = 22;
    pub const SELECT:       u64 = 23;
    pub const SCHED_YIELD:  u64 = 24;
    pub const MREMAP:       u64 = 25;
    pub const MADVISE:      u64 = 28;
    pub const DUP:          u64 = 32;
    pub const DUP2:         u64 = 33;
    pub const PAUSE:        u64 = 34;
    pub const NANOSLEEP:    u64 = 35;
    pub const GETITIMER:    u64 = 36;
    pub const ALARM:        u64 = 37;
    pub const SETITIMER:    u64 = 38;
    pub const GETPID:       u64 = 39;
    pub const SENDFILE:     u64 = 40;
    pub const SOCKET:       u64 = 41;
    pub const CONNECT:      u64 = 42;
    pub const ACCEPT:       u64 = 43;
    pub const SENDTO:       u64 = 44;
    pub const RECVFROM:     u64 = 45;
    pub const SENDMSG:      u64 = 46;
    pub const RECVMSG:      u64 = 47;
    pub const SHUTDOWN:     u64 = 48;
    pub const BIND:         u64 = 49;
    pub const LISTEN:       u64 = 50;
    pub const GETSOCKNAME:  u64 = 51;
    pub const GETPEERNAME:  u64 = 52;
    pub const SOCKETPAIR:   u64 = 53;
    pub const SETSOCKOPT:   u64 = 54;
    pub const GETSOCKOPT:   u64 = 55;
    pub const CLONE:        u64 = 56;
    pub const FORK:         u64 = 57;
    pub const VFORK:        u64 = 58;
    pub const EXECVE:       u64 = 59;
    pub const EXIT:         u64 = 60;
    pub const WAIT4:        u64 = 61;
    pub const KILL:         u64 = 62;
    pub const UNAME:        u64 = 63;
    pub const FCNTL:        u64 = 72;
    pub const FLOCK:        u64 = 73;
    pub const FSYNC:        u64 = 74;
    pub const FDATASYNC:    u64 = 75;
    pub const TRUNCATE:     u64 = 76;
    pub const FTRUNCATE:    u64 = 77;
    pub const GETDENTS:     u64 = 78;
    pub const GETCWD:       u64 = 79;
    pub const CHDIR:        u64 = 80;
    pub const FCHDIR:       u64 = 81;
    pub const RENAME:       u64 = 82;
    pub const MKDIR:        u64 = 83;
    pub const RMDIR:        u64 = 84;
    pub const CREAT:        u64 = 85;
    pub const LINK:         u64 = 86;
    pub const UNLINK:       u64 = 87;
    pub const SYMLINK:      u64 = 88;
    pub const READLINK:     u64 = 89;
    pub const CHMOD:        u64 = 90;
    pub const FCHMOD:       u64 = 91;
    pub const CHOWN:        u64 = 92;
    pub const FCHOWN:       u64 = 93;
    pub const LCHOWN:       u64 = 94;
    pub const UMASK:        u64 = 95;
    pub const GETTIMEOFDAY: u64 = 96;
    pub const GETRLIMIT:    u64 = 97;
    pub const GETRUSAGE:    u64 = 98;
    pub const SYSINFO:      u64 = 99;
    pub const TIMES:        u64 = 100;
    pub const PTRACE:       u64 = 101;
    pub const GETUID:       u64 = 102;
    pub const SYSLOG:       u64 = 103;
    pub const GETGID:       u64 = 104;
    pub const SETUID:       u64 = 105;
    pub const SETGID:       u64 = 106;
    pub const GETEUID:      u64 = 107;
    pub const GETEGID:      u64 = 108;
    pub const SETPGID:      u64 = 109;
    pub const GETPPID:      u64 = 110;
    pub const GETPGRP:      u64 = 111;
    pub const SETSID:       u64 = 112;
    pub const SETREUID:     u64 = 113;
    pub const SETREGID:     u64 = 114;
    pub const GETGROUPS:    u64 = 115;
    pub const SETGROUPS:    u64 = 116;
    pub const SETRESUID:    u64 = 117;
    pub const GETRESUID:    u64 = 118;
    pub const SETRESGID:    u64 = 119;
    pub const GETRESGID:    u64 = 120;
    pub const GETPGID:      u64 = 121;
    pub const SETFSUID:     u64 = 122;
    pub const SETFSGID:     u64 = 123;
    pub const GETSID:       u64 = 124;
    pub const CAPGET:       u64 = 125;
    pub const CAPSET:       u64 = 126;
    pub const RT_SIGSUSPEND: u64 = 130;
    pub const UTIME:        u64 = 132;
    pub const MKNOD:        u64 = 133;
    pub const STATFS:       u64 = 137;
    pub const FSTATFS:      u64 = 138;
    pub const GETPRIORITY:  u64 = 140;
    pub const SETPRIORITY:  u64 = 141;
    pub const PRCTL:        u64 = 157;
    pub const ARCH_PRCTL:   u64 = 158;
    pub const SETRLIMIT:    u64 = 160;
    pub const SYNC:         u64 = 162;
    pub const GETTID:       u64 = 186;
    pub const FUTEX:        u64 = 202;
    pub const SCHED_SETAFFINITY: u64 = 203;
    pub const SCHED_GETAFFINITY: u64 = 204;
    pub const EPOLL_CREATE: u64 = 213;
    pub const GETDENTS64:   u64 = 217;
    pub const SET_TID_ADDRESS: u64 = 218;
    pub const CLOCK_GETTIME: u64 = 228;
    pub const CLOCK_GETRES: u64 = 229;
    pub const CLOCK_NANOSLEEP: u64 = 230;
    pub const EXIT_GROUP:   u64 = 231;
    pub const EPOLL_WAIT:   u64 = 232;
    pub const EPOLL_CTL:    u64 = 233;
    pub const TGKILL:       u64 = 234;
    pub const OPENAT:       u64 = 257;
    pub const MKDIRAT:      u64 = 258;
    pub const MKNODAT:      u64 = 259;
    pub const FCHOWNAT:     u64 = 260;
    pub const FUTIMESAT:    u64 = 261;
    pub const NEWFSTATAT:   u64 = 262;
    pub const UNLINKAT:     u64 = 263;
    pub const RENAMEAT:     u64 = 264;
    pub const LINKAT:       u64 = 265;
    pub const SYMLINKAT:    u64 = 266;
    pub const READLINKAT:   u64 = 267;
    pub const FCHMODAT:     u64 = 268;
    pub const FACCESSAT:    u64 = 269;
    pub const PSELECT6:     u64 = 270;
    pub const PPOLL:        u64 = 271;
    pub const SET_ROBUST_LIST: u64 = 273;
    pub const GET_ROBUST_LIST: u64 = 274;
    pub const SPLICE:       u64 = 275;
    pub const EPOLL_PWAIT:  u64 = 281;
    pub const SIGNALFD:     u64 = 282;
    pub const TIMERFD_CREATE: u64 = 283;
    pub const EVENTFD:      u64 = 284;
    pub const FALLOCATE:    u64 = 285;
    pub const TIMERFD_SETTIME: u64 = 286;
    pub const TIMERFD_GETTIME: u64 = 287;
    pub const ACCEPT4:      u64 = 288;
    pub const SIGNALFD4:    u64 = 289;
    pub const EVENTFD2:     u64 = 290;
    pub const EPOLL_CREATE1: u64 = 291;
    pub const DUP3:         u64 = 292;
    pub const PIPE2:        u64 = 293;
    pub const INOTIFY_INIT1: u64 = 294;
    pub const PREADV:       u64 = 295;
    pub const PWRITEV:      u64 = 296;
    pub const RT_TGSIGQUEUEINFO: u64 = 297;
    pub const PERF_EVENT_OPEN: u64 = 298;
    pub const RECVMMSG:     u64 = 299;
    pub const PRLIMIT64:    u64 = 302;
    pub const SENDMMSG:     u64 = 307;
    pub const GETRANDOM:    u64 = 318;
    pub const MEMFD_CREATE: u64 = 319;
    pub const EXECVEAT:     u64 = 322;
    pub const COPY_FILE_RANGE: u64 = 326;
    pub const STATX:        u64 = 332;
    pub const PIDFD_OPEN:   u64 = 434;
    pub const CLONE3:       u64 = 435;
    pub const CLOSE_RANGE:  u64 = 436;
    pub const OPENAT2:      u64 = 437;
    pub const FACCESSAT2:   u64 = 439;
}

/// Hoofdvertaalfunctie — geroepen door syscall handler
/// als LINUX_COMPAT_MODE actief is voor huidig proces
pub fn translate_linux_syscall(
    nr: u64,
    a1: u64, a2: u64, a3: u64,
    a4: u64, a5: u64, a6: u64,
) -> i64 {
    match nr {
        // Directe mapping — zelfde semantiek, ander nummer
        linux_nr::READ         => euro_sys::read(a1 as i32, a2 as *mut u8, a3 as usize),
        linux_nr::WRITE        => euro_sys::write(a1 as i32, a2 as *const u8, a3 as usize),
        linux_nr::OPEN         => euro_sys::open(a1 as *const u8, a2 as u32, a3 as u32),
        linux_nr::CLOSE        => euro_sys::close(a1 as i32),
        linux_nr::STAT         => euro_sys::stat(a1 as *const u8, a2 as *mut Stat),
        linux_nr::FSTAT        => euro_sys::fstat(a1 as i32, a2 as *mut Stat),
        linux_nr::LSTAT        => euro_sys::lstat(a1 as *const u8, a2 as *mut Stat),
        linux_nr::LSEEK        => euro_sys::seek(a1 as i32, a2 as i64, a3 as u32),
        linux_nr::MMAP         => euro_sys::mmap(a1, a2, a3 as i32, a4 as i32, a5 as i32, a6 as i64),
        linux_nr::MPROTECT     => euro_sys::mprotect(a1, a2, a3 as i32),
        linux_nr::MUNMAP       => euro_sys::munmap(a1, a2),
        linux_nr::BRK          => euro_sys::brk(a1),
        linux_nr::FORK         => euro_sys::fork(),
        linux_nr::VFORK        => euro_sys::fork(), // vfork → fork (vereenvoudigd)
        linux_nr::EXECVE       => euro_sys::exec(a1 as *const u8, a2, a3),
        linux_nr::EXIT         => euro_sys::exit(a1 as i32),
        linux_nr::EXIT_GROUP   => euro_sys::exit(a1 as i32), // exit_group → exit
        linux_nr::WAIT4        => euro_sys::wait4(a1 as i32, a2 as *mut i32, a3 as i32, a4),
        linux_nr::KILL         => euro_sys::kill(a1 as i32, a2 as i32),
        linux_nr::GETPID       => euro_sys::getpid(),
        linux_nr::GETPPID      => euro_sys::getppid(),
        linux_nr::GETTID       => euro_sys::gettid(),
        linux_nr::GETUID       => euro_sys::getuid(),
        linux_nr::GETGID       => euro_sys::getgid(),
        linux_nr::GETEUID      => euro_sys::geteuid(),
        linux_nr::GETEGID      => euro_sys::getegid(),
        linux_nr::DUP          => euro_sys::dup(a1 as i32),
        linux_nr::DUP2         => euro_sys::dup2(a1 as i32, a2 as i32),
        linux_nr::DUP3         => euro_sys::dup3(a1 as i32, a2 as i32, a3 as i32),
        linux_nr::PIPE         => euro_sys::pipe(a1 as *mut [i32; 2]),
        linux_nr::PIPE2        => euro_sys::pipe2(a1 as *mut [i32; 2], a2 as i32),
        linux_nr::FCNTL        => euro_sys::fcntl(a1 as i32, a2 as i32, a3),
        linux_nr::IOCTL        => euro_sys::ioctl(a1 as i32, a2, a3),
        linux_nr::GETCWD       => euro_sys::getcwd(a1 as *mut u8, a2 as usize),
        linux_nr::CHDIR        => euro_sys::chdir(a1 as *const u8),
        linux_nr::MKDIR        => euro_sys::mkdir(a1 as *const u8, a2 as u32),
        linux_nr::RMDIR        => euro_sys::unlink(a1 as *const u8), // rmdir → unlink
        linux_nr::UNLINK       => euro_sys::unlink(a1 as *const u8),
        linux_nr::RENAME       => euro_sys::rename(a1 as *const u8, a2 as *const u8),
        linux_nr::LINK         => euro_sys::link(a1 as *const u8, a2 as *const u8),
        linux_nr::SYMLINK      => euro_sys::symlink(a1 as *const u8, a2 as *const u8),
        linux_nr::READLINK     => euro_sys::readlink(a1 as *const u8, a2 as *mut u8, a3 as usize),
        linux_nr::CHMOD        => euro_sys::chmod(a1 as *const u8, a2 as u32),
        linux_nr::CHOWN        => euro_sys::chown(a1 as *const u8, a2 as u32, a3 as u32),
        linux_nr::SOCKET       => euro_sys::socket(a1 as i32, a2 as i32, a3 as i32),
        linux_nr::CONNECT      => euro_sys::connect(a1 as i32, a2, a3 as u32),
        linux_nr::ACCEPT       => euro_sys::accept(a1 as i32, a2, a3 as *mut u32),
        linux_nr::ACCEPT4      => euro_sys::accept4(a1 as i32, a2, a3 as *mut u32, a4 as i32),
        linux_nr::BIND         => euro_sys::bind(a1 as i32, a2, a3 as u32),
        linux_nr::LISTEN       => euro_sys::listen(a1 as i32, a2 as i32),
        linux_nr::SENDTO       => euro_sys::sendto(a1 as i32, a2 as *const u8, a3, a4 as i32, a5, a6 as u32),
        linux_nr::RECVFROM     => euro_sys::recvfrom(a1 as i32, a2 as *mut u8, a3, a4 as i32, a5, a6 as *mut u32),
        linux_nr::SETSOCKOPT   => euro_sys::setsockopt(a1 as i32, a2 as i32, a3 as i32, a4, a5 as u32),
        linux_nr::GETSOCKOPT   => euro_sys::getsockopt(a1 as i32, a2 as i32, a3 as i32, a4, a5 as *mut u32),
        linux_nr::POLL         => euro_sys::poll(a1 as *mut PollFd, a2 as u32, a3 as i32),
        linux_nr::SELECT       => euro_sys::select(a1 as i32, a2, a3, a4, a5),
        linux_nr::EPOLL_CREATE | linux_nr::EPOLL_CREATE1 => euro_sys::epoll_create(a1 as i32),
        linux_nr::EPOLL_CTL    => euro_sys::epoll_ctl(a1 as i32, a2 as i32, a3 as i32, a4),
        linux_nr::EPOLL_WAIT | linux_nr::EPOLL_PWAIT => euro_sys::epoll_wait(a1 as i32, a2, a3 as i32, a4 as i32),
        linux_nr::NANOSLEEP    => euro_sys::nanosleep(a1 as *const Timespec, a2 as *mut Timespec),
        linux_nr::CLOCK_GETTIME => euro_sys::clock_gettime(a1 as i32, a2 as *mut Timespec),
        linux_nr::CLOCK_NANOSLEEP => euro_sys::clock_nanosleep(a1 as i32, a2 as i32, a3 as *const Timespec, a4 as *mut Timespec),
        linux_nr::GETTIMEOFDAY => euro_sys::gettimeofday(a1 as *mut Timeval, a2),
        linux_nr::FUTEX        => euro_compat::futex(a1, a2 as i32, a3 as i32, a4, a5, a6 as i32),
        linux_nr::CLONE        => euro_compat::clone(a1, a2, a3, a4, a5),
        linux_nr::CLONE3       => euro_compat::clone3(a1, a2),
        linux_nr::OPENAT       => euro_sys::openat(a1 as i32, a2 as *const u8, a3 as u32, a4 as u32),
        linux_nr::MKDIRAT      => euro_sys::mkdirat(a1 as i32, a2 as *const u8, a3 as u32),
        linux_nr::UNLINKAT     => euro_sys::unlinkat(a1 as i32, a2 as *const u8, a3 as i32),
        linux_nr::RENAMEAT     => euro_sys::renameat(a1 as i32, a2 as *const u8, a3 as i32, a4 as *const u8),
        linux_nr::NEWFSTATAT   => euro_sys::fstatat(a1 as i32, a2 as *const u8, a3 as *mut Stat, a4 as i32),
        linux_nr::GETDENTS64   => euro_sys::getdents64(a1 as i32, a2, a3 as u32),
        linux_nr::MADVISE      => 0, // Hint — negeer, retourneer succes
        linux_nr::SCHED_YIELD  => { euro_sys::sched_yield(); 0 },
        linux_nr::GETRANDOM    => euro_compat::getrandom(a1 as *mut u8, a2 as usize, a3 as u32),
        linux_nr::MEMFD_CREATE => euro_compat::memfd_create(a1 as *const u8, a2 as u32),
        linux_nr::PRCTL        => euro_compat::prctl(a1 as i32, a2, a3, a4, a5),
        linux_nr::ARCH_PRCTL   => euro_compat::arch_prctl(a1 as i32, a2),
        linux_nr::SET_TID_ADDRESS => euro_compat::set_tid_address(a1 as *mut u32),
        linux_nr::SET_ROBUST_LIST => 0, // Stub — retourneer succes
        linux_nr::GET_ROBUST_LIST => 0, // Stub
        linux_nr::RT_SIGACTION => euro_compat::rt_sigaction(a1 as i32, a2, a3, a4 as usize),
        linux_nr::RT_SIGPROCMASK => euro_compat::rt_sigprocmask(a1 as i32, a2, a3, a4 as usize),
        linux_nr::TGKILL       => euro_sys::kill(a2 as i32, a3 as i32), // tgkill → kill
        linux_nr::STATX        => euro_compat::statx(a1 as i32, a2 as *const u8, a3 as i32, a4 as u32, a5),
        linux_nr::PRLIMIT64    => euro_compat::prlimit64(a1 as i32, a2 as i32, a3, a4),
        linux_nr::UNAME        => euro_compat::uname(a1 as *mut LinuxUtsname),

        // Niet geïmplementeerd maar veilig negeren
        linux_nr::SYSLOG       => 0,
        linux_nr::PTRACE       => -EPERM,
        linux_nr::PERF_EVENT_OPEN => -ENOSYS,

        // Onbekend
        _ => {
            kwarn!("compat", &alloc::format!(
                "Onbekende Linux syscall: {} — retourneer ENOSYS", nr
            ));
            -ENOSYS
        }
    }
}
```

### 2.4 Semantische Verschillen — Speciale Gevallen

Sommige syscalls hebben subtiele gedragsverschillen die speciale
behandeling vereisen:

```rust
// kernel/src/compat/linux/special.rs

/// Linux uname — retourneert Linux versie info
/// Apps als Python en Node.js checken dit om features te detecteren
pub fn uname(buf: *mut LinuxUtsname) -> i64 {
    unsafe {
        let uts = &mut *buf;
        // Doe alsof we Linux zijn — apps verwachten dit
        copy_str(&mut uts.sysname,  b"Linux");
        copy_str(&mut uts.nodename, b"eurokernel");
        // Versie: recente Linux kernel — apps controleren minimale versie
        copy_str(&mut uts.release,  b"6.6.0-eurokernel");
        copy_str(&mut uts.version,  b"#1 EuroOS SMP");
        copy_str(&mut uts.machine,  b"x86_64");
        copy_str(&mut uts.domainname, b"(none)");
    }
    0
}

/// Linux /proc filesysteem emulatie
/// Veel apps lezen /proc/cpuinfo, /proc/meminfo, /proc/self/maps etc.
/// We implementeren een virtueel /proc op EuroFS

pub struct ProcFs {
    // Virtuele bestanden — gegenereerd on-demand
}

impl ProcFs {
    pub fn read_file(&self, path: &str, pid: Option<u64>) -> Option<Vec<u8>> {
        match path {
            "/proc/cpuinfo"   => Some(self.gen_cpuinfo()),
            "/proc/meminfo"   => Some(self.gen_meminfo()),
            "/proc/version"   => Some(b"Linux version 6.6.0-eurokernel".to_vec()),
            "/proc/mounts"    => Some(self.gen_mounts()),
            "/proc/stat"      => Some(self.gen_stat()),
            "/proc/uptime"    => Some(self.gen_uptime()),
            "/proc/loadavg"   => Some(self.gen_loadavg()),
            "/proc/filesystems" => Some(self.gen_filesystems()),
            p if p.starts_with("/proc/self/") => {
                self.read_self_file(p.strip_prefix("/proc/self/").unwrap())
            }
            p if p.starts_with("/proc/") => {
                // /proc/[pid]/...
                if let Some(pid_str) = p.split('/').nth(2) {
                    if let Ok(pid) = pid_str.parse::<u64>() {
                        return self.read_pid_file(pid, p);
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn gen_cpuinfo(&self) -> Vec<u8> {
        let cpu = CPU_INFO.get();
        alloc::format!(
            "processor\t: 0\n\
             vendor_id\t: {}\n\
             cpu family\t: 6\n\
             model name\t: {}\n\
             cpu MHz\t\t: {:.3}\n\
             cache size\t: {} KB\n\
             flags\t\t: fpu vme de pse tsc msr pae mce cx8 apic sep \
                          mtrr pge mca cmov pat pse36 clflush mmx fxsr \
                          sse sse2 ss ht syscall nx lm constant_tsc\n",
            cpu.vendor, cpu.model_name,
            cpu.freq_mhz as f64,
            cpu.cache_kb,
        ).into_bytes()
    }

    fn gen_meminfo(&self) -> Vec<u8> {
        let mem = EUROMM.stats();
        alloc::format!(
            "MemTotal:\t{} kB\n\
             MemFree:\t{} kB\n\
             MemAvailable:\t{} kB\n\
             Buffers:\t0 kB\n\
             Cached:\t\t{} kB\n\
             SwapTotal:\t0 kB\n\
             SwapFree:\t0 kB\n",
            mem.total_kb,
            mem.free_kb,
            mem.available_kb,
            mem.cached_kb,
        ).into_bytes()
    }
}

/// Futex implementatie — vereist voor pthreads
/// Futex is het fundamentele primitief achter mutex, condvar, rwlock in Linux
pub fn futex(
    uaddr: u64,
    op: i32,
    val: i32,
    timeout: u64,
    uaddr2: u64,
    val3: i32,
) -> i64 {
    let futex_op = op & 0x7F; // Strip private/realtime flags

    match futex_op {
        FUTEX_WAIT => {
            // Wacht tot *uaddr != val
            let current = unsafe { *(uaddr as *const i32) };
            if current != val {
                return -EAGAIN; // Al veranderd
            }
            // Slaap tot wake of timeout
            FUTEX_TABLE.wait(uaddr, val, timeout)
        }
        FUTEX_WAKE => {
            // Maak 'val' wachtende threads wakker
            FUTEX_TABLE.wake(uaddr, val as u32) as i64
        }
        FUTEX_WAIT_BITSET => {
            FUTEX_TABLE.wait_bitset(uaddr, val, timeout, val3 as u32)
        }
        FUTEX_WAKE_BITSET => {
            FUTEX_TABLE.wake_bitset(uaddr, val as u32, val3 as u32) as i64
        }
        FUTEX_REQUEUE => {
            FUTEX_TABLE.requeue(uaddr, val as u32, uaddr2, val3 as u32) as i64
        }
        _ => -ENOSYS,
    }
}
```

### 2.5 Glibc Compatibele Libc

Linux apps zijn gelinkt tegen glibc (GNU C Library). EuroOS gebruikt
musl. Er zijn subtiele verschillen. De compat libc biedt glibc-compatibele
interfaces bovenop musl:

```
crates/eurocompat-libc/
├── src/
│   ├── lib.rs              # Re-exports + glibc-specifieke toevoegingen
│   ├── string.rs           # glibc string extensies (strchrnul, etc.)
│   ├── stdio.rs            # glibc stdio uitbreidingen
│   ├── pthread.rs          # pthreads met glibc ABI
│   ├── dl.rs               # dlopen/dlsym/dlclose (dynamic loading)
│   ├── math.rs             # glibc math extensies
│   └── errno.rs            # glibc errno-locatie conventie
```

```c
/* eurocompat-libc: glibc-compatibele interface */

/* glibc stelt errno bloot via een thread-local functie */
/* musl doet dit anders — we bridgen dit */
int *__errno_location(void) {
    return &(current_thread()->errno_val);
}

/* glibc heeft __cxa_finalize voor C++ destructors */
void __cxa_finalize(void *dso_handle) {
    /* Roep geregistreerde destructors aan */
    run_atexit_handlers();
}

/* glibc __libc_start_main — wordt aangeroepen door CRT */
int __libc_start_main(
    int (*main)(int, char**, char**),
    int argc, char **argv,
    void (*init)(void),
    void (*fini)(void),
    void (*rtld_fini)(void),
    void *stack_end
) {
    /* Initialiseer glibc-compatibele omgeving */
    __init_tls();
    __init_ssp();  /* Stack smashing protector */

    if (init) init();

    /* Registreer cleanup */
    atexit(fini);
    if (rtld_fini) atexit(rtld_fini);

    return main(argc, argv, environ);
}
```

### 2.6 Dynamic Linker

Linux apps vereisen een dynamic linker (`ld-linux-x86-64.so.2`).
EuroOS biedt een compatibele implementatie:

```rust
// crates/eurocompat-linker/src/lib.rs

/// EuroOS dynamic linker — compatibel met Linux ld-linux-x86-64.so.2
/// Laadt gedeelde bibliotheken en lost symbolen op

pub struct DynamicLinker {
    loaded_libs: BTreeMap<String, LoadedLib>,
    search_paths: Vec<PathBuf>,
}

pub struct LoadedLib {
    pub name:    String,
    pub base:    u64,         // Laadadres in geheugen
    pub symbols: BTreeMap<String, u64>, // Naam → adres
    pub ref_count: u32,
}

impl DynamicLinker {
    /// Zoekpaden voor gedeelde bibliotheken
    /// Compatibel met Linux LD_LIBRARY_PATH conventie
    pub fn default_search_paths() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/lib"),
            PathBuf::from("/lib/x86_64-linux-gnu"),  // glibc compat pad
            PathBuf::from("/usr/lib"),
            PathBuf::from("/usr/local/lib"),
            PathBuf::from("/lib/eurokernel"),         // Eigen libs
        ]
    }

    /// Laad een gedeelde bibliotheek (dlopen equivalent)
    pub fn load(&mut self, name: &str) -> Result<*mut LoadedLib, DlError> {
        // Check al geladen
        if let Some(lib) = self.loaded_libs.get_mut(name) {
            lib.ref_count += 1;
            return Ok(lib as *mut _);
        }

        // Zoek bibliotheek in search paths
        let path = self.find_library(name)?;

        // Laad ELF
        let elf = Elf64::parse(&path)?;
        let base = self.map_library(&elf)?;

        // Verzamel symbolen
        let symbols = elf.exported_symbols()
            .map(|(name, offset)| (name.to_string(), base + offset))
            .collect();

        let lib = LoadedLib { name: name.to_string(), base, symbols, ref_count: 1 };
        self.loaded_libs.insert(name.to_string(), lib);

        // Verwerk relocaties
        self.apply_relocations(name)?;

        Ok(self.loaded_libs.get_mut(name).unwrap() as *mut _)
    }

    /// Zoek een symbool op (dlsym equivalent)
    pub fn symbol(&self, lib: *const LoadedLib, name: &str) -> Option<u64> {
        let lib = unsafe { &*lib };
        lib.symbols.get(name).copied()
    }
}
```

---

## 3. Laag 2 — X11/Wayland Bridge

### 3.1 Waarom X11 Emulatie

Vrijwel alle Linux GUI apps (GTK, Qt, Electron) gebruiken X11 of Wayland
als display protocol. EuroOS heeft een eigen compositor maar geen X11.
De bridge vertaalt X11 protocol berichten naar EuroDesktop compositor calls.

```
Linux GUI app
  → Opent connectie naar :0 (X11 display)
  → EuroXServer ontvangt verbinding via Unix socket
  → App stuurt X11 protocol berichten (XCreateWindow, XMapWindow, etc.)
  → EuroXServer vertaalt naar EuroDesktop IPC calls
  → EuroDesktop compositor rendert het venster
  → Input events gaan omgekeerde weg terug
```

### 3.2 EuroXServer Architectuur

```rust
// userland/euroxserver/src/main.rs

/// EuroXServer — minimale X11 server voor app compatibiliteit
/// Draait als userspace process, niet in de kernel
/// Luistert op /tmp/.X11-unix/X0 (standaard X11 socket pad)

pub struct EuroXServer {
    clients:     BTreeMap<ClientId, XClient>,
    windows:     BTreeMap<WindowId, XWindow>,
    compositor:  CompositorClient,   // IPC naar EuroDesktop
    socket:      UnixListener,
}

pub struct XWindow {
    pub id:       WindowId,
    pub client:   ClientId,
    pub parent:   Option<WindowId>,
    pub x: i16, pub y: i16,
    pub width: u16, pub height: u16,
    pub mapped:   bool,              // Zichtbaar?
    pub euro_surface: Option<SurfaceId>, // Corresponderende EuroDesktop surface
    pub attributes: WindowAttributes,
}

impl EuroXServer {
    pub fn run(&mut self) -> ! {
        loop {
            // Accept nieuwe clients
            self.accept_new_clients();

            // Verwerk berichten van bestaande clients
            for client_id in self.client_ids() {
                while let Some(request) = self.read_request(client_id) {
                    self.handle_request(client_id, request);
                }
            }

            // Forward events van compositor naar clients
            self.forward_compositor_events();
        }
    }

    fn handle_request(&mut self, client: ClientId, req: XRequest) {
        match req {
            XRequest::CreateWindow { wid, parent, x, y, width, height, .. } => {
                self.create_window(client, wid, parent, x, y, width, height);
            }
            XRequest::MapWindow { wid } => {
                self.map_window(wid);
                // Maak EuroDesktop surface aan
                let surface = self.compositor.create_surface(width, height);
                self.windows.get_mut(&wid).unwrap().euro_surface = Some(surface);
            }
            XRequest::UnmapWindow { wid } => {
                self.unmap_window(wid);
                if let Some(surface) = self.windows[&wid].euro_surface {
                    self.compositor.destroy_surface(surface);
                }
            }
            XRequest::ConfigureWindow { wid, x, y, width, height, .. } => {
                self.configure_window(wid, x, y, width, height);
                if let Some(surface) = self.windows[&wid].euro_surface {
                    self.compositor.resize_surface(surface, width, height);
                    self.compositor.move_surface(surface, x, y);
                }
            }
            XRequest::PutImage { wid, data, .. } => {
                // App schrijft pixels naar venster
                if let Some(surface) = self.windows[&wid].euro_surface {
                    self.compositor.update_surface_buffer(surface, &data);
                }
            }
            XRequest::CreateGC { gcid, .. }         => { self.create_gc(gcid); }
            XRequest::FreeGC { gcid }               => { self.free_gc(gcid); }
            XRequest::ChangeProperty { .. }         => { self.handle_property_change(req); }
            XRequest::InternAtom { name, .. }       => { self.intern_atom(client, &name); }
            XRequest::GetAtomName { atom }          => { self.get_atom_name(client, atom); }
            XRequest::SetInputFocus { wid, .. }     => { self.set_focus(wid); }
            XRequest::GetKeyboardMapping { .. }     => { self.send_keyboard_mapping(client); }
            XRequest::QueryExtension { name }       => {
                // Meld welke X11 extensies we ondersteunen
                self.send_extension_reply(client, &name);
            }
            _ => {
                // Onbekend verzoek — stuur NoError antwoord
                // Beter dan crashen voor onbekende verzoeken
                self.send_no_error(client, req.sequence());
            }
        }
    }
}
```

### 3.3 Input Event Forwarding

```rust
// Input events van EuroDesktop → X11 events naar apps

fn forward_input_event(&mut self, event: InputEvent) {
    match event {
        InputEvent::KeyPress { keycode, modifiers, .. } => {
            // Vertaal EuroOS keycode naar X11 keycode
            let x11_keycode = keycode_to_x11(keycode);
            let x11_state = modifiers_to_x11_state(modifiers);

            let focused = self.focused_window();
            if let Some(client) = self.client_for_window(focused) {
                self.send_to_client(client, XEvent::KeyPress {
                    keycode: x11_keycode,
                    state: x11_state,
                    window: focused,
                    root: self.root_window(),
                    time: current_time_ms(),
                    x: 0, y: 0,
                    x_root: 0, y_root: 0,
                });
            }
        }
        InputEvent::MouseMove { x, y, .. } => {
            // Stuur MotionNotify naar venster onder cursor
            if let Some(wid) = self.window_at(x as i16, y as i16) {
                let client = self.client_for_window(wid).unwrap();
                let (wx, wy) = self.window_relative_pos(wid, x, y);
                self.send_to_client(client, XEvent::MotionNotify {
                    window: wid,
                    x: wx, y: wy,
                    x_root: x as i16, y_root: y as i16,
                    state: self.current_button_state(),
                    time: current_time_ms(),
                });
            }
        }
        InputEvent::MouseButton { button, pressed, x, y } => {
            let event_type = if pressed { XEventType::ButtonPress } else { XEventType::ButtonRelease };
            let x11_button = mouse_button_to_x11(button);
            if let Some(wid) = self.window_at(x as i16, y as i16) {
                let client = self.client_for_window(wid).unwrap();
                self.send_to_client(client, XEvent::ButtonPress {
                    button: x11_button,
                    window: wid,
                    x: x as i16, y: y as i16,
                    x_root: x as i16, y_root: y as i16,
                    state: self.current_button_state(),
                    time: current_time_ms(),
                });
            }
        }
        _ => {}
    }
}
```

### 3.4 Wayland Bridge (Aanvullend)

Naast X11 bieden moderne apps ook Wayland ondersteuning.
Een Wayland compositor protocol implementatie op EuroDesktop:

```rust
// userland/eurowayland/src/main.rs

/// EuroWayland — Wayland compositor protocol implementatie
/// Draait als userspace service
/// Wayland apps verbinden via /run/wayland-0

/// Belangrijke Wayland interfaces die we implementeren:
/// - wl_compositor: aanmaken van surfaces
/// - wl_surface: buffer management
/// - wl_shm: shared memory buffers
/// - xdg_wm_base: venster decoraties en positie
/// - wl_seat: input (keyboard, pointer)
/// - wl_output: scherminfo

pub struct WaylandServer {
    display:    WlDisplay,
    compositor: Box<dyn EuroCompositor>,
    clients:    Vec<WaylandClient>,
}
```

---

## 4. Laag 3 — Flatpak/OCI Container Runtime

### 4.1 Concept

Flatpak apps zijn zelfbevattende bundles die hun eigen runtime
dependencies meebrengen. Dit lost het "dependency hell" probleem op —
EuroOS hoeft niet zelf GTK 3, GTK 4, Qt 5 én Qt 6 te leveren.

```
app.flatpak bevat:
  - app binary
  - eigen kopie van GTK/Qt/etc.
  - eigen kopie van glibc/musl
  - icon, desktop bestanden
  - sandbox policy
```

### 4.2 EuroBox — Container Runtime

```rust
// userland/eurobox/src/main.rs

/// EuroBox — minimale container/sandbox runtime
/// Compatibel met OCI (Docker) image formaat
/// Gebaseerd op Linux namespaces via onze compat laag

pub struct Container {
    pub id:         ContainerId,
    pub image:      OciImage,
    pub config:     ContainerConfig,
    pub state:      ContainerState,
    pub pid:        Option<u64>,
}

pub struct ContainerConfig {
    /// Geïsoleerde bestandssysteem view
    pub rootfs:     PathBuf,
    pub mounts:     Vec<BindMount>,

    /// Netwerk isolatie
    pub network:    NetworkMode,

    /// Process isolatie
    pub user:       String,
    pub env:        Vec<(String, String)>,
    pub cmd:        Vec<String>,

    /// Resource limits
    pub memory_mb:  Option<u32>,
    pub cpu_shares: Option<u32>,

    /// EuroGuard integratie
    pub capabilities: Vec<String>,
    pub sandbox_profile: SandboxProfile,
}

pub enum NetworkMode {
    None,           // Geen netwerktoegang
    Host,           // Deel host netwerk
    Bridge,         // Eigen netwerk namespace
}

pub struct BindMount {
    pub host_path:      PathBuf,
    pub container_path: PathBuf,
    pub read_only:      bool,
}

impl Container {
    pub fn start(&mut self) -> Result<u64, ContainerError> {
        // 1. Zet filesystem namespace op (chroot naar rootfs)
        self.setup_rootfs()?;

        // 2. Bind-mount /proc, /sys, /dev
        self.mount_proc()?;
        self.mount_dev()?;

        // 3. Start process in container
        let pid = fork_in_namespace(&self.config)?;

        self.pid = Some(pid);
        self.state = ContainerState::Running;

        Ok(pid)
    }
}
```

### 4.3 Flatpak Package Ondersteuning

```bash
# EuroOS kan Flatpak packages installeren en uitvoeren
# via de EuroBox container runtime

# Installeer een Flatpak package
eurobox install org.mozilla.firefox.flatpak

# Draai een Flatpak app
eurobox run org.mozilla.firefox

# Intern:
# 1. Unpack .flatpak (SquashFS archief)
# 2. Extraheer OCI-compatibele layered filesystem
# 3. Combineer app layer + runtime layer + platform layer
# 4. Start via EuroBox container runtime
# 5. EuroXServer of EuroWayland bridge voor GUI
```

---

## 5. Laag 4 — WebAssembly Runtime

### 5.1 WASM als Toekomstbestendige Sandbox

```rust
// crates/eurowasm/src/lib.rs

/// EuroWasm — WebAssembly runtime voor EuroOS
/// Gebaseerd op Wasmtime (Rust, open source, bytecode-alliance)
/// WASI (WebAssembly System Interface) als POSIX equivalent

pub struct WasmRuntime {
    engine: wasmtime::Engine,
    store:  wasmtime::Store<WasiCtx>,
}

impl WasmRuntime {
    pub fn new() -> Self {
        let engine = wasmtime::Engine::default();
        let wasi = WasiCtxBuilder::new()
            .inherit_stdio()
            .preopened_dir("/home", "/home")  // Sandbox: alleen home toegankelijk
            .build();
        let store = wasmtime::Store::new(&engine, wasi);
        Self { engine, store }
    }

    /// Voer een WASM module uit
    pub fn run(&mut self, wasm_path: &str, args: &[&str]) -> Result<(), WasmError> {
        let module = wasmtime::Module::from_file(&self.engine, wasm_path)?;
        let linker = self.setup_wasi_linker()?;
        let instance = linker.instantiate(&mut self.store, &module)?;

        let main = instance.get_typed_func::<(), ()>(&mut self.store, "_start")?;
        main.call(&mut self.store, ())?;

        Ok(())
    }
}
```

### 5.2 WASM App Formaat voor EuroStore

```toml
# app.eupkg met WASM binary

[package]
name = "mijnapp"
version = "1.0.0"
format = "wasm"  # Native eupkg = "elf", Linux compat = "linux-elf", WASM = "wasm"

[wasm]
binary = "bin/app.wasm"
runtime = "wasmtime"
wasi_version = "preview2"

[sandbox]
# WASM is gesandboxed by design — minimale permissies nodig
filesystem = ["~/Documents", "~/Downloads"]
network = false
```

---

## 6. EuroCompat Store Integratie

### 6.1 Meerdere App Formaten in EuroStore

```
EuroStore ondersteunt vier app formaten:

🔵 Native (.eupkg, ELF voor EuroOS)
   → Beste performance, diepste integratie
   → EuroUI design, EuroGuard permissies
   → Gebouwd specifiek voor EuroOS

🟢 Linux Compat (.eupkg, Linux ELF)
   → Bestaande Linux apps zonder aanpassing
   → Draait via Linux ABI compat laag
   → X11/Wayland bridge voor GUI

🟡 Container (.eurobox, OCI formaat)
   → Flatpak-achtige zelfbevattende bundles
   → Eigen runtime dependencies
   → Sterkste isolatie

🟣 WASM (.eupkg, .wasm binary)
   → Toekomstbestendige portable apps
   → Gesandboxed by design
   → Draait op elk platform
```

### 6.2 App Metadata en EuroGuard Integratie

```toml
# MANIFEST.toml voor een Linux compat app (bijv. Firefox)

[package]
name = "firefox"
display_name = "Firefox"
version = "126.0"
format = "linux-compat"      # Signaleert Linux ABI compat modus
source = "mozilla.org"
license = "MPL-2.0"

[compat]
requires_x11 = true          # EuroXServer opstarten
requires_gl = false          # OpenGL — indien true: llvmpipe software renderer
libc = "glibc-2.38"          # Welke glibc versie binary verwacht
arch = "x86_64"

[permissions]
# Ondanks compat modus — EuroGuard permissies gelden nog steeds
net.internet = "always"
fs.downloads.write = "always"
fs.home.read = "deny"
hw.camera = "ask"
hw.microphone = "ask"
sys.notifications = "always"

[sandbox]
# Extra isolatie voor compat apps
# Linux compat apps krijgen automatisch strengere sandbox
profile = "browser"
block_host_fs = true         # Alleen toegestane paden
no_new_privileges = true
```

---

## 7. EuroCompat Manager — Gebruikersinterface

### 7.1 Compat Status in EuroGuard

EuroGuard toont voor elke app welk compat formaat het gebruikt:

```
┌─────────────────────────────────────────────────────────────┐
│  🛡️ EuroGuard — Applicaties                                 │
│                                                             │
│  Firefox                                    [Details →]     │
│  🟢 Linux Compat  ·  X11 Bridge actief                      │
│  Netwerk: Altijd   Camera: Geweigerd                        │
│                                                             │
│  EuroMail                                   [Details →]     │
│  🔵 Native EuroOS  ·  Volledig geïntegreerd                 │
│  Netwerk: Altijd   Contacten: Altijd                        │
│                                                             │
│  Obsidian                                   [Details →]     │
│  🟢 Linux Compat  ·  Electron/X11                           │
│  Netwerk: Geweigerd   Bestanden: ~/Documents                │
│                                                             │
│  Rekenmachine                               [Details →]     │
│  🟣 WASM  ·  Gesandboxed                                    │
│  Netwerk: Geen   Bestanden: Geen                            │
└─────────────────────────────────────────────────────────────┘
```

---

## 8. Roadmap & Budget Track 8

### Fasering

| Fase | Inhoud | Na | Budget |
|---|---|---|---|
| 8.1 | Linux syscall vertaaltabel (kern 100) | Run 6 | €120.000 |
| 8.2 | ELF Linux binary loader + detectie | Run 6 | €60.000 |
| 8.3 | /proc virtueel filesysteem | Run 6 | €45.000 |
| 8.4 | Futex implementatie + pthreads compat | Run 6 | €90.000 |
| 8.5 | Glibc-compat libc + dynamic linker | Run 12 | €150.000 |
| 8.6 | EuroXServer v0.1 (CLI apps) | Run 7 | €90.000 |
| 8.7 | EuroXServer v0.2 (GUI apps + X11 proto) | Run 9 | €180.000 |
| 8.8 | EuroWayland bridge | Run 9 | €150.000 |
| 8.9 | EuroBox container runtime | Run 12 | €120.000 |
| 8.10 | EuroWasm runtime (Wasmtime integratie) | Run 13 | €90.000 |
| 8.11 | EuroStore meerdere formaten | Run 12 | €60.000 |
| **Totaal** | | | **€1.155.000** |

### Mijlpalen

| Mijlpaal | Wat werkt |
|---|---|
| Na Fase 8.1-8.4 | bash, curl, python, git draaien op EuroOS |
| Na Fase 8.5 | Vrijwel alle Linux CLI apps draaien |
| Na Fase 8.6 | Terminal apps (tmux, vim, htop) met X11 |
| Na Fase 8.7 | Firefox, Chromium, LibreOffice draaien |
| Na Fase 8.8 | Moderne Wayland-native apps draaien |
| Na Fase 8.9 | Flatpak apps installeerbaar en uitvoerbaar |
| Na Fase 8.10 | WASM apps in EuroStore |

---

## 9. Claude Code Build Prompt — Fase 8.1: Linux Syscall Kern

> **Geef sectie 9 aan Claude Code na Run 6 (VFS compleet).**
> Dit is de eerste stap — focus op de 50 meest gebruikte syscalls.

### Projectstructuur

```
kernel/src/compat/
├── mod.rs              # Compat module — activeer via kernel feature flag
├── linux/
│   ├── mod.rs          # Linux compat hoofdmodule
│   ├── detect.rs       # ELF Linux binary detectie
│   ├── syscalls.rs     # Volledige syscall vertaaltabel (zie sectie 2.3)
│   ├── special.rs      # Speciale cases: uname, futex, clone
│   ├── proc.rs         # /proc virtueel filesysteem
│   └── memory.rs       # Linux geheugenindeling setup

crates/eurocompat-libc/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── errno.rs
    ├── pthread.rs
    └── glibc_compat.rs

userland/euroxserver/
├── Cargo.toml
└── src/
    ├── main.rs
    ├── client.rs       # Client verbinding management
    ├── window.rs       # Window management
    ├── event.rs        # Event forwarding
    └── proto/          # X11 protocol parsing
        ├── mod.rs
        ├── requests.rs
        └── events.rs
```

### Integratie in Syscall Handler

```rust
// kernel/src/syscall/mod.rs — aanpassing

pub fn syscall_handler(
    nr: u64,
    a1: u64, a2: u64, a3: u64,
    a4: u64, a5: u64, a6: u64,
) -> i64 {
    let process = current_process();

    // Check of dit een Linux compat proces is
    if process.flags.contains(ProcessFlags::LINUX_COMPAT) {
        return crate::compat::linux::translate_linux_syscall(nr, a1, a2, a3, a4, a5, a6);
    }

    // Normale EuroOS syscall dispatch
    match nr {
        SYS_READ  => sys_read(a1 as i32, a2 as *mut u8, a3 as usize),
        SYS_WRITE => sys_write(a1 as i32, a2 as *const u8, a3 as usize),
        // ... rest van EuroOS syscalls
        _ => -ENOSYS,
    }
}
```

### Test Strategie

```bash
# Test 1: Eenvoudig hello world Linux binary
# Compileer op Linux:
echo 'int main() { write(1, "Hello EuroOS!\n", 14); return 0; }' > hello.c
gcc -static -o hello hello.c
# Kopieer naar EuroOS en probeer uit te voeren

# Test 2: Dynamisch gelinkt (vereist compat libc)
gcc -o hello_dynamic hello.c
# Vereist eurocompat-libc aanwezig in /lib

# Test 3: Bash
cp /bin/bash /tmp/bash_test
# Probeer te starten op EuroOS

# Test 4: Python
python3 -c "print('Hello from EuroOS compat!')"

# Test 5: curl
curl https://euro-os.eu/

# Successcriteria:
# - Hello world binary draait en print correct
# - Exit code correct teruggegeven
# - Bash start op en accepteert commando's
# - Python interpreter werkt
# - curl maakt netwerkaanvraag
```

---

## 10. Bedenkingen & Valkuilen

### Semantische Correctheid vs Snelheid

De verleiding is om snel veel syscalls te stubben (retourneer gewoon 0).
Dit werkt voor veel apps maar veroorzaakt subtiele bugs later die
moeilijk te debuggen zijn. Beter: implementeer 50 syscalls correct
dan 200 syscalls half.

Prioriteit voor correcte implementatie:
1. read, write, open, close, mmap, munmap — fundamenteel
2. fork, exec, wait, exit — procesmodel
3. futex — zonder dit werken geen threads
4. epoll/poll/select — zonder dit werkt geen async I/O
5. clock_gettime, nanosleep — timing

### Futex is Moeilijk

Futex is het meest complexe stuk. Het is het fundamentele primitief
achter elke mutex, condvar en rwlock in Linux userspace. Een incorrecte
futex implementatie veroorzaakt deadlocks, race conditions en crashes
die extreem moeilijk te debuggen zijn.

Aanpak: begin met een correcte maar trage implementatie (global lock),
optimaliseer daarna. Correctheid eerst.

### uname Spoofing

Apps als Python, Node.js en glibc zelf checken `uname()` om te
beslissen welke features beschikbaar zijn. Je moet "Linux" retourneren
met een recente kernel versie (minstens 4.19 voor moderne glibc).
Te oud → apps weigeren te starten.

### /proc is Cruciaal

Meer apps dan verwacht lezen /proc:
- `/proc/self/maps` — voor geheugeninspectie (Chromium, JVM)
- `/proc/self/exe` — voor self-location
- `/proc/cpuinfo` — voor CPU feature detectie
- `/proc/meminfo` — voor beschikbaar geheugen
- `/proc/sys/kernel/...` — voor kernel parameters

Begin met deze bestanden — ze ontblokkeren de meeste apps.

### Electron Apps

Electron (Chrome + Node.js) is de runtime voor VS Code, Discord,
Slack, Obsidian en tientallen andere populaire apps. Electron vereist:
- X11 of Wayland display
- OpenGL (minimaal software via llvmpipe)
- D-Bus (voor desktop integratie)
- libnotify (voor notificaties)

D-Bus is complex — overweeg een minimale stub implementatie.
llvmpipe (software OpenGL) compileert op EuroOS via compat laag.

### X11 is een Klein Protocol maar een Grote Implementatie

Het X11 protocol zelf is relatief eenvoudig — het zijn de extensions
die complex zijn. De meest gebruikte extensions die je moet ondersteunen:
- XFIXES — cursor hiding, region operations
- XINPUT2 — multi-touch, raw input
- XRANDR — resolutie en multi-monitor
- MIT-SHM — shared memory voor snelle pixel transfers
- COMPOSITE — voor compositing (Chromium gebruikt dit)
- DPMS — power management

Begin zonder extensions — veel apps werken al. Voeg extensions
toe op basis van specifieke app-problemen.

### Wayland vs X11 Volgorde

Bouw X11 bridge eerst — meer bestaande apps ondersteunen X11.
Wayland ondersteuning later toevoegen voor toekomstige apps.
Apps die alleen Wayland ondersteunen zijn nog zeldzaam in 2026.

### EuroGuard en Compat Apps

Linux compat apps hebben per definitie minder OS-integratie dan
native apps. EuroGuard moet ze automatisch strenger behandelen:
- Geen ambient filesystem toegang
- Netwerk expliciet toestaan per app
- X11 verbinding via EuroGuard gecontroleerd
- Sandbox profiel automatisch toepassen

Dit moet architectureel correct zijn van dag 1 — niet achteraf
toevoegen.

---

## 11. Checklist voor Claude Code

### Fase 8.1 (Linux Syscall Kern)
- [ ] `kernel/src/compat/linux/detect.rs` — ELF Linux binary detectie
- [ ] `kernel/src/compat/linux/syscalls.rs` — volledige vertaaltabel
- [ ] ProcessFlags::LINUX_COMPAT flag toevoegen aan Process struct
- [ ] Integratie in syscall_handler — check flag voor dispatch
- [ ] Eerste 50 syscalls correct geïmplementeerd (zie prioriteitslijst)
- [ ] Unit tests voor elke geïmplementeerde syscall

### Fase 8.2 (ELF Loader + /proc)
- [ ] `kernel/src/compat/linux/loader.rs` — Linux binary laden
- [ ] Linux geheugenindeling setup (vDSO, stack positie)
- [ ] `kernel/src/compat/linux/proc.rs` — /proc emulatie
- [ ] /proc/cpuinfo, /proc/meminfo, /proc/version minimum
- [ ] /proc/self/maps, /proc/self/exe, /proc/self/fd

### Fase 8.3 (Futex + Threads)
- [ ] `kernel/src/compat/linux/futex.rs` — futex implementatie
- [ ] FUTEX_WAIT, FUTEX_WAKE correct
- [ ] FUTEX_WAIT_BITSET, FUTEX_WAKE_BITSET
- [ ] clone() syscall met CLONE_THREAD, CLONE_VM, CLONE_FS flags
- [ ] set_tid_address, gettid implementatie

### Fase 8.4 (EuroXServer v0.1)
- [ ] `userland/euroxserver/src/` project aanmaken
- [ ] Unix socket op /tmp/.X11-unix/X0
- [ ] X11 protocol parser (big-endian, request/reply formaat)
- [ ] CreateWindow, MapWindow, UnmapWindow, DestroyWindow
- [ ] GetInputFocus, SetInputFocus
- [ ] InternAtom, GetAtomName (voor ICCCM compliance)
- [ ] XEvent forwarding van EuroDesktop input events
- [ ] Test: xterm of een eenvoudige X11 app start op
