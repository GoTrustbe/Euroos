//! Minimal shell that executes commands against an EuroFS volume.
//! Pure function: (filesystem, line) -> output lines. No I/O here — this makes
//! it trivial to reason about and (later) host-testable.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use eurofs::{EntryKind, FileSystem};
use euromm::FrameAllocator;

/// Context that the shell commands need: filesystem + memory.
pub struct ShellCtx<'a> {
    pub fs: &'a mut dyn FileSystem,
    pub mem: &'a mut FrameAllocator,
}

pub fn exec(ctx: &mut ShellCtx, line: &str) -> Vec<String> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }
    // sudo <cmd> (S5): run <cmd> with a root session (uid 0). Before the fs borrow,
    // so the recursive exec does not cause a borrow conflict. Only euro/root may sudo.
    if let Some(rest) = line.strip_prefix("sudo ") {
        let uid = crate::auth::session_uid();
        if uid != 1000 && uid != 0 {
            return vec!["sudo: this user is not in the sudoers".to_string()];
        }
        let gid = crate::auth::session_gid();
        let name = crate::auth::name_for_uid(ctx.fs, uid);
        crate::auth::set_session(0, 0, "root");
        let mut out = exec(ctx, rest);
        crate::auth::set_session(uid, gid, &name); // restore session
        out.insert(0, format!("[sudo] '{rest}' as root:"));
        return out;
    }
    // Pipeline (`A | B | C`): pass stage bytes through to coreutils filters.
    if is_pipeline(line) {
        return run_pipeline(ctx, line);
    }
    let fs = &mut *ctx.fs;
    // Keep the FS clock in sync with the real wall clock so that create/write get a
    // real mtime (EuroFS uses this value for the modification time).
    fs.set_clock(crate::rtc::epoch());
    let mut parts = line.splitn(3, ' ');
    let cmd = parts.next().unwrap_or("");
    let arg1 = parts.next().unwrap_or("");
    let arg2 = parts.next().unwrap_or("");

    // EuroCoreutils: GNU-compatible coreutils (CU-1/3/4/6). Dispatch first; None =
    // not a coreutils command → fall through to the EuroOS-native commands below.
    if let Some(out) = coreutils(cmd, line, fs) {
        return out;
    }

    match cmd {
        "help" => vec![
            "commands:".to_string(),
            "  ls [path]           directory contents".to_string(),
            "  cat <path>          show file".to_string(),
            "  write <path> <text> write file (CoW)".to_string(),
            "  mkdir <path>        create directory".to_string(),
            "  mv <src> <dst>      rename/move (replaces an existing file)".to_string(),
            "  rmdir <path>        remove empty directory".to_string(),
            "  rm <path>           remove file".to_string(),
            "  df                  free space".to_string(),
            "  fsck / scrub        EuroFS integrity check (checksums + structure)".to_string(),
            "  fsck repair         + heal a degraded superblock slot from the A/B copy".to_string(),
            "  net                 EuroNet packet self-test".to_string(),
            "  ping <host>          ICMP echo (IPv4) / ping6".to_string(),
            "  netstat              DNS cache + network statistics".to_string(),
            "  fetch <host>         HTTP GET http://<host>/".to_string(),
            "  https <host>         HTTPS GET via EuroTLS 1.3".to_string(),
            "  mem                 physical memory + frame allocator".to_string(),
            "  date                 real time/date (RTC)".to_string(),
            "  ps                   processes / scheduler tasks".to_string(),
            "  lspci                PCI devices".to_string(),
            "  eurodevice / lsdev   device tree + bound drivers (EuroDevice)".to_string(),
            "  dmesg [N]            kernel message buffer (last N lines)".to_string(),
            "  caps / euroguard     NATIVE EuroOS security model (capabilities per app)".to_string(),
            "  audit                append-only audit trail (tamper-evident, P3)".to_string(),
            "  eurosnap [create/rollback/delete]  CoW snapshots (Sprint S)".to_string(),
            "  europol [explain CAP]  declarative policy → capabilities (X)".to_string(),
            "  metrics              live OpenMetrics (Prometheus-scrapable, W)".to_string(),
            "  vault [get <label>]  capability-gated secrets store (U)".to_string(),
            "  firewall / vpn       packet filter (N3) / sovereign VPN tunnel (N2)".to_string(),
            "  services / euroctl   EuroInit service status".to_string(),
            "  hwprobe              hardware/driver inventory (paste into the HCL)".to_string(),
            "  battery / power      ACPI battery + AC adapter status".to_string(),
            "  guard                per-app + geo network control (block-country, app policy, report)".to_string(),
            "  apps / app <name>    total control per app: capabilities + permissions + network in one view".to_string(),
            "  print [text]         driverless print via IPP Everywhere".to_string(),
            "  scan [path]          driverless scan via eSCL/AirScan → EuroFiles".to_string(),
            "  uptime               timer ticks since boot".to_string(),
            "  reboot / shutdown    restart/shut down the system".to_string(),
            "  free                 memory usage (total/used/free)".to_string(),
            "  whoami / id          current session user".to_string(),
            "  login <u> <pw> / su  log in (verify against /etc/shadow)".to_string(),
            "  sudo <cmd> / logout  run as root / reset session".to_string(),
            "  sessions             session table (3E-3 lifecycle)".to_string(),
            "  chown <uid> <path>   change file owner (3E-3)".to_string(),
            "  quota [set <uid> <blocks>]  per-user disk quota (3E-9)".to_string(),
            "  hostname             system name (from /etc/hostname)".to_string(),
            "  uname [-a/-r/-m/-n]  system info / clear".to_string(),
        ],
        "uname" => {
            let host = hostname(fs);
            match arg1 {
                "-a" => vec![format!("EuroOS {host} 0.1-alpha #1 SMP EuroKernel x86_64")],
                "-r" => vec!["0.1-alpha".to_string()],
                "-m" => vec!["x86_64".to_string()],
                "-n" => vec![host],
                "-s" | "" => vec!["EuroOS".to_string()],
                _ => vec![format!("uname: invalid option '{arg1}' (use -a/-r/-m/-n/-s)")],
            }
        }
        "hostname" => vec![hostname(fs)],
        "whoami" => vec![current_user(fs).0],
        "login" | "su" => {
            // login <user> <password> — verify via EuroID (Argon2id,
            // memory-hard) + account state + lockout + audit log. /etc/passwd only
            // still provides the POSIX uid/gid mapping for the session.
            let user = if arg1.is_empty() { "euro" } else { arg1 };
            match crate::euroid::login(user, &arg2) {
                Ok(ok) => {
                    // gid from /etc/passwd (POSIX mapping); fall back to the euroid uid.
                    let gid = crate::auth::lookup_user(fs, &ok.name).map(|(_, g)| g).unwrap_or(ok.uid);
                    // 3E-3: real session lifecycle — closes the previous session,
                    // auto-creates the user-OWNED home, sets the FS uid-context.
                    let sid = crate::session::open(fs, ok.uid, gid, &ok.name, ok.caps, if cmd == "su" { "su" } else { "login" });
                    vec![format!("logged in as {} (uid={}, gid={}, session #{sid}, EuroID-Argon2id)", ok.name, ok.uid, gid)]
                }
                Err(reason) => vec![format!("login: {reason}")],
            }
        }
        "logout" => {
            // 3E-3: close the session; the seat falls back to the default desktop user.
            crate::session::close_active();
            let (uid, caps) = crate::euroid::user_caps("euro").unwrap_or((1000, 0));
            crate::session::open(fs, uid, uid, "euro", caps, "auto");
            vec!["logged out — back to the euro session".to_string()]
        }
        "sessions" => crate::session::list_lines(),
        "chmod" => {
            // chmod <octal> <path> — change permission bits. The FS gates it:
            // only the owner or uid 0, and immutable objects stay frozen.
            let mut it = arg2.split_whitespace();
            match (u16::from_str_radix(arg1, 8), it.next()) {
                (Ok(mode), Some(path)) => match fs.chmod(path, mode) {
                    Ok(()) => vec![format!("mode of {path} -> {mode:o}")],
                    Err(e) => vec![format!("chmod: {e:?}")],
                },
                _ => vec!["usage: chmod <octal-mode> <path>".to_string()],
            }
        }
        "chown" => {
            // chown <uid> <path> — 3E-3 ownership. Reserved for the admin seat
            // (root or the wheel desktop user), like the other admin commands.
            let su = crate::auth::session_uid();
            if su != 0 && su != 1000 {
                return vec!["chown: permission denied (admin only)".to_string()];
            }
            let mut it = arg2.split_whitespace();
            match (arg1.parse::<u32>(), it.next()) {
                (Ok(uid), Some(path)) => match fs.chown(path, uid) {
                    Ok(()) => vec![format!("owner of {path} → uid {uid}")],
                    Err(e) => vec![format!("chown: {e:?}")],
                },
                _ => vec!["usage: chown <uid> <path>".to_string()],
            }
        }
        "quota" => {
            // quota                → list (uid, used, limit) on the root FS
            // quota set <uid> <n>  → set the limit to n blocks (0 = remove)
            if arg1 == "set" {
                let su = crate::auth::session_uid();
                if su != 0 && su != 1000 {
                    return vec!["quota: permission denied (admin only)".to_string()];
                }
                let mut it = arg2.split_whitespace();
                match (it.next().and_then(|s| s.parse::<u32>().ok()), it.next().and_then(|s| s.parse::<u64>().ok())) {
                    (Some(uid), Some(blocks)) => match fs.quota_set(uid, blocks) {
                        Ok(()) => vec![format!("quota for uid {uid} → {blocks} blocks ({} KiB)", blocks * 4)],
                        Err(e) => vec![format!("quota: {e:?}")],
                    },
                    _ => vec!["usage: quota set <uid> <blocks>".to_string()],
                }
            } else {
                let list = fs.quota_list();
                if list.is_empty() {
                    vec!["no quotas set and no owned blocks (quota set <uid> <blocks>)".to_string()]
                } else {
                    let mut out = vec![format!("{:<8} {:>12} {:>12}", "UID", "USED(blk)", "LIMIT(blk)")];
                    for (uid, used, limit) in list {
                        out.push(format!(
                            "{:<8} {:>12} {:>12}",
                            uid,
                            used,
                            if limit == 0 { "-".to_string() } else { format!("{limit}") }
                        ));
                    }
                    out
                }
            }
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
                format!("{} scheduler tasks, {} CPU core(s) online", n, cores),
                "  (task 0 = shell/desktop, then kernel threads + ring-3 processes)".to_string(),
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
            v.insert(0, "PCI devices:".to_string());
            v
        }
        // R: the EuroDevice model — the device tree with bound drivers per device.
        "eurodevice" | "lsdev" => crate::eurodevice::probe_lines(),
        // P3: the append-only audit trail (tamper-evident security events).
        "audit" => {
            let mut v = crate::audit::recent(20);
            v.insert(0, format!("audit log ({} events, append-only /var/log/audit.log):", crate::audit::count()));
            v
        }
        // X: EuroPol — declarative policy → capabilities.
        "europol" => crate::europol::shell(&format!("{arg1} {arg2}")),
        // W: EuroObserve — live OpenMetrics export (the way Prometheus scrapes).
        "metrics" => crate::observe::render().lines().map(String::from).collect(),
        // U: EuroVault — capability-gated secrets.
        "vault" => crate::vault::shell(&format!("{arg1} {arg2}"), if crate::auth::session_uid() == 0 { crate::vault::CAP_DB_ACCESS } else { 0 }),
        // Y: EuroCrash — the last crash dump (recovery inspection).
        "eurocrash" => match crate::crashdump::read_last() {
            Some(d) => alloc::vec![
                format!("last crash dump (seq {}): {} @ rip {:#x}", d.seq, d.vector_name(), d.rip),
                format!("  error={:#x} cr2={:#x} cr3={:#x} rflags={:#x} uptime={}ms", d.error_code, d.cr2, d.cr3, d.rflags, d.uptime_ms),
            ],
            None => alloc::vec![String::from("no crash dump (clean previous boot)")],
        },
        // N3: EuroFW — packet filter status.
        "firewall" | "eurofw" => crate::firewall::shell(),
        // N2: EuroVPN — sovereign VPN tunnel.
        "vpn" | "eurovpn" => crate::vpn::shell(),
        // AA: EuroAgent — sovereign agent-first runtime.
        "agent" | "euroagent" => crate::agent::shell(&format!("{arg1} {arg2}")),
        // CU-5: find — walk the VFS tree and filter on -name/-type/-maxdepth.
        "find" => find_walk(fs, line),
        // AH-3: run a real `.wasm` from the VFS in the no-JIT sandbox (cap-gated WASI).
        "wasm" => {
            if arg1.is_empty() {
                alloc::vec![String::from("usage: wasm <file.wasm>   (e.g. wasm /agents/demo.wasm)")]
            } else {
                crate::wasm::run_file(fs, arg1)
            }
        }
        // P1: EuroLocale — localization for the 24 EU languages.
        "locale" => crate::locale::shell(&format!("{arg1} {arg2}")),
        // 3F-7: app permission portals — list grants + audit.
        "portal" => crate::portal::shell(),
        // 3F-6: audio routing — devices, default, per-app streams.
        "audio" => crate::audio::shell(),
        // 3F-5: MIME type + default-app for a file (file-manager open, in the shell).
        "open" => crate::mime::shell(fs, arg1, arg2),
        // 3G-1: the structured system journal (persisted across reboot).
        "journal" | "journalctl" => crate::journal::shell(arg1),
        // 3F-4: show or set the keyboard layout (us / be-azerty / fr-azerty / de-qwertz).
        "keymap" => {
            if arg1.is_empty() {
                let cur = crate::ps2::layout();
                let mut out = vec![format!("active keyboard layout: {} ({})", cur.name(), cur.tag())];
                out.push("available: us · be-azerty · fr-azerty · de-qwertz".to_string());
                out.push("usage: keymap <tag>".to_string());
                out
            } else if crate::ps2::set_layout_tag(arg1) {
                vec![format!("keyboard layout → {} ({})", crate::ps2::layout().name(), crate::ps2::layout().tag())]
            } else {
                vec![format!("keymap: unknown layout '{arg1}' (us/be-azerty/fr-azerty/de-qwertz)")]
            }
        }
        // Q1/AH-1: EuroInstall — dry-run plan, or `--to N` for a REAL installation.
        "euroinstall" => crate::installer::shell(line),
        // O3: EuroCA — sovereign local certificate authority.
        "euroca" => crate::ca::shell(),
        "euroattest" => crate::attest::shell(),
        "euroidm" => crate::idm::shell(),
        // K1: EuroID — sovereign user management (eurousers add/list/show/passwd/...).
        "eurousers" => {
            let out = crate::euroid::shell(&format!("{arg1} {arg2}"), crate::auth::session_uid());
            // Make mutating actions durable: write the store back to EuroFS.
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
        // Z: EuroHealth — system health (SMART + FS + memory).
        "eurohealth" => {
            let sr = fs.scrub();
            crate::health::shell(sr.errors, sr.data_unrecoverable, ctx.mem.free_frames() as u64, ctx.mem.total_frames() as u64)
        }
        // S: EuroSnap — manage CoW snapshots.
        "eurosnap" => match arg1 {
            "create" => {
                let label = if arg2.is_empty() { "user-checkpoint" } else { arg2 };
                match fs.snapshot_create(label, eurofs::SNAP_READONLY) {
                    Ok(id) => vec![format!("snapshot #{id} '{label}' created (CoW — nearly free)")],
                    Err(e) => vec![format!("snapshot failed: {e:?}")],
                }
            }
            "rollback" => match arg2.parse::<u64>() {
                Ok(id) => match fs.snapshot_rollback(id) {
                    Ok(()) => vec![format!("rolled back to snapshot #{id} (reboot recommended)")],
                    Err(e) => vec![format!("rollback failed: {e:?}")],
                },
                Err(_) => vec!["usage: eurosnap rollback <id>".to_string()],
            },
            "delete" => match arg2.parse::<u64>() {
                Ok(id) => match fs.snapshot_delete(id) {
                    Ok(()) => vec![format!("snapshot #{id} deleted (blocks GC'd)")],
                    Err(e) => vec![format!("delete failed: {e:?}")],
                },
                Err(_) => vec!["usage: eurosnap delete <id>".to_string()],
            },
            _ => {
                let snaps = fs.snapshot_list();
                let mut v = vec![format!("snapshots ({}):", snaps.len())];
                for s in &snaps {
                    v.push(format!("  #{:<3} ckpt={:<4} '{}'", s.id, s.checkpoint_id, s.label));
                }
                v.push("commands: eurosnap create [label] | rollback <id> | delete <id>".to_string());
                v
            }
        },
        // EuroFS lives directly on the disk → changes are already persistent.
        "reboot" => crate::power::reboot(),
        "shutdown" | "poweroff" => crate::power::shutdown(),
        "uptime" => {
            let t = crate::interrupts::ticks();
            let mut out = vec![format!("uptime: {t} timer ticks (~{} s) at 100 Hz APIC timer", t / 100)];
            if crate::hpet::present() {
                out.push(format!(
                    "HPET  : {} MHz, {} ms since activation (high-resolution HAL time source)",
                    crate::hpet::freq_hz() / 1_000_000,
                    crate::hpet::ns() / 1_000_000
                ));
            }
            out
        }
        "caps" | "euroguard" => {
            // Show the NATIVE EuroOS security model: per-app CAPABILITIES
            // (least-privilege, signed) — no ambient root/sudo. Mark which
            // binaries run via the Linux COMPAT layer vs. are EuroOS-native.
            let mut out = vec![
                "EuroGuard — capability security (NATIVE EuroOS model: no ambient".to_string(),
                "  root/sudo; each app gets minimal, signed rights)".to_string(),
                String::new(),
                format!("{:<18} {:<14} {}", "PROGRAM", "ABI", "CAPABILITIES"),
            ];
            for (path, caps, linux) in crate::ring3::program_list() {
                let abi = if linux { "linux-compat" } else { "EuroOS-native" };
                out.push(format!("{:<18} {:<14} {}", path, abi, crate::ring3::cap_names(caps)));
            }
            out.push(String::new());
            out.push("network policy (EuroGuard firewall):".to_string());
            out.extend(crate::euroguard::policy_lines());
            out.push(String::new());
            out.push("(try `guard` to control per-app + geo network policy)".to_string());
            out
        }
        // The user's network-control surface (Part 1): geo-blocking, per-app
        // network policy, and the per-app traffic report.
        "guard" => guard_cmd(line),
        // The unified per-app control surface (Part 2): capabilities +
        // permission grants + network policy + traffic, all in one place, with
        // actions to restrict an app or an AI agent.
        "app" | "apps" => app_cmd(line),
        // Notifications history (click the status panel to open the centre).
        "notify" | "notifications" => {
            let mut out = vec![format!("Notifications ({}):", crate::notify::count())];
            for l in crate::notify::history_lines() { out.push(format!("  {l}")); }
            out
        }
        // The trash: `trash` lists recoverable deletes (restore/empty via the desktop menu).
        "trash" => {
            let mut out = vec![format!("Trash: {} item(s) (right-click the desktop to restore)", crate::trash::count())];
            for l in crate::trash::list_lines() { out.push(format!("  {l}")); }
            out
        }
        // The system clipboard: `clip` shows history, `clip <text>` copies.
        "clip" => {
            let rest = line.strip_prefix("clip").unwrap_or("").trim();
            if rest.is_empty() {
                let mut out = vec!["System clipboard (most recent first; * = pinned):".to_string()];
                let h = crate::clipboard::history_lines();
                if h.is_empty() { out.push("  (empty)".to_string()); } else { out.extend(h.into_iter().map(|l| format!("  {l}"))); }
                out
            } else if crate::clipboard::copy(rest) {
                vec![format!("copied to clipboard: {rest}")]
            } else {
                vec!["not copied: looks like a secret (excluded by privacy policy)".to_string()]
            }
        }
        "fsck" | "scrub" => {
            // EuroFSck (S7): verify superblock + all inode checksums + structure.
            // `fsck repair` additionally heals a degraded superblock slot from the
            // valid A/B copy (self-healing of the redundancy).
            let repairing = matches!(arg1, "repair" | "--repair" | "-r");
            let r = if repairing { fs.repair() } else { fs.scrub() };
            let mut out = vec![
                format!(
                    "EuroFSck — {}:",
                    if repairing { "integrity check + repair" } else { "integrity check (scrub)" }
                ),
                format!("  superblock           : {}", if r.superblock_ok { "OK" } else { "CORRUPT" }),
                format!("  inodes checked       : {}", r.objects),
                format!(
                    "  data checksums (XXH3): {} verified{}",
                    r.data_verified,
                    if r.data_unrecoverable > 0 {
                        format!(", {} UNRECOVERABLE (no redundancy — mirror needed)", r.data_unrecoverable)
                    } else {
                        String::new()
                    }
                ),
                format!("  data blocks (ref)    : {}", r.blocks_referenced),
                format!("  free-space bitmap    : {}", if r.bitmap_ok { "consistent" } else { "INCONSISTENT" }),
                format!("  errors               : {}", r.errors),
            ];
            if repairing {
                out.push(format!("  repaired             : {}", r.repaired));
            }
            for m in &r.messages {
                out.push(format!("  ! {m}"));
            }
            out.push(
                if r.errors == 0 && r.superblock_ok && r.bitmap_ok {
                    "  => filesystem HEALTHY ✓".to_string()
                } else if !repairing {
                    "  => PROBLEMS found (try 'fsck repair')".to_string()
                } else {
                    "  => not everything recoverable".to_string()
                },
            );
            out
        }
        "services" | "euroctl" => crate::init::status_lines(),
        "hwprobe" => crate::pci::hwprobe_lines(),
        "battery" | "power" => crate::acpi_power::status_lines(),
        "scan" => crate::scan::shell(fs, line),
        "print" => crate::print::shell(line),
        "dmesg" => {
            // Kernel message buffer (kmsg ring). `dmesg N` shows the last N lines.
            let all = crate::klog::snapshot();
            let n: usize = arg1.parse().unwrap_or(40);
            let start = all.len().saturating_sub(n);
            all[start..].to_vec()
        }
        "net" => net_selftest(),
        "netstat" => crate::net::netstat_lines(),
        "nslookup" | "resolve" => {
            if arg1.is_empty() {
                vec!["usage: nslookup <host>".to_string()]
            } else {
                match crate::net::resolve(arg1) {
                    Some(ip) => vec![format!("{arg1} = {}", fmt_ip(ip))],
                    None => vec![format!("nslookup: cannot resolve '{arg1}'")],
                }
            }
        }
        "ping" => crate::net::cmd_ping(arg1),
        "ping6" => crate::net::cmd_ping6(),
        "fetch" | "wget" => {
            if arg1.is_empty() {
                vec!["usage: fetch <host> [save-path]   (HTTP GET, optionally to a file)".to_string()]
            } else if arg2.is_empty() {
                crate::net::cmd_fetch(arg1) // display mode
            } else {
                // download-to-file: fetch and write to EuroFS (persistent).
                match crate::net::http_download(arg1, "/") {
                    Some((status, body)) => match fs.write_file(arg2, &body) {
                        Ok(_) => vec![
                            status.trim().to_string(),
                            format!("saved: {} bytes -> {arg2}", body.len()),
                        ],
                        Err(e) => vec![format!("wget: writing to {arg2} failed: {e:?}")],
                    },
                    None => vec![format!("wget: fetching {arg1} failed")],
                }
            }
        }
        "https" => {
            if arg1.is_empty() {
                vec!["usage: https <host>   (HTTPS GET via EuroTLS 1.3)".to_string()]
            } else {
                crate::net::cmd_https(arg1)
            }
        }
        "tcpserve" => {
            // Demo of the POSIX server sockets: listen, accept one connection,
            // read the first line and reply. Default port 8080.
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
                    vec!["usage: container create <name>".to_string()]
                } else {
                    // Demo container: console + file + process info, no network.
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
                    vec!["usage: container run <name>   (demonstrates chroot with a ../ escape path)".to_string()]
                } else {
                    crate::container::run(fs, arg2, "../../../etc/passwd")
                }
            }
            _ => vec!["container: list | create <name> | run <name> <path>".to_string()],
        },
        "euroupdate" | "eup" => match arg1 {
            "" | "status" => crate::update::status(fs),
            "rollback" => crate::update::rollback(fs),
            "apply" => {
                if arg2.is_empty() {
                    vec!["usage: euroupdate apply <image>   (expects <image>.sig alongside)".to_string()]
                } else {
                    crate::update::apply(fs, arg2)
                }
            }
            "fetch" => {
                if arg2.is_empty() {
                    vec!["usage: euroupdate fetch <url>   (fetches <url> + <url>.sig, verifies, stages)".to_string()]
                } else {
                    crate::update::fetch(fs, arg2)
                }
            }
            // 3E-2: check a release channel on the update server (default = the
            // SLIRP host gateway; override: euroupdate check <channel> <host:port>).
            "check" => {
                let mut it = arg2.split_whitespace();
                let channel = it.next().unwrap_or("stable");
                let (host, port) = match it.next().and_then(|hp| hp.rsplit_once(':')) {
                    Some((h, p)) => (String::from(h), p.parse().unwrap_or(8722)),
                    None => (String::from("10.0.2.2"), 8722),
                };
                crate::update::check_channel(fs, &host, port, channel)
            }
            _ => vec!["euroupdate: status | check [stable|beta] | apply <image> | fetch <url> | rollback".to_string()],
        },
        // 3E-6: the package-manager EXECUTOR (signed index + content-addressed store).
        "eupkg" => crate::pkg::eupkg_shell(fs, arg1, arg2),
        // 3E-5: GDB serial-stub attach instructions.
        "gdbstub" => crate::gdbstub::shell(),
        "euroimmutable" | "immutable" => crate::immutable::shell(fs, arg1, arg2),
        // Phase-3 FS security: on-demand system-integrity sweep.
        // 3H per-file/dir version history.
        "versions" => {
            let mut it = arg2.split_whitespace();
            let path = it.next().unwrap_or("");
            match (arg1, path) {
                ("on", p) if !p.is_empty() => {
                    let cur = fs.get_flags(p).unwrap_or(0);
                    match fs.set_flags(p, cur | eurofs::FLAG_VERSIONED) {
                        Ok(()) => vec![format!("version history ON for {p}")],
                        Err(e) => vec![format!("versions: {e:?}")],
                    }
                }
                ("off", p) if !p.is_empty() => {
                    let cur = fs.get_flags(p).unwrap_or(0);
                    match fs.set_flags(p, cur & !eurofs::FLAG_VERSIONED) {
                        Ok(()) => vec![format!("version history OFF for {p}")],
                        Err(e) => vec![format!("versions: {e:?}")],
                    }
                }
                ("list", p) if !p.is_empty() => match fs.versions(p) {
                    Ok(v) if v.is_empty() => vec![format!("{p}: no stored versions")],
                    Ok(v) => {
                        let mut out = vec![format!("{p}: {} version(s), newest first:", v.len())];
                        for (n, size, mtime) in v {
                            out.push(format!("  v{n}  {size} B  {}", crate::rtc::short_datetime(mtime)));
                        }
                        out
                    }
                    Err(e) => vec![format!("versions: {e:?}")],
                },
                ("restore", p) if !p.is_empty() => match it.next().and_then(|n| n.parse::<u32>().ok()) {
                    Some(n) => match fs.restore_version(p, n) {
                        Ok(()) => vec![format!("{p} restored to v{n} (the replaced content was preserved as a new version)")],
                        Err(e) => vec![format!("versions: {e:?}")],
                    },
                    None => vec!["usage: versions restore <path> <n>".to_string()],
                },
                _ => vec!["usage: versions on|off|list|restore <path> [n]".to_string()],
            }
        }
        "integrity" => {
            let bins = crate::system_binaries();
            crate::integrity::shell(fs, &bins)
        }
        // 3D-5 user immutability: protect/unprotect your OWN files, no cap needed.
        "euroattr" => {
            let user = crate::auth::session_name();
            let joined = if arg2.is_empty() { alloc::string::String::from(arg1) } else { alloc::format!("{arg1} {arg2}") };
            crate::euroattr::shell(&joined, &user, fs)
        }
        "lsblk" | "blkid" => crate::fatmount::lsblk(),
        "mount" => {
            if arg1.is_empty() {
                let mut out = vec!["mounted filesystems:".to_string()];
                for m in fs.list_mounts() {
                    out.push(format!("  {m}"));
                }
                out.push("usage: mount <devN> <mountpoint>   ·   umount <mountpoint>".to_string());
                out
            } else if arg1.starts_with("nfs://") {
                crate::nfsmount::mount_cmd(fs, arg1, arg2) // NFS: mount nfs://ip/export point
            } else if arg1.starts_with("//") || arg1.starts_with("\\\\") {
                crate::smbfs::mount_cmd(fs, arg1, arg2) // SMB: mount //ip/share point [user] [pass]
            } else {
                crate::fatmount::mount_cmd(fs, arg1, arg2)
            }
        }
        "umount" | "unmount" => crate::fatmount::umount_cmd(fs, arg1),
        "format" | "mkfs" => crate::fatmount::format_cmd(arg1, arg2),
        "mem" => mem_report(ctx.mem),
        "ls" => {
            let path = if arg1.is_empty() { "/" } else { arg1 };
            match fs.list_dir(path) {
                Ok(mut e) => {
                    e.sort_by(|a, b| a.name.cmp(&b.name));
                    if e.is_empty() {
                        vec![format!("{path}: (empty)")]
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
                Err(_) => vec![format!("cat: {arg1}: binary ({} bytes)", data.len())],
            },
            Err(e) => vec![format!("cat: {arg1}: {e:?}")],
        },
        "write" => {
            if arg1.is_empty() {
                return vec!["usage: write <path> <text>".to_string()];
            }
            let mut content = arg2.to_string();
            content.push('\n');
            match fs.write_file(arg1, content.as_bytes()) {
                Ok(()) => vec![format!("written: {arg1} ({} bytes)", content.len())],
                Err(e) => vec![format!("write: {arg1}: {e:?}")],
            }
        }
        "mkdir" => match fs.create_dir(arg1) {
            Ok(()) => vec![format!("directory created: {arg1}")],
            Err(e) => vec![format!("mkdir: {arg1}: {e:?}")],
        },
        "rm" => match fs.remove_file(arg1) {
            Ok(()) => vec![format!("removed: {arg1}")],
            Err(e) => vec![format!("rm: {arg1}: {e:?}")],
        },
        // CU-2: ln -s <target> <linkpath> — create a symbolic link (3C-9).
        "ln" => {
            let toks: Vec<&str> = line.split_whitespace().skip(1).collect();
            let symbolic = toks.iter().any(|t| *t == "-s");
            let pos: Vec<&str> = toks.iter().filter(|t| !t.starts_with('-')).copied().collect();
            if !symbolic {
                vec!["ln: only symbolic links are supported — use 'ln -s <target> <link>'".to_string()]
            } else if pos.len() != 2 {
                vec!["usage: ln -s <target> <linkpath>".to_string()]
            } else {
                match fs.create_symlink(pos[1], pos[0]) {
                    Ok(()) => vec![format!("'{}' -> '{}'", pos[1], pos[0])],
                    Err(e) => vec![format!("ln: {}: {e:?}", pos[1])],
                }
            }
        }
        // CU-2: readlink <path> — print a symlink's target (no following).
        "readlink" => match fs.read_link(arg1) {
            Ok(t) => vec![t],
            Err(e) => vec![format!("readlink: {arg1}: {e:?}")],
        },
        // CU-2: realpath <path> — follow a final symlink to its target, else echo the path.
        "realpath" => {
            if let Ok(t) = fs.read_link(arg1) {
                // Absolute target is canonical; a relative one resolves against the link's dir.
                if t.starts_with('/') {
                    vec![t]
                } else {
                    let dir = arg1.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
                    vec![format!("{dir}/{t}")]
                }
            } else if fs.exists(arg1) {
                vec![arg1.to_string()]
            } else {
                vec![format!("realpath: {arg1}: NotFound")]
            }
        }
        // CU-2: mktemp — create a uniquely-named empty file under /tmp and print its path.
        "mktemp" => {
            let _ = fs.create_dir("/tmp");
            let stamp = crate::rtc::epoch() ^ (crate::interrupts::ticks().wrapping_mul(2654435761));
            let path = format!("/tmp/tmp.{:08x}", stamp & 0xFFFF_FFFF);
            match fs.write_file(&path, b"") {
                Ok(()) => vec![path],
                Err(e) => vec![format!("mktemp: {e:?}")],
            }
        }
        // CU-7: env / printenv — show the system environment.
        "env" | "printenv" => {
            let uid = crate::auth::session_uid();
            let user = crate::auth::name_for_uid(fs, uid);
            let home = if uid == 0 { String::from("/root") } else { format!("/home/{user}") };
            let lang = fs.read_file("/etc/locale.conf").ok().and_then(|d| String::from_utf8(d).ok())
                .and_then(|s| s.lines().find_map(|l| l.strip_prefix("LANG=").map(String::from)))
                .unwrap_or_else(|| String::from("en_EU.UTF-8"));
            let envv = [
                format!("PATH=/bin"),
                format!("HOME={home}"),
                format!("USER={user}"),
                format!("SHELL=/bin/eurosh"),
                format!("TERM=euroterm"),
                format!("LANG={lang}"),
            ];
            let toks: Vec<&str> = line.split_whitespace().skip(1).collect();
            if cmd == "printenv" && !toks.is_empty() {
                // printenv VAR → just that variable's value
                let key = format!("{}=", toks[0]);
                envv.iter().find_map(|e| e.strip_prefix(&key).map(String::from)).into_iter().collect()
            } else {
                envv.to_vec()
            }
        }
        // CU-4: comm / join — two SORTED file inputs. The two inputs are the last two
        // tokens that name readable files; everything else (in order) is passed through
        // as args so value-options like `-1 N` / `-t C` survive (don't split on '-').
        "comm" | "join" => {
            let toks: Vec<&str> = line.split_whitespace().skip(1).collect();
            let file_idxs: Vec<usize> = toks.iter().enumerate().filter(|(_, t)| fs.exists(t)).map(|(i, _)| i).collect();
            if file_idxs.len() < 2 {
                vec![format!("usage: {cmd} [opts] <file1> <file2>")]
            } else {
                let fa = file_idxs[file_idxs.len() - 2];
                let fb = file_idxs[file_idxs.len() - 1];
                match (fs.read_file(toks[fa]), fs.read_file(toks[fb])) {
                    (Ok(a), Ok(b)) => {
                        let args: Vec<&str> = toks.iter().enumerate().filter(|(i, _)| *i != fa && *i != fb).map(|(_, t)| *t).collect();
                        let out = if cmd == "comm" {
                            eurocoreutils::compare::comm(&args, &a, &b)
                        } else {
                            eurocoreutils::compare::join(&args, &a, &b)
                        };
                        render_bytes(out)
                    }
                    _ => vec![format!("{cmd}: cannot read both input files")],
                }
            }
        }
        // CU-4: split — break a file into pieces written back to the FS.
        "split" => {
            let toks: Vec<&str> = line.split_whitespace().skip(1).collect();
            let infile = toks.iter().rev().find(|t| !t.starts_with('-') && fs.exists(t)).copied();
            match infile.map(|f| fs.read_file(f)) {
                Some(Ok(data)) => {
                    let cargs: Vec<&str> = toks.iter().filter(|t| Some(**t) != infile).copied().collect();
                    let pieces = eurocoreutils::compare::split(&cargs, &data);
                    let mut out = Vec::new();
                    for (name, chunk) in &pieces {
                        let p = format!("/{name}");
                        match fs.write_file(&p, chunk) {
                            Ok(()) => out.push(format!("{p} ({} bytes)", chunk.len())),
                            Err(e) => out.push(format!("split: {p}: {e:?}")),
                        }
                    }
                    if out.is_empty() { vec!["split: no output".to_string()] } else { out }
                }
                _ => vec!["usage: split [-l N|-b N] <file> [prefix]".to_string()],
            }
        }
        // CU-2: cp — copy arg1 → arg2 (on the EuroFS primitives).
        "cp" => match fs.read_file(arg1) {
            Ok(data) => match fs.write_file(arg2, &data) {
                Ok(()) => vec![format!("'{arg1}' -> '{arg2}' ({} bytes)", data.len())],
                Err(e) => vec![format!("cp: {arg2}: {e:?}")],
            },
            Err(e) => vec![format!("cp: {arg1}: {e:?}")],
        },
        // CU-2: touch — create an empty file (or leave an existing one untouched).
        "touch" => {
            if fs.exists(arg1) {
                vec![format!("touch: {arg1} already exists (mtime update n/a)")]
            } else {
                match fs.write_file(arg1, b"") {
                    Ok(()) => vec![format!("empty file created: {arg1}")],
                    Err(e) => vec![format!("touch: {arg1}: {e:?}")],
                }
            }
        }
        // CU-2: stat — show file/directory metadata (+ EuroOS extra: immutability flags).
        "stat" => match fs.metadata(arg1) {
            Ok(m) => {
                let kind = match m.kind {
                    EntryKind::File => "regular file",
                    EntryKind::Directory => "directory",
                    EntryKind::Symlink => "symlink",
                };
                let flags = fs.get_flags(arg1).unwrap_or(0);
                let imm = if flags & eurofs::FLAG_IMMUTABLE != 0 { " IMMUTABLE" } else { "" };
                let app = if flags & eurofs::FLAG_APPEND_ONLY != 0 { " APPEND_ONLY" } else { "" };
                vec![
                    format!("  File: {arg1}"),
                    format!("  Size: {}  Type: {kind}  Mode: {:#o}", m.size, m.mode),
                    format!("  Modified: {}  Flags:{}{}", m.mtime, if imm.is_empty() && app.is_empty() { " (none)" } else { "" }, format!("{imm}{app}")),
                ]
            }
            Err(e) => vec![format!("stat: {arg1}: {e:?}")],
        },
        // CU-2: truncate -s N <file> — shrink/extend to N bytes.
        "truncate" => {
            // usage: truncate -s <N> <file>
            let toks: Vec<&str> = line.split_whitespace().skip(1).collect();
            let size = toks.iter().position(|t| *t == "-s").and_then(|i| toks.get(i + 1)).and_then(|v| v.parse::<usize>().ok());
            let file = toks.iter().rev().find(|t| !t.starts_with('-') && t.parse::<usize>().is_err());
            match (size, file) {
                (Some(n), Some(f)) => {
                    let mut data = fs.read_file(f).unwrap_or_default();
                    data.resize(n, 0);
                    match fs.write_file(f, &data) {
                        Ok(()) => vec![format!("'{f}' truncated/extended to {n} bytes")],
                        Err(e) => vec![format!("truncate: {f}: {e:?}")],
                    }
                }
                _ => vec!["usage: truncate -s <bytes> <file>".to_string()],
            }
        }
        "mv" | "rename" => {
            if arg1.is_empty() || arg2.is_empty() {
                vec!["usage: mv <src> <dst>".to_string()]
            } else {
                match fs.rename(arg1, arg2) {
                    Ok(()) => vec![format!("{arg1} -> {arg2}")],
                    Err(e) => vec![format!("mv: {e:?}")],
                }
            }
        }
        "rmdir" => match fs.remove_dir(arg1) {
            Ok(()) => vec![format!("directory removed: {arg1}")],
            Err(e) => vec![format!("rmdir: {arg1}: {e:?}")],
        },
        "df" => {
            let (total, free) = fs.space_info();
            vec![format!(
                "EuroFS: {} KiB total, {} KiB free, {} KiB used",
                total / 1024,
                free / 1024,
                (total - free) / 1024
            )]
        }
        "fsdebug" => {
            let path = if arg1.is_empty() { "/" } else { arg1 };
            match fs.alloc_debug(path) {
                Some(s) => s.lines().map(|l| l.to_string()).collect(),
                None => vec![format!("fsdebug: {path}: not supported by this filesystem")],
            }
        }
        "clear" => vec!["\x0c".to_string()], // signal for main to clear
        other => vec![format!("unknown command: {other}  (type 'help')")],
    }
}

