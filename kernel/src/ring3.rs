//! Ring-3 userspace + echte syscalls (Track 3.4).
//!
//! Een userspace-programma (geladen uit EuroFS) draait in **ring 3** en roept
//! syscalls aan via `SYSCALL`:
//!   - `sys_write(ptr, len)` (nr 1): schrijf tekst naar de kernel-console
//!   - `sys_exit(code)`      (nr 0): stop het programma, terug naar de kernel
//!
//! `sys_write` keert via `SYSRET` terug naar ring 3 (het programma loopt door);
//! `sys_exit` keert terug naar de kernel-aanroeper. Privilege-scheiding + een
//! echte syscall round-trip, met een programma dat van schijf komt.

use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};

use alloc::string::String;
use alloc::vec::Vec;
use euromm::FrameAllocator;
use spin::Mutex;

// ── Capability-based security (security-spec) ─────────────────────────────
// Een proces krijgt exact de rechten die het nodig heeft; de kernel handhaaft
// dit op de syscall-grens (least privilege, géén root/non-root).
pub const CAP_CONSOLE: u64 = 1 << 0; // schrijven naar console
pub const CAP_PROC_INFO: u64 = 1 << 1; // getpid/uname
pub const CAP_FILE: u64 = 1 << 2; // open/read/close
pub const CAP_NET: u64 = 1 << 3; // netwerktoegang
pub const CAP_IMMUTABLE_ADMIN: u64 = 1 << 4; // L2: immutability-vlaggen zetten/wissen

static CURRENT_CAPS: AtomicU64 = AtomicU64::new(0);
// Als true: het huidige proces gebruikt de LINUX-syscall-ABI (andere nummers +
// semantiek). De kernel vertaalt dan naar z'n eigen handlers (Track 6 fase 6.6).
static LINUX_ABI: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// App-identiteit van het draaiende proces (argv[0], bv. "/bin/msock"). EuroGuard
// (Track 7) gebruikt dit om policy-beslissingen, statistieken en audit-events aan
// de juiste app toe te wijzen.
static CURRENT_APP: Mutex<String> = Mutex::new(String::new());

/// De app-identiteit van het huidige ring-3 proces (voor EuroGuard).
pub fn current_app() -> String {
    CURRENT_APP.lock().clone()
}

// Userspace-heap (voor sbrk/malloc): break-pointer + grens.
static HEAP_BREAK: AtomicU64 = AtomicU64::new(0);
static HEAP_END: AtomicU64 = AtomicU64::new(0);

/// Virtuele basis van de 2 MiB-arena van het lopende ring-3-proces (audit C1).
/// Gezet bij programmastart; gebruikt om user-pointers te valideren vóór de kernel
/// ze dereferentieert, zodat een proces geen kernel-geheugen kan laten lezen/schrijven.
static ARENA_BASE: AtomicU64 = AtomicU64::new(0);
/// De arena is 2 MiB groot (zelfde `MIB2` als de paging-laag).
const ARENA_SPAN: u64 = 2 * 1024 * 1024;

/// Ligt `[ptr, ptr+len)` volledig binnen de arena van het lopende proces?
/// (Overloop-veilig. Als er nog geen arena gezet is — puur kernel-interne aanroep —
/// staan we 't toe.)
fn in_user_arena(ptr: u64, len: usize) -> bool {
    let base = ARENA_BASE.load(Ordering::Relaxed);
    if base == 0 {
        return true; // geen ring-3-context actief
    }
    let top = base + ARENA_SPAN;
    match ptr.checked_add(len as u64) {
        Some(end) => ptr >= base && end <= top,
        None => false,
    }
}

/// `-EFAULT`: door een syscall teruggegeven als een meegeleverde user-pointer niet
/// volledig in de arena van het lopende proces ligt.
const EFAULT: u64 = (-14i64) as u64;

// ── Veilige user-geheugentoegang ──────────────────────────────────────────────
// ÉÉN poort naar/uit userspace. Elke functie controleert `[ptr, ptr+len)` met
// `in_user_arena` VÓÓR ze dereferentieert, zodat een proces nooit kernel-geheugen
// kan laten lezen of overschrijven door een vervalste pointer mee te geven. Alle
// syscall-handlers die een user-pointer aanraken MOETEN via deze helpers gaan —
// nooit rechtstreeks `as *mut`/`as *const` op een syscall-argument.

/// Kopieer `src` naar user-adres `dst`. `false` = pointer faalt de arena-check
/// (de aanroeper geeft dan `-EFAULT` terug); er wordt niets geschreven.
#[must_use]
fn copy_to_user(dst: u64, src: &[u8]) -> bool {
    if !in_user_arena(dst, src.len()) {
        return false;
    }
    // SAFETY: arena-gevalideerd; arena is identity-mapped en schrijfbaar.
    unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst as *mut u8, src.len()) };
    true
}

/// Lees `len` bytes vanaf user-adres `src`. `None` = pointer faalt de arena-check.
#[must_use]
fn copy_from_user(src: u64, len: usize) -> Option<alloc::vec::Vec<u8>> {
    if !in_user_arena(src, len) {
        return None;
    }
    let mut v = alloc::vec::Vec::with_capacity(len);
    // SAFETY: arena-gevalideerd; identity-mapped.
    unsafe {
        v.set_len(len);
        core::ptr::copy_nonoverlapping(src as *const u8, v.as_mut_ptr(), len);
    }
    Some(v)
}

/// Schrijf `len` nul-bytes naar user-adres `dst`. `false` = arena-check faalt.
#[must_use]
fn zero_user(dst: u64, len: usize) -> bool {
    if !in_user_arena(dst, len) {
        return false;
    }
    // SAFETY: arena-gevalideerd; identity-mapped.
    unsafe { core::ptr::write_bytes(dst as *mut u8, 0, len) };
    true
}

/// Schrijf een scalair (`u32`/`u64`/…) op user-adres `ptr`. `false` = arena-check faalt.
#[must_use]
fn write_user<T: Copy>(ptr: u64, val: T) -> bool {
    if !in_user_arena(ptr, core::mem::size_of::<T>()) {
        return false;
    }
    // SAFETY: arena-gevalideerd; `write_unaligned` vereist geen alignment.
    unsafe { (ptr as *mut T).write_unaligned(val) };
    true
}

/// Lees een scalair van user-adres `ptr`. `None` = arena-check faalt.
#[must_use]
fn read_user<T: Copy>(ptr: u64) -> Option<T> {
    if !in_user_arena(ptr, core::mem::size_of::<T>()) {
        return None;
    }
    // SAFETY: arena-gevalideerd; `read_unaligned` vereist geen alignment.
    Some(unsafe { (ptr as *const T).read_unaligned() })
}

fn has_cap(c: u64) -> bool {
    CURRENT_CAPS.load(Ordering::Relaxed) & c == c
}

/// De capability die een syscall vereist (0 = altijd toegestaan).
fn required_cap(num: u64) -> u64 {
    match num {
        1 => CAP_CONSOLE,
        2 | 4 => CAP_PROC_INFO,
        20 | 21 | 22 => CAP_FILE,
        60 => CAP_NET,
        _ => 0, // exit (0) e.d. altijd toegestaan
    }
}
use x86_64::registers::control::{Cr4, Cr4Flags};
use x86_64::registers::model_specific::Msr;

use crate::serial_print;

// Door de assembly-stubs gedeelde globals (single-threaded; userspace draait
// vóór de scheduler).
#[no_mangle]
static mut SAVED_KERNEL_RSP: u64 = 0; // terugkeerpunt voor sys_exit
#[no_mangle]
static mut KERNEL_RSP: u64 = 0; // stack voor de syscall-handler
#[no_mangle]
static mut USER_RSP: u64 = 0; // bewaarde user-rsp tijdens een syscall
#[no_mangle]
static mut USER_RIP: u64 = 0; // bewaarde user-rip (clone: thread-resume-punt)
#[no_mangle]
static mut SAVED_REGS: u64 = 0; // wijst naar het opgeslagen registerblok (clone: child erft de regs)
#[no_mangle]
static mut EXITED: u64 = 0; // door sys_exit gezet

static mut EXIT_CODE: u64 = 0;
static OUTPUT: Mutex<String> = Mutex::new(String::new());

/// Het systeemmilieu (omgevingsvariabelen) dat elk ring-3 proces erft via `envp`
/// op de SysV-stack. Programma's lezen dit met `getenv()` (musl/libc).
static ENV: Mutex<alloc::vec::Vec<String>> = Mutex::new(alloc::vec::Vec::new());

/// Stel het systeemmilieu in (vervangt de huidige set). Bij boot ingesteld.
pub fn set_env(vars: &[&str]) {
    let mut e = ENV.lock();
    e.clear();
    for v in vars {
        e.push(String::from(*v));
    }
}

/// Voeg één omgevingsvariabele "KEY=value" toe (of vervang een bestaande met
/// dezelfde sleutel). Voor runtime-bepaalde waarden, bv. een DNS-resultaat.
pub fn push_env(entry: &str) {
    let key = match entry.split_once('=') {
        Some((k, _)) => k,
        None => entry,
    };
    let mut e = ENV.lock();
    e.retain(|v| v.split_once('=').map(|(k, _)| k) != Some(key));
    e.push(String::from(entry));
}

/// Optionele stdout-omleiding: als gezet, gaat alles wat het proces naar fd 1/2
/// schrijft naar dit VFS-bestand (index in FILES) i.p.v. de console. Zo doet de
/// shell `prog > bestand` / `prog >> bestand` (redirectie) af.
static STDOUT_REDIRECT: Mutex<Option<usize>> = Mutex::new(None);

// Minimale VFS voor userspace-file-I/O: bestanden (pad, inhoud) geladen uit
// EuroFS, plus een open-file-descriptor-tabel. Syscalls open/read/close hierop.
static FILES: Mutex<alloc::vec::Vec<(String, alloc::vec::Vec<u8>)>> = Mutex::new(alloc::vec::Vec::new());
const MAX_FD: usize = 16;
static OPEN_FDS: Mutex<[Option<(usize, usize)>; MAX_FD]> = Mutex::new([None; MAX_FD]);
/// Open DIRECTORY-fds (Linux getdents64): (genormaliseerd dir-pad, cursor in de
/// kinderlijst). Aparte tabel zodat een dir-fd niet als bestand wordt gelezen.
static OPEN_DIRS: Mutex<[Option<(String, usize)>; MAX_FD]> =
    Mutex::new([const { None }; MAX_FD]);

// ── PIPES (S3 IPC tussen processen) ─────────────────────────────────────────
// Een pipe is een in-kernel FIFO-buffer met twee uiteinden (lees/schrijf). De
// `pipe2`-syscall geeft twee fds terug; na fork() delen ouder en kind ze (de
// fd-tabellen zijn globaal), dus ze kunnen via de pipe communiceren.
static PIPES: Mutex<alloc::vec::Vec<alloc::vec::Vec<u8>>> = Mutex::new(alloc::vec::Vec::new());
/// Pipe-fds: per fd (pipe-id, is_write_end). Aparte tabel naast bestands-/dir-fds.
static PIPE_FDS: Mutex<[Option<(usize, bool)>; MAX_FD]> = Mutex::new([None; MAX_FD]);

/// pipe2(fds, flags): maak een pipe; ken een lees- en een schrijf-fd toe en schrijf
/// ze naar `fds[0]`/`fds[1]`. Geeft 0 / -EMFILE.
fn pipe_create(user_fds: u64) -> u64 {
    let id = {
        let mut p = PIPES.lock();
        p.push(alloc::vec::Vec::new());
        p.len() - 1
    };
    let files = OPEN_FDS.lock();
    let dirs = OPEN_DIRS.lock();
    let mut pf = PIPE_FDS.lock();
    let mut got = [usize::MAX; 2];
    let mut k = 0;
    for fd in 3..MAX_FD {
        if pf[fd].is_none() && files[fd].is_none() && dirs[fd].is_none() {
            got[k] = fd;
            k += 1;
            if k == 2 {
                break;
            }
        }
    }
    if k < 2 {
        return (-24i64) as u64; // -EMFILE
    }
    // Valideer de uitvoer-pointer (int[2]) VÓÓR we de fds vastleggen, zodat een
    // vervalste `fds` geen kernel-geheugen overschrijft.
    let fds = [got[0] as i32, got[1] as i32];
    if !in_user_arena(user_fds, 8) {
        return EFAULT;
    }
    pf[got[0]] = Some((id, false)); // leesuiteinde
    pf[got[1]] = Some((id, true)); // schrijfuiteinde
    let _ = write_user(user_fds, fds[0]);
    let _ = write_user(user_fds + 4, fds[1]);
    0
}

/// Schrijf naar een pipe-fd (schrijfuiteinde). None = `fd` is geen pipe-schrijf-fd.
fn pipe_write_fd(fd: usize, bytes: &[u8]) -> Option<u64> {
    if fd >= MAX_FD {
        return None;
    }
    if let Some((id, true)) = PIPE_FDS.lock()[fd] {
        PIPES.lock()[id].extend_from_slice(bytes);
        return Some(bytes.len() as u64);
    }
    None
}

/// Lees uit een pipe-fd (leesuiteinde). Leeg -> -EAGAIN (de lezer polt). None = geen
/// pipe-lees-fd.
fn pipe_read_fd(fd: usize, buf: u64, len: usize) -> Option<u64> {
    if fd >= MAX_FD {
        return None;
    }
    if let Some((id, false)) = PIPE_FDS.lock()[fd] {
        let mut pipes = PIPES.lock();
        let p = &mut pipes[id];
        if p.is_empty() {
            return Some((-11i64) as u64); // -EAGAIN
        }
        let n = len.min(p.len());
        // Valideer de doel-buffer VÓÓR we uit de pipe consumeren; bij een
        // vervalste pointer faalt de read zonder data te verliezen of kernel-
        // geheugen te raken.
        if !in_user_arena(buf, n) {
            return Some(EFAULT);
        }
        let data: alloc::vec::Vec<u8> = p.drain(0..n).collect();
        let _ = copy_to_user(buf, &data);
        if let Ok(s) = core::str::from_utf8(&data) {
            crate::kinfo!("[pipe] fd {fd} las {n} bytes uit pipe {id}: \"{s}\"");
        }
        return Some(n as u64);
    }
    None
}

/// Geef het huidige proces een VERSE fd-tabel (fd 0/1/2 impliciet console/VFS).
/// In het synchrone voorgrondmodel draait er één proces tegelijk, dus dit geeft
/// echte per-proces fd-semantiek: open fds lekken niet tussen programma's door.
fn reset_fd_table() {
    *OPEN_FDS.lock() = [None; MAX_FD];
    for slot in OPEN_DIRS.lock().iter_mut() {
        *slot = None;
    }
}

/// Registreer een bestand (pad + inhoud) zodat userspace het via open/read kan lezen.
pub fn register_file(path: &str, bytes: alloc::vec::Vec<u8>) {
    FILES.lock().push((String::from(path), bytes));
}

/// Programmaregister: per uitvoerbaar pad de toegekende capabilities en de ABI
/// (native EuroOS of Linux). Hiermee kan een shell een binary op NAAM starten en
/// weet de kernel met welke rechten + syscall-ABI die moet draaien.
static PROGRAMS: Mutex<alloc::vec::Vec<(String, u64, bool)>> = Mutex::new(alloc::vec::Vec::new());

/// Installeer een uitvoerbaar bestand: leg caps + ABI vast voor latere `exec`.
pub fn register_program(path: &str, caps: u64, linux_abi: bool) {
    let mut p = PROGRAMS.lock();
    if let Some(e) = p.iter_mut().find(|(q, _, _)| q == path) {
        e.1 = caps;
        e.2 = linux_abi;
    } else {
        p.push((String::from(path), caps, linux_abi));
    }
}

/// Zoek de caps + ABI van een geïnstalleerd programma op (None = onbekend).
pub fn program_caps_abi(path: &str) -> Option<(u64, bool)> {
    PROGRAMS
        .lock()
        .iter()
        .find(|(q, _, _)| q == path)
        .map(|(_, c, a)| (*c, *a))
}

/// Alle geïnstalleerde programma's met hun toegekende capabilities + ABI-vlag —
/// voor het `caps`-overzicht dat het NATIEVE EuroGuard-beveiligingsmodel toont.
pub fn program_list() -> alloc::vec::Vec<(String, u64, bool)> {
    PROGRAMS.lock().clone()
}

/// Decodeer een capability-bitmasker naar leesbare namen (EuroGuard-rechten).
pub fn cap_names(caps: u64) -> String {
    let mut v: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
    if caps & CAP_CONSOLE != 0 {
        v.push("console");
    }
    if caps & CAP_PROC_INFO != 0 {
        v.push("procinfo");
    }
    if caps & CAP_FILE != 0 {
        v.push("file");
    }
    if caps & CAP_NET != 0 {
        v.push("net");
    }
    if v.is_empty() {
        v.push("(geen)");
    }
    v.join(" ")
}

/// /proc-synthese (Track 8.2): genereer LIVE de inhoud van bekende /proc-bestanden
/// (version/cpuinfo/meminfo/uptime/self/maps) en zet die in de VFS, zodat Linux-
/// programma's die /proc lezen echte waarden krijgen i.p.v. -ENOENT. Geeft true als
/// `path` een /proc-bestand is dat nu (vers gegenereerd) in de VFS staat.
fn ensure_proc(path: &[u8]) -> bool {
    if !path.starts_with(b"/proc/") {
        return false;
    }
    let cores = crate::smp::AP_ONLINE.load(Ordering::Relaxed) + 1;
    let up = crate::interrupts::ticks() / 100;
    let (_used, free) = crate::allocator::stats();
    let content: alloc::vec::Vec<u8> = match path {
        b"/proc/version" => {
            b"Linux version 6.6.0-euroos (euro@eurokernel) (EuroToolchain rustc) #1 SMP EuroOS\n"
                .to_vec()
        }
        b"/proc/cpuinfo" => {
            let mut s = String::new();
            for i in 0..cores {
                s.push_str(&alloc::format!(
                    "processor\t: {i}\nvendor_id\t: EuroOS\nmodel name\t: EuroOS Virtual CPU @ LAPIC\n\
                     flags\t\t: fpu tsc msr apic sse sse2 long\ncores\t\t: {cores}\n\n"
                ));
            }
            s.into_bytes()
        }
        b"/proc/meminfo" => {
            // MemTotal: de QEMU-RAM (256 MiB). MemFree/Available: live kernel-heap-vrij.
            let free_kb = free as u64 / 1024;
            alloc::format!(
                "MemTotal:       262144 kB\nMemFree:        {free_kb:>8} kB\n\
                 MemAvailable:   {free_kb:>8} kB\nBuffers:             0 kB\nCached:              0 kB\n"
            )
            .into_bytes()
        }
        b"/proc/uptime" => alloc::format!("{up}.00 {up}.00\n").into_bytes(),
        b"/proc/self/maps" => {
            // Eén regel voor het heap-venster van het huidige voorgrondproces.
            let lo = HEAP_BREAK.load(Ordering::Relaxed) & !0xFFF;
            let hi = (HEAP_END.load(Ordering::Relaxed) + 0xFFF) & !0xFFF;
            alloc::format!("{lo:012x}-{hi:012x} rw-p 00000000 00:00 0          [heap]\n").into_bytes()
        }
        b"/proc/self/stat" => {
            alloc::format!("1 (prog) R 0 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {up} 0 0\n").into_bytes()
        }
        b"/proc/self/cmdline" => {
            // NUL-getermineerde argv (hier alleen argv[0] = het programmapad).
            let mut v = CURRENT_APP.lock().clone().into_bytes();
            v.push(0);
            v
        }
        b"/proc/loadavg" => alloc::format!("0.00 0.00 0.00 1/{cores} 1\n").into_bytes(),
        b"/proc/stat" => {
            // Minimale cpu-regel + per-core regels (zoals tools als `top` lezen).
            let mut s = alloc::string::String::from("cpu  0 0 0 0 0 0 0 0 0 0\n");
            for i in 0..cores {
                s.push_str(&alloc::format!("cpu{i} 0 0 0 0 0 0 0 0 0 0\n"));
            }
            s.push_str(&alloc::format!("btime 0\nprocesses {cores}\n"));
            s.into_bytes()
        }
        _ => return false,
    };
    let p = String::from_utf8_lossy(path).into_owned();
    let mut files = FILES.lock();
    match files.iter_mut().find(|(q, _)| q.as_str() == p) {
        Some(e) => e.1 = content, // ververs bestaande /proc-inhoud
        None => files.push((p, content)),
    }
    true
}

