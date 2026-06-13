//! Minimale shell die commando's uitvoert tegen een EuroFS-volume.
//! Pure functie: (filesystem, regel) -> uitvoerregels. Géén I/O hier — zo is
//! ze triviaal te redeneren en (later) host-testbaar.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use eurofs::{EntryKind, FileSystem};
use euromm::FrameAllocator;

/// Context die de shell-commando's nodig hebben: filesysteem + geheugen.
pub struct ShellCtx<'a> {
    pub fs: &'a mut dyn FileSystem,
    pub mem: &'a mut FrameAllocator,
}

pub fn exec(ctx: &mut ShellCtx, line: &str) -> Vec<String> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }
    // sudo <cmd> (S5): voer <cmd> uit met root-sessie (uid 0). Vóór de fs-borrow,
    // zodat de recursieve exec geen leen-conflict geeft. Alleen euro/root mogen sudo.
    if let Some(rest) = line.strip_prefix("sudo ") {
        let uid = crate::auth::session_uid();
        if uid != 1000 && uid != 0 {
            return vec!["sudo: deze gebruiker staat niet in de sudoers".to_string()];
        }
        let gid = crate::auth::session_gid();
        let name = crate::auth::name_for_uid(ctx.fs, uid);
        crate::auth::set_session(0, 0, "root");
        let mut out = exec(ctx, rest);
        crate::auth::set_session(uid, gid, &name); // sessie herstellen
        out.insert(0, format!("[sudo] '{rest}' als root:"));
        return out;
    }
    // Pijplijn (`A | B | C`): fase-bytes doorvoeren naar coreutils-filters.
    if is_pipeline(line) {
        return run_pipeline(ctx, line);
    }
    let fs = &mut *ctx.fs;
    // Houd de FS-klok gelijk met de echte wandklok zodat create/write een echte
    // mtime krijgen (EuroFS gebruikt deze waarde voor de wijzigingstijd).
    fs.set_clock(crate::rtc::epoch());
    let mut parts = line.splitn(3, ' ');
    let cmd = parts.next().unwrap_or("");
    let arg1 = parts.next().unwrap_or("");
    let arg2 = parts.next().unwrap_or("");

    // EuroCoreutils: GNU-compatibele coreutils (CU-1/3/4/6). Dispatch eerst; None =
    // geen coreutils-commando → val door naar de EuroOS-eigen commando's hieronder.
    if let Some(out) = coreutils(cmd, line, fs) {
        return out;
    }

    match cmd {
        "help" => vec![
            "commando's:".to_string(),
            "  ls [pad]            mapinhoud".to_string(),
            "  cat <pad>           bestand tonen".to_string(),
            "  write <pad> <tekst> bestand schrijven (CoW)".to_string(),
            "  mkdir <pad>         map aanmaken".to_string(),
            "  mv <bron> <doel>    hernoem/verplaats (vervangt een bestaand bestand)".to_string(),
            "  rmdir <pad>         lege map verwijderen".to_string(),
            "  rm <pad>            bestand verwijderen".to_string(),
            "  df                  vrije ruimte".to_string(),
            "  fsck / scrub        EuroFS-integriteitscontrole (checksums + structuur)".to_string(),
            "  fsck repair         + heel een gedegradeerd superblok-slot uit de A/B-kopie".to_string(),
            "  net                 EuroNet packet-selftest".to_string(),
            "  ping <host>          ICMP-echo (IPv4) / ping6".to_string(),
            "  netstat              DNS-cache + netwerkstatistiek".to_string(),
            "  fetch <host>         HTTP GET http://<host>/".to_string(),
            "  https <host>         HTTPS GET via EuroTLS 1.3".to_string(),
            "  mem                 fysiek geheugen + frame-allocator".to_string(),
            "  date                 echte tijd/datum (RTC)".to_string(),
            "  ps                   processen / scheduler-taken".to_string(),
            "  lspci                PCI-apparaten".to_string(),
            "  eurodevice / lsdev   device-tree + gebonden drivers (EuroDevice)".to_string(),
            "  dmesg [N]            kernel message buffer (laatste N regels)".to_string(),
            "  caps / euroguard     NATIEF EuroOS-beveiligingsmodel (capabilities per app)".to_string(),
            "  audit                append-only audit-spoor (tamper-evident, P3)".to_string(),
            "  eurosnap [create/rollback/delete]  CoW-snapshots (Sprint S)".to_string(),
            "  europol [explain CAP]  declaratief beleid → capabilities (X)".to_string(),
            "  metrics              live OpenMetrics (Prometheus-scrapebaar, W)".to_string(),
            "  vault [get <label>]  capability-gated secrets-store (U)".to_string(),
            "  firewall / vpn       packet-filter (N3) / soevereine VPN-tunnel (N2)".to_string(),
            "  services / euroctl   EuroInit service-status".to_string(),
            "  uptime               timer-ticks sinds boot".to_string(),
            "  reboot / shutdown    systeem herstarten/afsluiten".to_string(),
            "  free                 geheugengebruik (total/used/free)".to_string(),
            "  whoami / id          huidige sessie-gebruiker".to_string(),
            "  login <u> <pw> / su  inloggen (verifieer tegen /etc/shadow)".to_string(),
            "  sudo <cmd> / logout  als root draaien / sessie resetten".to_string(),
            "  hostname             systeemnaam (uit /etc/hostname)".to_string(),
            "  uname [-a/-r/-m/-n]  systeeminfo / clear".to_string(),
        ],
        "uname" => {
            let host = hostname(fs);
            match arg1 {
                "-a" => vec![format!("EuroOS {host} 0.1-alpha #1 SMP EuroKernel x86_64")],
                "-r" => vec!["0.1-alpha".to_string()],
                "-m" => vec!["x86_64".to_string()],
                "-n" => vec![host],
                "-s" | "" => vec!["EuroOS".to_string()],
                _ => vec![format!("uname: ongeldige optie '{arg1}' (gebruik -a/-r/-m/-n/-s)")],
            }
        }
        "hostname" => vec![hostname(fs)],
        "whoami" => vec![current_user(fs).0],
        "login" | "su" => {
            // login <gebruiker> <wachtwoord> — verifieer via EuroID (Argon2id,
            // memory-hard) + accountstaat + lockout + audit-log. /etc/passwd levert
            // alleen nog de POSIX uid/gid-mapping voor de sessie.
            let user = if arg1.is_empty() { "euro" } else { arg1 };
            match crate::euroid::login(user, &arg2) {
                Ok(ok) => {
                    // gid uit /etc/passwd (POSIX-mapping); terugval op de euroid-uid.
                    let gid = crate::auth::lookup_user(fs, &ok.name).map(|(_, g)| g).unwrap_or(ok.uid);
                    crate::auth::set_session(ok.uid, gid, &ok.name);
                    vec![format!("ingelogd als {} (uid={}, gid={}, EuroID-Argon2id)", ok.name, ok.uid, gid)]
                }
                Err(reason) => vec![format!("login: {reason}")],
            }
        }
        "logout" => {
            crate::auth::set_session(1000, 1000, "euro");
            vec!["uitgelogd — terug naar de euro-sessie".to_string()]
        }
        "id" => {
            let (u, uid, gid) = current_user(fs);
            vec![format!("uid={uid}({u}) gid={gid}({u}) groups={gid}({u}),0(root)")]
        }
        "free" => {
            let total = ctx.mem.usable_bytes() / 1024;
            let free = ctx.mem.free_bytes() / 1024;
            let used = total.saturating_sub(free);
            vec![
                "               total        used        free".to_string(),
                format!("Mem:    {total:>12} {used:>11} {free:>11}"),
                "Swap:              0           0           0".to_string(),
            ]
        }
        "date" => {
            let d = crate::rtc::now();
            vec![format!(
                "{} {} {:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][crate::rtc::weekday(&d) as usize % 7],
                "UTC", d.year, d.month, d.day, d.hour, d.min, d.sec
            )]
        }
        "ps" => {
            let n = crate::sched::task_count();
            let cores = crate::smp::AP_ONLINE.load(core::sync::atomic::Ordering::Relaxed) + 1;
            vec![
                format!("{} scheduler-taken, {} CPU-core(s) online", n, cores),
                "  (taak 0 = shell/desktop, daarna kernel-threads + ring-3 processen)".to_string(),
            ]
        }
        "lspci" => {
            let mut v: Vec<String> = crate::pci::enumerate()
                .iter()
                .map(|d| {
                    let name = crate::pci::device_name(d.vendor, d.device);
                    format!(
                        "  {:02x}:{:02x}.{} {:04x}:{:04x} {}{}",
                        d.bus, d.dev, d.func, d.vendor, d.device,
                        crate::pci::class_name(d.class, d.subclass),
                        if name.is_empty() { String::new() } else { format!(" ({name})") }
                    )
                })
                .collect();
            v.insert(0, "PCI-apparaten:".to_string());
            v
        }
        // R: het EuroDevice-model — de device-tree met gebonden drivers per apparaat.
        "eurodevice" | "lsdev" => crate::eurodevice::probe_lines(),
        // P3: het append-only audit-spoor (tamper-evident veiligheids-events).
        "audit" => {
            let mut v = crate::audit::recent(20);
            v.insert(0, format!("audit-log ({} events, append-only /var/log/audit.log):", crate::audit::count()));
            v
        }
        // X: EuroPol — declaratief beleid → capabilities.
        "europol" => crate::europol::shell(&format!("{arg1} {arg2}")),
        // W: EuroObserve — live OpenMetrics-export (zoals Prometheus scrapet).
        "metrics" => crate::observe::render().lines().map(String::from).collect(),
        // U: EuroVault — capability-gated secrets.
        "vault" => crate::vault::shell(&format!("{arg1} {arg2}"), if crate::auth::session_uid() == 0 { crate::vault::CAP_DB_ACCESS } else { 0 }),
        // Y: EuroCrash — de laatste crash-dump (recovery-inspectie).
        "eurocrash" => match crate::crashdump::read_last() {
            Some(d) => alloc::vec![
                format!("laatste crash-dump (seq {}): {} @ rip {:#x}", d.seq, d.vector_name(), d.rip),
                format!("  error={:#x} cr2={:#x} cr3={:#x} rflags={:#x} uptime={}ms", d.error_code, d.cr2, d.cr3, d.rflags, d.uptime_ms),
            ],
            None => alloc::vec![String::from("geen crash-dump (schone vorige boot)")],
        },
        // N3: EuroFW — packet-filter status.
        "firewall" | "eurofw" => crate::firewall::shell(),
        // N2: EuroVPN — soevereine VPN-tunnel.
        "vpn" | "eurovpn" => crate::vpn::shell(),
        // AA: EuroAgent — soevereine agent-first runtime.
        "agent" | "euroagent" => crate::agent::shell(&format!("{arg1} {arg2}")),
        // CU-5: find — loop de VFS-boom af en filter op -name/-type/-maxdepth.
        "find" => find_walk(fs, line),
        // AH-3: draai een echte `.wasm` uit het VFS in de no-JIT sandbox (cap-gated WASI).
        "wasm" => {
            if arg1.is_empty() {
                alloc::vec![String::from("gebruik: wasm <bestand.wasm>   (bv. wasm /agents/demo.wasm)")]
            } else {
                crate::wasm::run_file(fs, arg1)
            }
        }
        // P1: EuroLocale — lokalisatie voor de 24 EU-talen.
        "locale" => crate::locale::shell(&format!("{arg1} {arg2}")),
        // Q1/AH-1: EuroInstall — dry-run-plan, of `--to N` voor een ECHTE installatie.
        "euroinstall" => crate::installer::shell(line),
        // O3: EuroCA — soevereine lokale certificaatautoriteit.
        "euroca" => crate::ca::shell(),
        "euroattest" => crate::attest::shell(),
        "euroidm" => crate::idm::shell(),
        // K1: EuroID — soeverein gebruikersbeheer (eurousers add/list/show/passwd/...).
        "eurousers" => {
            let out = crate::euroid::shell(&format!("{arg1} {arg2}"), crate::auth::session_uid());
            // Muterende acties duurzaam maken: schrijf de opslag terug naar EuroFS.
            if matches!(arg1, "add" | "passwd" | "chpasswd" | "lock" | "unlock" | "del") {
                crate::euroid::persist_state(fs);
            }
            out
        }
        "europkg" => crate::pkg::shell(),
        "eurorepro" => crate::repro::shell(),
        "euroaccess" => crate::access::shell(),
        "eurosuite" => crate::suite::shell(&format!("{arg1} {arg2}")),
        "wifi" | "eurowifi" => crate::wifi::shell(),
        "gpu" | "eurogpu" => crate::gpu::shell(),
        // Z: EuroHealth — systeemgezondheid (SMART + FS + geheugen).
        "eurohealth" => {
            let sr = fs.scrub();
            crate::health::shell(sr.errors, sr.data_unrecoverable, ctx.mem.free_frames() as u64, ctx.mem.total_frames() as u64)
        }
        // S: EuroSnap — CoW-snapshots beheren.
        "eurosnap" => match arg1 {
            "create" => {
                let label = if arg2.is_empty() { "user-checkpoint" } else { arg2 };
                match fs.snapshot_create(label, eurofs::SNAP_READONLY) {
                    Ok(id) => vec![format!("snapshot #{id} '{label}' gemaakt (CoW — bijna gratis)")],
                    Err(e) => vec![format!("snapshot mislukt: {e:?}")],
                }
            }
            "rollback" => match arg2.parse::<u64>() {
                Ok(id) => match fs.snapshot_rollback(id) {
                    Ok(()) => vec![format!("teruggerold naar snapshot #{id} (reboot aanbevolen)")],
                    Err(e) => vec![format!("rollback mislukt: {e:?}")],
                },
                Err(_) => vec!["gebruik: eurosnap rollback <id>".to_string()],
            },
            "delete" => match arg2.parse::<u64>() {
                Ok(id) => match fs.snapshot_delete(id) {
                    Ok(()) => vec![format!("snapshot #{id} verwijderd (blokken ge-GC't)")],
                    Err(e) => vec![format!("delete mislukt: {e:?}")],
                },
                Err(_) => vec!["gebruik: eurosnap delete <id>".to_string()],
            },
            _ => {
                let snaps = fs.snapshot_list();
                let mut v = vec![format!("snapshots ({}):", snaps.len())];
                for s in &snaps {
                    v.push(format!("  #{:<3} ckpt={:<4} '{}'", s.id, s.checkpoint_id, s.label));
                }
                v.push("commando's: eurosnap create [label] | rollback <id> | delete <id>".to_string());
                v
            }
        },
        // EuroFS staat rechtstreeks op de schijf → wijzigingen zijn al persistent.
        "reboot" => crate::power::reboot(),
        "shutdown" | "poweroff" => crate::power::shutdown(),
        "uptime" => {
            let t = crate::interrupts::ticks();
            let mut out = vec![format!("uptime: {t} timer-ticks (~{} s) bij 100 Hz APIC-timer", t / 100)];
            if crate::hpet::present() {
                out.push(format!(
                    "HPET  : {} MHz, {} ms sinds activering (hoge-resolutie HAL-tijdbron)",
                    crate::hpet::freq_hz() / 1_000_000,
                    crate::hpet::ns() / 1_000_000
                ));
            }
            out
        }
        "caps" | "euroguard" => {
            // Toon het NATIEVE EuroOS-beveiligingsmodel: per-app CAPABILITIES
            // (least-privilege, gesigneerd) — géén ambient root/sudo. Markeer welke
            // binaries via de Linux-COMPAT-laag draaien vs. EuroOS-native zijn.
            let mut out = vec![
                "EuroGuard — capability-beveiliging (NATIEF EuroOS-model: geen ambient".to_string(),
                "  root/sudo; elke app krijgt minimale, gesigneerde rechten)".to_string(),
                String::new(),
                format!("{:<18} {:<14} {}", "PROGRAMMA", "ABI", "CAPABILITIES"),
            ];
            for (path, caps, linux) in crate::ring3::program_list() {
                let abi = if linux { "linux-compat" } else { "EuroOS-native" };
                out.push(format!("{:<18} {:<14} {}", path, abi, crate::ring3::cap_names(caps)));
            }
            out.push(String::new());
            out.push("netwerk-policy (EuroGuard-firewall):".to_string());
            out.extend(crate::euroguard::policy_lines());
            out
        }
        "fsck" | "scrub" => {
            // EuroFSck (S7): superblok + alle inode-checksums + structuur verifiëren.
            // `fsck repair` heelt bovendien een gedegradeerd superblok-slot uit de
            // geldige A/B-kopie (zelf-heling van de redundantie).
            let repairing = matches!(arg1, "repair" | "--repair" | "-r");
            let r = if repairing { fs.repair() } else { fs.scrub() };
            let mut out = vec![
                format!(
                    "EuroFSck — {}:",
                    if repairing { "integriteitscontrole + reparatie" } else { "integriteitscontrole (scrub)" }
                ),
                format!("  superblok            : {}", if r.superblock_ok { "OK" } else { "CORRUPT" }),
                format!("  inodes gecontroleerd : {}", r.objects),
                format!(
                    "  data-checksums (XXH3): {} geverifieerd{}",
                    r.data_verified,
                    if r.data_unrecoverable > 0 {
                        format!(", {} ONHERSTELBAAR (geen redundantie — mirror nodig)", r.data_unrecoverable)
                    } else {
                        String::new()
                    }
                ),
                format!("  datablokken (ref)    : {}", r.blocks_referenced),
                format!("  vrije-ruimte-bitmap  : {}", if r.bitmap_ok { "consistent" } else { "INCONSISTENT" }),
                format!("  fouten               : {}", r.errors),
            ];
            if repairing {
                out.push(format!("  hersteld             : {}", r.repaired));
            }
            for m in &r.messages {
                out.push(format!("  ! {m}"));
            }
            out.push(
                if r.errors == 0 && r.superblock_ok && r.bitmap_ok {
                    "  => filesysteem GEZOND ✓".to_string()
                } else if !repairing {
                    "  => PROBLEMEN gevonden (probeer 'fsck repair')".to_string()
                } else {
                    "  => niet alles herstelbaar".to_string()
                },
            );
            out
        }
        "services" | "euroctl" => crate::init::status_lines(),
        "dmesg" => {
            // Kernel message buffer (kmsg-ring). `dmesg N` toont de laatste N regels.
            let all = crate::klog::snapshot();
            let n: usize = arg1.parse().unwrap_or(40);
            let start = all.len().saturating_sub(n);
            all[start..].to_vec()
        }
        "net" => net_selftest(),
        "netstat" => crate::net::netstat_lines(),
        "nslookup" | "resolve" => {
            if arg1.is_empty() {
                vec!["gebruik: nslookup <host>".to_string()]
            } else {
                match crate::net::resolve(arg1) {
                    Some(ip) => vec![format!("{arg1} = {}", fmt_ip(ip))],
                    None => vec![format!("nslookup: kan '{arg1}' niet resolven")],
                }
            }
        }
        "ping" => crate::net::cmd_ping(arg1),
        "ping6" => crate::net::cmd_ping6(),
        "fetch" | "wget" => {
            if arg1.is_empty() {
                vec!["gebruik: fetch <host> [opslagpad]   (HTTP GET, optioneel naar bestand)".to_string()]
            } else if arg2.is_empty() {
                crate::net::cmd_fetch(arg1) // toon-modus
            } else {
                // download-naar-bestand: haal op en schrijf naar EuroFS (persistent).
                match crate::net::http_download(arg1, "/") {
                    Some((status, body)) => match fs.write_file(arg2, &body) {
                        Ok(_) => vec![
                            status.trim().to_string(),
                            format!("opgeslagen: {} bytes -> {arg2}", body.len()),
                        ],
                        Err(e) => vec![format!("wget: schrijven naar {arg2} mislukt: {e:?}")],
                    },
                    None => vec![format!("wget: ophalen van {arg1} mislukt")],
                }
            }
        }
        "https" => {
            if arg1.is_empty() {
                vec!["gebruik: https <host>   (HTTPS GET via EuroTLS 1.3)".to_string()]
            } else {
                crate::net::cmd_https(arg1)
            }
        }
        "tcpserve" => {
            // Demo van de POSIX server-sockets: luister, accepteer één verbinding,
            // lees de eerste regel en antwoord. Standaardpoort 8080.
            let port: u16 = if arg1.is_empty() { 8080 } else { arg1.parse().unwrap_or(8080) };
            crate::net::cmd_tcpserve(port)
        }
        "df" => {
            let mut out = vec!["Filesystem   Size(KiB)  Used(KiB)  Avail(KiB)  Mounted on".to_string()];
            for (mp, total, free) in fs.df() {
                let used = total.saturating_sub(free);
                out.push(format!("  EuroFS    {:>9}  {:>9}  {:>9}   {}", total / 1024, used / 1024, free / 1024, mp));
            }
            out
        }
        "syscallprofile" | "sprof" => crate::ring3::syscall_profile_lines(),
        "container" | "ctr" => match arg1 {
            "list" | "" => crate::container::list(),
            "create" => {
                if arg2.is_empty() {
                    vec!["gebruik: container create <naam>".to_string()]
                } else {
                    // Demo-container: console + bestand + proces-info, geen netwerk.
                    crate::container::create(
                        fs,
                        arg2,
                        crate::ring3::CAP_CONSOLE | crate::ring3::CAP_FILE | crate::ring3::CAP_PROC_INFO,
                        eurosandbox::NetScope::None,
                    )
                }
            }
            "run" => {
                if arg2.is_empty() {
                    vec!["gebruik: container run <naam>   (demonstreert chroot met een ../-ontsnappingspad)".to_string()]
                } else {
                    crate::container::run(fs, arg2, "../../../etc/passwd")
                }
            }
            _ => vec!["container: list | create <naam> | run <naam> <pad>".to_string()],
        },
        "euroupdate" | "eup" => match arg1 {
            "" | "status" => crate::update::status(fs),
            "rollback" => crate::update::rollback(fs),
            "apply" => {
                if arg2.is_empty() {
                    vec!["gebruik: euroupdate apply <image>   (verwacht <image>.sig ernaast)".to_string()]
                } else {
                    crate::update::apply(fs, arg2)
                }
            }
            "fetch" => {
                if arg2.is_empty() {
                    vec!["gebruik: euroupdate fetch <url>   (haalt <url> + <url>.sig op, verifieert, staget)".to_string()]
                } else {
                    crate::update::fetch(fs, arg2)
                }
            }
            _ => vec!["euroupdate: status | apply <image> | fetch <url> | rollback".to_string()],
        },
        "euroimmutable" | "immutable" => crate::immutable::shell(fs, arg1, arg2),
        "mem" => mem_report(ctx.mem),
        "ls" => {
            let path = if arg1.is_empty() { "/" } else { arg1 };
            match fs.list_dir(path) {
                Ok(mut e) => {
                    e.sort_by(|a, b| a.name.cmp(&b.name));
                    if e.is_empty() {
                        vec![format!("{path}: (leeg)")]
                    } else {
                        e.iter()
                            .map(|d| {
                                let dir = d.kind == EntryKind::Directory;
                                format!(
                                    "  {} {:>8} B  {}  {}",
                                    fmt_mode(d.mode, dir),
                                    d.size,
                                    fmt_epoch(d.mtime),
                                    d.name
                                )
                            })
                            .collect()
                    }
                }
                Err(e) => vec![format!("ls: {path}: {e:?}")],
            }
        }
        "cat" => match fs.read_file(arg1) {
            Ok(data) => match core::str::from_utf8(&data) {
                Ok(s) => s.lines().map(|l| l.to_string()).collect(),
                Err(_) => vec![format!("cat: {arg1}: binair ({} bytes)", data.len())],
            },
            Err(e) => vec![format!("cat: {arg1}: {e:?}")],
        },
        "write" => {
            if arg1.is_empty() {
                return vec!["gebruik: write <pad> <tekst>".to_string()];
            }
            let mut content = arg2.to_string();
            content.push('\n');
            match fs.write_file(arg1, content.as_bytes()) {
                Ok(()) => vec![format!("geschreven: {arg1} ({} bytes)", content.len())],
                Err(e) => vec![format!("write: {arg1}: {e:?}")],
            }
        }
        "mkdir" => match fs.create_dir(arg1) {
            Ok(()) => vec![format!("map aangemaakt: {arg1}")],
            Err(e) => vec![format!("mkdir: {arg1}: {e:?}")],
        },
        "rm" => match fs.remove_file(arg1) {
            Ok(()) => vec![format!("verwijderd: {arg1}")],
            Err(e) => vec![format!("rm: {arg1}: {e:?}")],
        },
        // CU-2: cp — kopieer arg1 → arg2 (op de EuroFS-primitieven).
        "cp" => match fs.read_file(arg1) {
            Ok(data) => match fs.write_file(arg2, &data) {
                Ok(()) => vec![format!("'{arg1}' -> '{arg2}' ({} bytes)", data.len())],
                Err(e) => vec![format!("cp: {arg2}: {e:?}")],
            },
            Err(e) => vec![format!("cp: {arg1}: {e:?}")],
        },
        // CU-2: touch — maak een leeg bestand (of laat een bestaand ongemoeid).
        "touch" => {
            if fs.exists(arg1) {
                vec![format!("touch: {arg1} bestaat al (mtime-update n.v.t.)")]
            } else {
                match fs.write_file(arg1, b"") {
                    Ok(()) => vec![format!("leeg bestand aangemaakt: {arg1}")],
                    Err(e) => vec![format!("touch: {arg1}: {e:?}")],
                }
            }
        }
        // CU-2: stat — toon bestand/map-metadata (+ EuroOS-extra: immutability-vlaggen).
        "stat" => match fs.metadata(arg1) {
            Ok(m) => {
                let kind = match m.kind {
                    EntryKind::File => "regulier bestand",
                    EntryKind::Directory => "map",
                    EntryKind::Symlink => "symlink",
                };
                let flags = fs.get_flags(arg1).unwrap_or(0);
                let imm = if flags & eurofs::FLAG_IMMUTABLE != 0 { " IMMUTABLE" } else { "" };
                let app = if flags & eurofs::FLAG_APPEND_ONLY != 0 { " APPEND_ONLY" } else { "" };
                vec![
                    format!("  Bestand: {arg1}"),
                    format!("  Grootte: {}  Type: {kind}  Modus: {:#o}", m.size, m.mode),
                    format!("  Wijziging: {}  Vlaggen:{}{}", m.mtime, if imm.is_empty() && app.is_empty() { " (geen)" } else { "" }, format!("{imm}{app}")),
                ]
            }
            Err(e) => vec![format!("stat: {arg1}: {e:?}")],
        },
        // CU-2: truncate -s N <bestand> — knip in/breid uit tot N bytes.
        "truncate" => {
            // gebruik: truncate -s <N> <bestand>
            let toks: Vec<&str> = line.split_whitespace().skip(1).collect();
            let size = toks.iter().position(|t| *t == "-s").and_then(|i| toks.get(i + 1)).and_then(|v| v.parse::<usize>().ok());
            let file = toks.iter().rev().find(|t| !t.starts_with('-') && t.parse::<usize>().is_err());
            match (size, file) {
                (Some(n), Some(f)) => {
                    let mut data = fs.read_file(f).unwrap_or_default();
                    data.resize(n, 0);
                    match fs.write_file(f, &data) {
                        Ok(()) => vec![format!("'{f}' afgekapt/uitgebreid tot {n} bytes")],
                        Err(e) => vec![format!("truncate: {f}: {e:?}")],
                    }
                }
                _ => vec!["gebruik: truncate -s <bytes> <bestand>".to_string()],
            }
        }
        "mv" | "rename" => {
            if arg1.is_empty() || arg2.is_empty() {
                vec!["gebruik: mv <bron> <doel>".to_string()]
            } else {
                match fs.rename(arg1, arg2) {
                    Ok(()) => vec![format!("{arg1} -> {arg2}")],
                    Err(e) => vec![format!("mv: {e:?}")],
                }
            }
        }
        "rmdir" => match fs.remove_dir(arg1) {
            Ok(()) => vec![format!("map verwijderd: {arg1}")],
            Err(e) => vec![format!("rmdir: {arg1}: {e:?}")],
        },
        "df" => {
            let (total, free) = fs.space_info();
            vec![format!(
                "EuroFS: {} KiB totaal, {} KiB vrij, {} KiB gebruikt",
                total / 1024,
                free / 1024,
                (total - free) / 1024
            )]
        }
        "clear" => vec!["\x0c".to_string()], // signaal voor main om te wissen
        other => vec![format!("onbekend commando: {other}  (typ 'help')")],
    }
}

