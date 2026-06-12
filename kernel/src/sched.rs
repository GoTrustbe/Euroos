//! Preemptieve round-robin scheduler met kernel-taken (Track 3.3).
//!
//! De timer-interrupt (IRQ0) springt naar `timer_switch` (assembly): die bewaart
//! ALLE registers van de huidige taak op diens stack, roept `schedule_tick` aan
//! (kiest de volgende taak), zet de stackpointer naar die taak en herstelt z'n
//! registers + `iretq`. Zo wordt willekeurige code preemptief onderbroken.
//!
//! Taak 0 = de hoofd-thread (shell). Taken 1..N zijn achtergrond-tellers; hun
//! oplopende counters bewijzen dat ze écht parallel (afgewisseld) draaien.

use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;
use x86_64::instructions::segmentation::{Segment, CS, SS};
use x86_64::registers::model_specific::Msr;

const IA32_FS_BASE: u32 = 0xC000_0100;

const MAX_TASKS: usize = 48;
const STACK_SIZE: usize = 16 * 1024;
const CONTEXT_WORDS: usize = 20; // 15 GP-registers + 5 (rip,cs,rflags,rsp,ss)

/// Per-taak teller (index 1..3 voor de kernel-achtergrondtaken).
pub static TASK_COUNTERS: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];

/// Taakstatus (S2 scheduler-volwassenheid). Vervangt de losse `dead`/`blocked`-
/// vlaggen door een volledige toestandsmachine — de basis voor blocking I/O,
/// nanosleep en (S3) fork/wait/zombie-reaping.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Runbaar (klaar om CPU te krijgen).
    Ready,
    /// Slaapt tot `ticks() >= wake`. Door de scheduler automatisch op Ready gezet.
    Sleeping(u64),
    /// Geblokkeerd op een wachtkanaal (een token/adres). Gewekt door [`wake`].
    Blocked(u64),
    /// Netjes geëindigd met exitcode; wacht op reaping door de ouder (waitpid).
    Zombie(i64),
    /// Definitief weg (gereaped, of hard beëindigd door een fault). Voorgoed
    /// overgeslagen.
    Dead,
}

struct Task {
    rsp: u64,
    /// Kernel-stack voor ring3->ring0 interrupts (0 = kernel-taak, geen rsp0).
    kstack: u64,
    /// FS_BASE (IA32_FS_BASE MSR) van deze taak — de musl-TLS-pointer. Per taak
    /// bewaard/hersteld bij elke context-switch, zodat preëmptieve musl-processen
    /// elk hun eigen thread-local opslag houden.
    fs_base: u64,
    /// Fysieke PML4 (CR3) van deze taak. 0 = de gedeelde boot-PML4 (kernel +
    /// kernel-taken). Een eigen waarde geeft een GEÏSOLEERDE adresruimte.
    cr3: u64,
    /// Volledige toestand (zie [`State`]).
    state: State,
    /// Nice-waarde (-20..19): lager = hogere prioriteit = vaker gescheduled.
    nice: i8,
    /// Virtuele looptijd (mini-CFS): de scheduler kiest steeds de runbare taak met
    /// de KLEINSTE vruntime. Een hogere nice laat 'm sneller oplopen → minder beurt.
    vruntime: u64,
    /// Procesidentiteit (S3: waitpid/SIGCHLD). 0 = niet als proces geregistreerd.
    pid: u32,
    /// Ouder-pid (S3: reaping + SIGCHLD-routing).
    ppid: u32,
    /// Onderkant (laagste adres) van de kernel-stack van deze taak (0 = geen wacht).
    /// Daar staat een CANARY; bij elke context-switch checkt de scheduler 'm — een
    /// stack-overflow (de stack groeit omlaag tot voorbij dit punt) overschrijft de
    /// canary en wordt zo gedetecteerd i.p.v. stil naburig geheugen te corrumperen (S6).
    stack_bottom: u64,
}

/// Stack-guard-canary (S6 memory hardening). Onwaarschijnlijke waarde onderaan elke
/// kernel-stack; als die verandert is de stack overgelopen.
const STACK_CANARY: u64 = 0x5350_524F_5547_4421; // "SPROUGD!" — herkenbaar in dumps

/// vruntime-stap per beurt, gewogen op nice (-20..19 -> 64..2560). Gelijke nice =
/// gelijke stap = eerlijke round-robin (gedragsbehoudend t.o.v. de oude scheduler).
fn vstep(nice: i8) -> u64 {
    ((nice as i64) + 21) as u64 * 64
}