/// Open een pad (bytes) in de VFS -> fd, of u64::MAX bij niet gevonden / vol.
/// Gedeeld door de native (sys_open) en Linux (openat) ABI.
fn vfs_open(path: &[u8]) -> u64 {
    ensure_proc(path); // /proc-bestanden vers genereren vóór de lookup
    let files = FILES.lock();
    match files.iter().position(|(p, _)| p.as_bytes() == path) {
        Some(fi) => {
            let mut fds = OPEN_FDS.lock();
            for (fd, slot) in fds.iter_mut().enumerate().skip(3) {
                if slot.is_none() {
                    *slot = Some((fi, 0));
                    return fd as u64;
                }
            }
            u64::MAX
        }
        None => u64::MAX,
    }
}

/// Lees uit een open fd naar een user-buffer -> aantal bytes (u64::MAX bij fout).
fn vfs_read(fd: usize, buf: u64, len: usize) -> u64 {
    if fd >= MAX_FD {
        return u64::MAX;
    }
    let mut fds = OPEN_FDS.lock();
    let (fi, off) = match fds[fd] {
        Some(x) => x,
        None => return u64::MAX,
    };
    let files = FILES.lock();
    let data = &files[fi].1;
    let n = len.min(data.len().saturating_sub(off));
    // Valideer de user-buffer vóór de kernel erin schrijft (audit C1): een proces mag
    // geen kernel-geheugen als doel opgeven.
    if !in_user_arena(buf, n) {
        return u64::MAX;
    }
    // SAFETY: buf ligt nu bewezen binnen de arena van het lopende proces.
    unsafe {
        core::ptr::copy_nonoverlapping(data[off..].as_ptr(), buf as *mut u8, n);
    }
    fds[fd] = Some((fi, off + n));
    n as u64
}

/// Paden die userspace voor schrijven opende — de shell schrijft deze na afloop
/// terug naar EuroFS (zodat ze persistent worden + in de bestandslijst verschijnen).
static DIRTY: Mutex<alloc::vec::Vec<String>> = Mutex::new(alloc::vec::Vec::new());

/// Open een pad voor SCHRIJVEN: maak het aan als het niet bestaat (O_CREAT),
/// kap het af bij `truncate` (O_TRUNC). Markeert het pad als 'dirty'.
fn vfs_open_create(path: &[u8], truncate: bool) -> u64 {
    let name = String::from_utf8_lossy(path).into_owned();
    {
        let mut files = FILES.lock();
        match files.iter_mut().find(|(p, _)| p.as_bytes() == path) {
            Some((_, d)) => {
                if truncate {
                    d.clear();
                }
            }
            None => files.push((name.clone(), alloc::vec::Vec::new())),
        }
    }
    let mut dirty = DIRTY.lock();
    if !dirty.iter().any(|p| p == &name) {
        dirty.push(name);
    }
    vfs_open(path)
}

/// Schrijf `len` bytes uit een user-buffer naar een open fd (in de VFS); de file
/// groeit zo nodig mee. Geeft het aantal geschreven bytes terug.
fn vfs_write(fd: usize, buf: u64, len: usize) -> u64 {
    if fd >= MAX_FD {
        return u64::MAX;
    }
    let mut fds = OPEN_FDS.lock();
    let (fi, off) = match fds[fd] {
        Some(x) => x,
        None => return u64::MAX,
    };
    // Valideer de user-buffer + overloop-veilige offsetberekening (audit C1/M9):
    // de kernel mag alleen uit de arena van het lopende proces lezen.
    if !in_user_arena(buf, len) {
        return u64::MAX;
    }
    let end = match off.checked_add(len) {
        Some(e) => e,
        None => return u64::MAX,
    };
    let mut files = FILES.lock();
    let data = &mut files[fi].1;
    if end > data.len() {
        data.resize(end, 0);
    }
    // SAFETY: buf ligt nu bewezen binnen de arena van het lopende proces.
    unsafe {
        core::ptr::copy_nonoverlapping(buf as *const u8, data[off..].as_mut_ptr(), len);
    }
    fds[fd] = Some((fi, end));
    len as u64
}

/// Haal de paden+inhoud op die userspace schreef sinds de vorige aanroep (en wis
/// de lijst). De shell gebruikt dit om EuroFS te synchroniseren na een `exec`.
pub fn take_dirty() -> alloc::vec::Vec<(String, alloc::vec::Vec<u8>)> {
    let paths: alloc::vec::Vec<String> = core::mem::take(&mut *DIRTY.lock());
    let files = FILES.lock();
    paths
        .into_iter()
        .filter_map(|p| {
            files
                .iter()
                .find(|(q, _)| q == &p)
                .map(|(_, d)| (p.clone(), d.clone()))
        })
        .collect()
}

/// Leid stdout (fd 1/2) om naar een VFS-bestand voor de duur van de volgende run
/// (shell-redirectie). `append`=true voegt toe (`>>`), anders afkappen (`>`).
/// `None` zet de console terug. Het pad wordt 'dirty' (de shell synct het).
pub fn set_stdout_redirect(path: Option<&str>, append: bool) {
    match path {
        Some(p) => {
            let idx = {
                let mut files = FILES.lock();
                match files.iter().position(|(q, _)| q.as_str() == p) {
                    Some(i) => {
                        if !append {
                            files[i].1.clear();
                        }
                        i
                    }
                    None => {
                        files.push((String::from(p), alloc::vec::Vec::new()));
                        files.len() - 1
                    }
                }
            };
            let mut dirty = DIRTY.lock();
            if !dirty.iter().any(|q| q == p) {
                dirty.push(String::from(p));
            }
            *STDOUT_REDIRECT.lock() = Some(idx);
        }
        None => *STDOUT_REDIRECT.lock() = None,
    }
}

/// Voeg bytes toe aan het stdout-omleidingsbestand (intern, voor write/writev).
fn redirect_append(fi: usize, bytes: &[u8]) {
    FILES.lock()[fi].1.extend_from_slice(bytes);
}

/// Standaardinvoer (fd 0): inhoud + leespositie. De shell vult dit met de stdout
/// van het vorige programma in een pipe (`a | b`); `read(0)` leest eruit.
static STDIN: Mutex<(alloc::vec::Vec<u8>, usize)> = Mutex::new((alloc::vec::Vec::new(), 0));

/// Zet de standaardinvoer voor de volgende run (pipe). Lege slice = geen invoer.
pub fn set_stdin(data: &[u8]) {
    let mut s = STDIN.lock();
    s.0 = data.to_vec();
    s.1 = 0;
}

/// Lees uit de standaardinvoer naar een user-buffer (fd 0). Geeft het aantal bytes.
fn stdin_read(buf: u64, len: usize) -> u64 {
    let mut s = STDIN.lock();
    let off = s.1;
    let n = len.min(s.0.len().saturating_sub(off));
    if !copy_to_user(buf, &s.0[off..off + n]) {
        return EFAULT;
    }
    s.1 = off + n;
    n as u64
}

/// Het aantal bytes dat nog in de standaardinvoer staat (voor fstat van fd 0).
fn stdin_len() -> usize {
    STDIN.lock().0.len()
}

// ── Achtergrond-daemon: een ring-3 programma dat PREEMPTIEF gescheduled draait
// (niet synchroon zoals run()) en periodiek syscalls doet. Zijn syscalls krijgen
// een EIGEN dispatcher + uitvoerbuffer, geselecteerd op de huidige scheduler-taak
// (zo botst het niet met de globale voorgrond-staat; voorgrond-execs draaien IF=0
// en kunnen dus nooit met de daemon overlappen). De daemon eindigt nooit, dus de
// lastige "sys_exit vanuit een gescheduelde taak" doet zich niet voor.
static DAEMON_TASK: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(usize::MAX);
static DAEMON_OUTPUT: Mutex<alloc::vec::Vec<String>> = Mutex::new(alloc::vec::Vec::new());
/// Onvolledige regel (de daemon schrijft een regel in meerdere write-calls).
static DAEMON_PARTIAL: Mutex<String> = Mutex::new(String::new());

/// De recente uitvoerregels van de achtergrond-daemon (voor weergave).
pub fn daemon_lines() -> alloc::vec::Vec<String> {
    DAEMON_OUTPUT.lock().clone()
}

/// Aparte syscall-dispatcher voor de daemon-taak (native ABI; eigen uitvoerbuffer).
fn daemon_dispatch(num: u64, a1: u64, _a2: u64, _a3: u64) -> u64 {
    // De daemon eindigt NOOIT: forceer EXITED=0 zodat `syscall_entry` na deze
    // syscall de normale SYSRET-terugkeer neemt en niet het sys_exit-pad met de
    // (voor de daemon ongeldige) SAVED_KERNEL_RSP van de laatste voorgrond-exec.
    unsafe { EXITED = 0 };
    match num {
        1 => {
            // sys_write(NUL-string): accumuleer in een regelbuffer; emit complete
            // regels (de daemon schrijft één regel in meerdere write-calls).
            let s = user_cstr(a1, 512);
            if let Ok(t) = core::str::from_utf8(&s) {
                let mut partial = DAEMON_PARTIAL.lock();
                partial.push_str(t);
                while let Some(nl) = partial.find('\n') {
                    let line: String = partial.drain(..=nl).collect();
                    let mut out = DAEMON_OUTPUT.lock();
                    out.push(String::from(line.trim_end()));
                    let len = out.len();
                    if len > 14 {
                        out.drain(0..len - 14); // alleen de recentste regels bewaren
                    }
                }
            }
            s.len() as u64
        }
        2 => 7, // getpid -> de daemon is pid 7
        _ => 0, // overige syscalls: stil slagen (daemon eindigt nooit)
    }
}

/// Laad `program` (native ABI) als een PREEMPTIEF gescheduelde achtergrond-daemon.
pub fn spawn_daemon(falloc: &mut FrameAllocator, program: &[u8]) {
    init_syscall_msrs();
    const MIB2: u64 = 1 << 21;
    // Eigen geïsoleerde 2 MiB-arena + PML4 (net als bg-musl) i.p.v. losse frames op
    // de boot-CR3: zo draait de daemon NIET meer op de supervisor-only boot-PML4 en
    // blijven SMEP/SMAP afgedwongen.
    // Exact 2 MiB, 2 MiB-uitgelijnd in één keer (de daemon reapt nooit → geen
    // free-pad; geen 4 MiB-over-allocatie meer).
    let arena = falloc.allocate_aligned(512, 512).expect("daemon-arena");
    let code = arena;
    let stack_top = arena + MIB2; // user-stack groeit omlaag vanaf de arena-top
    let kstack = falloc.allocate_contiguous(4).expect("daemon-kstack");
    let kstack_top = (kstack + 4 * 4096) & !0xF;
    let pages = program_span_pages(program);
    let info = load_program(program, code, pages);
    // SysV-stack (argv[0]="daemon") zodat ook musl/native _start geldig opstart.
    let rsp = unsafe { setup_user_stack(stack_top, &[b"daemon"], &info) };
    let sel = crate::gdt::selectors();
    let user_cs = (sel.user_code.0 | 3) as u64;
    let user_ss = (sel.user_data.0 | 3) as u64;
    let idx = crate::sched::spawn_user(info.entry, rsp, user_cs, user_ss, kstack_top);
    let pml4 = crate::paging::build_address_space(falloc, arena, &info.exec_pages, &info.writ_pages);
    crate::sched::set_task_cr3(idx, pml4);
    DAEMON_TASK.store(idx, Ordering::Relaxed);
    crate::serial_println!("[euro] daemon gescheduled als taak {idx} (pid 7), eigen adresruimte PML4 {pml4:#x}");
}

// ── Preemptief per-proces-model ───────────────────────────────────────────
// Meerdere ECHTE musl-processen tegelijk, elk preemptief gescheduled met een
// EIGEN context: eigen kernel-stack (sched), eigen FS_BASE/TLS (sched bewaart
// die per taak), eigen heap, eigen uitvoerbuffer en pid. De syscall-laag
// routeert per taak naar dit per-proces controleblok (PCB).
struct BgProc {
    task: usize,
    pid: u64,
    heap_break: u64,
    heap_end: u64,
    output: alloc::vec::Vec<String>,
    partial: String,
    // Fysieke frames van dit proces (om vrij te geven bij reaping).
    arena_raw: u64, // begin van de arena-allocatie
    arena_frames: u64, // aantal arena-frames (512 voor uitgelijnde bg-musl, 1024 voor pooled fork)
    /// VIRTUEEL adres waarop de 2 MiB-arena in DIT proces gemapt is (= waar code/
    /// stack draaien). Voor identity-processen == fysiek; voor een GEFORKT kind is
    /// het de virtuele arena van de OUDER (de kopie draait op dezelfde virt. adressen,
    /// andere frames). execve gebruikt dit als laad-/entry-/stackbasis.
    arena_virt: u64,
    kstack: u64,    // ring-0 stack (4 frames)
    pml4: u64,      // eigen adresruimte (PML4+PDPT+PD = 3 frames)
    /// Beëindigd en wachtend op opruiming (frames vrijgeven).
    zombie: bool,
    /// Reden van beëindiging (voor de tombstone in de systeemlog).
    kill_reason: Option<String>,
    /// Scheduler-task-indices van de THREADS van dit proces (clone, CLONE_VM).
    /// Threads delen de adresruimte/heap/uitvoer/pid; eigen stack/TLS/kstack.
    threads: alloc::vec::Vec<usize>,
    /// Per thread-taak het CLONE_CHILD_CLEARTID-adres: bij thread-exit schrijft
    /// de kernel hier 0 en doet een futex-wake — precies waar pthread_join op
    /// wacht. (task, ctid-userspace-adres)
    thread_ctids: alloc::vec::Vec<(usize, u64)>,
    /// Ouder-pid (S3 fork): 0 = geen ouder (bv. de boot-demo-processen).
    ppid: u64,
    /// Komen de frames van dit proces uit de PROCES-POOL (fork/exec) i.p.v. de
    /// hoofd-allocator? Bepaalt waar de reaper ze teruggeeft.
    pooled: bool,
}

/// Exit-statussen van beëindigde kinderen, los van het frame-reapen bewaard, zodat
/// waitpid de status nog kan ophalen nadat de frames al vrij zijn. (ppid, pid, code).
static CHILD_EXITS: Mutex<alloc::vec::Vec<(u64, u64, i64)>> = Mutex::new(alloc::vec::Vec::new());

/// Pid-teller voor geforkte kinderen (start hoog, los van de vaste demo-pids 1-16).
static NEXT_FORK_PID: AtomicU64 = AtomicU64::new(1000);

static BG: Mutex<alloc::vec::Vec<BgProc>> = Mutex::new(alloc::vec::Vec::new());
/// Tombstones van opgeruimde processen (voor weergave).
static REAPED: Mutex<alloc::vec::Vec<String>> = Mutex::new(alloc::vec::Vec::new());

/// De recente "opgeruimd"-meldingen van beëindigde processen.
pub fn reaped_lines() -> alloc::vec::Vec<String> {
    REAPED.lock().clone()
}

/// Geef de frames van alle beëindigde (zombie) processen vrij en verwijder ze
/// uit de tabel. Aangeroepen vanuit de desktop-lus (taak 0, boot-PML4), waar het
/// veilig is: een dood proces draait nooit meer en zijn frames zijn niet in gebruik.
pub fn reap_dead(falloc: &mut FrameAllocator) {
    let mut bg = BG.lock();
    let mut i = 0;
    while i < bg.len() {
        if bg[i].zombie {
            let p = bg.remove(i);
            if p.pooled {
                // Geforkte kinderen: frames teruggeven aan de PROCES-POOL.
                for f in 0..p.arena_frames {
                    crate::procpool::free(p.arena_raw + f * 4096);
                }
                for f in 0..4u64 {
                    crate::procpool::free(p.kstack + f * 4096);
                }
                // Eerst de arena-PT opzoeken (loopt door pml4->pdpt->pd) VÓÓR we die
                // tabelframes vrijgeven, anders use-after-free.
                let arena_pt = crate::paging::arena_pt(p.pml4, p.arena_virt);
                let (a, b, c) = crate::paging::table_frames(p.pml4);
                crate::procpool::free(a);
                crate::procpool::free(b);
                crate::procpool::free(c);
                if let Some(ptf) = arena_pt {
                    crate::procpool::free(ptf);
                }
            } else {
                for f in 0..p.arena_frames {
                    let _ = falloc.free(p.arena_raw + f * 4096);
                }
                for f in 0..4u64 {
                    let _ = falloc.free(p.kstack + f * 4096);
                }
                crate::paging::free_address_space(falloc, p.pml4);
            }
            let kib = (p.arena_frames + 4 + 4) as usize * 4; // arena + kstack(4) + tabelframes(~4)
            // Toon de laatste uitvoer (bv. het resultaat van een job) als die er
            // is, anders de beëindigingsreden (bv. de isolatie-overtreding).
            let label = p
                .output
                .last()
                .cloned()
                .or(p.kill_reason)
                .unwrap_or_else(|| String::from("beëindigd"));
            REAPED.lock().push(alloc::format!("pid {}: {label} -> opgeruimd ({kib} KiB vrij)", p.pid));
            let n = REAPED.lock().len();
            if n > 4 {
                REAPED.lock().drain(0..n - 4);
            }
        } else {
            i += 1;
        }
    }
}

/// Leeft een proces met deze pid nog? (een LEVENDE, niet-zombie BgProc). Door
/// EuroInit gebruikt om te zien of een service nog draait of herstart moet worden.
pub fn is_pid_alive(pid: u64) -> bool {
    BG.lock().iter().any(|p| p.pid == pid && !p.zombie)
}

/// De recentste uitvoerregel van elk achtergrond-musl-proces (voor weergave).
pub fn bg_lines() -> alloc::vec::Vec<String> {
    let bg = BG.lock();
    let mut out = alloc::vec::Vec::new();
    for p in bg.iter() {
        if let Some(last) = p.output.last() {
            out.push(last.clone());
        }
    }
    out
}

// Draait er nu een GEÏSOLEERDE voorgrond-exec (eigen PML4, synchroon)? Zo ja,
// dan beëindigt een page fault dat proces netjes (terug naar run_args) i.p.v.
// taak 0/de shell te doden.
static FG_ACTIVE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn fg_active() -> bool {
    FG_ACTIVE.load(Ordering::Relaxed)
}

/// Door de fault-handler aangeroepen bij een fout in een voorgrond-exec: zet de
/// exit-status en spring (via de trampoline) netjes terug in run_args.
pub fn fg_force_exit(addr: u64) -> ! {
    unsafe {
        EXIT_CODE = 139; // 128 + SIGSEGV
        EXITED = 1;
        FG_ACTIVE.store(false, Ordering::Relaxed);
    }
    crate::serial_println!("[isolatie] voorgrond-exec page fault op {addr:#x} -> nette exit (code 139)");
    // SAFETY: SAVED_KERNEL_RSP wijst naar het run_args-terugkeerpunt (enter_ring3
    // bewaarde het); de trampoline herstelt de stack en keert daar terug.
    unsafe { force_kernel_return() };
    loop {
        core::hint::spin_loop(); // onbereikbaar: de trampoline keert niet hier terug
    }
}

/// Door de page-fault-handler aangeroepen als een ring-3 proces buiten zijn
/// adresruimte grijpt: noteer het in z'n uitvoerbuffer en geef de pid terug.
pub fn note_isolation_kill(task: usize, addr: u64) -> u64 {
    let mut bg = BG.lock();
    if let Some(p) = bg.iter_mut().find(|p| p.task == task) {
        p.zombie = true; // klaar om opgeruimd te worden
        p.output.clear(); // de isolatie-reden is informatiever dan de laatste uitvoer
        p.kill_reason = Some(alloc::format!("geheugenisolatie: toegang {addr:#x} geweigerd"));
        return p.pid;
    }
    0
}

/// Procesoverzicht (`ps`): de achtergrond-musl-processen + de vaste systeemtaken.
pub fn ps_lines() -> alloc::vec::Vec<String> {
    let mut out = alloc::vec::Vec::new();
    out.push(String::from("  PID  TYPE     ADRESRUIMTE   STATUS"));
    out.push(String::from("    1  shell    gedeeld       actief (voorgrond)"));
    out.push(String::from("    7  daemon   gedeeld       actief (EuroMonitor)"));
    let bg = BG.lock();
    for p in bg.iter() {
        let status = if p.zombie { "beëindigd (reap)" } else { "actief" };
        out.push(alloc::format!("  {:3}  musl     eigen PML4    {}", p.pid, status));
    }
    out
}