/// Toon RAM-statistieken en demonstreer de frame-allocator (alloc + free).
/// Lees de hostnaam uit /etc/hostname (fallback "eurokernel").
fn hostname(fs: &mut dyn FileSystem) -> String {
    fs.read_file("/etc/hostname")
        .ok()
        .and_then(|d| String::from_utf8(d).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "eurokernel".to_string())
}

/// De ACTIEVE sessie-gebruiker (S5): uid/gid uit de auth-sessie, naam uit /etc/passwd.
fn current_user(fs: &mut dyn FileSystem) -> (String, u32, u32) {
    let uid = crate::auth::session_uid();
    let gid = crate::auth::session_gid();
    (crate::auth::name_for_uid(fs, uid), uid, gid)
}

fn mem_report(mem: &mut FrameAllocator) -> Vec<String> {
    let mut out = vec![
        format!(
            "RAM   : {} MiB bruikbaar, {} MiB vrij  ({} frames van 4 KiB)",
            mem.usable_bytes() / (1024 * 1024),
            mem.free_bytes() / (1024 * 1024),
            mem.usable_frames()
        ),
        format!(
            "frames: {} vrij van {} bruikbaar",
            mem.free_frames(),
            mem.usable_frames()
        ),
    ];
    let before = mem.free_frames();
    let mut got = Vec::new();
    for _ in 0..4 {
        if let Ok(f) = mem.allocate() {
            got.push(f);
        }
    }
    let addrs = got.iter().map(|f| format!("{f:#x}")).collect::<Vec<_>>().join(" ");
    out.push(format!("alloc : 4 frames -> {addrs}"));
    for f in &got {
        let _ = mem.free(*f);
    }
    out.push(format!(
        "free  : vrijgegeven; {} -> {} -> {} vrije frames (alloc/free OK)",
        before,
        before - got.len(),
        mem.free_frames()
    ));
    // S6 geheugen-hardening: stack-guard-canary's + frame-allocator-diagnostiek.
    out.push(format!(
        "hardening: stack-guard AAN (canary/kernel-taak, gecheckt bij switch); double-frees: {}; piek: {} MiB",
        mem.double_frees(),
        mem.high_water_frames() * 4096 / (1024 * 1024)
    ));
    // CPU-bescherming: SMEP (ring 0 voert geen user-code uit) + SMAP (ring 0 raakt
    // geen user-geheugen aan buiten een kort, niet-preemptief syscall-venster).
    out.push(format!(
        "cpu-bescherming: SMEP {} · SMAP {} · W^X/NX {} (CR4) — user-toegang via AC-venster per syscall; code R-X, data/stack NX",
        if crate::ring3::smep_active() { "AAN" } else { "n/b" },
        if crate::ring3::smap_active() { "AAN" } else { "n/b" },
        if crate::ring3::nx_active() { "AAN" } else { "n/b" },
    ));
    out
}