/// Blokkeer de HUIDIGE taak op het generieke futex-wachtkanaal (FUTEX_WAIT).
pub fn block_current() {
    block_on(0);
}

/// Blokkeer de HUIDIGE taak op wachtkanaal `chan` (een adres/token). De scheduler
/// slaat 'm over tot [`wake`]`(chan, ..)`. Basis voor futex, pipes, waitpid.
pub fn block_on(chan: u64) {
    let mut s = SCHED.lock();
    let cur = s.current;
    s.tasks[cur].state = State::Blocked(chan);
}

/// Deblokkeer taak `idx` (FUTEX_WAKE op een specifieke taak).
pub fn unblock(idx: usize) {
    let mut s = SCHED.lock();
    if idx < s.count && matches!(s.tasks[idx].state, State::Blocked(_)) {
        s.tasks[idx].state = State::Ready;
    }
}

/// Wek tot `n` taken die op wachtkanaal `chan` geblokkeerd zijn. Geeft het aantal
/// gewekte taken terug.
pub fn wake(chan: u64, n: usize) -> usize {
    let mut s = SCHED.lock();
    let count = s.count;
    let mut woken = 0;
    for i in 0..count {
        if woken >= n {
            break;
        }
        if s.tasks[i].state == State::Blocked(chan) {
            s.tasks[i].state = State::Ready;
            woken += 1;
        }
    }
    woken
}

/// Laat de HUIDIGE taak `n` ticks slapen (100 Hz → ~10 ms/tick). Echte timed wait:
/// de scheduler slaat 'm over tot de wektijd. (De aanroeper geeft daarna de CPU af
/// bij de eerstvolgende timer-tick.)
pub fn sleep_ticks(n: u64) {
    let wake = crate::interrupts::ticks() + n;
    let mut s = SCHED.lock();
    let cur = s.current;
    s.tasks[cur].state = State::Sleeping(wake);
}

/// Markeer de HUIDIGE taak als ZOMBIE met exitcode (net afgesloten proces). Wacht
/// op reaping door [`reap`]/[`take_zombie_child`] (waitpid). Geeft de index terug.
pub fn exit_current(code: i64) -> usize {
    let mut s = SCHED.lock();
    let cur = s.current;
    s.tasks[cur].state = State::Zombie(code);
    cur
}

/// Reap zombie-taak `idx`: geef diens exitcode terug en zet hem op Dead.
pub fn reap(idx: usize) -> Option<i64> {
    let mut s = SCHED.lock();
    if idx < s.count {
        if let State::Zombie(code) = s.tasks[idx].state {
            s.tasks[idx].state = State::Dead;
            return Some(code);
        }
    }
    None
}

/// Vind een zombie-kind van ouder `ppid` en reap het: (pid, exitcode). Voor waitpid.
pub fn take_zombie_child(ppid: u32) -> Option<(u32, i64)> {
    let mut s = SCHED.lock();
    let count = s.count;
    for i in 0..count {
        if s.tasks[i].ppid == ppid {
            if let State::Zombie(code) = s.tasks[i].state {
                s.tasks[i].state = State::Dead;
                return Some((s.tasks[i].pid, code));
            }
        }
    }
    None
}

/// Zet pid/ppid van taak `idx` (door de proces-laag bij spawn/fork).
pub fn set_ident(idx: usize, pid: u32, ppid: u32) {
    let mut s = SCHED.lock();
    if idx < s.count {
        s.tasks[idx].pid = pid;
        s.tasks[idx].ppid = ppid;
    }
}

/// Zet de nice-waarde (prioriteit) van taak `idx` (-20..19).
pub fn set_nice(idx: usize, nice: i8) {
    let mut s = SCHED.lock();
    if idx < s.count {
        s.tasks[idx].nice = nice.clamp(-20, 19);
    }
}

/// Markeer de HUIDIGE taak als beëindigd (door de fault-handler). Geeft de index.
pub fn mark_current_dead() -> usize {
    let mut s = SCHED.lock();
    let cur = s.current;
    s.tasks[cur].state = State::Dead;
    cur
}

/// Markeer een specifieke taak als beëindigd (bv. `kill <pid>` vanuit de shell).
pub fn mark_dead(idx: usize) {
    let mut s = SCHED.lock();
    if idx < s.count {
        s.tasks[idx].state = State::Dead;
    }
}

/// De gedeelde boot-PML4 (kernel-adresruimte). Door main na `paging::init` gezet.
static BOOT_PML4: AtomicU64 = AtomicU64::new(0);