/// Show RAM statistics and demonstrate the frame allocator (alloc + free).
/// Read the hostname from /etc/hostname (fallback "eurokernel").
fn hostname(fs: &mut dyn FileSystem) -> String {
    fs.read_file("/etc/hostname")
        .ok()
        .and_then(|d| String::from_utf8(d).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "eurokernel".to_string())
}

/// The ACTIVE session user (S5): uid/gid from the auth session, name from /etc/passwd.
fn current_user(fs: &mut dyn FileSystem) -> (String, u32, u32) {
    let uid = crate::auth::session_uid();
    let gid = crate::auth::session_gid();
    (crate::auth::name_for_uid(fs, uid), uid, gid)
}

fn mem_report(mem: &mut FrameAllocator) -> Vec<String> {
    let mut out = vec![
        format!(
            "RAM   : {} MiB usable, {} MiB free  ({} frames of 4 KiB)",
            mem.usable_bytes() / (1024 * 1024),
            mem.free_bytes() / (1024 * 1024),
            mem.usable_frames()
        ),
        format!(
            "frames: {} free of {} usable",
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
        "free  : released; {} -> {} -> {} free frames (alloc/free OK)",
        before,
        before - got.len(),
        mem.free_frames()
    ));
    // S6 memory hardening: stack-guard canaries + frame-allocator diagnostics.
    out.push(format!(
        "hardening: stack-guard ON (canary/kernel task, checked at switch); double-frees: {}; peak: {} MiB",
        mem.double_frees(),
        mem.high_water_frames() * 4096 / (1024 * 1024)
    ));
    // CPU protection: SMEP (ring 0 does not execute user code) + SMAP (ring 0 does not
    // touch user memory outside a short, non-preemptive syscall window).
    out.push(format!(
        "cpu-protection: SMEP {} · SMAP {} · W^X/NX {} (CR4) — user access via AC window per syscall; code R-X, data/stack NX",
        if crate::ring3::smep_active() { "ON" } else { "n/a" },
        if crate::ring3::smap_active() { "ON" } else { "n/a" },
        if crate::ring3::nx_active() { "ON" } else { "n/a" },
    ));
    out
}