/// `kill <pid>`: beëindig een achtergrond-musl-proces. Het wordt door de reaper
/// opgeruimd (frames vrij). Geeft terug of er een proces gevonden is.
pub fn kill_pid(pid: u64) -> bool {
    let task = {
        let mut bg = BG.lock();
        match bg.iter_mut().find(|p| p.pid == pid && !p.zombie) {
            Some(p) => {
                p.zombie = true;
                p.kill_reason = Some(String::from("beëindigd via shell (kill)"));
                p.task
            }
            None => return false,
        }
    };
    crate::sched::mark_dead(task);
    true
}

/// Futex-wachtrij: (userspace-adres, geblokkeerde taak). FUTEX_WAIT blokkeert de
/// taak (scheduler slaat 'm over); FUTEX_WAKE deblokkeert tot `n` wachters.
static FUTEX_QUEUE: Mutex<alloc::vec::Vec<(u64, usize)>> = Mutex::new(alloc::vec::Vec::new());

/// futex-wake: deblokkeer tot `n` taken die op `uaddr` wachten. Geeft het aantal
/// gewekte taken terug.
fn futex_wake(uaddr: u64, n: i32) -> u32 {
    let mut q = FUTEX_QUEUE.lock();
    let mut woken = 0i32;
    let mut i = 0;
    while i < q.len() && woken < n {
        if q[i].0 == uaddr {
            let task = q[i].1;
            crate::sched::unblock(task);
            q.swap_remove(i);
            woken += 1;
        } else {
            i += 1;
        }
    }
    woken as u32
}

/// FUTEX_WAIT: als *uaddr == val, blokkeer de huidige taak op uaddr en geef 0
/// terug (de waiter wordt op de volgende tick weggeschakeld tot een wake hem
/// deblokkeert; musl her-controleert na een spurious wakeup). Anders -EAGAIN.
fn futex_wait(uaddr: u64, val: u32) -> u64 {
    let cur_val: u32 = match read_user(uaddr) {
        Some(v) => v,
        None => return EFAULT,
    };
    if cur_val != val {
        return (-11i64) as u64; // -EAGAIN: de waarde veranderde al
    }
    let cur = crate::sched::current();
    let mut q = FUTEX_QUEUE.lock();
    if !q.iter().any(|&(a, t)| a == uaddr && t == cur) {
        q.push((uaddr, cur));
    }
    drop(q);
    crate::sched::block_current();
    0
}

// Statische pool van kernel-stacks voor THREADS (clone). Supervisor-gemapt
// (kernel .bss), dus een thread kan z'n eigen opgeslagen kernel-context niet
// vanuit ring 3 aanraken. 8 threads systeembreed (genoeg voor de demo's).
const MAX_THREADS: usize = 8;
const TKSTACK_SIZE: usize = 16 * 1024;
static mut THREAD_KSTACKS: [[u8; TKSTACK_SIZE]; MAX_THREADS] = [[0; TKSTACK_SIZE]; MAX_THREADS];
static THREAD_KSTACK_NEXT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Per-proces syscall-dispatcher (Linux-ABI-subset) met de staat van ÉÉN proces:
/// eigen heap, eigen uitvoerbuffer, eigen pid. Threads van het proces routeren
/// hier ook naartoe (gedeelde heap/uitvoer/pid; eigen stack/TLS).
/// S3 fork(): dupliceer het achtergrond-proces op index `pos`. Kopieert de 2 MiB
/// user-arena naar VERSE frames uit de proces-pool, bouwt een geremapte adresruimte
/// (zelfde virtuele adressen → nieuwe fysieke frames), en start een kind-taak die
/// in ring 3 hervat met rax=0. De OUDER krijgt de kind-pid terug.
fn do_fork(bg: &mut alloc::vec::Vec<BgProc>, pos: usize) -> u64 {
    const MIB2: u64 = 1 << 21;
    let (parent_pid, parent_arena_raw, parent_virt, heap_break, heap_end, parent_pml4) = {
        let p = &bg[pos];
        (p.pid, p.arena_raw, p.arena_virt, p.heap_break, p.heap_end, p.pml4)
    };
    let parent_arena = (parent_arena_raw + (MIB2 - 1)) & !(MIB2 - 1);

    // Frames uit de proces-pool: 4 MiB arena + 4 frames kstack + 3 tabelframes.
    let child_raw = match crate::procpool::alloc_contiguous(1024) {
        Some(a) => a,
        None => return (-12i64) as u64, // -ENOMEM
    };
    let child_arena = (child_raw + (MIB2 - 1)) & !(MIB2 - 1);
    let child_kstack = match crate::procpool::alloc_contiguous(4) {
        Some(a) => a,
        None => {
            for f in 0..1024u64 { crate::procpool::free(child_raw + f * 4096); }
            return (-12i64) as u64;
        }
    };
    // Vier tabelframes: PML4 + PDPT + PD + de fijnmazige arena-PT (voor W^X).
    let (pml4, pdpt, pd, pt) = match (crate::procpool::alloc(), crate::procpool::alloc(), crate::procpool::alloc(), crate::procpool::alloc()) {
        (Some(a), Some(b), Some(c), Some(d)) => (a, b, c, d),
        (a, b, c, d) => {
            for f in 0..1024u64 { crate::procpool::free(child_raw + f * 4096); }
            for f in 0..4u64 { crate::procpool::free(child_kstack + f * 4096); }
            for fr in [a, b, c, d].into_iter().flatten() { crate::procpool::free(fr); }
            return (-12i64) as u64;
        }
    };

    // SAFETY: ouder- en kind-arena liggen beide identity-gemapt; kopieer de hele
    // 2 MiB (code + stack + heap zoals NU, inclusief het fork-syscall-frame).
    unsafe {
        core::ptr::copy_nonoverlapping(parent_arena as *const u8, child_arena as *mut u8, MIB2 as usize);
    }
    // Map de VIRTUELE arena van de ouder -> de fysieke frames van het kind, met per
    // pagina DEZELFDE W^X-rechten als de ouder (gekloond uit diens arena-PT).
    let parent_pt = crate::paging::arena_pt(parent_pml4, parent_virt);
    crate::paging::fill_remap_tables_wx(pml4, pdpt, pd, pt, parent_virt, child_arena, parent_pt);

    // Kind-taak: hervat op het fork-returnpunt (USER_RIP) met de OUDER-userstack
    // (USER_RSP, nu in de kopie) en rax=0; eigen kstack + geremapte adresruimte.
    let kstack_top = (child_kstack + 4 * 4096) & !0xF;
    let (user_rip, user_rsp, saved) = unsafe { (USER_RIP, USER_RSP, SAVED_REGS) };
    let fs = unsafe { Msr::new(0xC000_0100).read() };
    let sel = crate::gdt::selectors();
    let user_cs = (sel.user_code.0 | 3) as u64;
    let user_ss = (sel.user_data.0 | 3) as u64;
    let child_pid = NEXT_FORK_PID.fetch_add(1, Ordering::Relaxed);
    let task = crate::sched::spawn_thread(user_rip, user_rsp, user_cs, user_ss, kstack_top, pml4, fs, saved);
    crate::sched::set_ident(task, child_pid as u32, parent_pid as u32);

    bg.push(BgProc {
        task,
        pid: child_pid,
        heap_break,
        heap_end,
        output: alloc::vec::Vec::new(),
        partial: String::new(),
        arena_raw: child_raw,
        arena_frames: 1024, // pooled fork-arena: 4 MiB uit de proces-pool
        arena_virt: parent_virt, // het kind draait op de virtuele arena van de ouder
        kstack: child_kstack,
        pml4,
        zombie: false,
        kill_reason: None,
        threads: alloc::vec::Vec::new(),
        thread_ctids: alloc::vec::Vec::new(),
        ppid: parent_pid,
        pooled: true,
    });
    crate::kinfo!("[fork] pid {parent_pid} -> kind pid {child_pid} (taak {task}, kopie-arena {child_arena:#x}, pml4 {pml4:#x})");
    child_pid // de OUDER krijgt de kind-pid; het kind kreeg rax=0 via spawn_thread
}

/// S3 waitpid/wait4: NON-BLOCKING reap. Haalt een geëindigd kind van `parent_pid`
/// uit CHILD_EXITS, schrijft de Linux-waitstatus (WEXITSTATUS = (status>>8)&0xff)
/// en geeft de kind-pid terug. Nog geen zombie -> 0 (de aanroeper polt opnieuw).
fn do_wait4(parent_pid: u64, _pid_arg: u64, status_ptr: u64) -> u64 {
    let mut ce = CHILD_EXITS.lock();
    if let Some(idx) = ce.iter().position(|&(pp, _, _)| pp == parent_pid) {
        let (_, cpid, code) = ce.remove(idx);
        if status_ptr != 0 {
            let status = (((code as i32) & 0xff) << 8) as u32;
            if !write_user(status_ptr, status) {
                return EFAULT;
            }
        }
        crate::kinfo!("[wait] pid {parent_pid} reapte kind {cpid} (exitcode {code})");
        return cpid;
    }
    0
}

/// S3 execve(path, argv, envp): vervang het IMAGE van het huidige proces door een
/// nieuw programma uit de userspace-VFS, IN dezelfde arena/adresruimte. Bij succes
/// keert de syscall terug in het NIEUWE image (we herschrijven het opgeslagen
/// registerblok zodat sysret naar de nieuwe entry springt). Faalt -> errno terug.
fn do_execve(p: &mut BgProc, path_ptr: u64, argv_ptr: u64) -> u64 {
    const MIB2: u64 = 1 << 21;
    let path_bytes = user_cstr(path_ptr, 256);
    let path = String::from_utf8_lossy(&path_bytes).into_owned();
    // Programmabytes uit de VFS; verify-before-execute (Ed25519) zoals elke exec.
    let program = match FILES.lock().iter().find(|(q, _)| *q == path) {
        Some((_, d)) => d.clone(),
        None => return (-2i64) as u64, // -ENOENT
    };
    if !verify_program(&path, &program) {
        return (-13i64) as u64; // -EACCES: ongeldige handtekening
    }
    // argv uit userspace parsen (NULL-getermineerde array van char*).
    let mut argv_owned: alloc::vec::Vec<alloc::vec::Vec<u8>> = alloc::vec::Vec::new();
    if argv_ptr != 0 {
        let mut i = 0;
        loop {
            // Elke argv[i]-pointer wordt arena-gevalideerd gelezen; een vervalste
            // array-pointer kan zo geen kernel-geheugen als argv-element lekken.
            let pptr: u64 = match read_user(argv_ptr + (i as u64) * 8) {
                Some(p) => p,
                None => break,
            };
            if pptr == 0 || i >= 64 {
                break;
            }
            argv_owned.push(user_cstr(pptr, 256));
            i += 1;
        }
    }
    if argv_owned.is_empty() {
        argv_owned.push(path_bytes);
    }
    let argv_refs: alloc::vec::Vec<&[u8]> = argv_owned.iter().map(|v| v.as_slice()).collect();

    // Laad in de BESTAANDE arena op het VIRTUELE adres waarop dit proces draait
    // (voor een geforkt kind ≠ fysiek). De huidige cr3 mapt dit USER -> de eigen
    // frames; met SMAP uit schrijft de kernel hier doorheen. Verse user-stack + heap.
    let arena = p.arena_virt;
    let stack_top = arena + MIB2;
    let pages = program_span_pages(&program);
    // W^X: de arena draait R-X-code; maak ze even volledig schrijfbaar om het NIEUWE
    // image te laden, en herstel daarna W^X op basis van de segmenten van dat image.
    // (Geen fijnmazige PT -> oude RWX-arena, gewoon laden.)
    let arena_pt = crate::paging::arena_pt(p.pml4, arena);
    if let Some(pt) = arena_pt {
        crate::paging::arena_set_writable(pt);
    }
    let info = load_program(&program, arena, pages);
    let rsp = unsafe { setup_user_stack(stack_top, &argv_refs, &info) };
    if let Some(pt) = arena_pt {
        crate::paging::arena_set_wx(pt, &info.exec_pages, &info.writ_pages);
    }
    p.heap_break = arena + 0x80000;
    p.heap_end = arena + 0x180000;

    // Laat de huidige syscall TERUGKEREN in het nieuwe image: herschrijf het
    // opgeslagen registerblok (slot 13 = rcx = sysret-rip, slot 12 = r11 = rflags,
    // 0..11 = gewiste GP-regs) en zet USER_RSP op de verse stack.
    unsafe {
        let regs = SAVED_REGS as *mut u64;
        for k in 0..14 {
            regs.add(k).write(0);
        }
        regs.add(13).write(info.entry); // sysret-doel = nieuwe entry
        regs.add(12).write(0x202); // rflags met IF=1
        USER_RSP = rsp;
    }
    crate::kinfo!("[exec] pid {} execve {path} -> entry {:#x} (zelfde arena {arena:#x})", p.pid, info.entry);
    0
}