pub fn set_boot_pml4(p: u64) {
    BOOT_PML4.store(p, Ordering::Relaxed);
}

pub fn boot_pml4() -> u64 {
    BOOT_PML4.load(Ordering::Relaxed)
}

/// Geef taak `idx` een eigen adresruimte (CR3). Vanaf de volgende switch draait
/// die taak op zijn eigen page tables.
pub fn set_task_cr3(idx: usize, cr3: u64) {
    SCHED.lock().tasks[idx].cr3 = cr3;
}

struct Scheduler {
    tasks: [Task; MAX_TASKS],
    count: usize,
    current: usize,
}

static SCHED: Mutex<Scheduler> = Mutex::new(Scheduler {
    tasks: [const {
        Task { rsp: 0, kstack: 0, fs_base: 0, cr3: 0, state: State::Ready, nice: 0, vruntime: 0, pid: 0, ppid: 0, stack_bottom: 0 }
    }; MAX_TASKS],
    count: 1,
    current: 0,
});

// Stacks voor de achtergrondtaken (taak 0 gebruikt de bestaande kernel-stack).
static mut STACKS: [[u8; STACK_SIZE]; MAX_TASKS] = [[0; STACK_SIZE]; MAX_TASKS];

/// G1: per-taakslot een GUARDED kernel-stacktop (0 = niet gezet → val terug op de
/// BSS-`STACKS`). main.rs vult deze vóór `init()` uit de guarded-stack-pool, zodat
/// een kernel-taak-overflow op een niet-gemapte guard-pagina faultt (→ hardware-#PF,
/// de fault-handler beëindigt enkel die taak) i.p.v. stil de buur-stack te slopen.
static TASK_GUARDED_TOP: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];

/// Zet de guarded stacktop voor taakslot `idx` (aanroepen vóór [`init`]).
pub fn set_task_guarded_stack(idx: usize, top: u64) {
    if idx < MAX_TASKS {
        TASK_GUARDED_TOP[idx].store(top, Ordering::Release);
    }
}

global_asm!(
    ".global timer_switch",
    "timer_switch:",
    "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi", "push rbp",
    "push r8", "push r9", "push r10", "push r11", "push r12", "push r13", "push r14", "push r15",
    "mov rdi, rsp",   // arg1 = huidige stackpointer (volledige opgeslagen context)
    "and rsp, -16",   // 16-uitlijnen voor de Rust-aanroep
    "call schedule_tick",
    "mov rsp, rax",   // wissel naar de stack van de volgende taak
    "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10", "pop r9", "pop r8",
    "pop rbp", "pop rdi", "pop rsi", "pop rdx", "pop rcx", "pop rbx", "pop rax",
    "iretq",
);

extern "C" {
    fn timer_switch();
}

pub fn stub_addr() -> u64 {
    timer_switch as usize as u64
}