fn fmt_ip(ip: euronet::Ipv4Addr) -> String {
    format!("{}.{}.{}.{}", ip.0[0], ip.0[1], ip.0[2], ip.0[3])
}

/// POSIX permissions as a `drwxr-xr-x` string (type bit + 9 rwx bits).
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

/// Unix epoch (seconds, UTC) → `YYYY-MM-DD HH:MM`. 0 = unknown.
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

/// Builds and parses real packets through the whole stack and reports per layer.
/// Proves that EuroNet also works no_std in the kernel (same code as host tests).
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

    // ARP: parse a request "who has 10.0.2.15?" and build a reply.
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
        "ARP  : {} asks {}  ->  reply: {} is-at {}",
        fmt_ip(arp.sender_ip),
        fmt_ip(arp.target_ip),
        fmt_ip(reply.sender_ip),
        fmt_mac(reply.sender_mac)
    ));

    // ICMP: echo-request -> reply, checksum verified by parse().
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
        if ok_icmp { "OK" } else { "BAD" }
    ));

    // IPv4 + UDP: build a complete DNS query and parse it back.
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

    // Full receive chain: Ethernet -> IPv4 -> UDP.
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
        "Frame: ethertype {:?}, {} bytes, fully parsed  [OK]",
        eth_h.ethertype,
        frame.len()
    ));
    out
}