fn bg_dispatch(p: &mut BgProc, num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64 {
    // Net als de daemon: nooit het globale sys_exit-pad nemen.
    unsafe { EXITED = 0 };
    match num {
        1 | 20 => {
            // write(fd,buf,len) / writev(fd,iov,cnt) -> eigen regelbuffer.
            let text: alloc::vec::Vec<u8> = if num == 1 {
                match copy_from_user(a2, a3 as usize) {
                    Some(v) => v,
                    None => return EFAULT,
                }
            } else {
                // writev: begrens iovcnt en valideer elke iov-struct + base/len
                // VÓÓR dereferentie. Zonder de bound kan een groot `a3` de kernel
                // laten dwalen; zonder de base-check kan een vervalste iov kernel-
                // geheugen uitlezen.
                if a3 > 1024 {
                    return (-22i64) as u64; // -EINVAL
                }
                let mut v = alloc::vec::Vec::new();
                for i in 0..a3 {
                    let iov_base = a2 + (i * 16);
                    let base: u64 = match read_user(iov_base) {
                        Some(b) => b,
                        None => return EFAULT,
                    };
                    let len = match read_user::<u64>(iov_base + 8) {
                        Some(l) => l as usize,
                        None => return EFAULT,
                    };
                    if len > 0 {
                        match copy_from_user(base, len) {
                            Some(chunk) => v.extend_from_slice(&chunk),
                            None => return EFAULT,
                        }
                    }
                }
                v
            };
            // write naar een pipe-schrijf-fd -> de pipe-FIFO (IPC), anders de
            // eigen regelbuffer (fd 1/2 = console).
            if let Some(r) = pipe_write_fd(a1 as usize, &text) {
                return r;
            }
            if let Ok(t) = core::str::from_utf8(&text) {
                p.partial.push_str(t);
                while let Some(nl) = p.partial.find('\n') {
                    let line: String = p.partial.drain(..=nl).collect();
                    p.output.push(String::from(line.trim_end()));
                    let len = p.output.len();
                    if len > 6 {
                        p.output.drain(0..len - 6);
                    }
                }
            }
            text.len() as u64
        }
        0 => pipe_read_fd(a1 as usize, a2, a3 as usize).unwrap_or(0), // read: pipe of EOF
        22 | 293 => pipe_create(a1),                                  // pipe / pipe2
        32 => a1, // dup(fd) -> zelfde fd (vereenvoudigd)
        33 => {
            // dup2(oldfd, newfd): kopieer het pipe-uiteinde naar newfd.
            let (old, new) = (a1 as usize, a2 as usize);
            if old < MAX_FD && new < MAX_FD {
                let v = PIPE_FDS.lock()[old];
                PIPE_FDS.lock()[new] = v;
                new as u64
            } else {
                (-9i64) as u64
            }
        }
        9 => {
            // mmap -> bump uit de EIGEN heap (anonieme allocatie, page-uitgelijnd).
            let len = (a2 + 0xFFF) & !0xFFF;
            let base = (p.heap_break + 0xFFF) & !0xFFF;
            if base + len > p.heap_end {
                return (-12i64) as u64; // -ENOMEM
            }
            p.heap_break = base + len;
            base
        }
        12 => {
            // brk(addr) -> nieuwe break uit de eigen heap.
            if a1 == 0 || a1 > p.heap_end {
                return p.heap_break;
            }
            p.heap_break = a1;
            a1
        }
        158 => match a1 {
            0x1002 => {
                unsafe { Msr::new(0xC000_0100).write(a2) }; // FS_BASE (musl-TLS)
                0
            }
            0x1001 => {
                unsafe { Msr::new(0xC000_0101).write(a2) };
                0
            }
            _ => (-22i64) as u64,
        },
        39 => p.pid,  // getpid
        218 => p.pid, // set_tid_address -> tid
        318 => {
            // getrandom: deterministische vulling (geen crypto-bron nodig hier).
            if !in_user_arena(a1, a2 as usize) {
                return EFAULT;
            }
            let buf: alloc::vec::Vec<u8> =
                (0..a2).map(|i| (0x9Eu64.wrapping_mul(i + 1)) as u8).collect();
            let _ = copy_to_user(a1, &buf);
            a2
        }
        56 => {
            // clone(flags, child_stack, ptid, ctid, tls): maak een THREAD die de
            // adresruimte (CLONE_VM) deelt maar een eigen stack/TLS/kernel-stack
            // heeft. Basis voor pthreads. Geen child_stack = (v)fork: niet onderst.
            let (flags, child_stack) = (a1, a2);
            if child_stack == 0 {
                return (-38i64) as u64; // -ENOSYS (geen fork)
            }
            let slot = THREAD_KSTACK_NEXT.fetch_add(1, Ordering::Relaxed);
            if slot >= MAX_THREADS {
                return (-11i64) as u64; // -EAGAIN
            }
            let kbase = unsafe { core::ptr::addr_of_mut!(THREAD_KSTACKS[slot]) as u64 };
            let kstack_top = (kbase + TKSTACK_SIZE as u64) & !0xF;
            let user_rip = unsafe { USER_RIP };
            let sel = crate::gdt::selectors();
            let user_cs = (sel.user_code.0 | 3) as u64;
            let user_ss = (sel.user_data.0 | 3) as u64;
            // TLS: bij CLONE_SETTLS (0x80000) gebruik de meegegeven tls (a5),
            // anders erf de huidige FS_BASE.
            let fs = if flags & 0x0008_0000 != 0 {
                a5
            } else {
                unsafe { Msr::new(0xC000_0100).read() }
            };
            let saved_regs = unsafe { SAVED_REGS };
            let child = crate::sched::spawn_thread(user_rip, child_stack, user_cs, user_ss, kstack_top, p.pml4, fs, saved_regs);
            p.threads.push(child);
            crate::serial_println!("[thread] clone: pid {} -> thread-taak {child} (gedeelde adresruimte, eigen stack/TLS)", p.pid);
            // CLONE_PARENT_SETTID (0x100000) / CLONE_CHILD_SETTID (0x1000000):
            // schrijf de tid naar *ptid / *ctid.
            if flags & 0x0010_0000 != 0 && a3 != 0 {
                let _ = write_user(a3, child as i32);
            }
            if flags & 0x0100_0000 != 0 && a4 != 0 {
                let _ = write_user(a4, child as i32);
            }
            // CLONE_CHILD_CLEARTID (0x200000): onthoud het adres; bij thread-exit
            // schrijft de kernel hier 0 (waar pthread_join op futex-wacht).
            if flags & 0x0020_0000 != 0 && a4 != 0 {
                p.thread_ctids.push((child, a4));
            }
            child as u64 // de ouder krijgt de thread-id
        }
        59 => do_execve(p, a1, a2), // execve(path, argv, envp) — image-replace
        202 => {
            // futex(uaddr, op, val, ...). FUTEX_WAIT=0, FUTEX_WAKE=1 (lage 7 bits;
            // PRIVATE/CLOCK-vlaggen negeren). Echte blokkering + wake.
            match a2 & 0x7f {
                0 => futex_wait(a1, a3 as u32),
                1 => futex_wake(a1, a3 as i32) as u64,
                _ => 0,
            }
        }
        // EuroIPC — eigen message-bus-syscalls (eigen nummerruimte 500-502).
        500 => crate::euroipc::register(p.pid, a1 as u32) as u64,
        501 => {
            let data = match copy_from_user(a2, a3 as usize) {
                Some(v) => v,
                None => return EFAULT,
            };
            crate::euroipc::send(p.pid, a1 as u32, &data) as u64
        }
        502 => crate::euroipc::recv(p.pid, a1, a2 as usize) as u64,
        // Geheugen-/signaal-/tijd-stubs die stil slagen.
        10 | 11 | 13 | 14 | 16 | 35 | 228 | 234 | 273 => 0,
        60 | 231 => {
            let cur = crate::sched::current();
            if p.threads.contains(&cur) {
                // THREAD-exit: beëindig alleen deze thread; het proces leeft door.
                // CLONE_CHILD_CLEARTID: schrijf 0 naar het ctid-adres + futex-wake,
                // zodat pthread_join in de ouder-thread doorgaat.
                if let Some(idx) = p.thread_ctids.iter().position(|&(t, _)| t == cur) {
                    let (_, ctid) = p.thread_ctids[idx];
                    let _ = write_user(ctid, 0i32);
                    futex_wake(ctid, i32::MAX);
                    p.thread_ctids.swap_remove(idx);
                }
                // We laten 'm IN p.threads staan: musl roept exit in een for(;;)-lus
                // aan, en die vervolg-syscalls moeten HIER blijven routeren (zodat
                // EXITED=0 blijft) tot de scheduler de dode thread overslaat. Hem nu
                // verwijderen zou de volgende exit naar linux_dispatch laten vallen,
                // dat EXITED=1 zet -> het sys_exit-pad met een verouderde
                // SAVED_KERNEL_RSP -> ret naar rommel. (Gevonden met QEMU+gdb.)
                crate::sched::mark_dead(cur);
                return 0;
            }
            // PROCES-exit (hoofd-task): markeer het hele proces als klaar (zombie);
            // de reaper geeft de frames vrij. musl spint hierna tot de timer schakelt.
            p.zombie = true;
            p.kill_reason = Some(alloc::format!("klaar (exit {a1})"));
            // S3: bewaar de exitstatus voor de ouder (waitpid) — alleen als er een
            // ouder is (ppid != 0). Services (ppid 0) zouden CHILD_EXITS anders
            // ongelimiteerd laten groeien (niemand waitpidt op ppid 0).
            if p.ppid != 0 {
                CHILD_EXITS.lock().push((p.ppid, p.pid, a1 as i64));
            }
            crate::sched::mark_current_dead();
            0
        }
        _ => 0,
    }
}

/// Laad `program` (musl, Linux-ABI) als een PREEMPTIEF gescheduled proces met
/// een eigen PCB (heap/uitvoer/pid/TLS). Het programma draait oneindig door.
pub fn spawn_bg_musl(falloc: &mut FrameAllocator, program: &[u8], pid: u64, argv0: &[u8]) {
    init_syscall_msrs();
    const MIB2: u64 = 1 << 21;
    // Eén 2 MiB-uitgelijnd user-arena: ALLE user-frames van dit proces (code,
    // stack, heap/TLS) liggen erin. Alleen dít blok krijgt straks de USER-bit in
    // de eigen PML4 -> geen ander ring-3 proces kan erbij (geheugenisolatie).
    // Exact 2 MiB, in één keer 2 MiB-uitgelijnd (geen 4 MiB-over-allocatie meer):
    // bespaart ~2 MiB per achtergrondproces. Bij reaping geven we precies deze 512
    // frames terug (arena_frames hieronder).
    let arena = falloc.allocate_aligned(512, 512).expect("bg-arena");
    let arena_raw = arena;
    let code = arena; // programmacode/segmenten onderaan het arena
    let heap = arena + 0x80000; // +512 KiB: eigen heap (musl mmap/TLS-blok)
    let stack_top = arena + MIB2; // user-stack groeit omlaag vanaf de arena-top
    let kstack = falloc.allocate_contiguous(4).expect("bg-kstack"); // ring-0 stack (supervisor)
    let kstack_top = (kstack + 4 * 4096) & !0xF;
    let pages = program_span_pages(program);
    let info = load_program(program, code, pages);
    let rsp = unsafe { setup_user_stack(stack_top, &[argv0], &info) };
    let sel = crate::gdt::selectors();
    let user_cs = (sel.user_code.0 | 3) as u64;
    let user_ss = (sel.user_data.0 | 3) as u64;
    let idx = crate::sched::spawn_user(info.entry, rsp, user_cs, user_ss, kstack_top);
    // Eigen geïsoleerde W^X-adresruimte; vanaf de volgende switch draait dit proces erop.
    let pml4 = crate::paging::build_address_space(falloc, arena, &info.exec_pages, &info.writ_pages);
    crate::sched::set_task_cr3(idx, pml4);
    BG.lock().push(BgProc {
        task: idx,
        pid,
        heap_break: heap,
        heap_end: arena + 0x180000, // ~1 MiB heap (ruimte voor thread-stacks)
        output: alloc::vec::Vec::new(),
        partial: String::new(),
        arena_raw,
        arena_frames: 512, // exact 2 MiB (uitgelijnd gealloceerd)
        arena_virt: arena, // identity-gemapt: virtueel == fysiek
        kstack,
        pml4,
        zombie: false,
        kill_reason: None,
        threads: alloc::vec::Vec::new(),
        thread_ctids: alloc::vec::Vec::new(),
        ppid: 0,
        pooled: false,
    });
    crate::serial_println!("[euro] bg-musl (pid {pid}) -> taak {idx}, eigen adresruimte PML4 {pml4:#x}, arena {arena:#x}");
}

/// Sluit een fd.
fn vfs_close(fd: usize) -> u64 {
    if fd < MAX_FD {
        OPEN_FDS.lock()[fd] = None;
        OPEN_DIRS.lock()[fd] = None;
    }
    0
}

/// Is `path` een MAP in de userspace-VFS? Een map heeft geen eigen FILES-entry,
/// maar is het prefix van minstens één bestand (of is de root "/").
fn is_vfs_dir(path: &[u8]) -> bool {
    if path == b"/" {
        return true;
    }
    let mut prefix = path.to_vec();
    prefix.push(b'/');
    FILES.lock().iter().any(|(p, _)| p.as_bytes().starts_with(&prefix))
}

/// Directe kinderen van een VFS-map: (naam, is_map). Afgeleid uit de platte
/// FILES-padlijst — tussenliggende padcomponenten worden als submappen herkend.
fn dir_children(path: &str) -> alloc::vec::Vec<(String, bool)> {
    let prefix = if path == "/" { String::from("/") } else { alloc::format!("{path}/") };
    let mut out: alloc::vec::Vec<(String, bool)> = alloc::vec::Vec::new();
    for (p, _) in FILES.lock().iter() {
        if let Some(rest) = p.strip_prefix(&prefix) {
            if rest.is_empty() {
                continue;
            }
            let (name, is_dir) = match rest.find('/') {
                Some(i) => (&rest[..i], true), // submap (eerste component)
                None => (rest, false),         // bestand
            };
            if !out.iter().any(|(n, _)| n == name) {
                out.push((String::from(name), is_dir));
            }
        }
    }
    out
}

/// Open een MAP -> dir-fd (geregistreerd in OPEN_DIRS), of u64::MAX bij vol.
fn diropen(path: &[u8]) -> u64 {
    let norm = String::from_utf8_lossy(path).into_owned();
    let fds = OPEN_FDS.lock();
    let mut dirs = OPEN_DIRS.lock();
    for fd in 3..MAX_FD {
        if fds[fd].is_none() && dirs[fd].is_none() {
            dirs[fd] = Some((norm, 0));
            return fd as u64;
        }
    }
    u64::MAX
}

/// getdents64(fd, buf, count): vul Linux `linux_dirent64`-records vanaf de cursor.
/// Geeft het aantal geschreven bytes terug, 0 aan het eind.
fn vfs_getdents64(fd: usize, buf: u64, count: usize) -> u64 {
    if fd >= MAX_FD {
        return (-9i64) as u64; // -EBADF
    }
    // De hele doel-buffer wordt één keer arena-gevalideerd; daarna liggen alle
    // per-record-schrijfacties (begrensd op `written + reclen <= count`) gegarandeerd
    // binnen [buf, buf+count) en kunnen ze geen kernel-geheugen raken.
    if !in_user_arena(buf, count) {
        return EFAULT;
    }
    let (path, mut cursor) = match &OPEN_DIRS.lock()[fd] {
        Some((p, c)) => (p.clone(), *c),
        None => return (-20i64) as u64, // -ENOTDIR
    };
    // "." en ".." vooraan, daarna de echte kinderen.
    let mut all: alloc::vec::Vec<(String, bool)> =
        alloc::vec![(String::from("."), true), (String::from(".."), true)];
    all.extend(dir_children(&path));

    let mut written = 0usize;
    while cursor < all.len() {
        let (name, is_dir) = &all[cursor];
        let reclen = (19 + name.len() + 1 + 7) & !7; // 8-byte uitgelijnd
        if written + reclen > count {
            break; // past niet meer in deze buffer-oproep
        }
        let rec = (buf as usize + written) as *mut u8;
        unsafe {
            core::ptr::write_bytes(rec, 0, reclen);
            (rec as *mut u64).write_unaligned((cursor as u64) + 1); // d_ino
            (rec.add(8) as *mut i64).write_unaligned((cursor as i64) + 1); // d_off
            (rec.add(16) as *mut u16).write_unaligned(reclen as u16); // d_reclen
            rec.add(18).write(if *is_dir { 4 } else { 8 }); // d_type DT_DIR/DT_REG
            core::ptr::copy_nonoverlapping(name.as_ptr(), rec.add(19), name.len());
        }
        written += reclen;
        cursor += 1;
    }
    OPEN_DIRS.lock()[fd] = Some((path, cursor));
    written as u64
}

/// De grootte (bytes) van het bestand achter een open fd, of None.
fn vfs_size(fd: usize) -> Option<usize> {
    if fd >= MAX_FD {
        return None;
    }
    let fds = OPEN_FDS.lock();
    let (fi, _) = fds[fd]?;
    Some(FILES.lock()[fi].1.len())
}

/// lseek(fd, offset, whence) -> nieuwe offset (u64::MAX bij fout).
fn vfs_lseek(fd: usize, offset: i64, whence: u64) -> u64 {
    if fd >= MAX_FD {
        return u64::MAX;
    }
    let size = match vfs_size(fd) {
        Some(s) => s as i64,
        None => return u64::MAX,
    };
    let mut fds = OPEN_FDS.lock();
    let (fi, cur) = match fds[fd] {
        Some(x) => x,
        None => return u64::MAX,
    };
    let base = match whence {
        0 => 0,            // SEEK_SET
        1 => cur as i64,   // SEEK_CUR
        2 => size,         // SEEK_END
        _ => return u64::MAX,
    };
    let newoff = base + offset;
    if newoff < 0 {
        return u64::MAX;
    }
    fds[fd] = Some((fi, newoff as usize));
    newoff as u64
}

/// Lees een NUL-getermineerde string uit userspace. Stopt op de NUL, op `max`,
/// OF zodra de volgende byte buiten de arena zou vallen — zo kan een vervalste
/// pointer nooit kernel-geheugen laten uitlezen. Een buiten-arena pointer levert
/// een lege vector op (de aanroeper behandelt dat als "geen pad").
fn user_cstr(ptr: u64, max: usize) -> alloc::vec::Vec<u8> {
    let mut v = alloc::vec::Vec::new();
    let mut i = 0;
    while i < max {
        let addr = match ptr.checked_add(i as u64) {
            Some(a) => a,
            None => break,
        };
        if !in_user_arena(addr, 1) {
            break;
        }
        // SAFETY: per-byte arena-gevalideerd; identity-mapped.
        let b = unsafe { *(addr as *const u8) };
        if b == 0 {
            break;
        }
        v.push(b);
        i += 1;
    }
    v
}

// Kernel-stack voor de syscall-handler.
const KSTACK_SIZE: usize = 16 * 1024;
static mut KSTACK: [u8; KSTACK_SIZE] = [0; KSTACK_SIZE];

global_asm!(
    // SYSCALL-entry vanuit ring 3: rcx=user-rip, r11=user-rflags, rsp=user-rsp.
    ".global syscall_entry",
    "syscall_entry:",
    "mov [rip + USER_RSP], rsp",
    "mov rsp, [rip + KERNEL_RSP]",
    // SMAP-venster OPEN: zet RFLAGS.AC (bit 18) zodat ring 0 voor de duur van deze
    // syscall user-pagina's (U=1) mag lezen/schrijven. De syscall draait met IF=0
    // (FMASK wist IF) → niet-preemptief, dus het venster kan niet door een
    // taakswitch lekken. AC i.p.v. `stac` → ook correct als de CPU geen SMAP heeft
    // (no-op). In ring 0 zet AC géén alignment-checks aan (dat vereist CPL=3).
    "pushfq",
    "bts qword ptr [rsp], 18",
    "popfq",
    // Bewaar ALLE user-registers die over een syscall heen bewaard moeten
    // blijven (echte syscall-ABI: alleen rax/rcx/r11 mogen veranderen).
    "push rcx",                       // user-rip
    "push r11",                       // user-rflags
    "push rbx",
    "push rbp",
    "push rdi",
    "push rsi",
    "push rdx",
    "push r8",
    "push r9",
    "push r10",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov [rip + SAVED_REGS], rsp",    // pointer naar het opgeslagen registerblok (clone)
    "mov [rip + USER_RIP], rcx",      // user-rip bewaren (clone: thread-resume-punt)
    "mov r9, r8",                     // dispatch arg5 = origineel r8 (clone: tls)
    "mov r8, r10",                    // dispatch arg4 = origineel r10 (clone: ctid)
    "mov rcx, rdx",                   // dispatch arg3 (origineel rdx)
    "mov rdx, rsi",                   // dispatch arg2 (rdi/rsi nog origineel)
    "mov rsi, rdi",                   // dispatch arg1
    "mov rdi, rax",                   // dispatch num
    "call syscall_dispatch",          // rax = return-waarde
    "mov r10, [rip + EXITED]",
    "test r10, r10",
    "jnz 9f",
    // Normale syscall → herstel registers (rax blijft return-waarde) en SYSRET.
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rdx",
    "pop rsi",
    "pop rdi",
    "pop rbp",
    "pop rbx",
    "pop r11",                        // user-rflags
    "pop rcx",                        // user-rip
    "mov rsp, [rip + USER_RSP]",
    "sysretq",
    "9:",                             // sys_exit → terug naar de kernel.
    "mov rsp, [rip + SAVED_KERNEL_RSP]",
    "pushfq",                         // SMAP-venster DICHT: wis AC (geen sysret die r11 herstelt)
    "btr qword ptr [rsp], 18",
    "popfq",
    "pop r15",                        // herstel callee-saved registers van run()
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    "ret",
    // force_kernel_return: identiek aan het sys_exit-epiloog, maar aanroepbaar
    // vanuit de page-fault-handler om een GEFAULTE voorgrond-exec af te breken
    // (nette terugkeer naar run_args i.p.v. taak 0/de shell te doden). Keert nooit
    // terug naar de aanroeper.
    ".global force_kernel_return",
    "force_kernel_return:",
    "mov rsp, [rip + SAVED_KERNEL_RSP]",
    "pushfq",                         // SMAP-venster DICHT (een gefaulte exec kan AC open hebben gelaten)
    "btr qword ptr [rsp], 18",
    "popfq",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    "ret",
    // enter_ring3(rdi=cs, rsi=ss, rdx=rip, rcx=rsp): spring ring 3 in via iretq.
    ".global enter_ring3",
    "enter_ring3:",
    // Bewaar callee-saved registers: het ring-3 programma klobbert ze, maar
    // run() (de aanroeper) verwacht ze intact ná de sys_exit-terugkeer.
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov [rip + SAVED_KERNEL_RSP], rsp", // wijst nu naar de bewaarde registers
    "push rsi",                       // ss
    "push rcx",                       // rsp
    "push 0x002",                     // rflags (IF=0: run() is synchroon en NIET-preemptief;
                                      // anders onderbreekt de timer de ring-3-excursie en wisselt
                                      // de scheduler van stack -> stackcorruptie/canary-fout.
                                      // Gescheduelde ring-3-taken lopen via sched::spawn_user met IF=1.)
    "push rdi",                       // cs
    "push rdx",                       // rip
    "iretq",
);

// Het userspace-programma /bin/hello — een echte, gestripte **ELF64**-binary,
// door de EuroToolchain (Track 6) uit C-broncode gecompileerd. De kernel parset
// de ELF-headers en laadt de PT_LOAD-segmenten (zie load_elf64).
static HELLO_ELF: &[u8] = include_bytes!("../../userland/hello.elf");
static CAT_ELF: &[u8] = include_bytes!("../../userland/cat.elf");
static LINUXPROG_ELF: &[u8] = include_bytes!("../../userland/linuxprog.elf");
static FORKTEST_ELF: &[u8] = include_bytes!("../../userland/forktest.elf");
static EXECEE_ELF: &[u8] = include_bytes!("../../userland/execee.elf");
static FORKPIPE_ELF: &[u8] = include_bytes!("../../userland/forkpipe.elf");
static TICKER_ELF: &[u8] = include_bytes!("../../userland/ticker.elf");
static MUSLPROG_ELF: &[u8] = include_bytes!("../../userland/muslprog.elf");
static ARGVPROG_ELF: &[u8] = include_bytes!("../../userland/argvprog.elf");
static PIEPROG_ELF: &[u8] = include_bytes!("../../userland/pieprog.elf");
// H3: dynamische-linking-testartefacten — een dynamisch-gelinkte exe + de .so.
static DYNTEST_ELF: &[u8] = include_bytes!("../../userland/dyntest.elf");
static LIBEURO_SO: &[u8] = include_bytes!("../../userland/libeuro.so");
static MUSLREAL_ELF: &[u8] = include_bytes!("../../userland/muslreal.elf");
static MUSLFILE_ELF: &[u8] = include_bytes!("../../userland/muslfile.elf");
static MCAT_ELF: &[u8] = include_bytes!("../../userland/mcat.elf");
static MWRITE_ELF: &[u8] = include_bytes!("../../userland/mwrite.elf");
static MECHO_ELF: &[u8] = include_bytes!("../../userland/mecho.elf");
static MUPPER_ELF: &[u8] = include_bytes!("../../userland/mupper.elf");
static DAEMON_ELF: &[u8] = include_bytes!("../../userland/daemon.elf");
static MSUM_ELF: &[u8] = include_bytes!("../../userland/msum.elf");
static MENV_ELF: &[u8] = include_bytes!("../../userland/menv.elf");
static MSOCK_ELF: &[u8] = include_bytes!("../../userland/msock.elf");
static MDNS_ELF: &[u8] = include_bytes!("../../userland/mdns.elf");
static MTRACK_ELF: &[u8] = include_bytes!("../../userland/mtrack.elf");
static TLSCOUNT_ELF: &[u8] = include_bytes!("../../userland/tlscount.elf");
static ISOTEST_ELF: &[u8] = include_bytes!("../../userland/isotest.elf");
static WORKER_ELF: &[u8] = include_bytes!("../../userland/worker.elf");
static MTHREAD_ELF: &[u8] = include_bytes!("../../userland/mthread.elf");
static MPTHREAD_ELF: &[u8] = include_bytes!("../../userland/mpthread.elf");
static MMUTEX_ELF: &[u8] = include_bytes!("../../userland/mmutex.elf");
static IPCRECV_ELF: &[u8] = include_bytes!("../../userland/ipcrecv.elf");
static IPCSEND_ELF: &[u8] = include_bytes!("../../userland/ipcsend.elf");

/// De ELF-bytes van /bin/mmutex (pthread_mutex onder contentie via futex).
pub fn mmutex_bytes() -> &'static [u8] {
    MMUTEX_ELF
}

/// De ELF-bytes van de EuroIPC-demo's (ontvanger + zender).
pub fn ipcrecv_bytes() -> &'static [u8] {
    IPCRECV_ELF
}
pub fn ipcsend_bytes() -> &'static [u8] {
    IPCSEND_ELF
}

/// De ELF-bytes van /bin/mthread (threads-demo: clone + gedeeld geheugen).
pub fn mthread_bytes() -> &'static [u8] {
    MTHREAD_ELF
}

/// De ELF-bytes van /bin/mpthread (echte musl-pthreads: create + join).
pub fn mpthread_bytes() -> &'static [u8] {
    MPTHREAD_ELF
}

/// De ELF-bytes van /bin/tlscount (musl-demo: per-proces __thread-teller).
pub fn tlscount_bytes() -> &'static [u8] {
    TLSCOUNT_ELF
}