/// Aangeroepen door de assembly-stub: bewaar de huidige rsp, kies de volgende
/// taak (round-robin) en geef diens rsp terug.
///
/// LET OP: expliciet `sysv64` — de UEFI-target maakt van `extern "C"` de Win64-
/// ABI (argument in RCX), maar onze stub levert het argument in RDI (SysV).
#[no_mangle]
pub extern "sysv64" fn schedule_tick(rsp: u64) -> u64 {
    crate::interrupts::TICKS.fetch_add(1, Ordering::Relaxed);
    crate::interrupts::send_timer_eoi();
    let mut s = SCHED.lock();
    let cur = s.current;
    s.tasks[cur].rsp = rsp;
    // S6 stack-guard: is de canary van de net-gedraaide taak nog intact? Zo niet,
    // dan liep z'n kernel-stack over -> stop met een duidelijke diagnose i.p.v.
    // stilletjes naburig geheugen te laten corrumperen.
    let sb = s.tasks[cur].stack_bottom;
    if sb != 0 && unsafe { (sb as *const u64).read_volatile() } != STACK_CANARY {
        panic!("STACK-OVERFLOW: kernel-taak {cur} liep over z'n stack (guard-canary @ {sb:#x} overschreven)");
    }
    // Bewaar de FS_BASE (musl-TLS-pointer) van de afgaande taak en herstel die
    // van de inkomende — zo houdt elk preëmptief musl-proces z'n eigen TLS.
    let mut fs = Msr::new(IA32_FS_BASE);
    s.tasks[cur].fs_base = unsafe { fs.read() };
    // 1. Slapers wekken: Sleeping -> Ready zodra hun wektijd bereikt is.
    let now = crate::interrupts::TICKS.load(Ordering::Relaxed);
    for i in 0..s.count {
        if let State::Sleeping(w) = s.tasks[i].state {
            if now >= w {
                s.tasks[i].state = State::Ready;
            }
        }
    }
    // 2. De afgaande taak (als die nog runbaar is) z'n vruntime laten oplopen,
    //    gewogen op nice. Hogere nice -> grotere stap -> minder vaak gekozen.
    if s.tasks[cur].state == State::Ready {
        let step = vstep(s.tasks[cur].nice);
        s.tasks[cur].vruntime = s.tasks[cur].vruntime.wrapping_add(step);
    }
    // 3. Mini-CFS: kies de runbare taak met de KLEINSTE vruntime. Begin de scan bij
    //    cur+1 met strikte '<', zodat gelijke-vruntime-taken round-robin rouleren
    //    (gedragsbehoudend bij gelijke nice). Taak 0 is altijd Ready -> terugval.
    let mut best = cur;
    let mut bestv = u64::MAX;
    let mut found = false;
    for k in 1..=s.count {
        let i = (cur + k) % s.count;
        if s.tasks[i].state == State::Ready && s.tasks[i].vruntime < bestv {
            bestv = s.tasks[i].vruntime;
            best = i;
            found = true;
        }
    }
    if !found {
        best = if s.tasks[cur].state == State::Ready { cur } else { 0 };
    }
    s.current = best;
    let next = s.current;
    unsafe { fs.write(s.tasks[next].fs_base) };
    // Wissel van adresruimte (CR3) als de inkomende taak een eigen PML4 heeft.
    // Elke PML4 mapt de kernel identiek (supervisor), dus deze code + alle
    // kernel-stacks blijven geldig over de switch heen; alleen de USER-zichtbare
    // user-frames verschillen per proces -> geheugenisolatie.
    let boot = BOOT_PML4.load(Ordering::Relaxed);
    let cur_cr3 = if s.tasks[cur].cr3 != 0 { s.tasks[cur].cr3 } else { boot };
    let next_cr3 = if s.tasks[next].cr3 != 0 { s.tasks[next].cr3 } else { boot };
    if next_cr3 != 0 && next_cr3 != cur_cr3 {
        unsafe { core::arch::asm!("mov cr3, {}", in(reg) next_cr3, options(nostack, preserves_flags)) };
    }
    // Voor een ring-3 taak: zet TSS.rsp0 op ZIJN kernel-stack, zodat z'n
    // interrupt-frames niet botsen met die van andere ring-3 processen.
    let ks = s.tasks[next].kstack;
    if ks != 0 {
        crate::gdt::set_rsp0(ks);
    }
    s.tasks[next].rsp
}

/// Maak een verse taak-stack met een initiële context die via `iretq` naar
/// `entry` springt (met interrupts aan).
fn init_stack(index: usize, entry: extern "C" fn() -> !) -> u64 {
    let cs = CS::get_reg().0 as u64;
    let ss = SS::get_reg().0 as u64;
    // G1: gebruik de GUARDED stack als die voor dit slot gezet is (overflow → guard-
    // #PF), anders de BSS-`STACKS`. Voor de guarded stack is `base` (laagste adres)
    // de eerste stack-pagina; de niet-gemapte guard-pagina ligt daar direct ónder.
    let guarded = TASK_GUARDED_TOP[index].load(Ordering::Acquire);
    let (base, top) = if guarded != 0 {
        let top = guarded & !0xF;
        ((top - STACK_SIZE as u64) as *mut u8, top)
    } else {
        // SAFETY: elke STACKS[index] wordt door exact één taak gebruikt.
        let b = unsafe { core::ptr::addr_of_mut!(STACKS[index]) as *mut u8 };
        (b, (b as u64 + STACK_SIZE as u64) & !0xF)
    };
    // S6: schrijf de stack-guard-canary onderaan (laagste adres) van de stack.
    unsafe { (base as *mut u64).write(STACK_CANARY) };
    let ctx = top - (CONTEXT_WORDS as u64) * 8;
    let words = ctx as *mut u64;
    unsafe {
        for i in 0..15 {
            words.add(i).write(0); // r15..rax = 0
        }
        words.add(15).write(entry as usize as u64); // rip
        words.add(16).write(cs); // cs
        words.add(17).write(0x202); // rflags (IF=1)
        words.add(18).write(top); // rsp voor de taak
        words.add(19).write(ss); // ss
    }
    ctx
}