/// EuroCoreutils dispatch: GNU-compatible coreutils as shell built-ins. The last
/// existing file argument is read as input (stdin replacement); the remaining
/// tokens are the options. Returns None if `cmd` is not a coreutils command.
/// The `guard` command family: the user's per-app + geo network-control surface.
///   guard                         overview: policy + per-app report
///   guard report                  the per-app traffic report (app -> country/IP/bytes)
///   guard block-country <CC>      block all traffic to a country (e.g. CN)
///   guard unblock-country <CC>    lift a country block
///   guard app <name> block        cut an app off the network entirely
///   guard app <name> allow        remove an app's per-app restriction
///   guard app <name> only <cidr>  restrict an app to one CIDR (repeatable)
fn guard_cmd(line: &str) -> Vec<String> {
    use crate::euroguard::{self, AppNet};
    let mut t = line.split_whitespace();
    t.next(); // "guard"
    match t.next() {
        None => {
            let mut out = euroguard::policy_lines();
            out.push(String::new());
            out.extend(euroguard::app_report_lines());
            out
        }
        Some("report") => euroguard::app_report_lines(),
        Some("block-country") => match t.next() {
            Some(cc) => {
                euroguard::set_country_blocked(cc, true);
                vec![format!("blocking all traffic to {} — connections to that country are now refused", cc.to_ascii_uppercase())]
            }
            None => vec!["usage: guard block-country <CC>   (e.g. CN, RU)".to_string()],
        },
        Some("unblock-country") => match t.next() {
            Some(cc) => {
                euroguard::set_country_blocked(cc, false);
                vec![format!("unblocked {}", cc.to_ascii_uppercase())]
            }
            None => vec!["usage: guard unblock-country <CC>".to_string()],
        },
        Some("app") => {
            let name = match t.next() {
                Some(n) => n,
                None => return vec!["usage: guard app <name> <block|allow|only <cidr>>".to_string()],
            };
            match t.next() {
                Some("block") => {
                    euroguard::set_app_net(name, AppNet::Blocked);
                    vec![format!("'{name}' is cut off from the network (all outbound connections refused)")]
                }
                Some("allow") => {
                    euroguard::set_app_net(name, AppNet::Default);
                    vec![format!("'{name}' back to default (system + geo rules only)")]
                }
                Some("only") => {
                    let mut nets = Vec::new();
                    for c in t {
                        if let Some((net, prefix)) = parse_cidr(c) {
                            nets.push((net, prefix));
                        }
                    }
                    if nets.is_empty() {
                        return vec!["usage: guard app <name> only <cidr> [<cidr>...]   (e.g. 10.0.0.0/8)".to_string()];
                    }
                    let n = nets.len();
                    euroguard::set_app_net(name, AppNet::AllowOnly(nets));
                    vec![format!("'{name}' restricted to {n} allow-listed network(s); everything else refused")]
                }
                _ => vec![format!("usage: guard app {name} <block|allow|only <cidr>>")],
            }
        }
        Some(other) => vec![format!("unknown: guard {other}   (try: report, block-country, app)")],
    }
}