/// De ELF-bytes van /bin/isotest (musl-demo: geheugenisolatie-overtreding).
pub fn isotest_bytes() -> &'static [u8] {
    ISOTEST_ELF
}

/// De ELF-bytes van /bin/worker (musl-demo: rekent, rapporteert, exit(0)).
pub fn worker_bytes() -> &'static [u8] {
    WORKER_ELF
}

// Een ring-3 proces dat eindeloos een teller in z'n EIGEN user-geheugen ophoogt.
// Door de scheduler preemptief afgewisseld; de kernel leest de teller en toont
// dat het proces vooruitgang boekt — bewijs van userspace-multitasking.
global_asm!(
    ".global utask_start",
    ".global utask_cnt",
    ".global utask_end",
    "utask_start:",
    "utask_loop:",
    "inc qword ptr [rip + utask_cnt]",
    "jmp utask_loop",
    ".align 8",
    "utask_cnt:",
    ".quad 0",
    "utask_end:",
);

extern "sysv64" {
    fn syscall_entry();
    fn enter_ring3(cs: u64, ss: u64, rip: u64, rsp: u64);
    /// Breek een voorgrond-exec af na een page fault: nette terugkeer in run_args.
    fn force_kernel_return();
}
extern "C" {
    static utask_start: u8;
    static utask_cnt: u8;
    static utask_end: u8;
}

/// Start een ring-3 proces dat een teller ophoogt en voeg het toe aan de
/// scheduler. ELK proces krijgt een eigen code-, stack- ÉN kernel-stack zodat
/// meerdere ring-3 processen tegelijk preemptief kunnen draaien. Geeft het adres
/// van de teller terug (de kernel leest 'm uit voor weergave).
pub fn spawn_counter_task(falloc: &mut FrameAllocator) -> u64 {
    init_syscall_msrs();
    const MIB2: u64 = 1 << 21;

    let start = core::ptr::addr_of!(utask_start) as usize;
    let end = core::ptr::addr_of!(utask_end) as usize;
    let cnt = core::ptr::addr_of!(utask_cnt) as usize;
    let bytes = unsafe { core::slice::from_raw_parts(start as *const u8, end - start) };
    let cnt_off = (cnt - start) as u64;

    // Eigen geïsoleerde 2 MiB-arena + PML4 i.p.v. losse frames op de boot-CR3, zodat
    // dit ring-3 proces niet op de supervisor-only boot-PML4 draait (SMEP/SMAP-veilig).
    // 2 MiB, exact 2 MiB-uitgelijnd in één keer (geen 4 MiB-over-allocatie meer):
    // de teller-taak reapt nooit, dus dit is de veiligste plek om allocate_aligned te
    // gebruiken. Bespaart ~2 MiB t.o.v. allocate_contiguous(1024)+handmatig uitlijnen.
    let arena = falloc.allocate_aligned(512, 512).expect("utask-arena");
    let code = arena;
    let stack_top = arena + MIB2; // user-stack groeit omlaag vanaf de arena-top
    // Eigen kernel-stack (4 frames = 16 KiB) voor de ring3->ring0 interrupt-frames.
    let kstack = falloc.allocate_contiguous(4).expect("utask kernel-stack");
    let kstack_top = (kstack + 4 * 4096) & !0xF;
    // SAFETY: arena ligt in de identity-gemapte onderste 1 GiB; onder de boot-CR3
    // (waar we nu draaien) is dat een supervisor-pagina -> schrijven mag.
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), code as *mut u8, bytes.len().min(4096));
    }
    let counter_ptr = code + cnt_off;

    let sel = crate::gdt::selectors();
    let user_cs = (sel.user_code.0 | 3) as u64;
    let user_ss = (sel.user_data.0 | 3) as u64;
    let idx = crate::sched::spawn_user(code, stack_top, user_cs, user_ss, kstack_top);
    // Teller-demo: rauw machinecode-blob met teller-variabele IN de codepagina →
    // code/data niet te scheiden, dus RWX i.p.v. W^X (zie build_address_space_rwx).
    let pml4 = crate::paging::build_address_space_rwx(falloc, arena);
    crate::sched::set_task_cr3(idx, pml4);
    counter_ptr
}

/// Lees een tellerstand. `ptr` is het FYSIEKE arena-adres; onder de boot-CR3 (waar
/// de kernel/shell draait) is dat een supervisor-identity-pagina -> gewoon leesbaar.
pub fn read_counter(ptr: u64) -> u64 {
    if ptr == 0 {
        return 0;
    }
    // SAFETY: ptr wijst naar de identity-mapped (supervisor) arena-pagina van een proces.
    unsafe { core::ptr::read_volatile(ptr as *const u64) }
}

/// De ELF-bytes van het door de EuroToolchain gecompileerde /bin/hello.
pub fn program_bytes() -> &'static [u8] {
    HELLO_ELF
}

/// De ELF-bytes van /bin/cat.
pub fn cat_bytes() -> &'static [u8] {
    CAT_ELF
}

/// De ELF-bytes van /bin/linuxprog (Linux-ABI).
pub fn linuxprog_bytes() -> &'static [u8] {
    LINUXPROG_ELF
}

/// De ELF-bytes van /bin/forktest (S3 fork/waitpid-test, Linux-ABI).
pub fn forktest_bytes() -> &'static [u8] {
    FORKTEST_ELF
}

/// De ELF-bytes van /bin/execee (S3 execve-doel, Linux-ABI).
pub fn execee_bytes() -> &'static [u8] {
    EXECEE_ELF
}

/// De ELF-bytes van /bin/forkpipe (S3 pipe+fork IPC-test, Linux-ABI).
pub fn forkpipe_bytes() -> &'static [u8] {
    FORKPIPE_ELF
}

/// De ELF-bytes van /bin/ticker (S4 demo-service, Linux-ABI).
pub fn ticker_bytes() -> &'static [u8] {
    TICKER_ELF
}

/// De ELF-bytes van /bin/muslprog (musl-achtige Linux-startup).
pub fn muslprog_bytes() -> &'static [u8] {
    MUSLPROG_ELF
}

/// De ELF-bytes van /bin/argvprog (leest argc/argv/envp/auxv van de SysV-stack).
pub fn argvprog_bytes() -> &'static [u8] {
    ARGVPROG_ELF
}

/// De ELF-bytes van /bin/pieprog (echte PIE met R_X86_64_RELATIVE-relocaties).
pub fn pieprog_bytes() -> &'static [u8] {
    PIEPROG_ELF
}

/// De ELF-bytes van /bin/muslreal (echte binary gelinkt tegen musl libc).
pub fn muslreal_bytes() -> &'static [u8] {
    MUSLREAL_ELF
}

/// De ELF-bytes van /bin/muslfile (musl-binary die EuroFS leest via fopen/fgets).
pub fn muslfile_bytes() -> &'static [u8] {
    MUSLFILE_ELF
}

/// De ELF-bytes van /bin/mcat (musl-`cat` die argv[1] als bestandsnaam gebruikt).
pub fn mcat_bytes() -> &'static [u8] {
    MCAT_ELF
}

/// De ELF-bytes van /bin/mwrite (musl-binary die een bestand schrijft).
pub fn mwrite_bytes() -> &'static [u8] {
    MWRITE_ELF
}

/// De ELF-bytes van /bin/mecho (musl-`echo`: print de argumenten).
pub fn mecho_bytes() -> &'static [u8] {
    MECHO_ELF
}

/// De ELF-bytes van /bin/mupper (musl-filter: stdin -> HOOFDLETTERS).
pub fn mupper_bytes() -> &'static [u8] {
    MUPPER_ELF
}

/// De ELF-bytes van /bin/daemon (native achtergrond-hartslag-daemon).
pub fn daemon_bytes() -> &'static [u8] {
    DAEMON_ELF
}

/// De ELF-bytes van /bin/menv (musl-programma dat envp/getenv leest).
pub fn menv_bytes() -> &'static [u8] {
    MENV_ELF
}

/// De ELF-bytes van /bin/msock (musl-programma dat netwerkt via POSIX-sockets).
pub fn msock_bytes() -> &'static [u8] {
    MSOCK_ELF
}

/// De ELF-bytes van /bin/mdns (musl-programma: DNS-lookup via een UDP-socket).
pub fn mdns_bytes() -> &'static [u8] {
    MDNS_ELF
}

/// De ELF-bytes van /bin/mtrack (EuroGuard-demo: geblokkeerde tracker-verbinding).
pub fn mtrack_bytes() -> &'static [u8] {
    MTRACK_ELF
}

/// De ingebakken Ed25519-handtekening (64 bytes) van een geïnstalleerd programma,
/// gemaakt op de host met de EuroOS-developer-sleutel (userland/sign.py). De kernel
/// verifieert deze tegen de ingebakken publieke sleutel vóór uitvoering.
/// Een installeerbaar pakket dat NIET in de boot-set zit: (ELF-bytes, caps, abi).
/// Wordt via de shell `install <naam>` geïnstalleerd na Ed25519-verificatie.
pub fn installable(name: &str) -> Option<(&'static [u8], u64, bool)> {
    match name {
        "msum" => Some((MSUM_ELF, CAP_CONSOLE, true)),
        _ => None,
    }
}

pub fn program_sig(path: &str) -> Option<&'static [u8]> {
    Some(match path {
        "/bin/hello" => include_bytes!("../../userland/hello.elf.sig"),
        "/bin/msum" => include_bytes!("../../userland/msum.elf.sig"),
        "/bin/menv" => include_bytes!("../../userland/menv.elf.sig"),
        "/bin/msock" => include_bytes!("../../userland/msock.elf.sig"),
        "/bin/mdns" => include_bytes!("../../userland/mdns.elf.sig"),
        "/bin/mtrack" => include_bytes!("../../userland/mtrack.elf.sig"),
        "/bin/tlscount" => include_bytes!("../../userland/tlscount.elf.sig"),
        "/bin/isotest" => include_bytes!("../../userland/isotest.elf.sig"),
        "/bin/worker" => include_bytes!("../../userland/worker.elf.sig"),
        "/bin/mthread" => include_bytes!("../../userland/mthread.elf.sig"),
        "/bin/mpthread" => include_bytes!("../../userland/mpthread.elf.sig"),
        "/bin/mmutex" => include_bytes!("../../userland/mmutex.elf.sig"),
        "/bin/ipcrecv" => include_bytes!("../../userland/ipcrecv.elf.sig"),
        "/bin/ipcsend" => include_bytes!("../../userland/ipcsend.elf.sig"),
        "/bin/cat" => include_bytes!("../../userland/cat.elf.sig"),
        "/bin/linuxprog" => include_bytes!("../../userland/linuxprog.elf.sig"),
        "/bin/forktest" => include_bytes!("../../userland/forktest.elf.sig"),
        "/bin/execee" => include_bytes!("../../userland/execee.elf.sig"),
        "/bin/forkpipe" => include_bytes!("../../userland/forkpipe.elf.sig"),
        "/bin/ticker" => include_bytes!("../../userland/ticker.elf.sig"),
        "/bin/muslprog" => include_bytes!("../../userland/muslprog.elf.sig"),
        "/bin/argvprog" => include_bytes!("../../userland/argvprog.elf.sig"),
        "/bin/pieprog" => include_bytes!("../../userland/pieprog.elf.sig"),
        "/bin/muslreal" => include_bytes!("../../userland/muslreal.elf.sig"),
        "/bin/muslfile" => include_bytes!("../../userland/muslfile.elf.sig"),
        "/bin/mcat" => include_bytes!("../../userland/mcat.elf.sig"),
        "/bin/mwrite" => include_bytes!("../../userland/mwrite.elf.sig"),
        "/bin/mecho" => include_bytes!("../../userland/mecho.elf.sig"),
        "/bin/mupper" => include_bytes!("../../userland/mupper.elf.sig"),
        "/bin/daemon" => include_bytes!("../../userland/daemon.elf.sig"),
        _ => return None,
    })
}

/// Verifieer de Ed25519-handtekening van een programma (op naam) over de
/// daadwerkelijk-geladen bytes. `true` = authentiek + ongewijzigd → mag draaien.
pub fn verify_program(path: &str, bytes: &[u8]) -> bool {
    match program_sig(path) {
        Some(sig) => crate::crypto::verify(bytes, sig),
        None => false, // geen handtekening bekend → niet vertrouwd
    }
}

// ── Minimale ELF64-loader ─────────────────────────────────────────────────
// Bounds-veilig (audit H11/kernel-H6): een misvormde/te korte ELF mag deze lezers
// niet laten panieken; bij een out-of-range offset → 0 (en de bound-checks erboven
// verwerpen de header verderop).
fn rd_u16(b: &[u8], o: usize) -> u16 {
    match b.get(o..o + 2) {
        Some(s) => u16::from_le_bytes([s[0], s[1]]),
        None => 0,
    }
}
fn rd_u32(b: &[u8], o: usize) -> u32 {
    match b.get(o..o + 4) {
        Some(s) => u32::from_le_bytes([s[0], s[1], s[2], s[3]]),
        None => 0,
    }
}
fn rd_u64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    match b.get(o..o + 8) {
        Some(s) => a.copy_from_slice(s),
        None => return 0,
    }
    u64::from_le_bytes(a)
}

/// Max. aantal aaneengesloten User-pagina's dat een programma mag beslaan (1 MiB).
/// Begrenst de allocatie en houdt alles binnen de USER-gemapte onderste 1 GiB.
const MAX_PROG_PAGES: usize = 256;

/// Hoeveel User-pagina's heeft dit programma nodig (hoogste vaddr+memsz, of de
/// platte lengte)? Bepaalt vooraf de aaneengesloten frame-allocatie.
fn program_span_pages(program: &[u8]) -> usize {
    let span = if program.len() >= 4 && &program[0..4] == b"\x7fELF" && program.len() >= 64 {
        let e_phoff = rd_u64(program, 32) as usize;
        let e_phentsize = rd_u16(program, 54) as usize;
        let e_phnum = rd_u16(program, 56) as usize;
        let mut hi = 0u64;
        for i in 0..e_phnum {
            let ph = e_phoff + i * e_phentsize;
            if ph + 56 > program.len() || rd_u32(program, ph) != 1 {
                continue; // alleen PT_LOAD
            }
            hi = hi.max(rd_u64(program, ph + 16) + rd_u64(program, ph + 40)); // vaddr+memsz
        }
        hi as usize
    } else {
        program.len()
    };
    (((span + 0xFFF) / 4096).max(1)).min(MAX_PROG_PAGES)
}

/// Resultaat van het laden: entry + program-header-info voor de auxv.
/// (musl's `_start` leest AT_PHDR/AT_PHENT/AT_PHNUM/AT_ENTRY/AT_BASE.)
#[derive(Clone, Copy)]
struct LoadInfo {
    entry: u64,
    phdr: u64,  // runtime-adres van de program-header-tabel (0 = geen)
    phent: u64, // grootte van één program-header
    phnum: u64, // aantal program-headers
    base: u64,  // load-bias (begin van het frame-venster)
    /// W^X-bitmaps over de 512 4 KiB-pagina's van de 2 MiB-arena. `exec_pages`: pagina
    /// valt onder een UITVOERBAAR segment (PF_X). `writ_pages`: onder een SCHRIJFBAAR
    /// segment (PF_W). build_address_space mapt exec-only → R-X, exec+writ → RWX (een
    /// binary met een gemengd RWE-segment kan W^X niet afdwingen), de rest → RW + NX.
    exec_pages: [u64; 8],
    writ_pages: [u64; 8],
}

/// Markeer de 4 KiB-pagina's die `[start, start+len)` (arena-relatieve offset)
/// raken als uitvoerbaar in de W^X-bitmap.
fn mark_exec_pages(bits: &mut [u64; 8], start: u64, len: u64) {
    if len == 0 {
        return;
    }
    let first = (start / 4096) as usize;
    let last = ((start + len - 1) / 4096) as usize;
    let mut p = first;
    while p <= last && p < 512 {
        bits[p / 64] |= 1u64 << (p % 64);
        p += 1;
    }
}

/// Pas R_X86_64_RELATIVE-relocaties toe: voor een PIE (ET_DYN) gelinkt op 0 en
/// geladen op `base` geldt `*(base + r_offset) = base + r_addend`. Dit is precies
/// wat musl's static-PIE self-reloc anders zelf doet — wij doen het in de kernel.
/// We lezen alle tabellen uit het GELADEN geheugen (base + vaddr): file-offset en
/// vaddr lopen in een PIE uiteen, maar in het geladen image klopt vaddr altijd.
fn apply_relocations(elf: &[u8], base: u64, limit: u64) {
    let e_phoff = rd_u64(elf, 32) as usize;
    let e_phentsize = rd_u16(elf, 54) as usize;
    let e_phnum = rd_u16(elf, 56) as usize;
    // Zoek PT_DYNAMIC (p_type == 2).
    let mut dyn_vaddr = 0u64;
    let mut dyn_sz = 0usize;
    for i in 0..e_phnum {
        let ph = e_phoff + i * e_phentsize;
        if ph + 56 > elf.len() || rd_u32(elf, ph) != 2 {
            continue;
        }
        dyn_vaddr = rd_u64(elf, ph + 16);
        dyn_sz = rd_u64(elf, ph + 32) as usize;
        break;
    }
    if dyn_vaddr == 0 {
        return; // geen dynamische sectie (flat/statisch-gelinkte ELF)
    }
    // Lees de dynamische entries uit het geladen geheugen; verzamel de RELA-tabel.
    let rd_loaded = |a: u64| unsafe { ((base + a) as *const u64).read() };
    let mut rela = 0u64;
    let mut relasz = 0u64;
    let mut relaent = 24u64;
    let mut o = 0u64;
    while (o as usize) + 16 <= dyn_sz {
        let tag = rd_loaded(dyn_vaddr + o);
        let val = rd_loaded(dyn_vaddr + o + 8);
        match tag {
            7 => rela = val,    // DT_RELA   (vaddr van de tabel)
            8 => relasz = val,  // DT_RELASZ (bytes)
            9 => relaent = val, // DT_RELAENT
            0 => break,         // DT_NULL
            _ => {}
        }
        o += 16;
    }
    if rela == 0 || relasz == 0 || relaent == 0 {
        return;
    }
    let mut off = 0u64;
    let mut applied = 0u32;
    while off + relaent <= relasz {
        let e = rela + off;
        let r_offset = rd_loaded(e);
        let r_info = rd_loaded(e + 8);
        let r_addend = rd_loaded(e + 16);
        if (r_info & 0xffff_ffff) == 8 && r_offset < limit {
            // R_X86_64_RELATIVE: *(base + r_offset) = base + r_addend
            unsafe { ((base + r_offset) as *mut u64).write(base.wrapping_add(r_addend)) };
            applied += 1;
        }
        off += relaent;
    }
    crate::serial_println!("[elf] {applied} R_X86_64_RELATIVE relocaties toegepast @ base {base:#x}");
}