/// De stack-onderkant (canary-locatie / laagste adres) van taakslot `index` —
/// guarded base als gezet, anders de BSS-`STACKS`. Moet matchen met `init_stack`.
fn task_stack_bottom(index: usize) -> u64 {
    let guarded = TASK_GUARDED_TOP[index].load(Ordering::Acquire);
    if guarded != 0 {
        (guarded & !0xF) - STACK_SIZE as u64
    } else {
        unsafe { core::ptr::addr_of_mut!(STACKS[index]) as u64 }
    }
}

/// Start de achtergrondtaken en activeer round-robin scheduling.
pub fn init() {
    let mut s = SCHED.lock();
    s.tasks[1].rsp = init_stack(1, task_a);
    s.tasks[2].rsp = init_stack(2, task_b);
    s.tasks[3].rsp = init_stack(3, task_c);
    s.tasks[4].rsp = init_stack(4, task_sleeper);
    // G1-zelftest: een taak op een GUARDED stack die opzettelijk z'n stack laat
    // overlopen. De guard-pagina vangt het als hardware-#PF; de fault-handler
    // beëindigt ALLEEN deze taak en de kernel draait door (bewijs van recovery).
    s.tasks[5].rsp = init_stack(5, task_overflow);
    // S6: registreer de stack-onderkant (canary-locatie) voor de overflow-bewaking.
    for i in 1..=5 {
        s.tasks[i].stack_bottom = task_stack_bottom(i);
    }
    // S2 prioriteitsdemo: a/b/c doen GELIJKE werklast maar krijgen verschillende
    // nice — hun tellers laten zien dat de mini-CFS hoge prioriteit vaker plant.
    s.tasks[1].nice = -10; // hoge prioriteit  -> meeste beurten
    s.tasks[2].nice = 0; //   normaal
    s.tasks[3].nice = 10; //  lage prioriteit  -> minste beurten
    s.count = 6;
    s.current = 0;
}

/// Voeg een **ring-3** taak toe aan de round-robin. `kstack_top` is de kernel-
/// stack waar de CPU bij een interrupt vanuit ring 3 naartoe switcht (TSS.rsp0);
/// daar bouwen we de initiële ring-3 context zodat de eerste switch er via
/// `iretq` naartoe springt.
/// De index van de momenteel draaiende taak (door de syscall-laag gebruikt om
/// een gescheduelde achtergrondtaak van een synchrone voorgrond-exec te scheiden).
pub fn current() -> usize {
    SCHED.lock().current
}

/// Aantal scheduler-taken (voor het live systeempaneel).
pub fn task_count() -> usize {
    SCHED.lock().count
}

/// Kies een vrij taakslot (groeit de tabel). Geeft None als de tabel vol is.
/// NB: nog geen hergebruik van DODE slots — dat vereist dat de bijbehorende BgProc
/// al gereaped is (anders deelt een nieuw proces het slot met een zombie). Met
/// MAX_TASKS=48 is er ruim genoeg voor de huidige werklast; slot-recycling = later.
fn alloc_slot(s: &mut Scheduler) -> Option<usize> {
    if s.count < MAX_TASKS {
        let i = s.count;
        s.count += 1;
        return Some(i);
    }
    None
}

pub fn spawn_user(rip: u64, rsp: u64, cs: u64, ss: u64, kstack_top: u64) -> usize {
    let mut s = SCHED.lock();
    let idx = alloc_slot(&mut s).expect("scheduler-taaktabel vol");
    let ctx = kstack_top - (CONTEXT_WORDS as u64) * 8;
    let words = ctx as *mut u64;
    // SAFETY: kstack_top is een geldige, exclusieve kernel-stack voor deze taak.
    unsafe {
        for i in 0..15 {
            words.add(i).write(0);
        }
        words.add(15).write(rip);
        words.add(16).write(cs); // ring-3 code-selector (RPL 3)
        words.add(17).write(0x202); // rflags (IF=1)
        words.add(18).write(rsp); // user-stacktop
        words.add(19).write(ss); // ring-3 data-selector (RPL 3)
    }
    s.tasks[idx].rsp = ctx;
    s.tasks[idx].kstack = kstack_top; // eigen interrupt-stack voor deze taak
    s.tasks[idx].state = State::Ready;
    s.tasks[idx].vruntime = s.tasks[s.current].vruntime; // start eerlijk op gelijke hoogte
    s.tasks[idx].nice = 0;
    idx
}