fn fmt_ip(ip: euronet::Ipv4Addr) -> String {
    format!("{}.{}.{}.{}", ip.0[0], ip.0[1], ip.0[2], ip.0[3])
}

/// POSIX-rechten als `drwxr-xr-x`-string (type-bit + 9 rwx-bits).
fn fmt_mode(mode: u16, dir: bool) -> String {
    let bit = |b: u16, c: char| if mode & b != 0 { c } else { '-' };
    let mut s = String::with_capacity(10);
    s.push(if dir { 'd' } else { '-' });
    for (r, w, x) in [(0o400, 0o200, 0o100), (0o040, 0o020, 0o010), (0o004, 0o002, 0o001)] {
        s.push(bit(r, 'r'));
        s.push(bit(w, 'w'));
        s.push(bit(x, 'x'));
    }
    s
}

/// Unix-epoch (seconden, UTC) → `YYYY-MM-DD HH:MM`. 0 = onbekend.
fn fmt_epoch(secs: u64) -> String {
    if secs == 0 {
        return "      -          ".to_string();
    }
    let (days, tod) = (secs / 86400, secs % 86400);
    let (h, mi) = (tod / 3600, (tod % 3600) / 60);
    let leap = |y: u64| (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let mut y = 1970u64;
    let mut d = days;
    loop {
        let yd = if leap(y) { 366 } else { 365 };
        if d < yd {
            break;
        }
        d -= yd;
        y += 1;
    }
    let mdays = [31u64, if leap(y) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0usize;
    while m < 12 && d >= mdays[m] {
        d -= mdays[m];
        m += 1;
    }
    format!("{y:04}-{:02}-{:02} {h:02}:{mi:02}", m + 1, d + 1)
}

fn fmt_mac(m: euronet::MacAddr) -> String {
    let b = m.0;
    format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", b[0], b[1], b[2], b[3], b[4], b[5])
}

/// Bouwt en parset echte packets door de hele stack en rapporteert per laag.
/// Bewijst dat EuroNet ook no_std in de kernel werkt (zelfde code als host-tests).
fn net_selftest() -> Vec<String> {
    use euronet::{
        ArpOp, ArpPacket, EtherType, EthernetHeader, IcmpEcho, IcmpType, Ipv4Addr, Ipv4Header,
        MacAddr, Protocol, UdpDatagram,
    };
    let mut out = Vec::new();
    let my_mac = MacAddr([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    let my_ip = Ipv4Addr::new(10, 0, 2, 15);
    let gw_ip = Ipv4Addr::new(10, 0, 2, 2);
    let dns = Ipv4Addr::new(8, 8, 8, 8);

    // ARP: parse een request "wie heeft 10.0.2.15?" en bouw een reply.
    let req = ArpPacket {
        op: ArpOp::Request,
        sender_mac: MacAddr([0xAA, 0xBB, 0xCC, 0x00, 0x00, 0x01]),
        sender_ip: gw_ip,
        target_mac: MacAddr::ZERO,
        target_ip: my_ip,
    };
    let arp = ArpPacket::parse(&req.build()).unwrap();
    let reply = ArpPacket::reply_to(&arp, my_mac);
    out.push(format!(
        "ARP  : {} vraagt {}  ->  reply: {} is-at {}",
        fmt_ip(arp.sender_ip),
        fmt_ip(arp.target_ip),
        fmt_ip(reply.sender_ip),
        fmt_mac(reply.sender_mac)
    ));

    // ICMP: echo-request -> reply, checksum geverifieerd door parse().
    let echo = IcmpEcho {
        kind: IcmpType::EchoRequest,
        identifier: 0x1234,
        sequence: 7,
        payload: b"europing".to_vec(),
    };
    let rep = IcmpEcho::reply_to(&echo);
    let ok_icmp = IcmpEcho::parse(&rep.build()).is_ok();
    out.push(format!(
        "ICMP : echo id={} seq={} -> reply, checksum {}",
        echo.identifier,
        echo.sequence,
        if ok_icmp { "OK" } else { "FOUT" }
    ));

    // IPv4 + UDP: bouw een volledige DNS-query en parse 'm terug.
    let udp = UdpDatagram {
        src_port: 5353,
        dst_port: 53,
        payload: b"DNS-query".to_vec(),
    };
    let udp_seg = udp.build(my_ip, dns);
    let ip = Ipv4Header {
        protocol: Protocol::Udp,
        ttl: 64,
        src: my_ip,
        dst: dns,
        total_length: 0,
        identification: 0xBEEF,
    };
    let ip_pkt = ip.build(&udp_seg);
    let eth = EthernetHeader {
        dst: MacAddr([0xAA, 0xBB, 0xCC, 0x00, 0x00, 0x01]),
        src: my_mac,
        ethertype: EtherType::Ipv4,
    };
    let frame = eth.build(&ip_pkt);

    // Volledige ontvangst-keten: Ethernet -> IPv4 -> UDP.
    let (eth_h, l3) = EthernetHeader::parse(&frame).unwrap();
    let (ip_h, l4) = Ipv4Header::parse(l3).unwrap();
    let udp_dg = UdpDatagram::parse(l4, ip_h.src, ip_h.dst).unwrap();
    out.push(format!(
        "IPv4 : {} -> {} ttl={} proto={:?}  hdr-checksum OK",
        fmt_ip(ip_h.src),
        fmt_ip(ip_h.dst),
        ip_h.ttl,
        ip_h.protocol
    ));
    out.push(format!(
        "UDP  : :{} -> :{}  '{}'  pseudo-checksum OK",
        udp_dg.src_port,
        udp_dg.dst_port,
        core::str::from_utf8(&udp_dg.payload).unwrap_or("?")
    ));
    out.push(format!(
        "Frame: ethertype {:?}, {} bytes, volledig geparsed  [OK]",
        eth_h.ethertype,
        frame.len()
    ));
    out
}

/// EuroCoreutils-dispatch: GNU-compatibele coreutils als shell-built-ins. Het laatste
/// bestaande bestand-argument wordt als invoer (stdin-vervanger) ingelezen; de overige
/// tokens zijn de opties. Geeft None als `cmd` geen coreutils-commando is.
fn coreutils(cmd: &str, line: &str, fs: &mut dyn FileSystem) -> Option<Vec<String>> {
    use eurocoreutils as cu;
    let toks: Vec<&str> = line.split_whitespace().skip(1).collect();

    // CU-7: arg-only reken-/control-commando's (geen bestand-invoer; een arg die
    // toevallig een bestandsnaam is mag hier niet als stdin opgeslokt worden).
    match cmd {
        "printf" => return Some(render_bytes(cu::compute::printf(&toks))),
        "expr" => return Some(render_bytes(cu::compute::expr(&toks).0)),
        "numfmt" => return Some(render_bytes(cu::compute::numfmt(&toks))),
        "factor" => return Some(render_bytes(cu::compute::factor(&toks))),
        "test" | "[" => {
            let code = cu::compute::test(&toks);
            return Some(vec![alloc::format!("test: {}", if code == 0 { "waar (exit 0)" } else { "onwaar (exit 1)" })]);
        }
        _ => {}
    }

    // Zoek (van achter) een positioneel token dat een leesbaar bestand is → invoer.
    let mut input: Vec<u8> = Vec::new();
    let mut file_idx: Option<usize> = None;
    for (i, t) in toks.iter().enumerate().rev() {
        if !t.starts_with('-') {
            if let Ok(data) = fs.read_file(t) {
                input = data;
                file_idx = Some(i);
                break;
            }
        }
    }
    let cargs: Vec<&str> = toks.iter().enumerate().filter(|(i, _)| Some(*i) != file_idx).map(|(_, t)| *t).collect();
    let name = file_idx.map(|i| toks[i]).unwrap_or("-");
    let has_d = cargs.contains(&"-d");

    let out: Vec<u8> = match cmd {
        "echo" => cu::echo(&cargs),
        "seq" => cu::seq(&cargs),
        "basename" => cu::basename(&cargs),
        "dirname" => cu::dirname(&cargs),
        "head" => cu::text::head(&cargs, &input),
        "tail" => cu::text::tail(&cargs, &input),
        "wc" => cu::text::wc(&cargs, &input),
        "tac" => cu::text::tac(&cargs, &input),
        "rev" => cu::text::rev(&cargs, &input),
        "nl" => cu::text::nl(&cargs, &input),
        "fold" => cu::text::fold(&cargs, &input),
        "sort" => cu::text::sort(&cargs, &input),
        "uniq" => cu::text::uniq(&cargs, &input),
        "cut" => cu::text::cut(&cargs, &input),
        "tr" => cu::text::tr(&cargs, &input),
        "grep" => cu::text::grep(&cargs, &input),
        "sha256sum" => cu::checksum::sha256sum(&input, name),
        "sha512sum" => cu::checksum::sha512sum(&input, name),
        "sha224sum" => cu::checksum::sha224sum(&input, name),
        "sha384sum" => cu::checksum::sha384sum(&input, name),
        "base64" => cu::encoding::base64(has_d, &input),
        "base32" => cu::encoding::base32(has_d, &input),
        "cksum" => cu::encoding::cksum(&input, name),
        "true" => return Some(Vec::new()),
        "false" => return Some(Vec::new()),
        "yes" => {
            let w = if cargs.is_empty() { String::from("y") } else { cargs.join(" ") };
            return Some((0..12).map(|_| w.clone()).collect());
        }
        "arch" => return Some(vec![String::from("x86_64")]),
        "nproc" => {
            let n = crate::acpi::parse().map(|m| m.enabled_cores()).unwrap_or(1).max(1);
            return Some(vec![n.to_string()]);
        }
        "pwd" => return Some(vec![String::from("/")]),
        _ => return None,
    };
    Some(render_bytes(out))
}

/// Zet ruwe coreutils-uitvoer-bytes om in shell-regels.
fn render_bytes(out: Vec<u8>) -> Vec<String> {
    String::from_utf8_lossy(&out).lines().map(String::from).collect()
}

/// Pas een coreutils-*filter* toe op stdin-bytes (de pijplijn-rol). Geeft `None`
/// als `cmd` geen filter is dat stdin verwerkt.
pub(crate) fn coreutils_filter(cmd: &str, args: &[&str], input: &[u8]) -> Option<Vec<u8>> {
    use eurocoreutils as cu;
    let out = match cmd {
        "cat" => input.to_vec(), // identiteit in een pijplijn
        "head" => cu::text::head(args, input),
        "tail" => cu::text::tail(args, input),
        "wc" => cu::text::wc(args, input),
        "tac" => cu::text::tac(args, input),
        "rev" => cu::text::rev(args, input),
        "nl" => cu::text::nl(args, input),
        "fold" => cu::text::fold(args, input),
        "sort" => cu::text::sort(args, input),
        "uniq" => cu::text::uniq(args, input),
        "cut" => cu::text::cut(args, input),
        "tr" => cu::text::tr(args, input),
        "grep" => cu::text::grep(args, input),
        "base64" => cu::encoding::base64(args.contains(&"-d"), input),
        "base32" => cu::encoding::base32(args.contains(&"-d"), input),
        "sha256sum" => cu::checksum::sha256sum(input, "-"),
        "sha512sum" => cu::checksum::sha512sum(input, "-"),
        "sha224sum" => cu::checksum::sha224sum(input, "-"),
        "sha384sum" => cu::checksum::sha384sum(input, "-"),
        "cksum" => cu::encoding::cksum(input, "-"),
        _ => return None,
    };
    Some(out)
}

/// Is `line` een pijplijn van ≥2 fasen?
pub(crate) fn is_pipeline(line: &str) -> bool {
    line.split('|').filter(|s| !s.trim().is_empty()).count() >= 2 && line.contains('|')
}

/// Voer een pijplijn `A | B | C` uit: fase 0 via de gewone shell (mag een bestand
/// lezen, `echo`, `ls`, …), elke volgende fase als een coreutils-filter op de bytes
/// van de vorige. Stdout van fase N → stdin van fase N+1.
pub(crate) fn run_pipeline(ctx: &mut ShellCtx, line: &str) -> Vec<String> {
    let stages: Vec<&str> = line.split('|').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if stages.len() < 2 {
        return exec(ctx, line);
    }
    // Fase 0: draai via de gewone exec en neem zijn uitvoer als beginstroom.
    let mut bytes: Vec<u8> = exec(ctx, stages[0]).join("\n").into_bytes();
    if !bytes.is_empty() {
        bytes.push(b'\n');
    }
    for st in &stages[1..] {
        let toks: Vec<&str> = st.split_whitespace().collect();
        let Some(cmd) = toks.first().copied() else { continue };
        // `tee FILE`: schrijf de huidige stroom naar FILE en geef ze ongewijzigd door.
        if cmd == "tee" {
            if let Some(fname) = toks.get(1) {
                let _ = ctx.fs.write_file(fname, &bytes);
            }
            continue;
        }
        // `xargs [-nN] [CMD [args...]]`: bouw uit de stdin-tokens een commando en
        // voer het uit (default CMD = echo). Heeft `ctx` nodig → apart afgehandeld.
        if cmd == "xargs" {
            bytes = run_xargs(ctx, &toks[1..], &bytes);
            continue;
        }
        match coreutils_filter(cmd, &toks[1..], &bytes) {
            Some(o) => bytes = o,
            None => return alloc::vec![alloc::format!("{cmd}: verwerkt geen pijplijn-invoer (geen filter)")],
        }
    }
    render_bytes(bytes)
}

/// `xargs [-n N] [CMD [args...]]` — lees de stdin-bytes, splits ze in tokens
/// (witruimte/regeleinden), en voer `CMD args... tokens...` uit via de shell.
/// Met `-n N` draait het CMD per batch van N tokens; zonder CMD is het `echo`.
/// De uitvoer van alle aanroepen wordt aaneengeschakeld teruggegeven.
fn run_xargs(ctx: &mut ShellCtx, args: &[&str], input: &[u8]) -> Vec<u8> {
    // Parse `-n N` / `-nN`; de rest is CMD + initiële args.
    let mut per: Option<usize> = None;
    let mut rest: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-n" {
            if let Some(v) = args.get(i + 1).and_then(|v| v.parse::<usize>().ok()) {
                per = Some(v.max(1));
                i += 2;
                continue;
            }
        } else if let Some(v) = args[i].strip_prefix("-n").and_then(|v| v.parse::<usize>().ok()) {
            per = Some(v.max(1));
            i += 1;
            continue;
        }
        rest.push(args[i]);
        i += 1;
    }
    let cmd = rest.first().copied().unwrap_or("echo");
    let base: Vec<&str> = rest.iter().skip(1).copied().collect();
    let text = core::str::from_utf8(input).unwrap_or("");
    let tokens: Vec<&str> = text.split_whitespace().collect();

    let batches: Vec<&[&str]> = match per {
        Some(n) if !tokens.is_empty() => tokens.chunks(n).collect(),
        _ => alloc::vec![tokens.as_slice()],
    };
    let mut out: Vec<u8> = Vec::new();
    for batch in batches {
        let mut cmdline = String::from(cmd);
        for a in base.iter().chain(batch.iter()) {
            cmdline.push(' ');
            cmdline.push_str(a);
        }
        for l in exec(ctx, &cmdline) {
            out.extend_from_slice(l.as_bytes());
            out.push(b'\n');
        }
    }
    out
}

/// `find [START] [-name GLOB] [-type f|d] [-maxdepth N]` — loop de VFS-boom af
/// vanaf `START` (default `/`) en print elk pad dat de filters matcht. De
/// matchlogica zit host-getest in `eurocoreutils::find`; hier doen we het lopen.
pub(crate) fn find_walk(fs: &mut dyn FileSystem, line: &str) -> Vec<String> {
    use eurocoreutils::find::FindOpts;
    let toks: Vec<&str> = line.split_whitespace().skip(1).collect();
    let opts = FindOpts::parse(&toks);
    let mut start = FindOpts::start_path(&toks);
    if start == "." {
        start = String::from("/");
    }
    let mut out: Vec<String> = Vec::new();
    // Het startpad zelf telt als diepte 0 (mits het matcht).
    let start_name = start.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("/");
    if opts.matches(start_name, true, 0) {
        out.push(start.clone());
    }
    let mut stack: Vec<(String, usize)> = alloc::vec![(start, 1usize)];
    let mut budget = 4096; // veiligheidslimiet tegen een ontaarde boom
    while let Some((dir, depth)) = stack.pop() {
        if let Some(md) = opts.maxdepth {
            if depth > md {
                continue;
            }
        }
        let entries = match fs.list_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries {
            if budget == 0 {
                out.push(String::from("find: (afgekapt — te veel items)"));
                return out;
            }
            budget -= 1;
            let is_dir = e.kind == eurofs::EntryKind::Directory;
            let path = if dir == "/" {
                alloc::format!("/{}", e.name)
            } else {
                alloc::format!("{dir}/{}", e.name)
            };
            if opts.matches(&e.name, is_dir, depth) {
                out.push(path.clone());
            }
            if is_dir {
                stack.push((path, depth + 1));
            }
        }
    }
    if out.is_empty() {
        out.push(String::from("find: geen overeenkomsten"));
    }
    out
}