/// Laad de PT_LOAD-segmenten van een ELF64-binary op basis-adres `base` (positie-
/// onafhankelijk; gelinkt op vaddr 0). `pages` = grootte van het frame-venster.
fn load_elf64(elf: &[u8], base: u64, pages: usize) -> Option<LoadInfo> {
    if elf.len() < 64 || &elf[0..4] != b"\x7fELF" || elf[4] != 2 || elf[5] != 1 {
        return None; // geen 64-bit little-endian ELF
    }
    if rd_u16(elf, 18) != 0x3E {
        return None; // niet x86-64
    }
    let limit = (pages * 4096) as u64;
    let e_entry = rd_u64(elf, 24);
    let e_phoff = rd_u64(elf, 32) as usize;
    let e_phentsize = rd_u16(elf, 54) as usize;
    let e_phnum = rd_u16(elf, 56) as usize;
    let mut phdr_vaddr = 0u64; // vaddr van de PHDR-tabel als die in een PT_LOAD valt
    let mut exec_pages = [0u64; 8]; // W^X: welke pagina's uitvoerbaar zijn (PF_X)
    let mut writ_pages = [0u64; 8]; // W^X: welke pagina's schrijfbaar zijn (PF_W)
    for i in 0..e_phnum {
        // Overloop-veilig (audit H11): een enorme e_phoff/e_phentsize mag de
        // bound-check niet via wrap-around omzeilen.
        let ph = match e_phoff.checked_add(i.checked_mul(e_phentsize)?) {
            Some(v) => v,
            None => continue,
        };
        if ph.checked_add(56).map_or(true, |e| e > elf.len()) {
            continue;
        }
        let p_type = rd_u32(elf, ph);
        // PT_PHDR (6) geeft de vaddr van de program-header-tabel rechtstreeks.
        if p_type == 6 {
            phdr_vaddr = rd_u64(elf, ph + 16);
        }
        if p_type != 1 {
            continue; // verder alleen PT_LOAD
        }
        let p_flags = rd_u32(elf, ph + 4);
        let p_offset = rd_u64(elf, ph + 8) as usize;
        let p_vaddr = rd_u64(elf, ph + 16);
        let p_filesz = rd_u64(elf, ph + 32) as usize;
        let p_memsz = rd_u64(elf, ph + 40) as usize;
        // Overloop-veilig (audit H11): wrap-around mag de venster-check niet omzeilen.
        let file_end = p_offset.checked_add(p_filesz)?;
        let mem_end = p_vaddr.checked_add(p_memsz as u64)?;
        if file_end > elf.len() || mem_end > limit {
            return None;
        }
        // W^X: noteer per pagina of een uitvoerbaar (PF_X = bit 0) en/of schrijfbaar
        // (PF_W = bit 1) segment hem dekt.
        if p_flags & 1 != 0 {
            mark_exec_pages(&mut exec_pages, p_vaddr, p_memsz as u64);
        }
        if p_flags & 2 != 0 {
            mark_exec_pages(&mut writ_pages, p_vaddr, p_memsz as u64);
        }
        // De PHDR-tabel zit standaard binnen het eerste PT_LOAD (op file-offset
        // e_phoff). Als er geen PT_PHDR is, leiden we de vaddr daaruit af.
        if phdr_vaddr == 0 && p_offset <= e_phoff && e_phoff < p_offset + p_filesz {
            phdr_vaddr = p_vaddr + (e_phoff - p_offset) as u64;
        }
        // SAFETY: het segment past binnen het toegewezen frame-venster (gecheckt).
        unsafe {
            let dst = (base + p_vaddr) as *mut u8;
            core::ptr::copy_nonoverlapping(elf[p_offset..].as_ptr(), dst, p_filesz);
            if p_memsz > p_filesz {
                core::ptr::write_bytes(dst.add(p_filesz), 0, p_memsz - p_filesz); // .bss nullen
            }
        }
    }
    // Relocaties toepassen (no-op voor niet-PIE/flat-statische binaries).
    apply_relocations(elf, base, limit);
    Some(LoadInfo {
        entry: base + e_entry,
        phdr: if phdr_vaddr != 0 { base + phdr_vaddr } else { 0 },
        phent: e_phentsize as u64,
        phnum: e_phnum as u64,
        base,
        exec_pages,
        writ_pages,
    })
}

/// Laad een programma (ELF of flat) op `base` (venster van `pages` frames).
fn load_program(program: &[u8], base: u64, pages: usize) -> LoadInfo {
    if program.len() >= 4 && &program[0..4] == b"\x7fELF" {
        if let Some(info) = load_elf64(program, base, pages) {
            return info;
        }
    }
    // Flat blob (geen ELF): entry = base, geen program-headers. De hele geladen
    // regio is machinecode → markeer die pagina's uitvoerbaar (W^X).
    let n = program.len().min(pages * 4096);
    // SAFETY: flat blob, past in het venster.
    unsafe {
        core::ptr::copy_nonoverlapping(program.as_ptr(), base as *mut u8, n);
    }
    // Flat blob = gemengde code+data (RWX); markeer de geladen regio zowel
    // uitvoerbaar als schrijfbaar zodat build_address_space hem RWX mapt.
    let mut exec_pages = [0u64; 8];
    let mut writ_pages = [0u64; 8];
    mark_exec_pages(&mut exec_pages, 0, n as u64);
    mark_exec_pages(&mut writ_pages, 0, n as u64);
    LoadInfo { entry: base, phdr: 0, phent: 0, phnum: 0, base, exec_pages, writ_pages }
}

// ── H3: in-kernel dynamische linker ────────────────────────────────────────
// Laadt een dynamisch-gelinkte executable + zijn DT_NEEDED-shared-libraries in
// dezelfde adresruimte en lost de cross-module-symbolen op (R_X86_64_JUMP_SLOT /
// GLOB_DAT) — zoals een userspace-`ld.so`, maar in de kernel (deterministisch,
// EuroGuard-bestuurd). Alle tabellen worden uit het GELADEN geheugen (base+vaddr)
// gelezen; de .so wordt op een eigen sub-offset binnen de 2 MiB-arena geplaatst.

/// Merge een W^X-bitmap geshift met `page_off` pagina's (voor een module die op een
/// arena-offset geladen is) in de gecombineerde arena-bitmap.
fn merge_shifted(dst: &mut [u64; 8], src: &[u64; 8], page_off: usize) {
    for p in 0..512usize {
        if src[p / 64] & (1u64 << (p % 64)) != 0 {
            let q = p + page_off;
            if q < 512 {
                dst[q / 64] |= 1u64 << (q % 64);
            }
        }
    }
}

/// Lees een `DT_<want>`-waarde uit de geladen dynamische tabel van een module.
fn dyn_value(base: u64, elf: &[u8], want: u64) -> Option<u64> {
    let e_phoff = rd_u64(elf, 32) as usize;
    let e_phentsize = rd_u16(elf, 54) as usize;
    let e_phnum = rd_u16(elf, 56) as usize;
    let (mut dv, mut dsz) = (0u64, 0usize);
    for i in 0..e_phnum {
        let ph = e_phoff + i * e_phentsize;
        if ph + 56 > elf.len() || rd_u32(elf, ph) != 2 {
            continue;
        }
        dv = rd_u64(elf, ph + 16);
        dsz = rd_u64(elf, ph + 32) as usize;
        break;
    }
    if dv == 0 {
        return None;
    }
    let mut o = 0u64;
    while (o as usize) + 16 <= dsz {
        let tag = unsafe { ((base + dv + o) as *const u64).read() };
        let val = unsafe { ((base + dv + o + 8) as *const u64).read() };
        if tag == 0 {
            return None; // DT_NULL
        }
        if tag == want {
            return Some(val);
        }
        o += 16;
    }
    None
}

/// Lees een C-string (max `buf.len()`) op een geladen adres in `buf`; geef de lengte.
fn read_cstr(addr: u64, buf: &mut [u8]) -> usize {
    let mut n = 0;
    while n + 1 < buf.len() {
        let c = unsafe { ((addr + n as u64) as *const u8).read() };
        if c == 0 {
            break;
        }
        buf[n] = c;
        n += 1;
    }
    n
}

/// Vind een GEËXPORTEERD symbool op naam in een geladen module → `base + st_value`.
/// Itereert de dynamische symbooltabel (aantal uit DT_HASH's nchain).
fn find_export(base: u64, elf: &[u8], name: &[u8]) -> Option<u64> {
    let symtab = dyn_value(base, elf, 6)?; // DT_SYMTAB
    let strtab = dyn_value(base, elf, 5)?; // DT_STRTAB
    // Symbool-aantal: uit DT_HASH's nchain als die er is, anders (moderne .so's hebben
    // alleen GNU_HASH) afgeleid uit (DT_STRTAB − DT_SYMTAB)/DT_SYMENT — de linker legt
    // `.dynsym` altijd direct vóór `.dynstr`.
    let syment = dyn_value(base, elf, 11).unwrap_or(24); // DT_SYMENT
    let count = if let Some(hash) = dyn_value(base, elf, 4) {
        (unsafe { ((base + hash + 4) as *const u32).read() }) as u64 // DT_HASH nchain
    } else if strtab > symtab && syment > 0 {
        (strtab - symtab) / syment
    } else {
        0
    };
    let mut nb = [0u8; 64];
    for i in 1..count {
        let sym = base + symtab + i * syment;
        let st_name = unsafe { (sym as *const u32).read() } as u64;
        let st_shndx = unsafe { ((sym + 6) as *const u16).read() };
        if st_shndx == 0 {
            continue; // SHN_UNDEF: niet hier gedefinieerd
        }
        let nl = read_cstr(base + strtab + st_name, &mut nb);
        if &nb[..nl] == name {
            let st_value = unsafe { ((sym + 8) as *const u64).read() };
            return Some(base + st_value);
        }
    }
    None
}

/// Resolve de symbool-relocaties (R_X86_64_JUMP_SLOT + GLOB_DAT) van de exe tegen de
/// geladen libs: schrijf het echte symbooladres in het GOT-slot. Geeft (resolved,
/// unresolved).
fn link_symbol_relocations(exe_base: u64, exe_elf: &[u8], libs: &[(u64, &[u8])]) -> (u32, u32) {
    let (symtab, strtab) = match (dyn_value(exe_base, exe_elf, 6), dyn_value(exe_base, exe_elf, 5)) {
        (Some(s), Some(t)) => (s, t),
        _ => return (0, 0),
    };
    let (mut resolved, mut unresolved) = (0u32, 0u32);
    // .rela.plt (DT_JMPREL=23, DT_PLTRELSZ=2) + .rela.dyn (DT_RELA=7, DT_RELASZ=8).
    let tables = [
        (dyn_value(exe_base, exe_elf, 23), dyn_value(exe_base, exe_elf, 2)),
        (dyn_value(exe_base, exe_elf, 7), dyn_value(exe_base, exe_elf, 8)),
    ];
    let mut nb = [0u8; 64];
    for (rela_opt, sz_opt) in tables {
        let (rela, sz) = match (rela_opt, sz_opt) {
            (Some(r), Some(s)) => (r, s),
            _ => continue,
        };
        let mut off = 0u64;
        while off + 24 <= sz {
            let e = exe_base + rela + off;
            let r_offset = unsafe { (e as *const u64).read() };
            let r_info = unsafe { ((e + 8) as *const u64).read() };
            let rtype = r_info & 0xffff_ffff;
            let sym_idx = r_info >> 32;
            // 7 = JUMP_SLOT (PLT), 6 = GLOB_DAT (data). Beide: *(GOT) = symbooladres.
            if (rtype == 7 || rtype == 6) && sym_idx != 0 {
                let sym = exe_base + symtab + sym_idx * 24;
                let st_name = unsafe { (sym as *const u32).read() } as u64;
                let nl = read_cstr(exe_base + strtab + st_name, &mut nb);
                let name = &nb[..nl];
                let mut done = false;
                for (lb, le) in libs {
                    if let Some(addr) = find_export(*lb, le, name) {
                        unsafe { ((exe_base + r_offset) as *mut u64).write(addr) };
                        resolved += 1;
                        done = true;
                        break;
                    }
                }
                if !done {
                    unresolved += 1;
                    crate::serial_println!(
                        "[h3] ONGERESOLVED symbool: {}",
                        core::str::from_utf8(name).unwrap_or("?")
                    );
                }
            }
            off += 24;
        }
    }
    (resolved, unresolved)
}

/// H3-zelftest: laad de dynamisch-gelinkte `dyntest.elf` + `libeuro.so` in één
/// adresruimte, link ze in-kernel, en draai dyntest in ring 3. dyntest roept
/// `euro_answer()` aan uit de .so (via PLT/GOT) → "H3: 42" + exit(42). Geeft
/// (output, exit_code).
/// De ingebedde dynamisch-gelinkte test-exe + .so (voor populate_fs/zelftests).
pub fn dyntest_bytes() -> &'static [u8] {
    DYNTEST_ELF
}
pub fn libeuro_bytes() -> &'static [u8] {
    LIBEURO_SO
}

/// Parse de DT_NEEDED-shared-library-namen uit een dynamisch-gelinkte ELF (uit de
/// FILE-bytes; vertaalt de DT_STRTAB-vaddr naar een file-offset via de PT_LOADs).
pub fn needed_libs(program: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    if program.len() < 64 || &program[0..4] != b"\x7fELF" {
        return out;
    }
    let e_phoff = rd_u64(program, 32) as usize;
    let e_phentsize = rd_u16(program, 54) as usize;
    let e_phnum = rd_u16(program, 56) as usize;
    let mut dyn_off = 0usize;
    let mut dyn_sz = 0usize;
    let mut loads: Vec<(u64, usize, usize)> = Vec::new(); // (vaddr, file_off, filesz)
    for i in 0..e_phnum {
        let ph = e_phoff + i * e_phentsize;
        if ph + 56 > program.len() {
            continue;
        }
        match rd_u32(program, ph) {
            2 => {
                dyn_off = rd_u64(program, ph + 8) as usize;
                dyn_sz = rd_u64(program, ph + 32) as usize;
            }
            1 => loads.push((rd_u64(program, ph + 16), rd_u64(program, ph + 8) as usize, rd_u64(program, ph + 32) as usize)),
            _ => {}
        }
    }
    if dyn_off == 0 {
        return out;
    }
    let v2o = |vaddr: u64| -> Option<usize> {
        loads.iter().find(|&&(va, _, fsz)| vaddr >= va && vaddr < va + fsz as u64).map(|&(va, fo, _)| fo + (vaddr - va) as usize)
    };
    let mut strtab = 0u64;
    let mut needed: Vec<u64> = Vec::new();
    let mut o = 0;
    while o + 16 <= dyn_sz && dyn_off + o + 16 <= program.len() {
        let tag = rd_u64(program, dyn_off + o);
        let val = rd_u64(program, dyn_off + o + 8);
        match tag {
            0 => break,
            1 => needed.push(val), // DT_NEEDED
            5 => strtab = val,     // DT_STRTAB
            _ => {}
        }
        o += 16;
    }
    let strtab_off = match v2o(strtab) {
        Some(x) => x,
        None => return out,
    };
    for noff in needed {
        let mut p = strtab_off + noff as usize;
        let mut s = String::new();
        while p < program.len() && program[p] != 0 {
            s.push(program[p] as char);
            p += 1;
        }
        if !s.is_empty() {
            out.push(s);
        }
    }
    out
}

/// Laad een dynamisch-gelinkte exe + zijn shared libraries in één adresruimte, link
/// ze in-kernel (DT_NEEDED → .so laden → JUMP_SLOT/GLOB_DAT resolven), en draai de
/// exe in ring 3. Geeft (output, exit_code). Tot 2 libs (in de arena vóór de heap).
pub fn run_dynamic(
    falloc: &mut FrameAllocator,
    exe: &[u8],
    libs: &[&[u8]],
    argv: &[&[u8]],
    caps: u64,
    linux_abi: bool,
) -> (String, u64) {
    init_syscall_msrs();
    CURRENT_CAPS.store(caps, Ordering::Relaxed);
    LINUX_ABI.store(linux_abi, Ordering::Relaxed);
    *CURRENT_APP.lock() = argv.first().map(|a| String::from_utf8_lossy(a).into_owned()).unwrap_or_default();
    unsafe {
        EXITED = 0;
        EXIT_CODE = 0;
    }
    OUTPUT.lock().clear();
    reset_fd_table();

    const MIB2: u64 = 1 << 21;
    let arena = match falloc.allocate_aligned(512, 512) {
        Ok(a) => a,
        Err(_) => return (String::from("(geen arena)"), u64::MAX),
    };
    let code = arena;
    let stack_top = arena + MIB2;
    HEAP_BREAK.store(arena + 0x80000, Ordering::Relaxed);
    ARENA_BASE.store(arena, Ordering::Relaxed); // audit C1: valideer user-pointers tegen deze arena
    HEAP_END.store(arena + 0x180000, Ordering::Relaxed);

    let exe_pages = program_span_pages(exe);
    let mut info = load_program(exe, code, exe_pages);
    // Plaats elke .so op een eigen 128 KiB-venster (0x40000, 0x60000) vóór de heap.
    let mut loaded: Vec<(u64, &[u8])> = Vec::new();
    for (i, lib) in libs.iter().enumerate().take(2) {
        let lib_base = arena + 0x40000 + (i as u64) * 0x20000;
        let lib_pages = program_span_pages(lib);
        if let Some(lib_info) = load_elf64(lib, lib_base, lib_pages) {
            let lib_poff = ((lib_base - arena) / 4096) as usize;
            merge_shifted(&mut info.exec_pages, &lib_info.exec_pages, lib_poff);
            merge_shifted(&mut info.writ_pages, &lib_info.writ_pages, lib_poff);
            loaded.push((lib_base, lib));
        }
    }
    let (resolved, unresolved) = link_symbol_relocations(code, exe, &loaded);
    crate::serial_println!(
        "[h3] dynlinker: {} lib(s) geladen, {} symbool-relocatie(s) resolved, {} ongeresolved",
        loaded.len(),
        resolved,
        unresolved
    );

    let rsp = unsafe { setup_user_stack(stack_top, argv, &info) };
    let entry = info.entry;
    let sel = crate::gdt::selectors();
    let user_cs = (sel.user_code.0 | 3) as u64;
    let user_ss = (sel.user_data.0 | 3) as u64;
    let pml4 = crate::paging::build_address_space(falloc, arena, &info.exec_pages, &info.writ_pages);
    let boot = crate::sched::boot_pml4();
    unsafe { crate::gdt::set_rsp0(KERNEL_RSP) };
    FG_ACTIVE.store(true, Ordering::Relaxed);
    // SAFETY: zelfde patroon als run_args — terugkeer via sys_exit of force-return.
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) pml4, options(nostack, preserves_flags));
        enter_ring3(user_cs, user_ss, entry, rsp);
        core::arch::asm!("mov cr3, {}", in(reg) boot, options(nostack, preserves_flags));
    }
    FG_ACTIVE.store(false, Ordering::Relaxed);
    for f in 0..512u64 {
        let _ = falloc.free(arena + f * 4096);
    }
    crate::paging::free_address_space(falloc, pml4);
    let exit_code = unsafe { EXIT_CODE };
    let out = OUTPUT.lock().clone();
    (out, exit_code)
}

/// H3-zelftest met de ingebedde artefacten: dyntest.elf + libeuro.so.
pub fn dynlink_selftest(falloc: &mut FrameAllocator) -> (String, u64) {
    run_dynamic(falloc, DYNTEST_ELF, &[LIBEURO_SO], &[b"dyntest"], CAP_CONSOLE, true)
}

/// `[uptr]` — bewijst dat de syscall-laag user-pointers tegen de arena valideert:
/// een pointer BINNEN de arena slaagt, een vervalste pointer ERBUITEN (kernel-
/// adres, of een arena-overschrijdende lengte) wordt geweigerd i.p.v. kernel-
/// geheugen te lezen/schrijven. Stelt tijdelijk een nep-arena in over een echte
/// stackbuffer en herstelt `ARENA_BASE` daarna.
pub fn user_ptr_selftest() {
    let mut scratch = [0u8; 64];
    let base = scratch.as_ptr() as u64;
    let prev = ARENA_BASE.load(Ordering::Relaxed);
    // Nep-arena met `base` als ondergrens. De arena-span is ARENA_SPAN (2 MiB),
    // dus we raken ALLEEN offset 0 met echte toegang (binnen de 64-B scratch); de
    // "geweigerd"-gevallen gebruiken adressen ECHT buiten [base, base+ARENA_SPAN),
    // zodat de check faalt vóór enige dereferentie — geen OOB op de stack.
    ARENA_BASE.store(base, Ordering::Relaxed);
    let outside = base.wrapping_add(ARENA_SPAN); // == top, valt buiten de arena

    // 1) Binnen de arena (offset 0): schrijven + teruglezen slaagt.
    let inside_ok = copy_to_user(base, b"euro") && {
        let rb: u32 = read_user(base).unwrap_or(0);
        rb == u32::from_le_bytes(*b"euro")
    };

    // 2) Een kernel-adres vlak vóór de arena (base-1) wordt geweigerd.
    let below_denied = !in_user_arena(base.wrapping_sub(1), 1)
        && !copy_to_user(base.wrapping_sub(1), b"x")
        && read_user::<u32>(base.wrapping_sub(1)).is_none();

    // 3) Een adres vlak ná de arena (base+ARENA_SPAN) wordt geweigerd; de helpers
    //    raken het geheugen niet (check faalt eerst).
    let above_denied = !in_user_arena(outside, 1)
        && !copy_to_user(outside, b"x")
        && !write_user(outside, 0xFFu8)
        && copy_from_user(outside, 16).is_none();

    // 4) Een lengte die de arena-bovengrens overschrijdt wordt geweigerd zonder te lezen.
    let span_denied =
        !in_user_arena(base, ARENA_SPAN as usize + 1) && copy_from_user(base, ARENA_SPAN as usize + 1).is_none();

    // 5) user_cstr op een buiten-arena pointer leest niets (lege string).
    let cstr_bounded = user_cstr(outside, 64).is_empty();

    ARENA_BASE.store(prev, Ordering::Relaxed); // arena herstellen

    let all = inside_ok && below_denied && above_denied && span_denied && cstr_bounded;
    crate::serial_println!(
        "[uptr] user-pointer-validatie: binnen={} onder={} boven={} span={} cstr={} -> {}",
        inside_ok, below_denied, above_denied, span_denied, cstr_bounded,
        if all { "OK" } else { "FAAL" }
    );
}