/// Maak een THREAD-taak: hervat in ring 3 op `rip`/`rsp` met rax=0 (de child
/// ziet clone() 0 teruggeven), DEELT de adresruimte `cr3` met z'n proces, maar
/// heeft een EIGEN kernel-stack en FS_BASE (TLS). Voor de `clone`-syscall.
#[allow(clippy::too_many_arguments)]
pub fn spawn_thread(rip: u64, rsp: u64, cs: u64, ss: u64, kstack_top: u64, cr3: u64, fs_base: u64, saved_regs: u64) -> usize {
    let mut s = SCHED.lock();
    let idx = alloc_slot(&mut s).expect("scheduler-taaktabel vol");
    let ctx = kstack_top - (CONTEXT_WORDS as u64) * 8;
    let words = ctx as *mut u64;
    // SAFETY: kstack_top is een exclusieve kernel-stack voor deze thread.
    // De child ERFT de registers van de ouder (alleen rax=0). musl's __clone
    // verwacht dit: in de child doet het `call *%r9` met r9 = thread-functie.
    // Het opgeslagen registerblok (syscall_entry) ligt als:
    //   [0]=r15 [1]=r14 [2]=r13 [3]=r12 [4]=r10 [5]=r9 [6]=r8 [7]=rdx [8]=rsi
    //   [9]=rdi [10]=rbp [11]=rbx [12]=r11 [13]=rcx
    // De context (timer_switch-pop-volgorde) is r15,r14,r13,r12,r11,r10,r9,r8,
    //   rbp,rdi,rsi,rdx,rcx,rbx,rax.
    let sr = saved_regs as *const u64;
    unsafe {
        words.add(0).write(sr.add(0).read()); // r15
        words.add(1).write(sr.add(1).read()); // r14
        words.add(2).write(sr.add(2).read()); // r13
        words.add(3).write(sr.add(3).read()); // r12
        words.add(4).write(sr.add(12).read()); // r11
        words.add(5).write(sr.add(4).read()); // r10
        words.add(6).write(sr.add(5).read()); // r9  (musl: thread-functie!)
        words.add(7).write(sr.add(6).read()); // r8
        words.add(8).write(sr.add(10).read()); // rbp
        words.add(9).write(sr.add(9).read()); // rdi
        words.add(10).write(sr.add(8).read()); // rsi
        words.add(11).write(sr.add(7).read()); // rdx
        words.add(12).write(sr.add(13).read()); // rcx
        words.add(13).write(sr.add(11).read()); // rbx
        words.add(14).write(0); // rax = 0 (de child ziet clone() 0 teruggeven)
        words.add(15).write(rip);
        words.add(16).write(cs);
        words.add(17).write(0x202); // IF=1
        words.add(18).write(rsp); // child-stack (door clone meegegeven)
        words.add(19).write(ss);
    }
    s.tasks[idx].rsp = ctx;
    s.tasks[idx].kstack = kstack_top;
    s.tasks[idx].cr3 = cr3; // GEDEELDE adresruimte met het proces
    s.tasks[idx].fs_base = fs_base; // eigen TLS
    s.tasks[idx].state = State::Ready;
    s.tasks[idx].vruntime = s.tasks[s.current].vruntime;
    s.tasks[idx].nice = 0;
    idx
}

/// S6-zelftest: overschrijf de stack-canary van een taak (simuleert een overflow)
/// zodat de scheduler-bewaking 'm bij de volgende switch detecteert.
#[doc(hidden)]
pub fn debug_corrupt_stack_canary(task: usize) {
    let s = SCHED.lock();
    if task < s.count && s.tasks[task].stack_bottom != 0 {
        unsafe { (s.tasks[task].stack_bottom as *mut u64).write(0xBADC0DE) };
    }
}

fn busy() {
    for _ in 0..150_000 {
        core::hint::spin_loop();
    }
}

// Gelijke werklast (één `busy()`) — het verschil in tellers komt nu PUUR van de
// nice-prioriteit (S2), niet van verschillende lusduur.
extern "C" fn task_a() -> ! {
    loop {
        TASK_COUNTERS[1].fetch_add(1, Ordering::Relaxed);
        busy();
    }
}
extern "C" fn task_b() -> ! {
    loop {
        TASK_COUNTERS[2].fetch_add(1, Ordering::Relaxed);
        busy();
    }
}
extern "C" fn task_c() -> ! {
    loop {
        TASK_COUNTERS[3].fetch_add(1, Ordering::Relaxed);
        busy();
    }
}