/// `[7app]` boot self-test — the unified per-app control surface on the live
/// kernel: the `apps` roster lists real programs with their capabilities, and
/// `app <name>` renders capabilities + permission grants + network policy +
/// traffic as one screen, with the network clamp taking effect. Proves the exact
/// code path the shell command runs (not a mock).
pub fn selftest() {
    use crate::euroguard::{self, AppNet};

    // Seed a network clamp on a throwaway app (the "restrict this agent" action).
    euroguard::set_app_net("selftest-agent", AppNet::Blocked);

    // (1) The roster lists every program with its capabilities column.
    let roster = app_cmd("apps");
    let roster_ok = roster.iter().any(|l| l.contains("CAPABILITIES"))
        && roster.iter().filter(|l| l.contains("linux-compat") || l.contains("EuroOS-native")).count() >= 1;

    // (2) The single-app screen fuses caps + permissions + network into one view,
    //     and the clamp we set is visible.
    let screen = app_cmd("app selftest-agent");
    let has_caps = screen.iter().any(|l| l.contains("capabilities") || l.contains("identity"));
    let has_perms = screen.iter().any(|l| l.contains("permissions"));
    let clamp_shown = screen.iter().any(|l| l.contains("network policy") && l.contains("BLOCKED"));

    // (3) `revoke` is a clean no-op on an app with no grants (does not panic).
    let revoke_ok = {
        let out = app_cmd("app selftest-agent revoke");
        out.iter().any(|l| l.contains("revoked"))
    };

    // Restore: lift the throwaway clamp so it never affects the running system.
    euroguard::set_app_net("selftest-agent", AppNet::Default);

    let ok = roster_ok && has_caps && has_perms && clamp_shown && revoke_ok;
    crate::serial_println!(
        "[7app] Unified app control: roster-with-caps={roster_ok}, one-screen(caps+perms+net)={}, network-clamp-visible={clamp_shown}, revoke-permissions={revoke_ok} → {}",
        has_caps && has_perms,
        if ok { "OK (total per-app control: rights + network in one place, clamp an AI agent) ✓" } else { "FAILED ✗" }
    );
}