/// Bouw een SysV-x86-64 initiële stack: `argc`, `argv[]`, `envp[]`, `auxv[]`,
/// plus de bijbehorende strings + 16 AT_RANDOM-bytes. Dit is precies het
/// contract dat een musl/glibc `_start` van de kernel verwacht. `info` levert de
/// program-header-info voor de auxv. Geeft de (16-uitgelijnde) rsp terug waar
/// `[rsp]==argc`.
unsafe fn setup_user_stack(stack_top: u64, argv: &[&[u8]], info: &LoadInfo) -> u64 {
    let mut p = stack_top;
    // 16 "random" bytes (AT_RANDOM) — musl gebruikt dit voor stack-canary/TLS-guard.
    p -= 16;
    let random_ptr = p;
    for i in 0..16 {
        (random_ptr as *mut u8).add(i).write(0x5Au8 ^ (i as u8).wrapping_mul(31));
    }
    // Elke argv-string (NUL-getermineerd) op de stack zetten; pointers bewaren.
    let mut argptrs: alloc::vec::Vec<u64> = alloc::vec::Vec::with_capacity(argv.len());
    for a in argv {
        p -= a.len() as u64 + 1;
        let ptr = p;
        for (i, b) in a.iter().enumerate() {
            (ptr as *mut u8).add(i).write(*b);
        }
        (ptr as *mut u8).add(a.len()).write(0);
        argptrs.push(ptr);
    }
    // Omgevingsvariabelen (envp) — het systeemmilieu dat elk proces erft.
    let env = ENV.lock();
    let mut envptrs: alloc::vec::Vec<u64> = alloc::vec::Vec::with_capacity(env.len());
    for e in env.iter() {
        let bytes = e.as_bytes();
        p -= bytes.len() as u64 + 1;
        let ptr = p;
        for (i, b) in bytes.iter().enumerate() {
            (ptr as *mut u8).add(i).write(*b);
        }
        (ptr as *mut u8).add(bytes.len()).write(0);
        envptrs.push(ptr);
    }
    p &= !0xF; // strings-regio 16-uitgelijnd

    // auxv-paren (type, waarde), afgesloten met AT_NULL. Volledige set voor musl:
    //   AT_PHDR=3, AT_PHENT=4, AT_PHNUM=5, AT_PAGESZ=6, AT_BASE=7,
    //   AT_ENTRY=9, AT_RANDOM=25.
    let aux: [(u64, u64); 8] = [
        (3, info.phdr),
        (4, info.phent),
        (5, info.phnum),
        (6, 4096),
        (7, 0), // geen interpreter (static-PIE)
        (9, info.entry),
        (25, random_ptr),
        (0, 0), // AT_NULL
    ];
    // Slots: argc(1) + argv-ptrs(n) + argv-NULL(1) + env-ptrs(m) + env-NULL(1) + auxv(2*8).
    let nslots = 1 + argptrs.len() as u64 + 1 + envptrs.len() as u64 + 1 + (aux.len() as u64) * 2;
    let sp = (p - nslots * 8) & !0xF;
    let mut w = sp;
    let mut put = |val: u64| {
        (w as *mut u64).write(val);
        w += 8;
    };
    put(argptrs.len() as u64); // argc
    for ptr in &argptrs {
        put(*ptr); // argv[i]
    }
    put(0); // argv-terminator
    for ptr in &envptrs {
        put(*ptr); // envp[i]
    }
    put(0); // envp-terminator
    for (t, v) in aux {
        put(t);
        put(v);
    }
    sp
}

// ── D1a: syscall-profilering (inventarisatie vóór de fijnmazige SMP-locking) ──
// Per syscall-nummer: aantal + totale tijd (ns), gemeten met de HPET rond de
// dispatch. Toont waar de kernel-tijd zit — de hot paths die straks per-subsysteem-
// locks (i.p.v. de globale IF=0-serialisatie) het meest opleveren.
const PROF_N: usize = 512;
static PROF_COUNT: [core::sync::atomic::AtomicU64; PROF_N] = {
    const Z: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    [Z; PROF_N]
};
static PROF_NS: [core::sync::atomic::AtomicU64; PROF_N] = {
    const Z: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    [Z; PROF_N]
};

/// RAII-meter: leest de HPET bij entry en boekt de verstreken tijd op exit (elk
/// `return`-pad). Lichtgewicht; verstoort de syscall-semantiek niet.
struct SyscallProfile {
    num: usize,
    start: u64,
}
impl SyscallProfile {
    fn start(num: u64) -> Self {
        SyscallProfile { num: (num as usize).min(PROF_N - 1), start: crate::hpet::ns() }
    }
}
impl Drop for SyscallProfile {
    fn drop(&mut self) {
        let dt = crate::hpet::ns().wrapping_sub(self.start);
        PROF_COUNT[self.num].fetch_add(1, Ordering::Relaxed);
        PROF_NS[self.num].fetch_add(dt, Ordering::Relaxed);
    }
}

/// Profielregels: de syscalls gesorteerd op totale tijd (top 12).
pub fn syscall_profile_lines() -> alloc::vec::Vec<alloc::string::String> {
    let mut rows: alloc::vec::Vec<(usize, u64, u64)> = (0..PROF_N)
        .map(|i| (i, PROF_COUNT[i].load(Ordering::Relaxed), PROF_NS[i].load(Ordering::Relaxed)))
        .filter(|&(_, c, _)| c > 0)
        .collect();
    rows.sort_by(|a, b| b.2.cmp(&a.2)); // op totale tijd
    let mut out = alloc::vec![alloc::string::String::from("SYSCALL  COUNT      TOTAAL(us)  GEMID(ns)")];
    for (num, count, ns) in rows.into_iter().take(12) {
        out.push(alloc::format!("  {num:<5} {count:>8}  {:>9}  {:>9}", ns / 1000, ns / count.max(1)));
    }
    if out.len() == 1 {
        out.push("  (nog geen syscalls geprofileerd)".into());
    }
    out
}