/// G1-zelftest: laat opzettelijk de eigen (GUARDED) kernel-stack overlopen. Elke
/// recursie-laag verbruikt ~256 B; na ~60 lagen kruist de stack de niet-gemapte
/// guard-pagina → hardware-#PF. De page-fault-handler (op z'n EIGEN IST-stack)
/// herkent het guard-adres, beëindigt ALLEEN deze taak, en de scheduler/desktop
/// draait door — bewijs van recovery uit een echte kernel-stack-overflow.
extern "C" fn task_overflow() -> ! {
    #[inline(never)]
    fn recurse(depth: u64) -> u64 {
        // `black_box` + gebruik-na-recursie verhindert tail-call/eliminatie, zodat
        // de stack ECHT per laag groeit (geen lus-optimalisatie).
        let mut buf = [0u8; 256];
        buf[0] = depth as u8;
        let buf = core::hint::black_box(buf);
        let deeper = recurse(core::hint::black_box(depth).wrapping_add(1));
        core::hint::black_box(buf[0] as u64).wrapping_add(deeper)
    }
    crate::kinfo!("G1 sched-selftest: taak 5 laat opzettelijk z'n guarded kernel-stack overlopen...");
    let _ = core::hint::black_box(recurse(0));
    loop {
        x86_64::instructions::hlt(); // onbereikbaar: de guard-#PF beëindigt deze taak
    }
}

/// S2-zelftest: bewijst dat `sleep_ticks` ECHTE timed waiting doet (de taak geeft
/// de CPU af i.p.v. te busy-loopen) en rapporteert de prioriteits-tellers.
extern "C" fn task_sleeper() -> ! {
    let mut woke = 0u64;
    loop {
        woke += 1;
        TASK_COUNTERS[4].fetch_add(1, Ordering::Relaxed);
        let t = crate::interrupts::TICKS.load(Ordering::Relaxed);
        if woke <= 4 {
            crate::kinfo!("S2 sched-selftest: sleeper ontwaakt #{woke} @ tick {t} (~0.5s/cyclus)");
        }
        if woke == 5 {
            let a = TASK_COUNTERS[1].load(Ordering::Relaxed);
            let b = TASK_COUNTERS[2].load(Ordering::Relaxed);
            let c = TASK_COUNTERS[3].load(Ordering::Relaxed);
            crate::kinfo!("S2 prio-selftest: tellers nice(-10/0/+10) = {a}/{b}/{c}, sleeper={}", TASK_COUNTERS[4].load(Ordering::Relaxed));
        }
        sleep_ticks(50); // ~0.5 s slapen
        x86_64::instructions::hlt(); // CPU afgeven; de volgende tick switcht weg (Sleeping)
    }
}

// ════════════════════════════════════════════════════════════════════════════
// SMP: per-CPU scheduler voor de application-processors (Run 1).
//
// Elke AP heeft een EIGEN run-queue + context-switch, los van de globale BSP-
// scheduler hierboven. Zo draait elke core onafhankelijk z'n eigen (aan die core
// vastgepinde) kernel-taken — echte per-CPU scheduling, zonder de bewezen BSP-
// scheduler (desktop/netwerk/ring-3) aan te raken. Geen cross-CPU-lock-contentie:
// een AP raakt alleen z'n eigen `AP_SCHED[cpu]` aan. Taakmigratie/IPIs = Run 2.
// ════════════════════════════════════════════════════════════════════════════

const MAX_CPU: usize = 8;
const AP_NTASK: usize = 4; // idle + 2 workers + ruimte voor 1 gebalanceerde taak

/// Huidige core-index (LAPIC-id, gemaskeerd op de per-CPU-tabellen).
pub fn this_cpu() -> usize {
    (crate::apic::lapic_id() & 7) as usize
}

#[derive(Clone, Copy)]
struct ApTask {
    rsp: u64,
}

struct ApSched {
    tasks: [ApTask; AP_NTASK],
    count: usize,
    current: usize,
}

static AP_SCHED: [Mutex<ApSched>; MAX_CPU] =
    [const { Mutex::new(ApSched { tasks: [ApTask { rsp: 0 }; AP_NTASK], count: 0, current: 0 }) }; MAX_CPU];

// Worker-stacks per core (slot 0 = idle, gebruikt de AP-hoofdstack; 1..2 = workers).
static mut AP_STACKS2: [[[u8; STACK_SIZE]; AP_NTASK]; MAX_CPU] =
    [[[0; STACK_SIZE]; AP_NTASK]; MAX_CPU];

/// Per-core werk-tellers — bewijzen dat elke AP z'n eigen taken draait/afwisselt.
pub static AP_WORK_A: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
pub static AP_WORK_B: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
/// Teller van de dynamisch GEBALANCEERDE taak (Run 2: load-balancing).
pub static AP_WORK_C: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];

