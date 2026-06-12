# EuroOS — Ontbrekende Subsystemen
## Claude Code Build Prompt — Wat nog ontbreekt voor een modern en stabiel OS
## Gebaseerd op bestaande roadmap (Runs 1–13) + architectuuranalyse

> **Gebruik dit document als aanvulling op de bestaande track-specs (Track 1–7).**
> De subsystemen hieronder zijn niet gedekt door de huidige runs maar zijn
> **vereist voor een productierijp OS**. Elk blok is zelfstandig implementeerbaar
> maar heeft afhankelijkheden — die staan expliciet vermeld.
>
> **Volgorde van prioriteit:**
> - 🔴 Kritiek — blokkeert andere subsystemen
> - 🟡 Hoog — vereist voor stabiele alpha
> - 🟢 Medium — vereist voor beta/v1.0
> - 🔵 Later — v1.0+ of enterprise

---

## Inhoudsopgave

1. [Kernel Observability](#1-kernel-observability)
2. [Memory Protection Hardening](#2-memory-protection-hardening)
3. [ACPI & Energiebeheer](#3-acpi--energiebeheer)
4. [PCI/PCIe Hardware Discovery](#4-pcipcie-hardware-discovery)
5. [Volwaardige Syscall-laag](#5-volwaardige-syscall-laag)
6. [Scheduler Volwassenheid](#6-scheduler-volwassenheid)
7. [Networking Stack Volwassenheid](#7-networking-stack-volwassenheid)
8. [Audio Subsysteem](#8-audio-subsysteem)
9. [USB Stack](#9-usb-stack)
10. [Storage Betrouwbaarheid](#10-storage-betrouwbaarheid)
11. [System Services & Init](#11-system-services--init)
12. [Update & Recovery Systeem](#12-update--recovery-systeem)
13. [User & Session Model](#13-user--session-model)
14. [Display Stack Volledigheid](#14-display-stack-volledigheid)
15. [Printer & Scanner](#15-printer--scanner)
16. [Hardware Abstraction Layer Uitbreidingen](#16-hardware-abstraction-layer-uitbreidingen)

---

## 1. Kernel Observability

**Prioriteit:** 🔴 Kritiek — implementeer dit vóór Run 2
**Afhankelijkheden:** Geen — kan parallel aan alles
**Reden:** Zonder observability is debuggen van SMP, memory bugs en crashes
extreem moeilijk. Dit is de multiplier op alle andere runs.

### Wat ontbreekt

Momenteel heeft EuroOS COM1 serial output. Dat is een begin maar niet genoeg
voor een productie OS. Vereist:

- Gestructureerde kernel logging met niveaus
- Panic dumps met volledige stack trace
- Kernel assertions met source locatie
- Boot diagnostics die de kernel state samenvatten
- Tracing van kritieke kernel paden
- Crash rapport generatie voor userspace

### Implementatie

#### 1.1 Gestructureerde Logger

```rust
// kernel/src/log.rs

/// Log niveaus — in volgorde van ernst
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Trace = 0,   // Heel gedetailleerd — alleen in debug builds
    Debug = 1,   // Nuttig voor ontwikkeling
    Info  = 2,   // Normale werking
    Warn  = 3,   // Iets onverwachts maar herstelbaar
    Error = 4,   // Fout maar kernel loopt door
    Fatal = 5,   // Kernel kan niet doorgaan
}

/// Een enkel logbericht
#[derive(Debug)]
pub struct LogEntry {
    pub level:     LogLevel,
    pub timestamp: u64,        // Nanoseconden since boot (via TSC)
    pub cpu_id:    u8,         // Welke CPU core
    pub module:    &'static str, // Bijv. "scheduler", "eurofs", "netwerk"
    pub message:   &'static str,
    pub file:      &'static str,
    pub line:      u32,
}

/// Ring buffer voor kernel logs — lock-free via atomics
pub struct KernelLogBuffer {
    entries:    [LogEntry; 4096],
    write_idx:  AtomicUsize,
    read_idx:   AtomicUsize,
    total_lost: AtomicU64,     // Hoeveel entries verloren gingen (buffer vol)
}

/// Globale kernel logger
static KERNEL_LOG: KernelLogBuffer = KernelLogBuffer::new();

/// Macros voor gebruik door andere kernel modules
#[macro_export]
macro_rules! klog {
    ($level:expr, $module:expr, $msg:expr) => {
        $crate::log::log($level, $module, $msg, file!(), line!())
    };
}

#[macro_export] macro_rules! ktrace { ($m:expr, $s:expr) => { klog!(LogLevel::Trace, $m, $s) }; }
#[macro_export] macro_rules! kdebug { ($m:expr, $s:expr) => { klog!(LogLevel::Debug, $m, $s) }; }
#[macro_export] macro_rules! kinfo  { ($m:expr, $s:expr) => { klog!(LogLevel::Info,  $m, $s) }; }
#[macro_export] macro_rules! kwarn  { ($m:expr, $s:expr) => { klog!(LogLevel::Warn,  $m, $s) }; }
#[macro_export] macro_rules! kerror { ($m:expr, $s:expr) => { klog!(LogLevel::Error, $m, $s) }; }
```

#### 1.2 Panic Handler met Stack Trace

```rust
// kernel/src/panic.rs

#[panic_handler]
fn kernel_panic(info: &PanicInfo) -> ! {
    // 1. Disable interrupts op alle cores via IPI
    disable_all_cpus();

    // 2. Toon rood paniekscherm op framebuffer
    if let Some(fb) = FRAMEBUFFER.try_lock() {
        fb.fill_screen(Color::PANIC_RED);
        fb.draw_text(40, 40, "KERNEL PANIC", Color::WHITE, FontSize::Large);

        if let Some(msg) = info.message() {
            fb.draw_text(40, 100, &alloc::format!("{}", msg), Color::WHITE, FontSize::Normal);
        }

        if let Some(loc) = info.location() {
            fb.draw_text(40, 130,
                &alloc::format!("{}:{}:{}", loc.file(), loc.line(), loc.column()),
                Color::LIGHT_GRAY, FontSize::Small);
        }
    }

    // 3. Dump registers
    let regs = capture_registers();
    serial_println!("=== KERNEL PANIC ===");
    serial_println!("RIP: {:#018x}  RSP: {:#018x}", regs.rip, regs.rsp);
    serial_println!("RAX: {:#018x}  RBX: {:#018x}", regs.rax, regs.rbx);
    serial_println!("RCX: {:#018x}  RDX: {:#018x}", regs.rcx, regs.rdx);
    serial_println!("CR2: {:#018x}  CR3: {:#018x}", read_cr2(), read_cr3());

    // 4. Stack trace
    serial_println!("\n=== STACK TRACE ===");
    let mut frame = regs.rbp;
    for depth in 0..32 {
        if frame == 0 || !is_kernel_address(frame) { break; }
        let ret_addr = unsafe { *(frame as *const u64).add(1) };
        serial_println!("  #{:02}  {:#018x}", depth, ret_addr);
        // Lookup symbool in kernel symboolmap (indien beschikbaar)
        if let Some(sym) = KERNEL_SYMBOLS.lookup(ret_addr) {
            serial_println!("       <{}>", sym);
        }
        frame = unsafe { *(frame as *const u64) };
    }

    // 5. Dump recente kernel log entries
    serial_println!("\n=== LAATSTE LOG ENTRIES ===");
    KERNEL_LOG.dump_last_n(50);

    // 6. Eindeloze halt — machine blijft aan voor hardware debugger
    loop { unsafe { core::arch::x86_64::_mm_pause(); } }
}
```

#### 1.3 Boot Diagnostics

```rust
// kernel/src/diagnostics.rs
// Roep aan aan het einde van kernel init, vóór eerste userspace proces

pub fn print_boot_diagnostics() {
    kinfo!("boot", "=== EuroOS Boot Diagnostics ===");
    kinfo!("boot", &alloc::format!("Kernel versie: {}", env!("CARGO_PKG_VERSION")));
    kinfo!("boot", &alloc::format!("Build: {} ({})", BUILD_TIMESTAMP, BUILD_GIT_HASH));

    // CPU info
    let cpu = detect_cpu();
    kinfo!("boot", &alloc::format!("CPU: {} cores, {}MHz, features: {:?}",
        cpu.core_count, cpu.freq_mhz, cpu.features));

    // Geheugen
    let mem = EUROMM.stats();
    kinfo!("boot", &alloc::format!("RAM: {} MiB totaal, {} MiB vrij, {} MiB kern",
        mem.total_mb, mem.free_mb, mem.kernel_mb));

    // Subsystemen
    kinfo!("boot", &alloc::format!("EuroFS: gemount op /, {} MB vrij", EUROFS.free_mb()));
    kinfo!("boot", &alloc::format!("Scheduler: {} cores actief", SCHEDULER.active_cores()));
    kinfo!("boot", &alloc::format!("Interrupts: IDT geladen, APIC geïnitialiseerd"));

    // ACPI (indien aanwezig)
    if let Some(acpi) = ACPI.as_ref() {
        kinfo!("boot", &alloc::format!("ACPI: versie {}, {} tables", acpi.version, acpi.table_count));
    }

    kinfo!("boot", "=== Boot compleet ===");
}
```

#### 1.4 Kernel Assertions

```rust
// Gebruik overal in kernel code

/// Debug-only assertion — wordt weggecompileerd in release
macro_rules! kassert {
    ($cond:expr) => {
        #[cfg(debug_assertions)]
        if !($cond) {
            panic!("Kernel assertion mislukt: {}", stringify!($cond));
        }
    };
    ($cond:expr, $msg:expr) => {
        #[cfg(debug_assertions)]
        if !($cond) {
            panic!("Kernel assertion mislukt: {} — {}", stringify!($cond), $msg);
        }
    };
}

/// Altijd actief — voor invarianten die nooit mogen falen
macro_rules! kguard {
    ($cond:expr, $msg:expr) => {
        if !($cond) {
            panic!("Kernel invariant geschonden: {}", $msg);
        }
    };
}
```

#### 1.5 Symboolmap (voor stack traces)

```bash
# scripts/build-symbols.sh
# Extraheer symbolen uit kernel ELF en embed als read-only sectie

llvm-nm --numeric-sort --print-size target/.../eurokernel.elf \
  | grep " T " \
  | awk '{print $1, $4}' \
  > kernel-symbols.txt

# Embed in kernel via include_bytes! of apart laad-mechanisme
```

---

## 2. Memory Protection Hardening

**Prioriteit:** 🔴 Kritiek — implementeer tijdens/na Run 3 (EuroMM)
**Afhankelijkheden:** EuroMM slab allocator, eigen page tables (al aanwezig)

### Wat ontbreekt

De huidige paging implementatie is functioneel maar mist beveiligingslagen
die een productie kernel nodig heeft.

#### 2.1 Guard Pages

```rust
// kernel/src/memory/guard.rs

/// Voeg een guard page toe aan het einde van elke kernel stack
/// Guard pages zijn NIET-gemapt → page fault bij stack overflow
/// ipv stille stack corruptie

pub fn allocate_kernel_stack_with_guard(size: usize) -> Result<StackAlloc, AllocError> {
    // Alloceer size + 1 extra page
    let total_frames = (size + PAGE_SIZE - 1) / PAGE_SIZE + 1;
    let base = EUROMM.allocate_contiguous(total_frames)?;

    // Map alles behalve de laatste pagina (de guard)
    for i in 0..total_frames - 1 {
        let phys = base.as_u64() + (i as u64 * PAGE_SIZE as u64);
        let virt = KERNEL_STACK_BASE + current_stack_offset();
        page_table::map(
            VirtAddr::new(virt),
            PhysAddr::new(phys),
            PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::NO_EXECUTE,
        )?;
    }

    // Laatste pagina: NIET mappen = guard page
    // Elke write hiernaar → #PF → kernel panic met "stack overflow"

    Ok(StackAlloc { base, size })
}

/// Page fault handler: detecteer stack overflow
pub fn handle_page_fault(addr: VirtAddr, error: PageFaultError) {
    // Is dit adres een bekende guard page?
    if GUARD_PAGES.contains(addr) {
        panic!("STACK OVERFLOW gedetecteerd op adres {:#x}", addr.as_u64());
    }
    // Anders: normale page fault afhandeling
    handle_normal_page_fault(addr, error);
}
```

#### 2.2 SMEP & SMAP

```rust
// kernel/src/cpu/features.rs

/// SMEP: Supervisor Mode Execution Prevention
/// Voorkomt dat kernel code uitvoert in user-space pagina's
/// → Exploit mitigatie: attacker kan geen shellcode in user heap plaatsen

/// SMAP: Supervisor Mode Access Prevention
/// Voorkomt dat kernel data leest/schrijft uit user-space
/// → Forceer expliciete user-memory kopieën via copy_from_user/copy_to_user

pub fn enable_smep_smap() {
    let mut cr4 = read_cr4();
    cr4 |= CR4_SMEP; // Bit 20
    cr4 |= CR4_SMAP; // Bit 21
    write_cr4(cr4);

    kinfo!("cpu", "SMEP en SMAP ingeschakeld");
}

/// Veilige kopie van user-space naar kernel
/// Zet AC-bit (SMAP bypass) enkel tijdens de kopie
pub fn copy_from_user(dst: &mut [u8], src_user: *const u8) -> Result<(), FaultError> {
    unsafe {
        core::arch::x86_64::_stac(); // Zet AC-bit: SMAP tijdelijk uit
        let result = safe_memcopy(dst.as_mut_ptr(), src_user, dst.len());
        core::arch::x86_64::_clac(); // Zet AC-bit: SMAP terug aan
        result
    }
}
```

#### 2.3 NX-bit (No-Execute)

```rust
// kernel/src/memory/paging.rs

/// Page table flags — NX-bit correct instellen
pub struct PageFlags(u64);

impl PageFlags {
    pub const PRESENT:    u64 = 1 << 0;
    pub const WRITABLE:   u64 = 1 << 1;
    pub const USER:       u64 = 1 << 2;
    pub const NO_EXECUTE: u64 = 1 << 63; // NX-bit

    /// Kernel code: aanwezig, niet schrijfbaar, uitvoerbaar
    pub const KERNEL_CODE: PageFlags = PageFlags(Self::PRESENT);

    /// Kernel data: aanwezig, schrijfbaar, NIET uitvoerbaar (W^X)
    pub const KERNEL_DATA: PageFlags = PageFlags(Self::PRESENT | Self::WRITABLE | Self::NO_EXECUTE);

    /// User code: aanwezig, user-toegankelijk, uitvoerbaar
    pub const USER_CODE: PageFlags = PageFlags(Self::PRESENT | Self::USER);

    /// User data: aanwezig, schrijfbaar, user-toegankelijk, NIET uitvoerbaar
    pub const USER_DATA: PageFlags = PageFlags(
        Self::PRESENT | Self::WRITABLE | Self::USER | Self::NO_EXECUTE
    );

    /// Stack: zelfde als data maar wordt nooit als code gebruikt
    pub const STACK: PageFlags = Self::USER_DATA;
}

/// W^X enforcement: een pagina mag nooit tegelijk schrijfbaar EN uitvoerbaar zijn
pub fn validate_page_flags(flags: PageFlags) -> Result<(), SecurityError> {
    let writable   = flags.0 & PageFlags::WRITABLE   != 0;
    let executable = flags.0 & PageFlags::NO_EXECUTE == 0; // NX niet gezet = uitvoerbaar
    if writable && executable {
        return Err(SecurityError::WxViolation);
    }
    Ok(())
}
```

#### 2.4 KASLR (Kernel Address Space Layout Randomization)

```rust
// kernel/src/boot/kaslr.rs

/// Randomiseer het kernel-laadadres bij elke boot
/// Maakt exploits die kernel-adressen hardcoden onbetrouwbaar

pub fn compute_kaslr_offset() -> u64 {
    // Gebruik hardware RNG via RDRAND instructie
    let mut rand: u64;
    unsafe {
        core::arch::x86_64::_rdrand64_step(&mut rand);
    }

    // Align op 2MB (huge page grens) voor efficiëntie
    // Bereik: 0 tot 512MB randomisatie
    let max_offset = 512 * 1024 * 1024u64;
    let offset = rand % max_offset;
    offset & !(2 * 1024 * 1024 - 1) // Align op 2MB
}

/// Pas KASLR offset toe op kernel virtual addresses
/// Roep aan vóór kernel symbolen worden gebruikt
pub fn apply_kaslr_offset(offset: u64) {
    KASLR_OFFSET.store(offset, Ordering::SeqCst);
    kinfo!("kaslr", &alloc::format!("KASLR offset: {:#x}", offset));
}
```

#### 2.5 Stack Canaries

```rust
// kernel/src/security/canary.rs

/// Stack canary — een geheime waarde die op de stack geplaatst wordt
/// Als de canary veranderd is bij functie-return → stack corruptie gedetecteerd

static STACK_CANARY: AtomicU64 = AtomicU64::new(0);

pub fn init_stack_canary() {
    let mut canary: u64;
    unsafe { core::arch::x86_64::_rdrand64_step(&mut canary); }
    STACK_CANARY.store(canary, Ordering::SeqCst);
    // Rust's compiler ondersteunt stack canaries via -Z stack-protector=strong
    // Stel in in .cargo/config.toml:
    // [build]
    // rustflags = ["-Z", "stack-protector=strong"]
}

/// Canary check mislukt — aanroep door compiler gegenereerde proloog/epiloog code
#[no_mangle]
pub extern "C" fn __stack_chk_fail() -> ! {
    panic!("STACK CANARY MISLUKT — mogelijke buffer overflow gedetecteerd");
}
```

---

## 3. ACPI & Energiebeheer

**Prioriteit:** 🔴 Kritiek — vereist voor afsluiten, herstart, en hardware discovery
**Afhankelijkheden:** PCI discovery (sectie 4)
**Noot:** Zonder ACPI kan EuroOS niet netjes afsluiten of herstarten.
Dit maakt het ongeschikt voor demo's buiten QEMU.

### Wat ontbreekt

ACPI (Advanced Configuration and Power Interface) is de standaard manier
waarop het OS praat met de firmware over hardware topologie, power management,
en system events.

#### 3.1 ACPI Tabellen Parsen

```rust
// crates/euroacpi/src/lib.rs

/// ACPI Root System Description Pointer
/// UEFI geeft ons dit adres via de configuratietabel
#[repr(C, packed)]
pub struct Rsdp {
    signature:  [u8; 8],  // "RSD PTR "
    checksum:   u8,
    oem_id:     [u8; 6],
    revision:   u8,       // 0 = ACPI 1.0, 2 = ACPI 2.0+
    rsdt_addr:  u32,      // 32-bit adres van RSDT
    // ACPI 2.0+ velden:
    length:     u32,
    xsdt_addr:  u64,      // 64-bit adres van XSDT (gebruik dit op x86-64)
    ext_checksum: u8,
    _reserved:  [u8; 3],
}

/// Generieke ACPI tabel header
#[repr(C, packed)]
pub struct AcpiTableHeader {
    signature: [u8; 4],   // Bijv. "FACP", "APIC", "HPET", "MCFG"
    length:    u32,
    revision:  u8,
    checksum:  u8,
    oem_id:    [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id:   u32,
    creator_revision: u32,
}

pub struct AcpiTables {
    pub fadt:  Option<&'static Fadt>,   // Fixed ACPI Description Table — power management
    pub madt:  Option<&'static Madt>,   // Multiple APIC Description Table — CPU/APIC info
    pub hpet:  Option<&'static Hpet>,   // High Precision Event Timer
    pub mcfg:  Option<&'static Mcfg>,   // PCI Express memory-mapped config
    pub dsdt:  Option<&'static [u8]>,   // Differentiated System Description Table (AML bytecode)
}

impl AcpiTables {
    /// Initialiseer vanuit UEFI RSDP adres
    pub fn from_rsdp(rsdp_addr: u64) -> Result<Self, AcpiError> {
        let rsdp = unsafe { &*(rsdp_addr as *const Rsdp) };
        rsdp.validate()?;

        // Gebruik XSDT (64-bit) op moderne hardware
        let xsdt_addr = rsdp.xsdt_addr;
        let xsdt = unsafe { &*(xsdt_addr as *const AcpiTableHeader) };
        xsdt.validate()?;

        let mut tables = Self::empty();
        // Itereer over alle tabel-pointers in XSDT
        let entry_count = (xsdt.length as usize - core::mem::size_of::<AcpiTableHeader>()) / 8;
        let entries = unsafe {
            core::slice::from_raw_parts(
                (xsdt_addr as usize + core::mem::size_of::<AcpiTableHeader>()) as *const u64,
                entry_count,
            )
        };

        for &table_addr in entries {
            let header = unsafe { &*(table_addr as *const AcpiTableHeader) };
            match &header.signature {
                b"FACP" => tables.fadt = Some(unsafe { &*(table_addr as *const Fadt) }),
                b"APIC" => tables.madt = Some(unsafe { &*(table_addr as *const Madt) }),
                b"HPET" => tables.hpet = Some(unsafe { &*(table_addr as *const Hpet) }),
                b"MCFG" => tables.mcfg = Some(unsafe { &*(table_addr as *const Mcfg) }),
                _ => {}
            }
        }

        Ok(tables)
    }
}
```

#### 3.2 Shutdown & Reboot

```rust
// kernel/src/power.rs

pub enum PowerAction {
    Shutdown,
    Reboot,
    Sleep(SleepState),
    Hibernate,
}

pub enum SleepState {
    S3, // Suspend to RAM
    S4, // Suspend to Disk (Hibernate)
}

pub fn perform_power_action(action: PowerAction) {
    match action {
        PowerAction::Shutdown => {
            kinfo!("power", "Systeem afsluiten...");
            flush_all_buffers();
            sync_all_filesystems();
            notify_all_services_shutdown();

            // ACPI S5 (soft off) via FADT
            if let Some(fadt) = ACPI.fadt() {
                acpi_enter_sleep_state(5);
            } else {
                // Fallback: QEMU debug exit
                unsafe { x86_out16(0x604, 0x2000); } // QEMU-specifiek
            }
        }
        PowerAction::Reboot => {
            kinfo!("power", "Systeem herstarten...");
            flush_all_buffers();
            sync_all_filesystems();

            // ACPI reset via FADT reset register
            if let Some(fadt) = ACPI.fadt() {
                if fadt.flags & FADT_RESET_REG_SUPPORTED != 0 {
                    acpi_reset();
                    return;
                }
            }

            // Fallback: keyboard controller reset (klassiek)
            unsafe {
                while x86_in8(0x64) & 0x02 != 0 {} // Wacht op leeg
                x86_out8(0x64, 0xFE); // Pulse reset lijn
            }
        }
        PowerAction::Sleep(state) => {
            // Sla systeemstate op, stuur naar sleep state
            prepare_sleep(state);
            acpi_enter_sleep_state(state as u8);
        }
        _ => kwarn!("power", "Power actie nog niet ondersteund"),
    }
}
```

#### 3.3 CPU Power States (C-states)

```rust
// kernel/src/cpu/power.rs

/// Wanneer een CPU core niets te doen heeft, zet hem in een lagere power state
/// C0 = actief, C1 = halt, C2/C3 = dieper slaap

pub fn cpu_idle_loop() -> ! {
    loop {
        // Controleer of er werk is
        if SCHEDULER.has_runnable_tasks() {
            SCHEDULER.schedule();
            continue;
        }

        // Geen werk — gebruik de meest efficiënte idle state
        // HLT instructie: CPU stopt tot volgende interrupt
        unsafe { core::arch::x86_64::_mm_pause(); }
        unsafe { core::arch::asm!("hlt"); }
        // CPU hervat hier na interrupt (bijv. timer, toetsenbord)
    }
}
```

---

## 4. PCI/PCIe Hardware Discovery

**Prioriteit:** 🔴 Kritiek — vereist voor netwerk, GPU, NVMe, en vrijwel alle hardware
**Afhankelijkheden:** ACPI MCFG tabel (sectie 3)

### Wat ontbreekt

PCI/PCIe is de verbindingsbus voor bijna alle moderne hardware.
Zonder PCI enumeration kan EuroOS niet weten welke hardware aanwezig is
en kunnen drivers niet geladen worden.

#### 4.1 PCI Configuratieruimte

```rust
// crates/europci/src/lib.rs

/// PCI apparaat identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PciAddress {
    pub bus:      u8,  // 0–255
    pub device:   u8,  // 0–31
    pub function: u8,  // 0–7
}

/// PCI configuratieruimte — eerste 64 bytes (standaard header)
#[repr(C)]
pub struct PciConfigHeader {
    pub vendor_id:     u16,  // 0xFFFF = geen apparaat
    pub device_id:     u16,
    pub command:       u16,
    pub status:        u16,
    pub revision_id:   u8,
    pub prog_if:       u8,
    pub subclass:      u8,
    pub class_code:    u8,   // Zie PciClass enum
    pub cache_line:    u8,
    pub latency:       u8,
    pub header_type:   u8,   // Bit 7 = multi-function
    pub bist:          u8,
    // Type 0 (normaal apparaat):
    pub bar:           [u32; 6], // Base Address Registers
    pub cardbus_cis:   u32,
    pub subsys_vendor: u16,
    pub subsys_id:     u16,
    pub expansion_rom: u32,
    pub cap_pointer:   u8,
    pub _reserved:     [u8; 7],
    pub interrupt_line: u8,
    pub interrupt_pin:  u8,
    pub min_grant:      u8,
    pub max_latency:    u8,
}

/// Klasse codes (subclass in PciConfigHeader.class_code)
#[derive(Debug, Clone, Copy)]
pub enum PciClass {
    MassStorage    = 0x01, // NVMe, SATA, IDE
    Network        = 0x02, // Ethernet, WiFi
    Display        = 0x03, // GPU
    Multimedia     = 0x04, // Audio, Video
    Bridge         = 0x06, // PCI-PCI bridges
    SimplComm      = 0x07, // UART, modem
    Input          = 0x09, // Toetsenbord, muis (via PCI)
    Serial         = 0x0C, // USB, FireWire
    Wireless       = 0x0D, // WiFi, Bluetooth
    Unknown        = 0xFF,
}

/// Lees PCI configuratieruimte
/// Twee methoden beschikbaar:
/// 1. Legacy I/O poorten (0xCF8/0xCFC) — altijd beschikbaar
/// 2. MMIO via MCFG — sneller, voorkeur op moderne hardware

pub struct PciConfigAccess {
    mcfg_base: Option<u64>, // Van ACPI MCFG tabel
}

impl PciConfigAccess {
    pub fn read_u32(&self, addr: PciAddress, offset: u8) -> u32 {
        if let Some(mcfg) = self.mcfg_base {
            // MMIO methode (PCIe)
            let cfg_addr = mcfg
                + (addr.bus as u64) * 0x100000
                + (addr.device as u64) * 0x8000
                + (addr.function as u64) * 0x1000
                + offset as u64;
            unsafe { core::ptr::read_volatile(cfg_addr as *const u32) }
        } else {
            // Legacy I/O poort methode (PCI)
            let address = 0x8000_0000u32
                | (addr.bus as u32) << 16
                | (addr.device as u32) << 11
                | (addr.function as u32) << 8
                | (offset as u32 & 0xFC);
            unsafe {
                x86_out32(0xCF8, address);
                x86_in32(0xCFC)
            }
        }
    }
}
```

#### 4.2 PCI Enumeratie

```rust
// crates/europci/src/enumeration.rs

pub struct PciDevice {
    pub address:   PciAddress,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class:     PciClass,
    pub subclass:  u8,
    pub bars:      [PciBar; 6],
    pub irq:       Option<u8>,
}

#[derive(Debug)]
pub enum PciBar {
    None,
    Memory { base: u64, size: u64, prefetchable: bool },
    Io     { base: u32, size: u32 },
}

pub struct PciBus {
    devices: Vec<PciDevice>,
}

impl PciBus {
    /// Doorzoek alle PCI bussen en verzamel apparaten
    pub fn enumerate(access: &PciConfigAccess) -> Self {
        let mut devices = Vec::new();

        for bus in 0..=255u8 {
            for device in 0..32u8 {
                let addr = PciAddress { bus, device, function: 0 };
                let vendor = access.read_u16(addr, 0x00);

                if vendor == 0xFFFF {
                    continue; // Geen apparaat aanwezig
                }

                let header_type = access.read_u8(addr, 0x0E);
                let max_functions = if header_type & 0x80 != 0 { 8 } else { 1 };

                for function in 0..max_functions {
                    let faddr = PciAddress { bus, device, function };
                    if let Some(dev) = Self::probe_device(access, faddr) {
                        kinfo!("pci", &alloc::format!(
                            "PCI {:02x}:{:02x}.{}: {:04x}:{:04x} [{:?}]",
                            bus, device, function,
                            dev.vendor_id, dev.device_id, dev.class
                        ));
                        devices.push(dev);
                    }
                }
            }
        }

        Self { devices }
    }

    /// Zoek een apparaat op vendor/device ID
    pub fn find(&self, vendor: u16, device: u16) -> Option<&PciDevice> {
        self.devices.iter().find(|d| d.vendor_id == vendor && d.device_id == device)
    }

    /// Zoek apparaten op klasse
    pub fn find_by_class(&self, class: PciClass) -> impl Iterator<Item = &PciDevice> {
        self.devices.iter().filter(move |d| d.class == class)
    }
}
```

#### 4.3 Driver Registry

```rust
// kernel/src/drivers/registry.rs

/// Een driver registratie — koppelt PCI IDs aan driver instanties
pub struct DriverRegistration {
    pub name:    &'static str,
    pub matches: fn(&PciDevice) -> bool,
    pub probe:   fn(&PciDevice) -> Result<Box<dyn Driver>, DriverError>,
}

/// Globale driver registry
static DRIVER_REGISTRY: Mutex<Vec<DriverRegistration>> = Mutex::new(Vec::new());

pub fn register_driver(reg: DriverRegistration) {
    DRIVER_REGISTRY.lock().push(reg);
}

/// Probeer elke geregistreerde driver voor elk gevonden PCI apparaat
pub fn probe_all_devices(bus: &PciBus) {
    let registry = DRIVER_REGISTRY.lock();
    for device in bus.devices() {
        for driver_reg in registry.iter() {
            if (driver_reg.matches)(device) {
                match (driver_reg.probe)(device) {
                    Ok(driver) => {
                        kinfo!("drivers", &alloc::format!(
                            "Driver '{}' geladen voor {:04x}:{:04x}",
                            driver_reg.name, device.vendor_id, device.device_id
                        ));
                        LOADED_DRIVERS.lock().push(driver);
                        break;
                    }
                    Err(e) => {
                        kwarn!("drivers", &alloc::format!(
                            "Driver '{}' mislukt: {:?}", driver_reg.name, e
                        ));
                    }
                }
            }
        }
    }
}

/// Driver trait — alle hardware drivers implementeren dit
pub trait Driver: Send + Sync {
    fn name(&self) -> &str;
    fn device_type(&self) -> DeviceType;
    fn shutdown(&mut self);
}
```

---

## 5. Volwaardige Syscall-laag

**Prioriteit:** 🔴 Kritiek — Run 4 en verder
**Afhankelijkheden:** Process model (Run 4), VFS (Run 5)

### Wat ontbreekt

De huidige syscall implementatie heeft `sys_write` en `sys_exit`.
Voor POSIX-compatibiliteit en het draaien van bestaande software
zijn minimaal de volgende syscalls nodig.

#### 5.1 Volledige Syscall Tabel

```rust
// kernel/src/syscall/table.rs

/// EuroOS syscall nummers — eigen ABI, geen Linux-kopie
/// maar POSIX-semantisch compatibel voor eenvoudige portabiliteit
pub enum Syscall {
    // Proces management
    Exit        = 0,
    Fork        = 1,
    Exec        = 2,
    GetPid      = 3,
    GetPpid     = 4,
    Wait        = 5,
    WaitPid     = 6,
    GetTid      = 7,
    Clone       = 8,   // Threads
    SetTidAddr  = 9,

    // Geheugen
    Mmap        = 10,
    Munmap      = 11,
    Mprotect    = 12,
    Madvise     = 13,
    Brk         = 14,  // Heap groei (legacy, Mmap voorkeur)

    // Bestandssysteem
    Open        = 20,
    Close       = 21,
    Read        = 22,
    Write       = 23,
    Seek        = 24,
    Stat        = 25,
    Fstat       = 26,
    ReadDir     = 27,
    Mkdir       = 28,
    Unlink      = 29,
    Rename      = 30,
    Link        = 31,
    Symlink     = 32,
    ReadLink    = 33,
    Truncate    = 34,
    Chmod       = 35,
    Chown       = 36,
    Access      = 37,
    Dup         = 38,
    Dup2        = 39,
    Pipe        = 40,
    Fcntl       = 41,
    Ioctl       = 42,
    Sync        = 43,
    Fsync       = 44,
    Mount       = 45,
    Umount      = 46,
    Chdir       = 47,
    Getcwd      = 48,
    Openat      = 49,  // POSIX.1-2008 dirfd variant
    Mkdirat     = 50,

    // IPC
    Pipe2       = 60,
    Socket      = 61,
    Bind        = 62,
    Listen      = 63,
    Accept      = 64,
    Connect     = 65,
    Send        = 66,
    Recv        = 67,
    SendTo      = 68,
    RecvFrom    = 69,
    Shutdown    = 70,
    GetSockOpt  = 71,
    SetSockOpt  = 72,
    Poll        = 73,
    Select      = 74,
    EPollCreate = 75,
    EPollCtl    = 76,
    EPollWait   = 77,

    // Signalen
    Kill        = 80,
    Signal      = 81,
    SigAction   = 82,
    SigProcMask = 83,
    SigReturn   = 84,
    Alarm       = 85,
    Pause       = 86,

    // Tijd
    ClockGetTime = 90,
    ClockSetTime = 91,
    NanoSleep   = 92,
    GetTimeOfDay = 93,
    Times       = 94,

    // Gebruikers & groepen
    GetUid      = 100,
    GetGid      = 101,
    SetUid      = 102,
    SetGid      = 103,
    GetEuid     = 104,
    GetEgid     = 105,

    // Systeem
    Uname       = 110,
    Sysinfo     = 111,
    Reboot      = 112,
    Sync2       = 113,

    // EuroOS-specifiek (geen POSIX equivalent)
    EuroCapGet  = 200,  // Capability opvragen
    EuroCapSet  = 201,  // Capability instellen
    EuroSnapshot= 202,  // EuroFS snapshot
    EuroGuard   = 203,  // EuroGuard policy interactie
    EuroVault   = 204,  // EuroVault secret toegang
}
```

#### 5.2 Syscall Error Codes

```rust
// kernel/src/syscall/errors.rs

/// POSIX-compatibele foutcodes — negatief teruggegeven in RAX
#[repr(i64)]
pub enum Errno {
    EPERM   = -1,   // Operatie niet toegestaan
    ENOENT  = -2,   // Bestand of map niet gevonden
    ESRCH   = -3,   // Geen enkel proces
    EINTR   = -4,   // Systeemaanroep onderbroken
    EIO     = -5,   // I/O fout
    ENXIO   = -6,   // Geen enkel apparaat of adres
    E2BIG   = -7,   // Argumentenlijst te lang
    ENOEXEC = -8,   // Uitvoerformaatfout
    EBADF   = -9,   // Slechte bestandsdescriptor
    ECHILD  = -10,  // Geen kindprocessen
    EAGAIN  = -11,  // Probeer later opnieuw
    ENOMEM  = -12,  // Onvoldoende geheugen
    EACCES  = -13,  // Toestemming geweigerd
    EFAULT  = -14,  // Slecht adres
    EBUSY   = -16,  // Apparaat of resource bezet
    EEXIST  = -17,  // Bestand bestaat al
    EXDEV   = -18,  // Cross-device link
    ENODEV  = -19,  // Geen enkel apparaat
    ENOTDIR = -20,  // Geen map
    EISDIR  = -21,  // Is een map
    EINVAL  = -22,  // Ongeldig argument
    ENFILE  = -23,  // Bestandstabel overgelopen
    EMFILE  = -24,  // Te veel open bestanden
    ENOSPC  = -28,  // Geen ruimte meer op apparaat
    EROFS   = -30,  // Alleen-lezen bestandssysteem
    EPIPE   = -32,  // Gebroken pijp
    ERANGE  = -34,  // Resultaat buiten bereik
    ENOSYS  = -38,  // Functie niet geïmplementeerd
    ENOTEMPTY = -39, // Map niet leeg
    ELOOP   = -40,  // Te veel symbolische links
    ETIMEDOUT = -110, // Verbinding verlopen
    ECONNREFUSED = -111, // Verbinding geweigerd
}
```

#### 5.3 Syscall Tracing (voor debugging)

```rust
// kernel/src/syscall/trace.rs

/// Schakel syscall tracing in voor een specifiek proces
/// Nuttig voor strace-equivalent en security auditing

pub struct SyscallTracer {
    enabled_pids: BTreeSet<u64>,
    log_buffer:   RingBuffer<SyscallEvent>,
}

pub struct SyscallEvent {
    pub pid:       u64,
    pub tid:       u64,
    pub syscall:   Syscall,
    pub args:      [u64; 6],
    pub result:    i64,
    pub duration_ns: u64,
    pub timestamp: u64,
}

impl SyscallTracer {
    pub fn trace_entry(&self, pid: u64, syscall: Syscall, args: [u64; 6]) -> Option<u64> {
        if self.enabled_pids.contains(&pid) {
            Some(read_tsc()) // Starttijd voor duur meting
        } else {
            None
        }
    }

    pub fn trace_exit(&mut self, pid: u64, syscall: Syscall, result: i64, start: Option<u64>) {
        if let Some(start_tsc) = start {
            let event = SyscallEvent {
                pid, tid: current_tid(), syscall,
                args: [0; 6], // Bewaar args van entry
                result,
                duration_ns: tsc_to_ns(read_tsc() - start_tsc),
                timestamp: current_time_ns(),
            };
            self.log_buffer.push(event);
        }
    }
}
```

---

## 6. Scheduler Volwassenheid

**Prioriteit:** 🟡 Hoog — na Run 1-2 (SMP)
**Afhankelijkheden:** SMP (Run 1-2), Process model (Run 4)

### Wat ontbreekt

De huidige round-robin scheduler is functioneel maar mist:
- Prioriteiten en starvation prevention
- Blocking I/O (sleep/wakeup)
- Timer precisie (nanosleep)
- Realtime-ish prioriteiten voor audio/compositor

#### 6.1 Prioriteiten & Wacht-queues

```rust
// kernel/src/sched/priority.rs

/// Scheduler prioriteiten
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Idle     = 0,   // Idle taak — alleen als niets anders klaar is
    Low      = 1,   // Achtergrond taken (backup, indexering)
    Normal   = 2,   // Standaard gebruikersprocessen
    High     = 3,   // Interactieve apps (hebben responsiviteit nodig)
    Realtime = 4,   // Compositor, audio — minimale latency
}

/// Wacht-queue — processen die wachten op een event
pub struct WaitQueue {
    waiters: Mutex<Vec<WaiterEntry>>,
}

pub struct WaiterEntry {
    pub task_id:   u64,
    pub condition: WakeCondition,
    pub deadline:  Option<u64>, // Maximale wachttijd (nanoseconden)
}

pub enum WakeCondition {
    Any,                    // Wakker bij elke wakeup
    DataAvailable,          // Wachten op I/O data
    ChildExited(Option<u64>), // Wachten op specifiek kindproces
    TimerExpired(u64),      // Wachten op tijdstip
    FdReady(i32, FdEvent),  // Wachten op file descriptor event
}

impl WaitQueue {
    /// Slaap tot conditie vervuld of deadline verstreken
    pub fn sleep_until(&self, condition: WakeCondition, timeout_ns: Option<u64>) {
        let deadline = timeout_ns.map(|t| current_time_ns() + t);
        SCHEDULER.block_current_task(self, condition, deadline);
        // Scheduler kiest nu een andere taak
        // Huidige taak hervat hier na wakeup
    }

    /// Maak wachtende taken wakker
    pub fn wake_one(&self) {
        let mut waiters = self.waiters.lock();
        if let Some(entry) = waiters.pop() {
            SCHEDULER.unblock_task(entry.task_id);
        }
    }

    pub fn wake_all(&self) {
        let mut waiters = self.waiters.lock();
        for entry in waiters.drain(..) {
            SCHEDULER.unblock_task(entry.task_id);
        }
    }
}
```

#### 6.2 Starvation Prevention

```rust
// kernel/src/sched/aging.rs

/// Aging mechanisme — verhoog prioriteit van taken die lang wachten
/// Voorkomt dat lage-prioriteit taken nooit aan de beurt komen

pub struct AgingScheduler {
    last_scheduled: BTreeMap<u64, u64>, // task_id → laatste run timestamp
    virtual_runtime: BTreeMap<u64, u64>, // CFS-achtig
}

impl AgingScheduler {
    /// Bereken effectieve prioriteit rekening houdend met wachttijd
    pub fn effective_priority(&self, task: &Task) -> u64 {
        let base = task.priority as u64;
        let waited_ns = current_time_ns()
            .saturating_sub(*self.last_scheduled.get(&task.id).unwrap_or(&0));

        // Elke 100ms wachten verhoogt effectieve prioriteit met 1
        let aging_bonus = waited_ns / 100_000_000;

        base + aging_bonus.min(10) // Max 10 extra prioriteitspunten
    }
}
```

---

## 7. Networking Stack Volwassenheid

**Prioriteit:** 🟡 Hoog — vereist voor browser, mail, updates
**Afhankelijkheden:** PCI discovery (sectie 4), Syscall socket API (sectie 5)
**Noot:** EuroNet heeft al Ethernet/ARP/IPv4/ICMP/UDP. Wat ontbreekt:

#### 7.1 TCP Implementatie

```rust
// crates/euronet/src/tcp/mod.rs
// (Zie Track 4 spec voor volledige TCP state machine)
// Ontbrekende onderdelen specifiek:

// 7.1.1 Nagle algoritme (kleine pakket coalescing)
pub struct TcpSocket {
    // ...bestaande velden...
    nagle_enabled: bool,
    send_buffer_min: usize, // Wacht tot buffer X bytes heeft voor Nagle
}

// 7.1.2 Congestion control (TCP CUBIC of Reno)
pub struct CongestionController {
    cwnd:       u32,  // Congestion window
    ssthresh:   u32,  // Slow start threshold
    algorithm:  CongestionAlgorithm,
}

// 7.1.3 Keepalive
pub struct TcpKeepalive {
    enabled:   bool,
    idle_secs: u32,  // Seconden inactief voor eerste keepalive
    interval:  u32,  // Seconden tussen keepalives
    probes:    u8,   // Aantal probes voor close
}
```

#### 7.2 DHCP Client

```rust
// crates/euronet/src/dhcp.rs

pub struct DhcpClient {
    interface:  &'static dyn NetworkInterface,
    state:      DhcpState,
    xid:        u32,         // Transactie ID
    our_mac:    MacAddr,
    lease:      Option<DhcpLease>,
}

pub struct DhcpLease {
    pub our_ip:       Ipv4Addr,
    pub subnet_mask:  Ipv4Addr,
    pub gateway:      Ipv4Addr,
    pub dns_servers:  Vec<Ipv4Addr>,
    pub lease_secs:   u32,
    pub renewal_secs: u32,
    pub obtained_at:  u64,
}

pub enum DhcpState {
    Init,
    Selecting,   // Na DISCOVER, wacht op OFFER
    Requesting,  // Na OFFER, stuur REQUEST
    Bound,       // Lease verkregen
    Renewing,    // Verlenging bezig
    Rebinding,   // Server niet bereikbaar, probeer anderen
    Expired,     // Lease verlopen — opnieuw beginnen
}

impl DhcpClient {
    pub fn obtain_lease(&mut self) -> Result<&DhcpLease, DhcpError> {
        // 1. Broadcast DHCPDISCOVER
        self.send_discover()?;
        self.state = DhcpState::Selecting;

        // 2. Wacht op DHCPOFFER (timeout: 5 seconden)
        let offer = self.wait_for_offer(5_000_000_000)?;

        // 3. Stuur DHCPREQUEST
        self.send_request(&offer)?;
        self.state = DhcpState::Requesting;

        // 4. Wacht op DHCPACK
        let ack = self.wait_for_ack(5_000_000_000)?;

        // 5. Configureer interface
        self.apply_lease(&ack);
        self.state = DhcpState::Bound;

        Ok(self.lease.as_ref().unwrap())
    }
}
```

#### 7.3 DNS Resolver met Cache

```rust
// crates/euronet/src/dns.rs

pub struct DnsResolver {
    servers:    Vec<Ipv4Addr>,
    cache:      BTreeMap<DnsCacheKey, DnsCacheEntry>,
    use_doh:    bool,          // DNS-over-HTTPS (privacy)
    doh_host:   &'static str,  // Bijv. "dns.quad9.net"
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub struct DnsCacheKey {
    pub name:      String,
    pub record_type: DnsRecordType,
}

pub struct DnsCacheEntry {
    pub addresses:  Vec<Ipv4Addr>,
    pub ttl_secs:   u32,
    pub cached_at:  u64,
}

impl DnsResolver {
    pub fn resolve(&mut self, name: &str) -> Result<Vec<Ipv4Addr>, DnsError> {
        // Check cache
        let key = DnsCacheKey { name: name.to_string(), record_type: DnsRecordType::A };
        if let Some(entry) = self.cache.get(&key) {
            if !entry.is_expired() {
                return Ok(entry.addresses.clone());
            }
            self.cache.remove(&key);
        }

        // DNS query
        let result = if self.use_doh {
            self.resolve_via_doh(name)?
        } else {
            self.resolve_via_udp(name)?
        };

        // Sla op in cache
        self.cache.insert(key, DnsCacheEntry {
            addresses: result.clone(),
            ttl_secs: result.ttl,
            cached_at: current_time_ns(),
        });

        Ok(result.addresses)
    }
}
```

#### 7.4 virtio-net Driver

```rust
// kernel/src/drivers/net/virtio_net.rs

pub struct VirtioNetDriver {
    // VirtIO queue management
    rx_queue: VirtQueue,
    tx_queue: VirtQueue,
    mac:      MacAddr,
    // Statistieken
    rx_packets: u64,
    tx_packets: u64,
    rx_bytes:   u64,
    tx_bytes:   u64,
    rx_errors:  u64,
}

impl NetworkInterface for VirtioNetDriver {
    fn mac_address(&self) -> MacAddr { self.mac }

    fn transmit(&mut self, frame: &[u8]) -> NicResult<()> {
        // Voeg VirtIO netwerk header toe
        let header = VirtioNetHeader {
            flags: 0,
            gso_type: VIRTIO_NET_HDR_GSO_NONE,
            hdr_len: 0,
            gso_size: 0,
            csum_start: 0,
            csum_offset: 0,
        };

        // Zet descriptor in TX ring
        self.tx_queue.add_buffer_chain(&[
            BufferDesc::readable(&header as *const _ as *const u8,
                                 core::mem::size_of::<VirtioNetHeader>()),
            BufferDesc::readable(frame.as_ptr(), frame.len()),
        ])?;

        // Notificeer device
        self.tx_queue.notify();
        self.tx_packets += 1;
        self.tx_bytes += frame.len() as u64;
        Ok(())
    }

    fn receive(&mut self) -> NicResult<Option<Vec<u8>>> {
        if let Some(used) = self.rx_queue.pop_used() {
            let frame_start = core::mem::size_of::<VirtioNetHeader>();
            let frame = used.data[frame_start..].to_vec();
            self.rx_packets += 1;
            self.rx_bytes += frame.len() as u64;
            // Zet buffer terug in RX ring voor volgend frame
            self.rx_queue.refill_buffer(used.desc_idx);
            Ok(Some(frame))
        } else {
            Ok(None)
        }
    }
}
```

---

## 8. Audio Subsysteem

**Prioriteit:** 🟢 Medium — vereist voor beta
**Afhankelijkheden:** PCI discovery, virtio-sound of Intel HDA driver

### Wat ontbreekt

Volledig ontbrekend. Audio is essentieel voor een desktop OS.

#### 8.1 Audio Architectuur

```rust
// kernel/src/audio/mod.rs

/// Audio stack architectuur:
/// App → EuroAudio API → Mixer → Audio Server → Hardware Driver
///
/// Mixer: meerdere apps kunnen tegelijk audio afspelen
/// Server: in userspace (veiligheid) maar met kernel support
/// Driver: Intel HDA of virtio-sound

pub trait AudioDriver: Send + Sync {
    fn name(&self) -> &str;
    fn sample_rate(&self) -> u32;    // Bijv. 44100 of 48000 Hz
    fn channels(&self) -> u8;        // 2 = stereo
    fn bit_depth(&self) -> u8;       // 16 of 32 bit
    fn play_buffer(&mut self, samples: &[i16]) -> AudioResult<()>;
    fn set_volume(&mut self, volume: f32); // 0.0 tot 1.0
    fn mute(&mut self, muted: bool);
}

/// virtio-sound driver — werkt in QEMU
pub struct VirtioSoundDriver {
    tx_queue:     VirtQueue,
    ctrl_queue:   VirtQueue,
    sample_rate:  u32,
    channels:     u8,
}

/// Intel HDA driver — werkt op echte hardware
pub struct IntelHdaDriver {
    mmio_base:    u64,
    corb:         CorbRing,    // Command Outbound Ring Buffer
    rirb:         RirbRing,    // Response Inbound Ring Buffer
    streams:      [HdaStream; 8],
}
```

#### 8.2 Software Mixer

```rust
// kernel/src/audio/mixer.rs

/// Eenvoudige software mixer — meerdere streams mengen
pub struct AudioMixer {
    streams:     Vec<AudioStream>,
    output_buf:  Vec<i32>,     // i32 voor mixing (voorkomt clipping)
    sample_rate: u32,
    channels:    u8,
}

pub struct AudioStream {
    pub client_id: u64,
    pub volume:    f32,         // Per-stream volume
    pub buffer:    RingBuffer<i16>,
    pub priority:  StreamPriority,
}

pub enum StreamPriority {
    System,       // Systeemsounds (hoogste prioriteit)
    Interactive,  // Muziek, video
    Background,   // Notificatiesounds
}

impl AudioMixer {
    /// Meng alle actieve streams en schrijf naar hardware
    pub fn mix_and_output(&mut self, driver: &mut dyn AudioDriver) {
        let frame_size = 1024usize; // Samples per frame

        // Reset output buffer
        self.output_buf.iter_mut().for_each(|s| *s = 0);

        // Meng alle streams
        for stream in &mut self.streams {
            if let Some(samples) = stream.buffer.read_n(frame_size) {
                for (i, &sample) in samples.iter().enumerate() {
                    let scaled = (sample as f32 * stream.volume) as i32;
                    self.output_buf[i] = self.output_buf[i].saturating_add(scaled);
                }
            }
        }

        // Clamp naar i16 bereik en converteer
        let output: Vec<i16> = self.output_buf.iter()
            .map(|&s| s.clamp(i16::MIN as i32, i16::MAX as i32) as i16)
            .collect();

        driver.play_buffer(&output).ok();
    }
}
```

---

## 9. USB Stack

**Prioriteit:** 🟢 Medium — vereist voor v1.0
**Afhankelijkheden:** PCI discovery (XHCI controller via PCI)

### Wat ontbreekt

USB is nodig voor toetsenbord/muis op moderne hardware (PS/2 verdwijnt),
USB storage, webcams, printers en andere randapparaten.

#### 9.1 XHCI Host Controller

```rust
// kernel/src/drivers/usb/xhci.rs

/// xHCI = eXtensible Host Controller Interface
/// Standaard USB 3.x controller — ook backward-compatible met USB 2.0/1.1

pub struct XhciController {
    mmio_base:     u64,          // Via PCI BAR0
    cap_regs:      &'static XhciCapRegs,
    op_regs:       &'static mut XhciOpRegs,
    port_count:    u8,
    slots:         [Option<UsbDevice>; 255],
    command_ring:  CommandRing,
    event_ring:    EventRing,
}

/// USB apparaat nadat het geënumereerd is
pub struct UsbDevice {
    pub slot_id:    u8,
    pub vendor_id:  u16,
    pub product_id: u16,
    pub class:      UsbClass,
    pub speed:      UsbSpeed,
    pub endpoints:  Vec<UsbEndpoint>,
}

pub enum UsbClass {
    Hid,           // Human Interface Device (toetsenbord, muis, gamepad)
    MassStorage,   // USB schijven, flash drives
    Audio,         // USB headsets, microfoons
    Video,         // Webcams
    Printer,
    Hub,
    Cdc,           // Communication Device Class (serieel, netwerk)
    Unknown(u8),
}

pub enum UsbSpeed {
    Low,       // 1.5 Mbps (USB 1.0)
    Full,      // 12 Mbps (USB 1.1)
    High,      // 480 Mbps (USB 2.0)
    Super,     // 5 Gbps (USB 3.0)
    SuperPlus, // 10/20 Gbps (USB 3.1/3.2)
}
```

#### 9.2 USB HID Driver (Toetsenbord & Muis)

```rust
// kernel/src/drivers/usb/hid.rs

pub struct UsbHidDriver {
    device:      UsbDevice,
    descriptor:  HidDescriptor,
    report_desc: Vec<u8>,        // Beschrijft het rapport formaat
    input_ep:    UsbEndpoint,    // Interrupt endpoint voor input
}

/// USB HID report voor een standaard toetsenbord
#[repr(C)]
pub struct KeyboardReport {
    pub modifier: u8,            // Ctrl/Alt/Shift/etc.
    pub reserved: u8,
    pub keys: [u8; 6],           // Tot 6 gelijktijdige toetsen (NKRO)
}

/// USB HID report voor een standaard muis
#[repr(C)]
pub struct MouseReport {
    pub buttons: u8,             // Bits 0-7: links/rechts/midden/...
    pub x: i8,                   // Relatieve X beweging
    pub y: i8,                   // Relatieve Y beweging
    pub wheel: i8,               // Scrollwiel
}

impl UsbHidDriver {
    pub fn poll_input(&mut self) -> Option<HidEvent> {
        // Lees interrupt endpoint
        if let Some(data) = self.input_ep.read_interrupt() {
            Some(self.parse_report(&data))
        } else {
            None
        }
    }

    fn parse_report(&self, data: &[u8]) -> HidEvent {
        // Gebruik HID report descriptor om data te interpreteren
        // Eenvoudige implementatie: hard-code standaard toetsenbord/muis
        match self.descriptor.usage_page {
            0x01 if self.descriptor.usage == 0x06 => {
                // Generic Desktop > Keyboard
                let report = unsafe { &*(data.as_ptr() as *const KeyboardReport) };
                HidEvent::Keyboard(self.decode_keyboard(report))
            }
            0x01 if self.descriptor.usage == 0x02 => {
                // Generic Desktop > Mouse
                let report = unsafe { &*(data.as_ptr() as *const MouseReport) };
                HidEvent::Mouse(self.decode_mouse(report))
            }
            _ => HidEvent::Unknown(data.to_vec()),
        }
    }
}
```

---

## 10. Storage Betrouwbaarheid

**Prioriteit:** 🟡 Hoog — vereist voor stabiele alpha
**Afhankelijkheden:** EuroFS (al aanwezig), VFS (Run 5)

### Wat ontbreekt

EuroFS heeft al CoW en checksums. Wat nog ontbreekt voor productie:

#### 10.1 EuroFS Checker (fsck equivalent)

```rust
// crates/eurofs/src/checker.rs

/// EuroFS integriteitscontrole — uitvoeren bij mount na ongepland afsluiten
pub struct FsChecker {
    device: Box<dyn BlockDevice>,
    superblock: EuroFsSuperblock,
    errors: Vec<FsError>,
    repaired: Vec<FsRepair>,
}

pub enum FsError {
    OrphanedInode { oid: u64 },
    BadChecksum { block: u64, expected: u64, found: u64 },
    CircularDirectory { path: String },
    InvalidExtent { oid: u64, start: u64, count: u32 },
    LeakedBlocks { count: u64 },
    DuplicateBlock { block: u64, oid1: u64, oid2: u64 },
    CorruptedBTree { node: u64 },
}

impl FsChecker {
    pub fn check(&mut self) -> CheckResult {
        self.check_superblock();
        self.check_btree_integrity();
        self.check_all_inodes();
        self.check_all_extents();
        self.check_free_space_bitmap();
        self.cross_check_references();

        CheckResult {
            errors: self.errors.clone(),
            repaired: self.repaired.clone(),
            is_clean: self.errors.is_empty(),
        }
    }

    fn check_superblock(&mut self) {
        let computed = self.superblock.compute_checksum();
        if computed != self.superblock.checksum {
            self.errors.push(FsError::BadChecksum {
                block: 1,
                expected: self.superblock.checksum,
                found: computed,
            });
            // Probeer backup superblok
            if let Ok(backup) = self.read_backup_superblock() {
                self.superblock = backup;
                self.repaired.push(FsRepair::SuperblockRestored);
            }
        }
    }
}
```

#### 10.2 Disk Cache & Write-back

```rust
// kernel/src/storage/cache.rs

/// Block cache — houdt veelgebruikte blokken in RAM
/// Write-back: writes gaan eerst naar cache, dan naar schijf (in batch)

pub struct BlockCache {
    entries:   BTreeMap<(DeviceId, u64), CacheEntry>,
    lru:       VecDeque<(DeviceId, u64)>,
    max_entries: usize,
    dirty_count: usize,
}

pub struct CacheEntry {
    pub data:      Box<[u8; 4096]>,
    pub dirty:     bool,           // Gewijzigd maar nog niet naar schijf
    pub pinned:    bool,           // Mag niet verdrongen worden
    pub last_used: u64,
    pub write_count: u32,
}

impl BlockCache {
    /// Lees een blok — vanuit cache of schijf
    pub fn read(&mut self, device: DeviceId, block: u64) -> Result<&[u8], IoError> {
        if let Some(entry) = self.entries.get(&(device, block)) {
            self.lru_touch(device, block);
            return Ok(&entry.data[..]);
        }

        // Cache miss — lees van schijf
        let mut data = Box::new([0u8; 4096]);
        device.read_block(block, &mut *data)?;

        self.insert(device, block, data, false);
        Ok(&self.entries[&(device, block)].data[..])
    }

    /// Schrijf naar cache — mark dirty
    pub fn write(&mut self, device: DeviceId, block: u64, data: &[u8]) -> Result<(), IoError> {
        let entry = self.entries.entry((device, block)).or_insert_with(|| {
            CacheEntry::new()
        });
        entry.data[..data.len()].copy_from_slice(data);
        entry.dirty = true;
        self.dirty_count += 1;

        // Flush als te veel dirty entries
        if self.dirty_count > self.max_entries / 4 {
            self.flush_lru_dirty()?;
        }

        Ok(())
    }

    /// Schrijf alle dirty entries naar schijf
    pub fn sync(&mut self) -> Result<(), IoError> {
        for ((device, block), entry) in self.entries.iter_mut() {
            if entry.dirty {
                device.write_block(*block, &entry.data[..])?;
                entry.dirty = false;
            }
        }
        self.dirty_count = 0;
        Ok(())
    }
}
```

#### 10.3 NVMe Driver

```rust
// kernel/src/drivers/storage/nvme.rs

/// NVMe is de standaard interface voor moderne SSD's
/// Sneller dan AHCI/SATA, vereist PCIe

pub struct NvmeController {
    mmio_base:      u64,           // Via PCI BAR0
    admin_queue:    NvmeQueue,     // Admin commando's
    io_queues:      Vec<NvmeQueue>,// I/O queues (één per CPU core voor SMP)
    namespace_count: u32,
    namespaces:     Vec<NvmeNamespace>,
}

pub struct NvmeNamespace {
    pub id:           u32,
    pub sector_count: u64,
    pub sector_size:  u32,   // Typisch 512 of 4096 bytes
    pub model:        [u8; 40],
    pub serial:       [u8; 20],
}

impl NvmeController {
    pub fn read_sectors(&mut self, ns_id: u32, lba: u64, count: u32, buf: &mut [u8])
        -> Result<(), NvmeError>
    {
        let cmd = NvmeReadCmd {
            opcode: NvmeOpcode::Read,
            nsid: ns_id,
            slba: lba,           // Starting LBA
            nlb: count - 1,      // Number of Logical Blocks (0-based)
            prp1: buf.as_ptr() as u64, // Physical Region Page (DMA adres)
            prp2: 0,
            ..Default::default()
        };

        self.submit_io_command(cmd)?;
        self.wait_completion()
    }
}

impl BlockDevice for NvmeController {
    fn block_size(&self) -> u32 { self.namespaces[0].sector_size }
    fn block_count(&self) -> u64 { self.namespaces[0].sector_count }

    fn read_blocks(&self, start: u64, count: u32, buf: &mut [u8]) -> BlockResult<()> {
        self.read_sectors(1, start, count, buf)
            .map_err(|_| BlockError::IoError)
    }

    fn write_blocks(&mut self, start: u64, count: u32, buf: &[u8]) -> BlockResult<()> {
        self.write_sectors(1, start, count, buf)
            .map_err(|_| BlockError::IoError)
    }

    fn flush(&mut self) -> BlockResult<()> {
        self.flush_cache().map_err(|_| BlockError::IoError)
    }
}
```

---

## 11. System Services & Init

**Prioriteit:** 🟡 Hoog — vereist voor userspace stabiel draaien
**Afhankelijkheden:** Process model (Run 4), IPC

### Wat ontbreekt

Wanneer de kernel klaar is met initialiseren, start het het eerste userspace
proces. Op Linux is dat systemd/init. EuroOS heeft hier niets voor.

#### 11.1 EuroInit — Init Systeem

```rust
// userland/euroinit/src/main.rs
// Het eerste userspace proces — PID 1

fn main() {
    // 1. Stel fundamentele omgeving in
    setup_environment();

    // 2. Mount essentiële pseudo-filesystemen
    mount_proc_fs();    // /proc — proces informatie
    mount_sys_fs();     // /sys  — sysfs (hardware info)
    mount_dev_fs();     // /dev  — device bestanden
    mount_tmp_fs();     // /tmp  — tijdelijke bestanden (RAM)

    // 3. Start systeem services in volgorde
    start_service("euroguard-daemon");   // Permissies & netwerk
    start_service("euronet-daemon");     // Netwerkconfiguratie
    start_service("eurolog-daemon");     // Logging
    start_service("eurocrypto-daemon");  // Encryptie services
    start_service("eurostore-daemon");   // Package manager daemon

    // 4. Start login manager / desktop
    start_service("eurologin");

    // 5. Adopteer weesprocessen (PID 1 verantwoordelijkheid)
    loop {
        // Wacht op kindproces exits (SIGCHLD)
        let status = wait_any();
        handle_child_exit(status);

        // Herstart gecrashe services indien nodig
        check_service_health();
    }
}

fn start_service(name: &str) -> Result<u64, ServiceError> {
    let config = ServiceConfig::load(&format!("/etc/services/{}.toml", name))?;

    let pid = fork_exec(&config.executable, &config.args, &config.env)?;

    klog!(LogLevel::Info, "init",
        &alloc::format!("Service '{}' gestart (PID {})", name, pid));

    SERVICE_TABLE.insert(name.to_string(), ServiceEntry {
        pid,
        config,
        restarts: 0,
        started_at: current_time(),
    });

    Ok(pid)
}
```

#### 11.2 Service Definitie Formaat

```toml
# /etc/services/euronet-daemon.toml

[service]
name = "EuroNet Daemon"
description = "Netwerkconfiguratie en DHCP"
executable = "/usr/sbin/euronetd"
args = ["--config", "/etc/euronet.toml"]

[lifecycle]
restart_on_crash = true
restart_delay_secs = 2
max_restarts = 5
shutdown_timeout_secs = 10

[dependencies]
after = ["euroguard-daemon"]  # Start pas nadat deze services draaien
before = []

[security]
user = "network"              # Draai als niet-root gebruiker
capabilities = ["net.admin", "net.bind"]
sandbox = true
read_only_root = true
```

#### 11.3 EuroLog — Logging Daemon

```rust
// userland/eurologd/src/main.rs

/// Centraliseer alle kernel en service logs
/// Sla op in gestructureerd formaat
/// Roteer automatisch

pub struct LogDaemon {
    sources:    Vec<LogSource>,
    writers:    Vec<Box<dyn LogWriter>>,
    filters:    Vec<LogFilter>,
}

pub enum LogSource {
    KernelRingBuffer,           // Kernel log entries
    UnixSocket(&'static str),   // Services loggen via socket
    SerialPort(u8),             // COM poort (voor debug)
}

pub trait LogWriter {
    fn write(&mut self, entry: &LogEntry) -> io::Result<()>;
}

pub struct FileLogWriter {
    path:     PathBuf,
    file:     File,
    max_size: u64,
    rotations: u8,
}

pub struct JsonlLogWriter {
    // Structured logging in JSONL formaat voor SIEM integratie
    file: File,
}

impl LogWriter for JsonlLogWriter {
    fn write(&mut self, entry: &LogEntry) -> io::Result<()> {
        let json = serde_json::json!({
            "timestamp": entry.timestamp,
            "level": format!("{:?}", entry.level),
            "module": entry.module,
            "message": entry.message,
            "cpu": entry.cpu_id,
            "pid": entry.pid,
        });
        writeln!(self.file, "{}", json)
    }
}
```

---

## 12. Update & Recovery Systeem

**Prioriteit:** 🟢 Medium — vereist voor v1.0
**Afhankelijkheden:** EuroFS snapshots, eupkg, signed binaries

### Wat ontbreekt

Geen enkel OS is compleet zonder een veilig update- en recoverymechanisme.

#### 12.1 A/B Update Systeem

```
Schijfindeling voor A/B updates:

Partitie 1: EFI (FAT32, 512MB)
  EFI/BOOT/BOOTX64.EFI  ← EuroBoot bootloader

Partitie 2: EuroOS-A (EuroFS, hoofd-OS)
  /boot/kernel.efi
  /boot/kernel.hash      ← SHA256 van kernel
  /boot/kernel.sig       ← Ed25519 handtekening
  /usr/                  ← Systeem bestanden
  /etc/                  ← Configuratie

Partitie 3: EuroOS-B (EuroFS, backup/update slot)
  (Zelfde structuur — update wordt hier geïnstalleerd)

Partitie 4: Gebruikersdata (EuroFS)
  /home/                 ← Nooit overschreven bij update
  /var/                  ← Variabele data

Bootloader kiest actief slot via markering in EFI variabelen
Na succesvolle boot van nieuwe versie: markeer B als actief
Bij boot-fout: automatisch terug naar A (rollback)
```

```rust
// userland/euroupdate/src/main.rs

pub struct UpdateManager {
    current_slot: Slot,    // A of B
    active_slot:  Slot,
}

pub enum Slot { A, B }

pub struct UpdatePackage {
    pub version:   String,
    pub kernel:    Vec<u8>,         // Kernel binary
    pub rootfs:    Vec<u8>,         // Nieuw rootfs image
    pub signature: [u8; 64],       // Ed25519 handtekening van Anthropic... van EuroOS team
    pub hash:      [u8; 32],        // SHA256 van rootfs
}

impl UpdateManager {
    pub fn apply_update(&mut self, pkg: UpdatePackage) -> Result<(), UpdateError> {
        // 1. Verifieer handtekening
        let pubkey = EUROKERNEL_UPDATE_PUBKEY;
        if !verify_ed25519(&pkg.signature, &pkg.hash, pubkey) {
            return Err(UpdateError::InvalidSignature);
        }

        // 2. Verifieer hash
        let computed_hash = sha256(&pkg.rootfs);
        if computed_hash != pkg.hash {
            return Err(UpdateError::HashMismatch);
        }

        // 3. Schrijf naar inactief slot
        let target_slot = self.inactive_slot();
        self.write_to_slot(target_slot, &pkg)?;

        // 4. Markeer nieuw slot als "te proberen"
        self.mark_slot_pending(target_slot);

        // 5. Herstart naar nieuw slot
        klog!(LogLevel::Info, "update",
            &alloc::format!("Update {} geïnstalleerd — herstart vereist", pkg.version));

        // Gebruiker bevestigt herstart
        Ok(())
    }

    pub fn confirm_boot(&mut self) {
        // Geroepen na succesvolle boot van nieuw slot
        // Markeert het slot als "goed" — geen rollback meer
        self.mark_slot_good(self.active_slot);
    }

    pub fn rollback(&mut self) {
        // Keer terug naar vorig slot
        let previous = self.inactive_slot();
        self.set_active_slot(previous);
    }
}
```

#### 12.2 Recovery Modus

```rust
// kernel/src/recovery.rs

/// Detecteer of recovery modus vereist is
/// Boot in recovery als:
/// - Gebruiker houdt toets ingedrukt bij boot
/// - Vorige boot mislukte (boot-counter mechaisme)
/// - Kernel detecteert ernstige corruptie

pub fn check_recovery_needed() -> bool {
    // Check boot counter in EFI variabelen
    let boot_count = efi_get_variable("EuroBootAttempt").unwrap_or(0);
    if boot_count >= 3 {
        kinfo!("recovery", "Drie opeenvolgende boot-fouten — recovery modus");
        return true;
    }

    // Check recovery-toets (bijv. F12 of R ingedrukt bij UEFI)
    if efi_get_variable("EuroRecoveryRequested").unwrap_or(false) {
        kinfo!("recovery", "Recovery modus handmatig gevraagd");
        return true;
    }

    false
}

/// Recovery shell — minimale omgeving voor reparatie
pub fn enter_recovery_mode() -> ! {
    kinfo!("recovery", "Recovery modus gestart");

    // Start minimale shell zonder grafische desktop
    // Beschikbare commando's:
    // - eurocheck: EuroFS integriteitscontrole
    // - eurorestore: herstel vanuit snapshot
    // - euromount: mount een partitie handmatig
    // - euroreset: fabrieksinstellingen (met bevestiging)
    // - eurorollback: rollback naar vorig OS slot

    minimal_recovery_shell();
}
```

---

## 13. User & Session Model

**Prioriteit:** 🟡 Hoog — vereist voor multi-user ondersteuning
**Afhankelijkheden:** Syscall setuid/getuid, EuroFS permissies

### Wat ontbreekt

EuroOS heeft al een login scherm in de UI. De kernel-ondersteuning
voor echte gebruikersisolatie ontbreekt nog.

#### 13.1 Gebruikersdatabase

```toml
# /etc/users.toml — Versleuteld op EuroFS

[[user]]
uid = 0
name = "root"
home = "/root"
shell = "/bin/euroshell"
# root heeft geen wachtwoord — toegang via sudo-equivalent

[[user]]
uid = 1000
name = "euro"
display_name = "Euro User"
home = "/home/euro"
shell = "/bin/euroshell"
groups = ["users", "admin", "audio", "video"]
# Wachtwoord hash opgeslagen in /etc/shadow.toml (Argon2id)

[[user]]
uid = 1001
name = "marie"
display_name = "Marie"
home = "/home/marie"
shell = "/bin/euroshell"
groups = ["users"]

# Systeem gebruikers (geen login mogelijk)
[[user]]
uid = 100
name = "network"
system = true
home = "/var/lib/network"
shell = "/dev/null"
```

#### 13.2 Sessie Management

```rust
// userland/eurosession/src/main.rs

pub struct SessionManager {
    sessions: BTreeMap<SessionId, Session>,
}

pub struct Session {
    pub id:       SessionId,
    pub user_id:  u32,
    pub display:  u8,          // Welk scherm (0 = eerste)
    pub started:  u64,
    pub state:    SessionState,
    pub locked:   bool,
}

pub enum SessionState {
    Active,       // Gebruiker is aangemeld en actief
    Idle,         // Geen input voor X minuten
    Locked,       // Schermvergrendeling actief
    Switching,    // Gebruikerswissel bezig
}

impl SessionManager {
    /// Vergrendel scherm na inactiviteit
    pub fn lock_session(&mut self, session_id: SessionId) {
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.locked = true;
            session.state = SessionState::Locked;
            // Stuur lock-event naar desktop
            EUROIPC.send_to_desktop(session_id, DesktopEvent::LockScreen);
        }
    }

    /// Schakel naar andere gebruiker (zonder uitloggen)
    pub fn switch_to_user(&mut self, target_uid: u32) -> Result<SessionId, SessionError> {
        // Vergrendel huidige sessie
        let current = self.active_session()?;
        self.lock_session(current.id);

        // Zoek of maak een sessie voor de doelgebruiker
        if let Some(existing) = self.session_for_user(target_uid) {
            self.activate_session(existing.id)
        } else {
            self.create_new_session(target_uid)
        }
    }
}
```

---

## 14. Display Stack Volledigheid

**Prioriteit:** 🟡 Hoog — voor goede gebruikerservaring
**Afhankelijkheden:** EuroDesktop compositor (al aanwezig)

### Wat ontbreekt

#### 14.1 Klembord

```rust
// kernel/src/desktop/clipboard.rs

/// Systeem-breed klembord — beveiligd via EuroGuard
pub struct Clipboard {
    content:    Option<ClipboardContent>,
    owner:      Option<u64>,    // PID van app die klembord bezit
    history:    VecDeque<ClipboardContent>, // Optioneel: klembordgeschiedenis
}

pub enum ClipboardContent {
    Text(String),
    Html(String),
    Image(Vec<u8>, ImageFormat),
    Files(Vec<PathBuf>),
    Custom { mime_type: String, data: Vec<u8> },
}

impl Clipboard {
    pub fn set(&mut self, content: ClipboardContent, owner_pid: u64) {
        // EuroGuard check: mag deze app naar klembord schrijven?
        if !EUROGUARD.check_clipboard_write(owner_pid) {
            return;
        }
        self.owner = Some(owner_pid);
        self.content = Some(content);
    }

    pub fn get(&self, requester_pid: u64) -> Option<&ClipboardContent> {
        // EuroGuard check: mag deze app klembord lezen?
        if !EUROGUARD.check_clipboard_read(requester_pid) {
            // Notificeer gebruiker: app X probeert klembord te lezen
            EUROGUARD.notify_clipboard_attempt(requester_pid);
            return None;
        }
        self.content.as_ref()
    }
}
```

#### 14.2 Multi-Monitor

```rust
// kernel/src/compositor/multimonitor.rs

pub struct MonitorManager {
    monitors:   Vec<Monitor>,
    primary:    usize,         // Index van primaire monitor
    layout:     MonitorLayout,
}

pub struct Monitor {
    pub id:         MonitorId,
    pub name:       String,    // Bijv. "eDP-1", "HDMI-1"
    pub width:      u32,
    pub height:     u32,
    pub refresh_hz: u32,
    pub dpi:        f32,
    pub position:   (i32, i32), // Positie in virtuele desktop
    pub rotation:   Rotation,
    pub connected:  bool,
}

pub enum MonitorLayout {
    Mirror,           // Zelfde beeld op alle monitors
    Extend(Direction),// Uitbreid naar links/rechts/boven/onder
    Independent,      // Elk scherm onafhankelijk
}

impl MonitorManager {
    /// Herken nieuwe monitor (hot-plug via ACPI event)
    pub fn handle_hotplug(&mut self, monitor_id: MonitorId, connected: bool) {
        if connected {
            let monitor = self.detect_monitor(monitor_id);
            self.monitors.push(monitor);
            // Notificeer desktop — toon monitor-instellingen
            DESKTOP.notify_monitor_connected(monitor_id);
        } else {
            self.monitors.retain(|m| m.id != monitor_id);
            DESKTOP.notify_monitor_disconnected(monitor_id);
        }
        self.recalculate_layout();
    }
}
```

#### 14.3 Drag & Drop

```rust
// kernel/src/desktop/dragdrop.rs

pub struct DragDropManager {
    active_drag: Option<DragOperation>,
}

pub struct DragOperation {
    pub source_window: WindowId,
    pub source_pid:    u64,
    pub content:       DragContent,
    pub current_pos:   (f32, f32),
    pub allowed_ops:   DragActions,
}

pub enum DragContent {
    Text(String),
    Files(Vec<PathBuf>),
    Image(Vec<u8>),
    Custom { mime: String, data: Vec<u8> },
}

pub struct DragActions {
    pub copy:  bool,
    pub move_: bool,
    pub link:  bool,
}

impl DragDropManager {
    pub fn begin_drag(&mut self, source: WindowId, content: DragContent) {
        self.active_drag = Some(DragOperation {
            source_window: source,
            source_pid:    DESKTOP.pid_for_window(source),
            content,
            current_pos:   CURSOR.position(),
            allowed_ops:   DragActions::all(),
        });
    }

    pub fn handle_drop(&mut self, target: WindowId) {
        if let Some(drag) = self.active_drag.take() {
            // EuroGuard check: mag source naar target droppen?
            // (bijv. bestanden vanuit beveiligde map)
            if EUROGUARD.check_drag_drop(&drag, target) {
                DESKTOP.deliver_drop(target, drag);
            }
        }
    }
}
```

---

## 15. Printer & Scanner

**Prioriteit:** 🔵 Later — v1.0+
**Afhankelijkheden:** USB stack (sectie 9), Netwerk (sectie 7)

### 15.1 Print Architectuur

```rust
// userland/europrint/src/main.rs

/// EuroPrint — print spooler en driver manager
/// CUPS-geïnspireerd maar eigen implementatie

pub struct PrintSpooler {
    printers:  Vec<Printer>,
    jobs:      VecDeque<PrintJob>,
}

pub struct Printer {
    pub id:       PrinterId,
    pub name:     String,
    pub uri:      PrinterUri,   // usb://... of ipp://... of lpd://...
    pub driver:   Box<dyn PrintDriver>,
    pub status:   PrinterStatus,
}

pub enum PrinterUri {
    Usb { vendor: u16, product: u16 },
    Ipp(String),       // Internet Printing Protocol — netwerk printers
    Lpd(String),       // Line Printer Daemon — legacy
    Pdf,               // Afdrukken naar PDF
}

pub trait PrintDriver: Send + Sync {
    fn render_page(&self, page: &DocumentPage, settings: &PrintSettings) -> Vec<u8>;
    fn supported_formats(&self) -> Vec<PrintFormat>;
}
```

---

## 16. Hardware Abstraction Layer Uitbreidingen

**Prioriteit:** 🟡 Hoog — diverse kleinere onderdelen
**Afhankelijkheden:** PCI discovery, ACPI

### 16.1 RTC (Real Time Clock)

```rust
// kernel/src/drivers/rtc.rs
// Al gepland in Run 6 — implementatiedetails

pub struct RtcDriver;

impl RtcDriver {
    /// Lees huidige datum en tijd van hardware RTC
    pub fn read_datetime(&self) -> DateTime {
        // Lees via CMOS registers (I/O poorten 0x70/0x71)
        // Wacht op update-in-progress bit te clearen
        while self.read_cmos(0x0A) & 0x80 != 0 {}

        let seconds = self.bcd_to_bin(self.read_cmos(0x00));
        let minutes = self.bcd_to_bin(self.read_cmos(0x02));
        let hours   = self.bcd_to_bin(self.read_cmos(0x04));
        let day     = self.bcd_to_bin(self.read_cmos(0x07));
        let month   = self.bcd_to_bin(self.read_cmos(0x08));
        let year    = self.bcd_to_bin(self.read_cmos(0x09)) as u32 + 2000;

        DateTime { year, month, day, hours, minutes, seconds }
    }

    fn read_cmos(&self, reg: u8) -> u8 {
        unsafe {
            x86_out8(0x70, reg);
            x86_in8(0x71)
        }
    }

    fn bcd_to_bin(&self, bcd: u8) -> u8 {
        (bcd >> 4) * 10 + (bcd & 0x0F)
    }
}
```

### 16.2 HPET (High Precision Event Timer)

```rust
// kernel/src/timer/hpet.rs

/// HPET — nauwkeuriger dan PIT voor timing
/// Typisch resolutie: ~100 nanoseconden

pub struct HpetTimer {
    mmio_base:    u64,      // Van ACPI HPET tabel
    tick_period:  u64,      // Femtoseconden per tick
    max_timers:   u8,
}

impl HpetTimer {
    pub fn current_nanoseconds(&self) -> u64 {
        let counter = self.read_main_counter();
        // counter * tick_period / 1_000_000 (fs → ns)
        counter * self.tick_period / 1_000_000
    }

    pub fn schedule_interrupt(&mut self, timer: u8, ns_from_now: u64) {
        let ticks = (ns_from_now * 1_000_000) / self.tick_period;
        let current = self.read_main_counter();
        self.write_comparator(timer, current + ticks);
        self.enable_timer_interrupt(timer);
    }
}
```

### 16.3 Temperatuur & Hardware Monitoring

```rust
// kernel/src/drivers/hwmon.rs

/// Hardware monitoring — temperatuur, ventilatorsnelheden, spanning
/// Via ACPI thermal zones of directe chip toegang (it87, nct6775, etc.)

pub struct HardwareMonitor {
    sensors: Vec<Box<dyn Sensor>>,
}

pub trait Sensor: Send + Sync {
    fn name(&self) -> &str;
    fn sensor_type(&self) -> SensorType;
    fn read(&self) -> SensorValue;
}

pub enum SensorType {
    Temperature,     // Graden Celsius
    FanSpeed,        // RPM
    Voltage,         // Millivolt
    Power,           // Milliwatt
    Current,         // Milliampere
}

pub enum SensorValue {
    Temperature(f32),   // °C
    FanSpeed(u32),      // RPM
    Voltage(u32),       // mV
    Unavailable,
}

/// ACPI Thermal Zone — standaard methode voor CPU temperatuur
pub struct AcpiThermalSensor {
    zone_path: AcpiPath,   // Bijv. "\_TZ.TZ00"
}

impl Sensor for AcpiThermalSensor {
    fn read(&self) -> SensorValue {
        // Evalueer ACPI _TMP methode
        let raw = acpi_eval_integer(&self.zone_path, "_TMP").unwrap_or(0);
        // ACPI geeft temperatuur in decikelvin (2732 = 0°C)
        let celsius = (raw as f32 - 2732.0) / 10.0;
        SensorValue::Temperature(celsius)
    }
}
```

---

## Prioriteitenmatrix — Aanbevolen Volgorde

```
ONMIDDELLIJK (parallel aan Run 1-3):
  ✓ Sectie 1:  Kernel Observability      — multiplier op alles
  ✓ Sectie 2:  Memory Protection         — guard pages, SMEP/SMAP, NX

VOOR ALPHA (Run 4-6):
  ✓ Sectie 3:  ACPI & Energiebeheer      — afsluiten/herstarten
  ✓ Sectie 4:  PCI Discovery             — hardware vinden
  ✓ Sectie 5:  Volledige Syscall-laag    — POSIX basis
  ✓ Sectie 6:  Scheduler Volwassenheid   — blocking I/O, priorities
  ✓ Sectie 7:  Networking Volwassenheid  — TCP, DHCP, DNS
  ✓ Sectie 10: Storage Betrouwbaarheid   — fsck, NVMe, disk cache
  ✓ Sectie 11: System Services & Init    — euroinit, eurologd
  ✓ Sectie 13: User & Session Model      — multi-user isolatie

VOOR BETA:
  ✓ Sectie 8:  Audio                     — muziek, systeemgeluiden
  ✓ Sectie 9:  USB Stack                 — moderne toetsenbord/muis
  ✓ Sectie 12: Update & Recovery         — A/B updates, rollback
  ✓ Sectie 14: Display Stack             — klembord, multi-monitor, DnD
  ✓ Sectie 16: HAL Uitbreidingen         — RTC, HPET, temperatuur

VOOR V1.0+:
  ✓ Sectie 15: Printer & Scanner         — enterprise gebruik
```

---

## Sleutelinzicht: Wat Eerst

Van alles hierboven zijn **drie onderdelen het meest urgent** omdat ze
de rest blokkeren of veiliger maken:

1. **Kernel Observability** (sectie 1) — Doe dit als eerste.
   Zonder goede logging en panic-dumps wordt alles daarna moeilijker debuggen.

2. **ACPI + PCI** (sectie 3+4) — Doe dit na Run 2.
   Zonder ACPI kan je niet afsluiten. Zonder PCI kan je geen hardware vinden.
   Beide samen ontgrendelen netwerk, storage en alle drivers.

3. **Volledige Syscall-laag** (sectie 5) — Doe dit tijdens Run 4.
   POSIX semantiek correct van bij het begin bespaart herschrijf-werk later.
   Elke app die je ooit port verwacht deze syscalls.