/// Syscall-dispatcher (ring 0). Geeft de return-waarde in rax.
#[no_mangle]
pub extern "sysv64" fn syscall_dispatch(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64 {
    let _prof = SyscallProfile::start(num);
    // Komt deze syscall van de gescheduelde achtergrond-daemon? Dan een aparte
    // dispatcher + uitvoerbuffer (los van de globale voorgrond-staat).
    let cur = crate::sched::current();
    if cur == DAEMON_TASK.load(Ordering::Relaxed) {
        return daemon_dispatch(num, a1, a2, a3);
    }
    // Preemptief per-proces (PCB): routeer naar de juiste achtergrond-musl-proces-
    // staat (eigen heap/uitvoer/pid). Een THREAD deelt de PCB van zijn proces, dus
    // matchen we op het hoofd-task OF op een van de thread-tasks.
    {
        let mut bg = BG.lock();
        if let Some(pos) = bg.iter().position(|p| p.task == cur || p.threads.contains(&cur)) {
            // fork/vfork/wait4 MUTEREN de BG-tabel (een kind toevoegen / status
            // ophalen) en kunnen dus niet onder de p-borrow van bg_dispatch draaien.
            match num {
                57 | 58 => return do_fork(&mut bg, pos), // fork / vfork
                61 => {
                    // wait4(pid, *status, options, *rusage): non-blocking reap.
                    let parent_pid = bg[pos].pid;
                    return do_wait4(parent_pid, a1, a2);
                }
                _ => {
                    let p = &mut bg[pos];
                    return bg_dispatch(p, num, a1, a2, a3, a4, a5);
                }
            }
        }
    }
    // Linux-ABI-compatibiliteit: programma's gecompileerd voor x86_64-linux
    // gebruiken Linux-syscallnummers + -semantiek. Vertaal naar onze handlers.
    if LINUX_ABI.load(Ordering::Relaxed) {
        return linux_dispatch(num, a1, a2, a3, a4, a5);
    }
    // Capability-handhaving: weiger syscalls waarvoor het proces geen recht heeft.
    let need = required_cap(num);
    if need != 0 && !has_cap(need) {
        crate::serial_println!("[cap] syscall {num} GEWEIGERD — ontbrekende capability");
        return u64::MAX; // -EPERM
    }
    match num {
        60 => 0, // sys_net() — netwerktoegang (vereist CAP_NET; stub die slaagt indien toegestaan)
        12 => {
            // sys_sbrk(inc) -> oud break (of -1 bij overschrijding). inc=0 = query.
            let old = HEAP_BREAK.load(Ordering::Relaxed);
            if a1 == 0 {
                return old;
            }
            // Overloop-veilig (audit M7): een enorme `a1` mag de `> HEAP_END`-poort
            // niet via wrap-around omzeilen.
            let new = match old.checked_add(a1) {
                Some(n) => n,
                None => return u64::MAX,
            };
            if new > HEAP_END.load(Ordering::Relaxed) {
                return u64::MAX; // out of memory
            }
            HEAP_BREAK.store(new, Ordering::Relaxed);
            old
        }
        0 => {
            // sys_exit(code)
            unsafe {
                EXIT_CODE = a1;
                EXITED = 1;
            }
            0
        }
        2 => 1, // sys_getpid() — eerste userspace-proces = pid 1
        20 => {
            // sys_open(path) -> fd (of -1). Pad uit userspace, zoek in de VFS.
            let path = user_cstr(a1, 256);
            vfs_open(&path)
        }
        22 => vfs_read(a1 as usize, a2, a3 as usize), // sys_read(fd, buf, len)
        21 => vfs_close(a1 as usize),                 // sys_close(fd)
        4 => {
            // sys_uname(buf, size) — schrijf de kernelversie in de user-buffer.
            let s: &[u8] = b"EuroKernel 0.1-alpha x86_64";
            let cap = (a2 as usize).saturating_sub(1);
            let n = s.len().min(cap);
            // Valideer buf voor n+1 bytes (data + NUL) vóór het schrijven.
            if !in_user_arena(a1, n + 1) {
                return EFAULT;
            }
            let _ = copy_to_user(a1, &s[..n]);
            let _ = write_user(a1 + n as u64, 0u8); // NUL-terminator
            n as u64
        }
        1 => {
            // sys_write(ptr) — NUL-getermineerde string uit userspace (arena-veilig).
            let bytes = user_cstr(a1, 4096);
            let len = bytes.len();
            if let Ok(text) = core::str::from_utf8(&bytes) {
                OUTPUT.lock().push_str(text);
                serial_print!("[ring3→sys_write] {text}\n");
            }
            len as u64
        }
        _ => u64::MAX, // ENOSYS
    }
}

/// Linux x86-64 syscall-ABI → onze handlers. Linux-semantiek (bv. write/read
/// nemen (fd, buf, count); exit-nummer is 60). Minimale set voor eerste binaries.
/// De capability die een LINUX-syscall vereist (0 = altijd toegestaan). Zo geldt
/// least-privilege ook voor de Linux-ABI: een musl-proces zonder CAP_FILE kan
/// geen bestanden openen, precies zoals onze native programma's.
fn linux_required_cap(num: u64, a1: u64) -> u64 {
    // I/O op een socket-fd (read/write/close) valt onder CAP_NET — niet onder
    // CAP_FILE/CAP_CONSOLE. Zo heeft een netwerkprogramma genoeg aan CAP_NET.
    if crate::net::is_sock_fd(a1) && matches!(num, 0 | 1 | 3) {
        return CAP_NET;
    }
    match num {
        1 | 16 | 20 => CAP_CONSOLE,            // write/ioctl/writev (tty)
        0 | 2 | 3 | 5 | 8 | 19 | 89 | 217 | 257 | 262 | 267 => CAP_FILE, // read/open/close/(f)stat/lseek/readv/readlink/getdents64/openat
        41 | 42 | 44 | 45 => CAP_NET,           // socket/connect/sendto/recvfrom
        39 => CAP_PROC_INFO,                    // getpid
        _ => 0, // geheugen-/procesbeheer (mmap, brk, arch_prctl, exit, …) vrij
    }
}

fn linux_dispatch(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64 {
    let _ = a4; // niet elke syscall gebruikt arg4/arg5 (r10/r8)
    // Capability-handhaving ook in de Linux-ABI: weiger zonder het juiste recht.
    let need = linux_required_cap(num, a1);
    if need != 0 && !has_cap(need) {
        crate::serial_println!("[cap] Linux-syscall {num} GEWEIGERD — ontbrekende capability");
        return (-1i64) as u64; // -EPERM
    }
    match num {
        1 => {
            // write(fd, buf, count) — count bytes (NIET NUL-getermineerd).
            if a1 == 1 || a1 == 2 {
                let bytes = match copy_from_user(a2, a3 as usize) {
                    Some(v) => v,
                    None => return EFAULT,
                };
                if let Some(fi) = *STDOUT_REDIRECT.lock() {
                    redirect_append(fi, &bytes); // shell-redirectie: stdout -> bestand
                } else if let Ok(t) = core::str::from_utf8(&bytes) {
                    OUTPUT.lock().push_str(t);
                    serial_print!("[linux-abi] {t}");
                }
                a3
            } else if crate::net::is_sock_fd(a1) {
                // write() naar een socket = send().
                let bytes = match copy_from_user(a2, a3 as usize) {
                    Some(v) => v,
                    None => return EFAULT,
                };
                crate::net::sock_send(a1, &bytes)
            } else {
                // Schrijven naar een geopend VFS-bestand (fd >= 3).
                vfs_write(a1 as usize, a2, a3 as usize)
            }
        }
        39 => 1,  // getpid()
        60 | 231 => {
            // exit(code) / exit_group(code)
            unsafe {
                EXIT_CODE = a1;
                EXITED = 1;
            }
            0
        }
        12 => {
            // brk(addr) — Linux-semantiek: zet break, geef NIEUWE break terug.
            let cur = HEAP_BREAK.load(Ordering::Relaxed);
            if a1 == 0 || a1 > HEAP_END.load(Ordering::Relaxed) {
                return cur;
            }
            HEAP_BREAK.store(a1, Ordering::Relaxed);
            a1
        }
        9 => {
            // mmap(addr, len, prot, flags, fd, off) — alleen anonieme allocaties:
            // bump uit het heap-venster, page-uitgelijnd. Genoeg voor musl TLS/malloc.
            let len = (a2 + 0xFFF) & !0xFFF;
            let base = (HEAP_BREAK.load(Ordering::Relaxed) + 0xFFF) & !0xFFF;
            if base + len > HEAP_END.load(Ordering::Relaxed) {
                return (-12i64) as u64; // -ENOMEM
            }
            HEAP_BREAK.store(base + len, Ordering::Relaxed);
            crate::serial_println!("[linux-abi] mmap({len} bytes) -> {base:#x}");
            base
        }
        11 => 0, // munmap — bump-allocator geeft niet terug, maar slaagt stil
        158 => {
            // arch_prctl(code, addr): ARCH_SET_FS=0x1002 zet FS_BASE (musl TLS).
            match a1 {
                0x1002 => {
                    unsafe { Msr::new(0xC000_0100).write(a2) }; // IA32_FS_BASE
                    crate::serial_println!("[linux-abi] arch_prctl SET_FS = {a2:#x}");
                    0
                }
                0x1001 => {
                    unsafe { Msr::new(0xC000_0101).write(a2) }; // IA32_GS_BASE
                    0
                }
                _ => (-22i64) as u64, // -EINVAL
            }
        }
        20 => {
            // writev(fd, iov, iovcnt): array van {base,len}; tel geschreven bytes.
            // fd 1/2 -> console; fd >= 3 -> schrijf naar het VFS-bestand (musl-stdio).
            let to_file = a1 != 1 && a1 != 2;
            if a3 > 1024 {
                return (-22i64) as u64; // -EINVAL: begrens iovcnt
            }
            let mut written = 0u64;
            for i in 0..a3 {
                let iov_base = a2 + (i * 16);
                let base: u64 = match read_user(iov_base) {
                    Some(b) => b,
                    None => return EFAULT,
                };
                let len = match read_user::<u64>(iov_base + 8) {
                    Some(l) => l as usize,
                    None => return EFAULT,
                };
                if len == 0 {
                    continue;
                }
                if to_file {
                    let n = vfs_write(a1 as usize, base, len); // vfs_write valideert base
                    if n == u64::MAX {
                        return if written > 0 { written } else { (-9i64) as u64 };
                    }
                    written += n;
                } else {
                    let bytes = match copy_from_user(base, len) {
                        Some(v) => v,
                        None => return EFAULT,
                    };
                    if let Some(fi) = *STDOUT_REDIRECT.lock() {
                        redirect_append(fi, &bytes); // shell-redirectie: stdout -> bestand
                    } else if let Ok(t) = core::str::from_utf8(&bytes) {
                        OUTPUT.lock().push_str(t);
                        serial_print!("[linux-abi] {t}");
                    }
                    written += len as u64;
                }
            }
            written
        }
        0 => {
            // read(fd, buf, count): fd 0 = standaardinvoer (pipe), socket, of VFS.
            if a1 == 0 {
                stdin_read(a2, a3 as usize)
            } else if crate::net::is_sock_fd(a1) {
                let data = crate::net::sock_recv(a1, a3 as usize);
                if !copy_to_user(a2, &data) {
                    return EFAULT;
                }
                data.len() as u64
            } else {
                vfs_read(a1 as usize, a2, a3 as usize)
            }
        }
        19 => {
            // readv(fd, iov, iovcnt): lees in elke iovec-buffer; tel bytes (musl-stdio).
            let fd = a1 as usize;
            if a3 > 1024 {
                return (-22i64) as u64; // -EINVAL: begrens iovcnt
            }
            let mut total = 0u64;
            for i in 0..a3 {
                let iov_base = a2 + (i * 16);
                let base: u64 = match read_user(iov_base) {
                    Some(b) => b,
                    None => return EFAULT,
                };
                let len = match read_user::<u64>(iov_base + 8) {
                    Some(l) => l as usize,
                    None => return EFAULT,
                };
                if len == 0 {
                    continue;
                }
                let n = if fd == 0 { stdin_read(base, len) } else { vfs_read(fd, base, len) };
                if n == u64::MAX {
                    return if total > 0 { total } else { (-9i64) as u64 };
                }
                total += n;
                if (n as usize) < len {
                    break; // korte read = EOF/eind van bestand
                }
            }
            total
        }
        3 => {
            // close(fd): socket of VFS-bestand.
            if crate::net::is_sock_fd(a1) {
                crate::net::sock_close(a1)
            } else {
                vfs_close(a1 as usize)
            }
        }
        41 => {
            // socket(domain, type, protocol): AF_INET (2) + SOCK_STREAM (1, TCP)
            // of SOCK_DGRAM (2, UDP).
            let typ = a2 & 0xff; // negeer SOCK_CLOEXEC/NONBLOCK-vlaggen
            match (a1, typ) {
                (2, 1) => crate::net::sock_open(false), // TCP
                (2, 2) => crate::net::sock_open(true),  // UDP
                _ => (-1i64) as u64,
            }
        }
        42 => {
            // connect(fd, *sockaddr_in, addrlen): sin_port BE @2, sin_addr @4.
            if a3 < 8 {
                return (-1i64) as u64;
            }
            // Lees de 8-byte sockaddr arena-veilig (port @2..4, addr @4..8).
            let sa = match copy_from_user(a2, 8) {
                Some(v) => v,
                None => return EFAULT,
            };
            let port = ((sa[2] as u16) << 8) | sa[3] as u16;
            crate::net::sock_connect(a1, euronet::ipv4::Ipv4Addr([sa[4], sa[5], sa[6], sa[7]]), port)
        }
        44 => {
            // sendto(fd, buf, len, flags, dest, destlen): verbonden TCP → negeer dest.
            let bytes = match copy_from_user(a2, a3 as usize) {
                Some(v) => v,
                None => return EFAULT,
            };
            crate::net::sock_send(a1, &bytes)
        }
        45 => {
            // recvfrom(fd, buf, len, flags, src, srclen).
            let data = crate::net::sock_recv(a1, a3 as usize);
            if !copy_to_user(a2, &data) {
                return EFAULT;
            }
            data.len() as u64
        }
        8 => vfs_lseek(a1 as usize, a2 as i64, a3),  // lseek(fd, offset, whence)
        257 => {
            // openat(dirfd, path, flags, mode): negeer dirfd (AT_FDCWD). flags in a3.
            // O_CREAT=0x40 maakt aan; O_TRUNC=0x200 kapt af; O_APPEND=0x400 -> aan 't eind.
            let path = user_cstr(a2, 256);
            let flags = a3;
            // Een map openen (geen O_CREAT) -> dir-fd voor getdents64.
            if flags & 0x40 == 0 && is_vfs_dir(&path) {
                return diropen(&path);
            }
            let fd = if flags & 0x40 != 0 {
                vfs_open_create(&path, flags & 0x200 != 0)
            } else {
                vfs_open(&path)
            };
            if fd != u64::MAX && flags & 0x400 != 0 {
                // O_APPEND: zet de schrijfpositie op het eind van het bestand.
                if let Some(sz) = vfs_size(fd as usize) {
                    let mut fds = OPEN_FDS.lock();
                    if let Some((fi, _)) = fds[fd as usize] {
                        fds[fd as usize] = Some((fi, sz));
                    }
                }
            }
            fd
        }
        2 => {
            // open(path, flags, mode) — oudere libc-variant (flags in a2).
            let path = user_cstr(a1, 256);
            if a2 & 0x40 == 0 && is_vfs_dir(&path) {
                return diropen(&path);
            }
            if a2 & 0x40 != 0 {
                vfs_open_create(&path, a2 & 0x200 != 0)
            } else {
                vfs_open(&path)
            }
        }
        5 | 262 => {
            // fstat(fd, statbuf) / newfstatat(dirfd, path, statbuf, flags):
            // vul een Linux struct stat (144 B) zodat musl het als regulier
            // bestand met de juiste grootte ziet (anders weigert stdio te bufferen).
            // fstat op een open dir-fd -> meld een MAP (S_IFDIR), niet -EBADF.
            if num == 5 && (a1 as usize) < MAX_FD && OPEN_DIRS.lock()[a1 as usize].is_some() {
                if !in_user_arena(a2, 144) {
                    return EFAULT;
                }
                unsafe {
                    core::ptr::write_bytes(a2 as *mut u8, 0, 144);
                    (a2 as *mut u32).add(6).write(0o040755); // st_mode: S_IFDIR|0755
                    ((a2 + 56) as *mut u64).write(4096); // st_blksize
                }
                return 0;
            }
            let (fd_ok, statbuf) = if num == 5 {
                // fd 0 = standaardinvoer (pipe): rapporteer de buffergrootte.
                let sz = if a1 == 0 { Some(stdin_len()) } else { vfs_size(a1 as usize) };
                (sz, a2)
            } else {
                // newfstatat: pad in a2, statbuf in a3.
                let path = user_cstr(a2, 256);
                ensure_proc(&path); // /proc op aanvraag synthetiseren
                let files = FILES.lock();
                let sz = files
                    .iter()
                    .find(|(p, _)| p.as_bytes() == path.as_slice())
                    .map(|(_, d)| d.len());
                (sz, a3)
            };
            let size = match fd_ok {
                Some(s) => s,
                None => return (-9i64) as u64, // -EBADF / -ENOENT
            };
            if !in_user_arena(statbuf, 144) {
                return EFAULT;
            }
            // SAFETY: statbuf-regio (144 B) arena-gevalideerd; identity-mapped.
            unsafe {
                core::ptr::write_bytes(statbuf as *mut u8, 0, 144);
                (statbuf as *mut u32).add(6).write(0o100644); // st_mode (offset 24): S_IFREG|0644
                ((statbuf + 48) as *mut u64).write(size as u64); // st_size (offset 48)
                ((statbuf + 56) as *mut u64).write(4096); // st_blksize (offset 56)
            }
            0
        }
        89 | 267 => {
            // readlink(path, buf, sz) / readlinkat(dirfd, path, buf, sz): de enige
            // "symlinks" zijn de /proc/self-pseudolinks. /proc/self/exe -> het pad van
            // het lopende programma (Python/Go/Node zoeken zo hun eigen binary).
            let (pathptr, bufptr, sz) =
                if num == 89 { (a1, a2, a3 as usize) } else { (a2, a3, a4 as usize) };
            let path = user_cstr(pathptr, 256);
            let target: Option<String> = match path.as_slice() {
                b"/proc/self/exe" => Some(CURRENT_APP.lock().clone()),
                b"/proc/self/cwd" | b"/proc/self/root" => Some(String::from("/")),
                _ => None,
            };
            match target {
                Some(t) if bufptr != 0 => {
                    let n = t.len().min(sz);
                    if !copy_to_user(bufptr, &t.as_bytes()[..n]) {
                        return EFAULT;
                    }
                    n as u64
                }
                Some(_) => 0,
                None => (-22i64) as u64, // -EINVAL: geen symlink
            }
        }
        217 => vfs_getdents64(a1 as usize, a2, a3 as usize), // getdents64(fd, dirp, count)
        16 => 0,  // ioctl — pretend succes (isatty/TCGETS): stdout is een tty
        10 => 0,  // mprotect — sta toe (musl maakt zijn RELRO read-only); no-op
        13 => 0,  // rt_sigaction — geen signalen; doe alsof het lukt
        14 => 0,  // rt_sigprocmask
        218 => 1, // set_tid_address -> tid
        273 => 0, // set_robust_list
        202 => 0, // futex — geen contention in single-thread; succes
        228 => {
            // clock_gettime(clk, *timespec): CLOCK_REALTIME(0)/CLOCK_TAI(11) geven de
            // ECHTE wandklok (RTC-epoch); CLOCK_MONOTONIC(1)/BOOTTIME(7) de uptime.
            if a2 != 0 {
                let (sec, nsec) = if a1 == 0 || a1 == 11 {
                    (crate::rtc::epoch(), 0)
                } else {
                    let ticks = crate::interrupts::ticks();
                    (ticks / 100, (ticks % 100) * 10_000_000) // 100 Hz PIT
                };
                if !write_user(a2, sec) || !write_user(a2 + 8, nsec) {
                    return EFAULT;
                }
            }
            0
        }
        96 => {
            // gettimeofday(*timeval, tz): {tv_sec, tv_usec} uit de echte RTC-wandklok.
            if a1 != 0 {
                if !write_user(a1, crate::rtc::epoch()) || !write_user(a1 + 8, 0u64) {
                    return EFAULT;
                }
            }
            0
        }
        63 => {
            // uname(*utsname): 6 velden van 65 bytes. We spiegelen een Linux-kernel
            // (sysname "Linux", machine "x86_64") zodat ongewijzigde Linux-binaries
            // die de kernelversie inspecteren tevreden zijn — release noemt EuroOS.
            if a1 != 0 {
                let fields: [&[u8]; 6] =
                    [b"Linux", b"euroos", b"6.6.0-euroos", b"#1 EuroOS SMP", b"x86_64", b""];
                if !in_user_arena(a1, 6 * 65) {
                    return EFAULT;
                }
                unsafe {
                    core::ptr::write_bytes(a1 as *mut u8, 0, 6 * 65);
                    for (i, f) in fields.iter().enumerate() {
                        let dst = (a1 as *mut u8).add(i * 65);
                        let n = f.len().min(64);
                        core::ptr::copy_nonoverlapping(f.as_ptr(), dst, n);
                    }
                }
            }
            0
        }
        102 | 107 => crate::auth::session_uid() as u64, // getuid/geteuid -> sessie-uid
        104 | 108 => crate::auth::session_gid() as u64, // getgid/getegid -> sessie-gid
        24 => 0,                    // sched_yield — single-thread voorgrond: no-op
        72 => 0,                    // fcntl — F_GETFL/F_SETFL/F_SETFD: doe alsof het lukt
        79 => {
            // getcwd(buf, size): EuroOS-voorgrondproces draait in "/".
            if a1 != 0 && a2 >= 2 {
                if !copy_to_user(a1, b"/\0") {
                    return EFAULT;
                }
                2
            } else {
                (-34i64) as u64 // -ERANGE
            }
        }
        97 | 302 => 0, // getrlimit / prlimit64 — onbeperkt; succes
        334 => (-38i64) as u64, // rseq — niet ondersteund; glibc valt netjes terug
        21 | 269 => {
            // access(path, mode) / faccessat(dirfd, path, mode): 0 als 't bestaat.
            let pathptr = if num == 21 { a1 } else { a2 };
            let path = user_cstr(pathptr, 256);
            ensure_proc(&path); // /proc op aanvraag synthetiseren
            let exists = FILES.lock().iter().any(|(p, _)| p.as_bytes() == path.as_slice());
            if exists { 0 } else { (-2i64) as u64 } // -ENOENT
        }
        99 => {
            // sysinfo(*info): vul uptime + ram zodat tools als `uptime`/`free` werken.
            if a1 != 0 {
                let up = crate::interrupts::ticks() / 100;
                if !in_user_arena(a1, 112) {
                    return EFAULT;
                }
                unsafe {
                    core::ptr::write_bytes(a1 as *mut u8, 0, 112);
                    (a1 as *mut i64).write(up as i64); // uptime (seconden)
                    ((a1 + 24) as *mut u64).write(256 * 1024 * 1024); // totalram
                    ((a1 + 32) as *mut u64).write(128 * 1024 * 1024); // freeram
                    ((a1 + 104) as *mut u32).write(1); // mem_unit
                }
            }
            0
        }
        332 => {
            // statx(dirfd, path, flags, mask, *statxbuf): moderne glibc-stat. statxbuf
            // is arg5 (a5). Vul stx_mask/blksize/nlink/mode/size voor een regulier
            // bestand zodat glibc-stdio het bestand correct ziet.
            let path = user_cstr(a2, 256);
            ensure_proc(&path); // /proc op aanvraag synthetiseren
            let sz = FILES
                .lock()
                .iter()
                .find(|(p, _)| p.as_bytes() == path.as_slice())
                .map(|(_, d)| d.len());
            match sz {
                Some(size) if a5 != 0 => {
                    if !in_user_arena(a5, 256) {
                        return EFAULT;
                    }
                    unsafe {
                        core::ptr::write_bytes(a5 as *mut u8, 0, 256);
                        (a5 as *mut u32).write(0x7ff); // stx_mask = STATX_BASIC_STATS
                        ((a5 + 0x04) as *mut u32).write(4096); // stx_blksize
                        ((a5 + 0x10) as *mut u32).write(1); // stx_nlink
                        ((a5 + 0x1c) as *mut u16).write(0o100644); // stx_mode: S_IFREG|0644
                        ((a5 + 0x28) as *mut u64).write(size as u64); // stx_size
                    }
                    0
                }
                Some(_) => 0,
                None => (-2i64) as u64, // -ENOENT
            }
        }
        318 => {
            // getrandom(buf, len, flags): pseudo-willekeur (deterministisch maar
            // gevuld) — genoeg voor musl-init; geen crypto-bron.
            if !in_user_arena(a1, a2 as usize) {
                return EFAULT;
            }
            let buf: alloc::vec::Vec<u8> =
                (0..a2).map(|i| (0x9Eu64.wrapping_mul(i + 1)) as u8).collect();
            let _ = copy_to_user(a1, &buf);
            a2
        }
        35 => 0,  // nanosleep
        234 => 0, // tgkill
        _ => {
            crate::serial_println!("[linux-abi] ENOSYS Linux-syscall {num}");
            (-38i64) as u64 // -ENOSYS (Linux-conventie: negatieve errno)
        }
    }
}

/// Status van de hardware-bescherming, voor `shell`/diagnostiek.
static SMEP_ON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static SMAP_ON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static NX_ON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static SMAP_LOGGED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Zet SMEP + SMAP **aan** (mits de CPU ze ondersteunt — anders zou `Cr4::write`
/// een #GP geven). SMEP belet ring 0 ooit een user-pagina (U=1) uit te voeren;
/// SMAP belet ring 0 user-pagina's te lezen/schrijven, behalve binnen een
/// expliciet, kortstondig AC-venster (zie de syscall-entry). Dit vervangt de
/// vroegere globale *uitschakeling* tijdens proces-setup. Idempotent.
pub fn enable_smep_smap() {
    // CPUID.(EAX=7,ECX=0):EBX  bit 7 = SMEP, bit 20 = SMAP.
    let leaf7 = unsafe { core::arch::x86_64::__cpuid_count(7, 0) };
    let smep = leaf7.ebx & (1 << 7) != 0;
    let smap = leaf7.ebx & (1 << 20) != 0;
    let mut f = Cr4::read();
    if smep {
        f.insert(Cr4Flags::SUPERVISOR_MODE_EXECUTION_PROTECTION);
    }
    if smap {
        f.insert(Cr4Flags::SUPERVISOR_MODE_ACCESS_PREVENTION);
    }
    if smep || smap {
        unsafe { Cr4::write(f) };
    }
    SMEP_ON.store(smep, Ordering::Relaxed);
    SMAP_ON.store(smap, Ordering::Relaxed);
    if !SMAP_LOGGED.swap(true, Ordering::Relaxed) {
        crate::serial_println!(
            "[sec] SMEP {} · SMAP {} (CR4; ring 0 kan user-pagina's niet meer {}, behalve in een kort syscall-venster)",
            if smep { "AAN" } else { "n/b" },
            if smap { "AAN" } else { "n/b" },
            if smep && smap { "uitvoeren/aanraken" } else if smap { "aanraken" } else { "uitvoeren" },
        );
    }
}

/// Of SMAP nu actief afdwingt (voor de `hardening`-shell-regel).
pub fn smap_active() -> bool {
    SMAP_ON.load(Ordering::Relaxed)
}

/// Of SMEP nu actief afdwingt.
pub fn smep_active() -> bool {
    SMEP_ON.load(Ordering::Relaxed)
}

/// Of NX (No-Execute / W^X) nu actief afdwingt.
pub fn nx_active() -> bool {
    NX_ON.load(Ordering::Relaxed)
}

fn init_syscall_msrs() {
    enable_smep_smap(); // hardware-bescherming AAN vóór elke ring-3-excursie (idempotent)
    let sel = crate::gdt::selectors();
    let kcode = sel.code.0 as u64;
    let kdata = sel.data.0 as u64;
    // NX (No-Execute) inschakelen mits de CPU het ondersteunt — CPUID.80000001h:EDX
    // bit 20. Zonder NXE heeft de NX-bit (bit 63) in een PTE geen effect; mét NXE
    // dwingt hij W^X af (data/stack/heap niet uitvoerbaar). Idempotent.
    let nx = {
        let r = unsafe { core::arch::x86_64::__cpuid(0x8000_0001) };
        r.edx & (1 << 20) != 0
    };
    NX_ON.store(nx, Ordering::Relaxed);
    unsafe {
        let mut efer = Msr::new(0xC000_0080);
        let v = efer.read();
        let nxe = if nx { 1 << 11 } else { 0 }; // EFER.NXE
        efer.write(v | 1 | nxe); // EFER.SCE (+ NXE)
        Msr::new(0xC000_0081).write((kdata << 48) | (kcode << 32)); // STAR
        Msr::new(0xC000_0082).write(syscall_entry as usize as u64); // LSTAR
        Msr::new(0xC000_0084).write(0x200); // FMASK: IF wissen bij entry
        // Kernel-stack voor de syscall-handler.
        let top = (core::ptr::addr_of!(KSTACK) as u64 + KSTACK_SIZE as u64) & !0xF;
        KERNEL_RSP = top;
    }
}

/// Laad `program` in een User-frame, draai het in ring 3, en geef
/// `(exit_code, uitvoer)` terug zodra het `sys_exit` doet.
pub fn run(falloc: &mut FrameAllocator, program: &[u8], caps: u64, linux_abi: bool) -> (u64, String) {
    run_args(falloc, program, &[b"prog"], caps, linux_abi)
}

/// Zoals [`run`], maar met een expliciete programma-naam die als `argv[0]` op de
/// SysV-stack komt te staan.
pub fn run_named(
    falloc: &mut FrameAllocator,
    program: &[u8],
    name: &[u8],
    caps: u64,
    linux_abi: bool,
) -> (u64, String) {
    run_args(falloc, program, &[name], caps, linux_abi)
}

/// Zoals [`run_named`], maar met een volledige `argv` (argv[0] = pad, argv[1..] =
/// argumenten). De kernel zet deze op de SysV-stack; het programma leest ze via
/// het standaard `main(argc, argv)`-contract.
pub fn run_args(
    falloc: &mut FrameAllocator,
    program: &[u8],
    argv: &[&[u8]],
    caps: u64,
    linux_abi: bool,
) -> (u64, String) {
    init_syscall_msrs();
    CURRENT_CAPS.store(caps, Ordering::Relaxed); // de rechten van DIT proces
    LINUX_ABI.store(linux_abi, Ordering::Relaxed); // Linux- of native-ABI
    // App-identiteit (argv[0]) vastleggen voor EuroGuard (Track 7).
    *CURRENT_APP.lock() = argv
        .first()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .unwrap_or_default();
    unsafe {
        EXITED = 0;
        EXIT_CODE = 0;
    }
    OUTPUT.lock().clear();
    reset_fd_table(); // verse per-proces fd-tabel

    // GEÏSOLEERDE adresruimte per voorgrond-exec: alle user-frames in één
    // 2 MiB-arena, alleen die krijgt de USER-bit. Zo kan een voorgrondprogramma
    // (ook ongesigneerde/buggy code) geen kernelgeheugen meer lezen/schrijven.
    const MIB2: u64 = 1 << 21;
    // Exact 2 MiB, 2 MiB-uitgelijnd (geen 4 MiB-over-allocatie); we geven hieronder
    // precies deze 512 frames weer vrij na de synchrone exec.
    let arena = falloc.allocate_aligned(512, 512).expect("fg-arena");
    let arena_raw = arena;
    let code = arena;
    let heap = arena + 0x80000; // +512 KiB
    let stack_top = arena + MIB2; // user-stack groeit omlaag vanaf de arena-top
    HEAP_BREAK.store(heap, Ordering::Relaxed);
    ARENA_BASE.store(arena, Ordering::Relaxed); // audit C1
    HEAP_END.store(arena + 0x180000, Ordering::Relaxed); // ~1 MiB heap

    // Laad het programma in de arena (CR3 nog boot: arena is daar schrijfbaar).
    let pages = program_span_pages(program);
    let info = load_program(program, code, pages);
    let rsp = unsafe { setup_user_stack(stack_top, argv, &info) };
    let entry = info.entry;

    let sel = crate::gdt::selectors();
    let user_cs = (sel.user_code.0 | 3) as u64;
    let user_ss = (sel.user_data.0 | 3) as u64;

    // Bouw de eigen W^X-PML4 en wissel ernaartoe vlak vóór de ring-3-excursie.
    let pml4 = crate::paging::build_address_space(falloc, arena, &info.exec_pages, &info.writ_pages);
    let boot = crate::sched::boot_pml4();
    // Kernel-stack voor een eventuele fault vanuit dit voorgrondproces.
    unsafe { crate::gdt::set_rsp0(KERNEL_RSP) };
    FG_ACTIVE.store(true, Ordering::Relaxed);

    // SAFETY: paging/MSR/GDT zijn opgezet. We komen terug via sys_exit (de "9:"-
    // epiloog) of, bij een page fault, via de force_kernel_return-trampoline —
    // beide landen ná `enter_ring3`, dus de boot-CR3-herstel hieronder draait.
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) pml4, options(nostack, preserves_flags));
        enter_ring3(user_cs, user_ss, entry, rsp);
        core::arch::asm!("mov cr3, {}", in(reg) boot, options(nostack, preserves_flags));
    }
    FG_ACTIVE.store(false, Ordering::Relaxed);

    // Ruim de adresruimte op (frames vrij): geen lek per voorgrond-exec. Precies de
    // 512 uitgelijnd gealloceerde arena-frames.
    for f in 0..512u64 {
        let _ = falloc.free(arena_raw + f * 4096);
    }
    crate::paging::free_address_space(falloc, pml4);

    let out = OUTPUT.lock().clone();
    unsafe { (core::ptr::read(core::ptr::addr_of!(EXIT_CODE)), out) }
}