global_asm!(
    ".global ap_timer_switch",
    "ap_timer_switch:",
    "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi", "push rbp",
    "push r8", "push r9", "push r10", "push r11", "push r12", "push r13", "push r14", "push r15",
    "mov rdi, rsp",
    "and rsp, -16",
    "call ap_schedule_tick",
    "mov rsp, rax",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10", "pop r9", "pop r8",
    "pop rbp", "pop rdi", "pop rsi", "pop rdx", "pop rcx", "pop rbx", "pop rax",
    "iretq",
);

extern "C" {
    fn ap_timer_switch();
}

/// Adres van de AP-context-switch-stub (voor de IDT van de AP-timer, vector 0x41).
pub fn ap_stub_addr() -> u64 {
    ap_timer_switch as usize as u64
}

/// Per-CPU round-robin: bewaar de huidige rsp van deze core en geef de volgende
/// taak-rsp terug. Raakt enkel `AP_SCHED[cpu]` aan (single-writer → geen contentie).
#[no_mangle]
pub extern "sysv64" fn ap_schedule_tick(rsp: u64) -> u64 {
    crate::apic::eoi();
    let cpu = this_cpu();
    let mut s = AP_SCHED[cpu].lock();
    if s.count == 0 {
        return rsp;
    }
    let cur = s.current;
    s.tasks[cur].rsp = rsp;
    let n = (cur + 1) % s.count;
    s.current = n;
    s.tasks[n].rsp
}

fn ap_init_stack(cpu: usize, slot: usize, entry: extern "C" fn() -> !) -> u64 {
    let cs = CS::get_reg().0 as u64;
    let ss = SS::get_reg().0 as u64;
    let base = unsafe { core::ptr::addr_of_mut!(AP_STACKS2[cpu][slot]) as *mut u8 };
    let top = (base as u64 + STACK_SIZE as u64) & !0xF;
    let ctx = top - (CONTEXT_WORDS as u64) * 8;
    let words = ctx as *mut u64;
    unsafe {
        for i in 0..15 {
            words.add(i).write(0);
        }
        words.add(15).write(entry as usize as u64); // rip
        words.add(16).write(cs);
        words.add(17).write(0x202); // IF=1
        words.add(18).write(top);
        words.add(19).write(ss);
    }
    ctx
}

/// Zet de per-CPU run-queue van deze core op: idle (slot 0, gevuld op de eerste
/// tick = de lopende ap_main-hlt-lus) + twee worker-taken. Aanroepen op de AP
/// zelf, ná `gdt::init_ap`, vóór de timer aangaat.
pub fn ap_setup(cpu: usize) {
    let mut s = AP_SCHED[cpu].lock();
    s.tasks[0].rsp = 0; // idle — rsp komt bij de eerste preemptie
    s.tasks[1].rsp = ap_init_stack(cpu, 1, ap_worker_a);
    s.tasks[2].rsp = ap_init_stack(cpu, 2, ap_worker_b);
    s.count = 3;
    s.current = 0;
}

extern "C" fn ap_worker_a() -> ! {
    let c = this_cpu();
    loop {
        AP_WORK_A[c].fetch_add(1, Ordering::Relaxed);
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
}

extern "C" fn ap_worker_b() -> ! {
    let c = this_cpu();
    loop {
        AP_WORK_B[c].fetch_add(1, Ordering::Relaxed);
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
}

extern "C" fn ap_worker_c() -> ! {
    let c = this_cpu();
    loop {
        AP_WORK_C[c].fetch_add(1, Ordering::Relaxed);
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
}

/// Huidige run-queue-lengte van core `cpu` (voor load-balancing-beslissingen).
pub fn ap_load(cpu: usize) -> usize {
    AP_SCHED[cpu].lock().count
}

/// Load-balancing: plaats dynamisch een extra kernel-taak op core `cpu`. Veilig
/// vanaf de BSP terwijl de AP draait (de per-CPU Mutex serialiseert met diens
/// scheduler-tick). De AP pakt 'm bij de volgende ronde op. False = queue vol.
pub fn ap_enqueue_worker(cpu: usize) -> bool {
    let mut s = AP_SCHED[cpu].lock();
    if s.count >= AP_NTASK {
        return false;
    }
    let slot = s.count;
    s.tasks[slot].rsp = ap_init_stack(cpu, slot, ap_worker_c);
    s.count += 1;
    true
}