/// The unified per-app control surface (Part 2): one place to see and control
/// EVERYTHING an app can do, its capabilities, its permission grants, and its
/// network policy + traffic, plus actions to restrict it (the AI-agent clamp).
///   apps                      list every app with its caps + network policy
///   app <name>                the full control screen for one app
///   app <name> revoke         revoke all of the app's permission grants
///   app <name> net <block|allow|only <cidr>>   set its network policy
fn app_cmd(line: &str) -> Vec<String> {
    let mut t = line.split_whitespace();
    let cmd = t.next().unwrap_or("app"); // "app" or "apps"
    let name = t.next();

    // `apps` (or `app` with no name): the roster, caps + network policy per app.
    if cmd == "apps" || name.is_none() {
        let mut out = vec![
            "Applications — capabilities + network policy (control any of them with `app <name>`)".to_string(),
            String::new(),
            format!("{:<18} {:<14} {:<22} {}", "APP", "ABI", "CAPABILITIES", "NETWORK"),
        ];
        for (path, caps, linux) in crate::ring3::program_list() {
            let abi = if linux { "linux-compat" } else { "EuroOS-native" };
            let net = crate::euroguard::app_net_label(&path);
            out.push(format!("{:<18} {:<14} {:<22} {}", path, abi, crate::ring3::cap_names(caps), net));
        }
        return out;
    }

    let name = name.unwrap();
    // Actions.
    match t.next() {
        Some("revoke") => {
            let n = crate::portal::revoke_app(name);
            return vec![format!("revoked {n} permission grant(s) from '{name}' (it must ask again next time)")];
        }
        Some("net") => {
            // Delegate to the same engine as `guard app ...`.
            let rest: alloc::vec::Vec<&str> = t.collect();
            return guard_cmd(&format!("guard app {name} {}", rest.join(" ")));
        }
        Some(other) => return vec![format!("unknown action '{other}' (try: revoke, net <block|allow|only <cidr>>)")],
        None => {}
    }

    // The full control screen for one app.
    let mut out = vec![format!("── {name} ──────────────────────────────")];
    // Capabilities (least-privilege, signed) from the program table.
    let mut found = false;
    for (path, caps, linux) in crate::ring3::program_list() {
        if path == name || path.ends_with(name) {
            found = true;
            out.push(format!("  identity:     {} ({})", path, if linux { "Linux-compat binary" } else { "EuroOS-native" }));
            out.push(format!("  capabilities: {}  (least-privilege, signed; what it needs to run)", crate::ring3::cap_names(caps)));
            break;
        }
    }
    if !found {
        out.push("  identity:     (not a registered program — showing runtime policy only)".to_string());
    }
    // Permission grants (camera/mic/files/... via the portal).
    let grants = crate::portal::grant_lines_for(name);
    if grants.is_empty() {
        out.push("  permissions:  no grants (it must ask, and you allow/deny)".to_string());
    } else {
        out.push("  permissions:  (granted via the permission portal)".to_string());
        for g in grants {
            out.push(format!("    · {g}"));
        }
    }
    // Network policy + live traffic.
    out.push(String::new());
    out.extend(crate::euroguard::app_summary_lines(name));
    out.push(String::new());
    out.push("  control it:   app ".to_string() + name + " revoke   ·   app " + name + " net block|allow|only <cidr>");
    out
}

/// Parse `a.b.c.d/prefix` into (host-byte-order net, prefix).
fn parse_cidr(s: &str) -> Option<(u32, u8)> {
    let (addr, pfx) = s.split_once('/')?;
    let ip = crate::net::parse_ipv4(addr)?;
    let prefix: u8 = pfx.parse().ok()?;
    if prefix > 32 {
        return None;
    }
    Some((u32::from_be_bytes(ip.0), prefix))
}

fn coreutils(cmd: &str, line: &str, fs: &mut dyn FileSystem) -> Option<Vec<String>> {
    use eurocoreutils as cu;
    let toks: Vec<&str> = line.split_whitespace().skip(1).collect();

    // CU-7: arg-only compute/control commands (no file input; an arg that
    // happens to be a filename must not be swallowed as stdin here).
    match cmd {
        "printf" => return Some(render_bytes(cu::compute::printf(&toks))),
        "expr" => return Some(render_bytes(cu::compute::expr(&toks).0)),
        "numfmt" => return Some(render_bytes(cu::compute::numfmt(&toks))),
        "factor" => return Some(render_bytes(cu::compute::factor(&toks))),
        "test" | "[" => {
            let code = cu::compute::test(&toks);
            return Some(vec![alloc::format!("test: {}", if code == 0 { "true (exit 0)" } else { "false (exit 1)" })]);
        }
        _ => {}
    }

    // Search (from the back) for a positional token that is a readable file → input.
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
        "sha1sum" => cu::checksum::sha1sum(&input, name),
        "md5sum" => cu::checksum::md5sum(&input, name),
        "b2sum" => cu::checksum::b2sum(&input, name),
        "shuf" => cu::text::shuf(&cargs, &input),
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

/// Convert raw coreutils output bytes into shell lines.
fn render_bytes(out: Vec<u8>) -> Vec<String> {
    String::from_utf8_lossy(&out).lines().map(String::from).collect()
}

/// Apply a coreutils *filter* to stdin bytes (the pipeline role). Returns `None`
/// if `cmd` is not a filter that processes stdin.
pub(crate) fn coreutils_filter(cmd: &str, args: &[&str], input: &[u8]) -> Option<Vec<u8>> {
    use eurocoreutils as cu;
    let out = match cmd {
        "cat" => input.to_vec(), // identity in a pipeline
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
        "sha1sum" => cu::checksum::sha1sum(input, "-"),
        "md5sum" => cu::checksum::md5sum(input, "-"),
        "b2sum" => cu::checksum::b2sum(input, "-"),
        "shuf" => cu::text::shuf(args, input),
        "cksum" => cu::encoding::cksum(input, "-"),
        _ => return None,
    };
    Some(out)
}

/// Is `line` a pipeline of ≥2 stages?
pub(crate) fn is_pipeline(line: &str) -> bool {
    line.split('|').filter(|s| !s.trim().is_empty()).count() >= 2 && line.contains('|')
}

/// Run a pipeline `A | B | C`: stage 0 via the normal shell (may read a file,
/// `echo`, `ls`, …), each following stage as a coreutils filter on the bytes
/// of the previous one. Stdout of stage N → stdin of stage N+1.
pub(crate) fn run_pipeline(ctx: &mut ShellCtx, line: &str) -> Vec<String> {
    let stages: Vec<&str> = line.split('|').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if stages.len() < 2 {
        return exec(ctx, line);
    }
    // Stage 0: run via the normal exec and take its output as the initial stream.
    let mut bytes: Vec<u8> = exec(ctx, stages[0]).join("\n").into_bytes();
    if !bytes.is_empty() {
        bytes.push(b'\n');
    }
    for st in &stages[1..] {
        let toks: Vec<&str> = st.split_whitespace().collect();
        let Some(cmd) = toks.first().copied() else { continue };
        // `tee FILE`: write the current stream to FILE and pass it on unchanged.
        if cmd == "tee" {
            if let Some(fname) = toks.get(1) {
                let _ = ctx.fs.write_file(fname, &bytes);
            }
            continue;
        }
        // `xargs [-nN] [CMD [args...]]`: build a command from the stdin tokens and
        // run it (default CMD = echo). Needs `ctx` → handled separately.
        if cmd == "xargs" {
            bytes = run_xargs(ctx, &toks[1..], &bytes);
            continue;
        }
        match coreutils_filter(cmd, &toks[1..], &bytes) {
            Some(o) => bytes = o,
            None => return alloc::vec![alloc::format!("{cmd}: does not process pipeline input (not a filter)")],
        }
    }
    render_bytes(bytes)
}

/// `xargs [-n N] [CMD [args...]]` — read the stdin bytes, split them into tokens
/// (whitespace/newlines), and run `CMD args... tokens...` via the shell.
/// With `-n N` it runs the CMD per batch of N tokens; without CMD it is `echo`.
/// The output of all invocations is concatenated and returned.
fn run_xargs(ctx: &mut ShellCtx, args: &[&str], input: &[u8]) -> Vec<u8> {
    // Parse `-n N` / `-nN`; the rest is CMD + initial args.
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

/// `find [START] [-name GLOB] [-type f|d] [-maxdepth N]` — walk the VFS tree
/// from `START` (default `/`) and print every path that matches the filters. The
/// match logic is host-tested in `eurocoreutils::find`; here we do the walking.
pub(crate) fn find_walk(fs: &mut dyn FileSystem, line: &str) -> Vec<String> {
    use eurocoreutils::find::FindOpts;
    let toks: Vec<&str> = line.split_whitespace().skip(1).collect();
    let opts = FindOpts::parse(&toks);
    let mut start = FindOpts::start_path(&toks);
    if start == "." {
        start = String::from("/");
    }
    let mut out: Vec<String> = Vec::new();
    // The start path itself counts as depth 0 (if it matches).
    let start_name = start.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("/");
    if opts.matches(start_name, true, 0) {
        out.push(start.clone());
    }
    let mut stack: Vec<(String, usize)> = alloc::vec![(start, 1usize)];
    let mut budget = 4096; // safety limit against a degenerate tree
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
                out.push(String::from("find: (truncated — too many items)"));
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
        out.push(String::from("find: no matches"));
    }
    out
}
