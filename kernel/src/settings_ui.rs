//! EuroBeheer — the settings panel of EuroOS.
//!
//! A section list on the left, and on the right a form (`euroui::form`) built
//! from the REAL, live kernel state. Every control changes that state at once
//! and saves it through `prefs` (see `docs/SETTINGS.md` for the files and
//! keys); every control carries a short explanation of what it does and what
//! follows from it. Nothing is offered that does not work: what a release
//! cannot do yet is stated as text, not shown as a dead switch.
//!
//! `main.rs` forwards input with three calls: [`click`], [`key`] and [`tick`].
//! The compositor calls [`render`], which has no filesystem access; what needs
//! the filesystem (host name, disk usage, update policy) is read into a small
//! snapshot by `click`/`key`/`tick`.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use alloc::string::String;
use alloc::vec::Vec;
use eurofs::FileSystem;
use eurokeymap::Layout;
use eurolocale::{Lang, Locale};
use europrefs::{tz, validate};
use spin::Mutex;

use crate::euroui::form::{self, Event, FormState, Id, Item, Rect};
use crate::graphics::{Color, FrameBuffer};
use crate::prefs;
use crate::ps2::Key;
use crate::text;

const TITLEBAR_H: usize = 44;
const NAV_W: usize = 190;
const NAV_ROW: usize = 40;
const HEADER_H: usize = 70;
/// Room under the form for the result of the last action (two lines).
const FOOTER_H: usize = 44;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sec {
    System,
    Display,
    Language,
    DateTime,
    Network,
    EuroGuard,
    Apps,
    Storage,
    Updates,
}

/// The sections in navigation order: (section, name, one-line description).
const NAV: [(Sec, &str, &str); 9] = [
    (Sec::System, "System", "This computer: name, version, live figures."),
    (Sec::Display, "Display", "Desktop background and text size."),
    (Sec::Language, "Language & keyboard", "Keyboard layout and regional formats."),
    (Sec::DateTime, "Date & time", "Time zone and clock format."),
    (Sec::Network, "Network", "Connection state and the built-in web server."),
    (Sec::EuroGuard, "EuroGuard", "System-wide rules for what apps may reach."),
    (Sec::Apps, "Apps", "What each app can access, and how to limit it."),
    (Sec::Storage, "Storage", "Disks, usage and the integrity check."),
    (Sec::Updates, "Updates", "Version, update policy and manual check."),
];

// Item ids (unique across sections, so an event names its control).
const ID_HOSTNAME: Id = 10;
const ID_WALLPAPER: Id = 20;
const ID_TEXTSCALE: Id = 21;
const ID_KEYMAP: Id = 30;
const ID_KEYTEST: Id = 31;
const ID_REGION: Id = 32;
const ID_ZONE: Id = 40;
const ID_24H: Id = 41;
const ID_HTTPD: Id = 50;
const ID_DOMAIN: Id = 60;
const ID_DOMLIST: Id = 61;
const ID_UNBLOCK: Id = 62;
const ID_APPLIST: Id = 70;
const ID_APPNET: Id = 71;
const ID_REVOKE: Id = 72;
const ID_SCRUB: Id = 80;
const ID_POLICY: Id = 90;
const ID_CHECK: Id = 91;

/// Index into [`NAV`] of the section shown.
static SECTION: AtomicUsize = AtomicUsize::new(0);
static FORM: Mutex<FormState> = Mutex::new(FormState::new());
static SELECTED_APP: AtomicUsize = AtomicUsize::new(0);
static SELECTED_DOMAIN: AtomicUsize = AtomicUsize::new(0);
/// Result of the last action, shown under the section it belongs to.
static STATUS: Mutex<Option<(usize, String)>> = Mutex::new(None);
/// Screen size, noted at render (the Display section shows it).
/// Result of the last check started from the panel (the log file is the
/// first source; this covers a log that cannot be read).
static LAST_MANUAL_SCRUB: Mutex<String> = Mutex::new(String::new());
static SCREEN_W: AtomicUsize = AtomicUsize::new(0);
static SCREEN_H: AtomicUsize = AtomicUsize::new(0);
/// Tick of the last render / snapshot refresh (the snapshot only refreshes
/// while the panel is actually being shown).
static LAST_RENDER: AtomicU64 = AtomicU64::new(0);
static LAST_REFRESH: AtomicU64 = AtomicU64::new(0);

/// What `render` needs from the filesystem, read by `refresh`.
struct Snap {
    loaded: bool,
    hostname: String,
    user: String,
    live: bool,
    df: Vec<(String, u64, u64)>,
    scrub_last: String,
    policy: usize,
}

static SNAP: Mutex<Snap> = Mutex::new(Snap {
    loaded: false,
    hostname: String::new(),
    user: String::new(),
    live: true,
    df: Vec::new(),
    scrub_last: String::new(),
    policy: 1,
});

const POLICY_NAMES: [&str; 3] = ["auto", "ask", "manual"];

fn read_trimmed(fs: &mut dyn FileSystem, path: &str) -> Option<String> {
    crate::sysctx::as_system(fs, |fs| fs.read_file(path)).ok().map(|b| String::from(String::from_utf8_lossy(&b).trim()))
}

/// Re-read the filesystem-backed values the panel shows.
fn refresh(fs: &mut dyn FileSystem) {
    let hostname = read_trimmed(fs, "/etc/hostname").filter(|h| !h.is_empty()).unwrap_or_else(|| String::from("eurokernel"));
    let user = crate::session::active().map(|s| s.name).unwrap_or_default();
    let df = fs.df();
    let scrub_last = read_trimmed(fs, "/var/log/fsck.log").and_then(|log| log.lines().last().map(String::from)).unwrap_or_default();
    let policy = match crate::sysctx::as_system(fs, |fs| crate::update::policy(fs)) {
        crate::update::Policy::Auto => 0,
        crate::update::Policy::Ask => 1,
        crate::update::Policy::Manual => 2,
    };
    let mut s = SNAP.lock();
    *s = Snap { loaded: true, hostname, user, live: !crate::rootblk::booted_from_disk(), df, scrub_last, policy };
    LAST_REFRESH.store(crate::interrupts::ticks(), Ordering::Relaxed);
}

fn sec() -> Sec {
    NAV[SECTION.load(Ordering::Relaxed) % NAV.len()].0
}

fn show(index: usize) {
    if index < NAV.len() && index != SECTION.load(Ordering::Relaxed) {
        SECTION.store(index, Ordering::Relaxed);
        FORM.lock().reset();
        *STATUS.lock() = None;
    }
}

fn show_sec(s: Sec) {
    if let Some(i) = NAV.iter().position(|n| n.0 == s) {
        show(i);
    }
}

fn set_status(msg: String) {
    *STATUS.lock() = Some((SECTION.load(Ordering::Relaxed), msg));
}

// ── Compatibility with the existing main.rs call sites ─────────────────────
// The click and key paths now go through `click` and `key`; these keep the
// older calls compiling and meaningful.

/// Legacy section id: 0 = EuroGuard, 1 = Apps, 2 = Network, 3 = Display.
pub fn set_section(i: usize) {
    match i {
        0 => show_sec(Sec::EuroGuard),
        1 => show_sec(Sec::Apps),
        2 => show_sec(Sec::Network),
        3 => show_sec(Sec::Display),
        _ => {}
    }
}
/// Legacy section id of the section shown (see [`set_section`]); other
/// sections report `usize::MAX`.
pub fn section() -> usize {
    match sec() {
        Sec::EuroGuard => 0,
        Sec::Apps => 1,
        Sec::Network => 2,
        Sec::Display => 3,
        _ => usize::MAX,
    }
}
pub fn editing() -> bool {
    FORM.lock().editing()
}
pub fn begin_domain_edit() {}
pub fn edit_key(_ch: char) -> Option<String> {
    None
}
pub fn domain_field_at(_wx: usize, _wy: usize, _mx: usize, _my: usize) -> bool {
    false
}
pub fn nav_at(_wx: usize, _wy: usize, _mx: usize, _my: usize) -> Option<usize> {
    None
}
pub fn toggle_at(_wx: usize, _wy: usize, _mx: usize, _my: usize) -> bool {
    false
}
pub fn app_row_at(_wx: usize, _wy: usize, _mx: usize, _my: usize) -> Option<usize> {
    None
}
pub fn app_net_toggle_at(_wx: usize, _wy: usize, _mx: usize, _my: usize) -> bool {
    false
}
pub fn app_revoke_at(_wx: usize, _wy: usize, _mx: usize, _my: usize) -> bool {
    false
}
/// Toggle the HTTP server (real kernel action) and return the new state.
pub fn toggle_httpd() -> bool {
    crate::net::httpd_toggle()
}

// ── Apps ───────────────────────────────────────────────────────────────────

/// The app roster (name, caps, linux-compat) — the same list the `apps` shell
/// command shows. Stable order, so a row index maps to a fixed app.
fn roster() -> Vec<(String, u64, bool)> {
    crate::ring3::program_list()
}

fn selected_app_name() -> Option<String> {
    let r = roster();
    if r.is_empty() {
        return None;
    }
    let i = SELECTED_APP.load(Ordering::Relaxed).min(r.len() - 1);
    Some(r[i].0.clone())
}

pub fn select_app(i: usize) {
    SELECTED_APP.store(i, Ordering::Relaxed);
}

/// Toggle the selected app between full network block and default (REAL action).
/// Returns the new blocked state.
pub fn toggle_app_net() -> bool {
    let Some(name) = selected_app_name() else { return false };
    let now_blocked = crate::euroguard::app_is_blocked(&name);
    let rule = if now_blocked { crate::euroguard::AppNet::Default } else { crate::euroguard::AppNet::Blocked };
    crate::euroguard::set_app_net(&name, rule);
    !now_blocked
}

/// Revoke all of the selected app's permission grants (REAL action).
pub fn revoke_app_perms() -> usize {
    match selected_app_name() {
        Some(name) => crate::portal::revoke_app(&name),
        None => 0,
    }
}

// ── The forms ──────────────────────────────────────────────────────────────

const LANG_NAMES: [(Lang, &str); 24] = [
    (Lang::Bg, "Bulgarian"),
    (Lang::Hr, "Croatian"),
    (Lang::Cs, "Czech"),
    (Lang::Da, "Danish"),
    (Lang::Nl, "Dutch"),
    (Lang::En, "English"),
    (Lang::Et, "Estonian"),
    (Lang::Fi, "Finnish"),
    (Lang::Fr, "French"),
    (Lang::De, "German"),
    (Lang::El, "Greek"),
    (Lang::Hu, "Hungarian"),
    (Lang::Ga, "Irish"),
    (Lang::It, "Italian"),
    (Lang::Lv, "Latvian"),
    (Lang::Lt, "Lithuanian"),
    (Lang::Mt, "Maltese"),
    (Lang::Pl, "Polish"),
    (Lang::Pt, "Portuguese"),
    (Lang::Ro, "Romanian"),
    (Lang::Sk, "Slovak"),
    (Lang::Sl, "Slovenian"),
    (Lang::Es, "Spanish"),
    (Lang::Sv, "Swedish"),
];

const WALLPAPER_NAMES: [&str; 3] = ["EuroOS gradient", "Warm sand", "Blue mist"];

const LIVE_NOTE: &str = "Live session: this system runs from memory. Changes apply at once and last until shutdown; on an installed system they are kept.";

fn mib(bytes: u64) -> String {
    let m = bytes / (1024 * 1024);
    if m >= 10 * 1024 {
        alloc::format!("{} GiB", m / 1024)
    } else if m >= 1024 {
        alloc::format!("{}.{} GiB", m / 1024, (m % 1024) * 10 / 1024)
    } else {
        alloc::format!("{m} MiB")
    }
}

/// A string the UI font can show, or the fallback.
fn renderable(s: String, fallback: impl FnOnce() -> String) -> String {
    if text::can_render(&s) {
        s
    } else {
        fallback()
    }
}

/// Live kernel output, one item per line, so a long block scrolls line by line
/// instead of disappearing as a whole when it does not fit.
fn push_lines(v: &mut Vec<Item>, lines: Vec<String>) {
    for l in lines {
        v.push(Item::lines(alloc::vec![l]));
    }
}

fn build(section: Sec) -> Vec<Item> {
    let snap = SNAP.lock();
    let mut v: Vec<Item> = Vec::new();
    let live_note = |v: &mut Vec<Item>| {
        if snap.live {
            v.push(Item::text(LIVE_NOTE));
        }
    };
    match section {
        Sec::System => {
            live_note(&mut v);
            v.push(Item::field(ID_HOSTNAME, "Computer name", &snap.hostname, "eurokernel", 63).hint(
                "The name of this computer in the shell prompt. Letters, digits and hyphens. Press Enter to apply; stored in /etc/hostname.",
            ));
            v.push(Item::heading("About this computer"));
            let ver = crate::update::running_version();
            v.push(Item::info("EuroOS version", &if ver > 1 { alloc::format!("{ver}") } else { String::from("development build") }));
            v.push(Item::info("Kernel", "EuroKernel (Rust, no_std)"));
            v.push(Item::info("Processor", "x86-64 with SMEP, SMAP, W^X"));
            v.push(Item::info("Kernel heap", &mib(crate::allocator::size() as u64)));
            v.push(Item::info("Running tasks", &alloc::format!("{}", crate::sched::task_count())));
            let up = crate::interrupts::ticks() / 100;
            v.push(Item::info("Uptime", &alloc::format!("{}h {:02}m {:02}s", up / 3600, (up % 3600) / 60, up % 60)));
            v.push(Item::info("Signed in as", if snap.user.is_empty() { "(no session)" } else { snap.user.as_str() }));
            v.push(Item::info("System storage", if snap.live { "Live session (memory)" } else { "Installed on disk" }));
        }
        Sec::Display => {
            live_note(&mut v);
            v.push(
                Item::choice(ID_WALLPAPER, "Desktop background", WALLPAPER_NAMES.iter().map(|s| String::from(*s)).collect(), crate::compositor::wallpaper())
                    .hint("What is drawn behind the windows. Applies at once."),
            );
            v.push(
                Item::slider(
                    ID_TEXTSCALE,
                    "Text size in settings",
                    crate::euroui::TEXT_SCALE_MIN as i32,
                    crate::euroui::TEXT_SCALE_MAX as i32,
                    crate::euroui::TEXT_SCALE_STEP as i32,
                    crate::euroui::text_scale() as i32,
                    "%",
                )
                .hint("Size of text and controls in this panel and others built on the same toolkit. Other apps keep their own size."),
            );
            let (w, h) = (SCREEN_W.load(Ordering::Relaxed), SCREEN_H.load(Ordering::Relaxed));
            v.push(
                Item::info("Screen resolution", &if w > 0 { alloc::format!("{w} x {h}") } else { String::from("unknown") })
                    .hint("Chosen by the firmware at boot; it cannot be changed while the system runs."),
            );
            v.push(Item::text("Both settings are saved per user (~/.config/euroos/desktop.conf)."));
            v.push(Item::text("Not adjustable in this release: accent colour, dark theme and display scaling. The design colours are fixed when the system is built."));
        }
        Sec::Language => {
            live_note(&mut v);
            let cur = Layout::ALL.iter().position(|&l| l == crate::ps2::layout()).unwrap_or(0);
            v.push(
                Item::choice(ID_KEYMAP, "Keyboard layout", Layout::ALL.iter().map(|l| String::from(l.name())).collect(), cur)
                    .hint("Which characters the keys produce. Applies at once; after a restart also on the lock screen. Stored in /etc/keymap."),
            );
            v.push(Item::field(ID_KEYTEST, "Try the layout", "", "click here and type", 40).hint("Shows what the keys produce. Nothing typed here is kept."));
            let region = prefs::region();
            let ri = LANG_NAMES.iter().position(|(l, _)| *l == region).unwrap_or(5);
            v.push(
                Item::choice(ID_REGION, "Regional formats", LANG_NAMES.iter().map(|(l, n)| alloc::format!("{n} ({})", l.code())).collect(), ri)
                    .hint("How numbers, currency and dates are written by apps that format through EuroLocale. Stored in /etc/locale.conf."),
            );
            let loc = Locale::new(region);
            let d = crate::rtc::local_now();
            v.push(Item::info("Number", &renderable(loc.int(1_234_567), || String::from("1234567"))));
            v.push(Item::info("Currency", &renderable(loc.money(123_456), || alloc::format!("{} {}", loc.int(1234), loc.currency_code()))));
            v.push(Item::info("Date", &renderable(loc.date(d.year as i32, d.month, d.day), String::new)));
            v.push(Item::text("The interface itself is in English. Translated menus and messages are a later package; choosing a region does not change the interface language."));
        }
        Sec::DateTime => {
            live_note(&mut v);
            v.push(
                Item::choice(ID_ZONE, "Time zone", tz::ZONES.iter().map(|z| String::from(z.label)).collect(), prefs::zone_index())
                    .hint("Sets the clock in the status panel, on the lock screen and in EuroClock. Summer time follows the EU rule by itself. Applies to all users."),
            );
            v.push(Item::toggle(ID_24H, "24-hour clock", prefs::hour24()).hint("Off shows 2:05 PM instead of 14:05. Saved per user."));
            let utc = crate::rtc::epoch();
            let d = crate::rtc::local_now();
            let off = tz::offset_minutes(prefs::zone(), utc);
            let u = crate::rtc::from_epoch(utc);
            v.push(Item::info("Local time", &alloc::format!("{}  {}", prefs::format_clock(d.hour, d.min), crate::rtc::date_string())));
            v.push(Item::info("Date in regional format", &Locale::new(prefs::region()).date(d.year as i32, d.month, d.day)));
            v.push(Item::info(
                "Offset from UTC",
                &alloc::format!("{}{}", tz::offset_label(off), if off != prefs::zone().std_offset_min { " (summer time)" } else { "" }),
            ));
            v.push(Item::info("Hardware clock (UTC)", &alloc::format!("{:02}:{:02}", u.hour, u.min)).hint("The clock chip is read as UTC. If it holds local time instead, choose UTC above."));
            v.push(Item::text("Linux-compatible programs still see UTC: their /etc/localtime is not switched yet."));
        }
        Sec::Network => {
            let (on, served) = crate::net::httpd_status();
            v.push(Item::toggle(ID_HTTPD, "Built-in web server (port 80)", on).hint(
                "Serves a status page of this computer to the network. Anyone who can reach this computer can open it; leave it off unless you need it. Kept across restarts.",
            ));
            v.push(Item::info("Pages served since start", &alloc::format!("{served}")));
            v.push(Item::heading("Connection"));
            push_lines(&mut v, crate::net::cmd_net());
        }
        Sec::EuroGuard => {
            v.push(Item::text("EuroGuard checks every outgoing connection and name lookup of every app against these rules."));
            v.push(
                Item::field(ID_DOMAIN, "Block a domain", "", "ads.example.com", 253)
                    .hint("No app can look up this name or its subdomains any more. Type the name and press Enter."),
            );
            let mine = crate::euroguard::user_domains();
            let sel = SELECTED_DOMAIN.load(Ordering::Relaxed).min(mine.len().saturating_sub(1));
            v.push(Item::list(ID_DOMLIST, "Domains you blocked", mine.clone(), sel, 3).hint("Kept across restarts (/etc/euroos/euroguard.conf). The built-in list under Policy is separate."));
            v.push(
                Item::button(ID_UNBLOCK, "Selected domain", "Unblock", false)
                    .hint("Removes the selected name from your list; apps can look it up again at once.")
                    .enabled(!mine.is_empty()),
            );
            v.push(Item::heading("Status"));
            push_lines(&mut v, crate::euroguard::stats_lines());
            v.push(Item::heading("Policy"));
            push_lines(&mut v, crate::euroguard::policy_lines());
            v.push(Item::heading("Recent activity"));
            push_lines(&mut v, crate::euroguard::audit_lines(6));
        }
        Sec::Apps => {
            let r = roster();
            if r.is_empty() {
                v.push(Item::text("No applications are registered."));
                return v;
            }
            let sel = SELECTED_APP.load(Ordering::Relaxed).min(r.len() - 1);
            let rows = r
                .iter()
                .map(|(name, _, _)| if crate::euroguard::app_is_blocked(name) { alloc::format!("{name}  (network blocked)") } else { name.clone() })
                .collect();
            v.push(Item::list(ID_APPLIST, "Application", rows, sel, 5));
            let (name, caps, linux) = &r[sel];
            v.push(
                Item::toggle(ID_APPNET, "Block all network access", crate::euroguard::app_is_blocked(name))
                    .hint(&alloc::format!("While on, {name} cannot open any connection. Kept across restarts.")),
            );
            v.push(
                Item::button(ID_REVOKE, "Permissions granted through the portal", "Revoke all", false)
                    .hint("Withdraws every file, device and other grant of this app. It has to ask again."),
            );
            v.push(Item::heading(name));
            v.push(Item::info("Type", if *linux { "Linux-compatible binary" } else { "EuroOS-native" }));
            v.push(Item::heading("Capabilities (what it needs to run)"));
            v.push(Item::text(&crate::ring3::cap_names(*caps)));
            v.push(Item::heading("Permissions"));
            let grants = crate::portal::grant_lines_for(name);
            if grants.is_empty() {
                v.push(Item::text("None. The app must ask; you allow or deny."));
            } else {
                push_lines(&mut v, grants);
            }
            v.push(Item::heading("Network policy and traffic"));
            push_lines(&mut v, crate::euroguard::app_summary_lines(name).iter().map(|l| String::from(l.trim_start())).collect());
        }
        Sec::Storage => {
            v.push(Item::info("System storage", if snap.live { "Live session (memory)" } else { "Installed on disk" }).hint(if snap.live {
                "Files and settings are held in memory and are gone at shutdown. Install EuroOS to keep them."
            } else {
                "Files and settings are written to the system disk."
            }));
            for (mp, total, free) in &snap.df {
                let used = total.saturating_sub(*free);
                let pct = if *total > 0 { (used * 100 / *total) as u8 } else { 0 };
                v.push(Item::meter(&alloc::format!("EuroFS on {mp}"), pct, &alloc::format!("{} of {} used ({pct}%)", mib(used), mib(*total))));
            }
            v.push(Item::heading("Disks"));
            push_lines(&mut v, crate::fatmount::lsblk());
            v.push(Item::heading("Integrity check"));
            v.push(Item::button(ID_SCRUB, "Verify all files", "Check now", true).hint(
                "Reads every file and compares it with its stored checksum, to find silent disk errors. The desktop pauses while it runs: seconds on a small disk, longer on a full one. It also runs by itself in the background.",
            ));
            v.push(Item::info("Checks since start", &alloc::format!("{}", crate::scrub::runs())));
            let manual = LAST_MANUAL_SCRUB.lock().clone();
            v.push(Item::text(&if !snap.scrub_last.is_empty() {
                alloc::format!("Last result (/var/log/fsck.log): {}", snap.scrub_last)
            } else if !manual.is_empty() {
                alloc::format!("Last result: {manual}")
            } else if crate::scrub::runs() > 0 {
                String::from("The log of the checks so far (/var/log/fsck.log) is empty or cannot be read. Press Check now for a result.")
            } else {
                String::from("No check has run yet.")
            }));
        }
        Sec::Updates => {
            let ver = crate::update::running_version();
            v.push(Item::info("Installed version", &if ver > 1 { alloc::format!("{ver}") } else { String::from("development build") }));
            v.push(Item::info("Channel", "stable").hint("The only channel that is published."));
            v.push(
                Item::choice(
                    ID_POLICY,
                    "When an update is available",
                    alloc::vec![String::from("Install and restart"), String::from("Download, then ask"), String::from("Only tell me")],
                    snap.policy,
                )
                .hint("Install and restart: restarts two minutes after a notice. Download, then ask: applied at your next restart. Only tell me: nothing is downloaded. Stored in /etc/euroupdate.conf."),
            );
            v.push(Item::info("Last check", &crate::update::last_result().unwrap_or_else(|| String::from("not yet"))));
            let staged = crate::update::staged_version();
            v.push(Item::info("Ready to install", &if staged > 0 { alloc::format!("version {staged} (restart to apply)") } else { String::from("nothing") }));
            let net = crate::net::get().is_some();
            let busy = crate::update::check_busy();
            let slots = crate::update::slots_present();
            let hint = if !net {
                "Not available: there is no network connection."
            } else if busy {
                "A check is running; the result appears under Last check."
            } else if !slots {
                "This system has no update slots (live or preview image): the check tells you whether a newer version exists, but cannot install it. Install EuroOS to receive updates."
            } else {
                "Asks euro-os.eu for the newest signed release. Runs in the background; the result appears under Last check."
            };
            v.push(Item::button(ID_CHECK, "Check for updates", "Check now", true).hint(hint).enabled(net && !busy));
            v.push(Item::heading("Signed updates"));
            v.push(Item::text(
                "Every update is checked twice before it is used: the release list and the system image each carry an Ed25519 signature that must match keys built into this system. An unsigned, altered or older image is refused. The update is written to the unused system slot; the running version stays as fallback until the new one has started correctly.",
            ));
        }
    }
    drop(snap);
    v
}

// ── Applying an event to the real state ────────────────────────────────────

fn saved_note(r: Result<prefs::Saved, prefs::PrefsError>) -> String {
    match r {
        Ok(_) if !crate::rootblk::booted_from_disk() => String::from("Applied. Live session: kept until shutdown."),
        Ok(_) => String::from("Applied and saved."),
        Err(prefs::PrefsError::NoSession) => String::from("Applied for this session. Not saved: nobody is signed in."),
        Err(e) => alloc::format!("Applied, but saving failed ({e:?})."),
    }
}

fn save_etc(fs: &mut dyn FileSystem, path: &str, content: &str) -> Result<prefs::Saved, prefs::PrefsError> {
    prefs::write_system_file(fs, path, content.as_bytes()).map_err(prefs::PrefsError::Fs)
}

fn save_system_key(fs: &mut dyn FileSystem, set: impl FnOnce(&mut prefs::Prefs)) -> Result<prefs::Saved, prefs::PrefsError> {
    let mut p = prefs::open_system(fs, prefs::SETTINGS);
    if p.to_text().is_empty() {
        *p = prefs::Conf::with_header("EuroOS system settings (written by Settings; see docs/SETTINGS.md)");
    }
    set(&mut p);
    p.save(fs)
}

fn save_user_key(fs: &mut dyn FileSystem, set: impl FnOnce(&mut prefs::Prefs)) -> Result<prefs::Saved, prefs::PrefsError> {
    let mut p = prefs::open_user(fs, prefs::DESKTOP);
    set(&mut p);
    p.save(fs)
}

fn apply(fs: &mut dyn FileSystem, ev: Event) {
    match ev {
        Event::Submitted(ID_HOSTNAME, name) => {
            let name = String::from(name.trim());
            if !validate::valid_hostname(&name) {
                set_status(String::from("Name not changed: use 1 to 63 letters, digits or hyphens, not starting or ending with a hyphen."));
            } else {
                let r = save_etc(fs, "/etc/hostname", &alloc::format!("{name}\n"));
                crate::serial_println!("[set] computer name set to {name}: {r:?}");
                set_status(match r {
                    Ok(_) => alloc::format!("Computer name is now {name}. {}", saved_note(r)),
                    Err(e) => alloc::format!("Name not changed: /etc/hostname could not be written ({e:?})."),
                });
            }
        }
        Event::Chosen(ID_WALLPAPER, i) if i < prefs::WALLPAPERS.len() => {
            crate::compositor::set_wallpaper(i);
            set_status(saved_note(save_user_key(fs, |p| {
                p.set("display.wallpaper", prefs::WALLPAPERS[i]);
            })));
        }
        Event::Slid(ID_TEXTSCALE, pct) => {
            crate::euroui::set_text_scale(pct.max(0) as u32);
            let pct = crate::euroui::text_scale();
            set_status(saved_note(save_user_key(fs, |p| {
                p.set_i64("ui.text_scale", pct as i64);
            })));
        }
        Event::Chosen(ID_KEYMAP, i) if i < Layout::ALL.len() => {
            let layout = Layout::ALL[i];
            crate::ps2::set_layout(layout);
            let r = save_etc(fs, "/etc/keymap", &alloc::format!("{}\n", layout.tag()));
            crate::serial_println!("[set] keyboard layout set to {}: {r:?}", layout.tag());
            set_status(alloc::format!("Keyboard layout: {}. {}", layout.name(), saved_note(r)));
        }
        Event::Chosen(ID_REGION, i) if i < LANG_NAMES.len() => {
            let lang = LANG_NAMES[i].0;
            prefs::set_region(lang);
            let r = save_etc(fs, "/etc/locale.conf", &alloc::format!("LANG={}\n", lang.code()));
            set_status(alloc::format!("Regional formats: {}. {}", LANG_NAMES[i].1, saved_note(r)));
        }
        Event::Chosen(ID_ZONE, i) if i < tz::ZONES.len() => {
            prefs::set_zone_index(i);
            let r = save_system_key(fs, |p| {
                p.set("time.zone", tz::ZONES[i].id);
            });
            crate::serial_println!("[set] time zone set to {}: {r:?}", tz::ZONES[i].id);
            set_status(alloc::format!("Time zone: {}. {}", tz::ZONES[i].id, saved_note(r)));
        }
        Event::Toggled(ID_24H, on) => {
            prefs::set_hour24(on);
            set_status(saved_note(save_user_key(fs, |p| {
                p.set_bool("clock.24h", on);
            })));
        }
        Event::Toggled(ID_HTTPD, on) => {
            if crate::net::httpd_status().0 != on {
                crate::net::httpd_toggle();
            }
            let r = save_system_key(fs, |p| {
                p.set_bool("net.httpd", on);
            });
            set_status(alloc::format!("Web server {}. {}", if on { "on" } else { "off" }, saved_note(r)));
        }
        Event::Submitted(ID_DOMAIN, name) => {
            let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
            if !validate::valid_domain(&name) {
                set_status(String::from("Not blocked: that is not a valid domain name (example: ads.example.com)."));
            } else {
                crate::euroguard::add_blocked_domain(&name);
                let saved = crate::euroguard::save(fs);
                crate::serial_println!("[set] EuroGuard: domain blocked via the settings panel: {name} (saved={saved})");
                set_status(alloc::format!("{name} is blocked. {}", if saved { "Saved." } else { "Could not be saved; it lasts until shutdown." }));
            }
        }
        Event::Selected(ID_DOMLIST, i) => SELECTED_DOMAIN.store(i, Ordering::Relaxed),
        Event::Pressed(ID_UNBLOCK) => {
            let mine = crate::euroguard::user_domains();
            if let Some(name) = mine.get(SELECTED_DOMAIN.load(Ordering::Relaxed).min(mine.len().saturating_sub(1))) {
                crate::euroguard::remove_blocked_domain(name);
                let saved = crate::euroguard::save(fs);
                set_status(alloc::format!("{name} is no longer blocked. {}", if saved { "Saved." } else { "Could not be saved." }));
            }
        }
        Event::Selected(ID_APPLIST, i) => select_app(i),
        Event::Toggled(ID_APPNET, _) => {
            let blocked = toggle_app_net();
            let saved = crate::euroguard::save(fs);
            let name = selected_app_name().unwrap_or_default();
            set_status(alloc::format!(
                "{name}: network {}. {}",
                if blocked { "blocked" } else { "allowed again" },
                if saved { "Saved." } else { "Could not be saved; it lasts until shutdown." }
            ));
        }
        Event::Pressed(ID_REVOKE) => {
            let n = revoke_app_perms();
            set_status(alloc::format!("{n} permission(s) revoked for {}.", selected_app_name().unwrap_or_default()));
        }
        Event::Pressed(ID_SCRUB) => {
            // In system context, so the pass can append its line to /var/log/fsck.log.
            let r = crate::sysctx::as_system(fs, |fs| crate::scrub::run(fs));
            let line = alloc::format!(
                "{} files, {} verified, {} error(s), {} unrecoverable, superblock {}, bitmap {}",
                r.objects,
                r.data_verified,
                r.errors,
                r.data_unrecoverable,
                if r.superblock_ok { "OK" } else { "FAIL" },
                if r.bitmap_ok { "OK" } else { "FAIL" }
            );
            set_status(alloc::format!("Check finished: {line}."));
            *LAST_MANUAL_SCRUB.lock() = line;
        }
        Event::Chosen(ID_POLICY, i) if i < POLICY_NAMES.len() => {
            let out = crate::sysctx::as_system(fs, |fs| crate::update::set_policy(fs, POLICY_NAMES[i]));
            let ok = out.first().map(|l| l.starts_with("euroupdate policy:")).unwrap_or(false);
            set_status(if ok { String::from("Update policy saved.") } else { String::from("Update policy could not be saved.") });
        }
        Event::Pressed(ID_CHECK) => {
            set_status(match crate::update::check_now(fs, crate::interrupts::ticks()) {
                Ok(()) => String::from("Check started. The result appears under Last check."),
                Err(why) => alloc::format!("Check not started: {why}"),
            });
        }
        _ => {}
    }
}

// ── Geometry, input, drawing ───────────────────────────────────────────────

fn form_area(wx: usize, wy: usize, ww: usize, wh: usize) -> Rect {
    Rect::new(wx + NAV_W + 20, wy + TITLEBAR_H + HEADER_H, ww.saturating_sub(NAV_W + 28), wh.saturating_sub(TITLEBAR_H + HEADER_H + FOOTER_H))
}

fn nav_row(wx: usize, wy: usize, i: usize) -> Rect {
    Rect::new(wx + 8, wy + TITLEBAR_H + 46 + i * NAV_ROW, NAV_W - 16, NAV_ROW - 4)
}

/// A left click inside the settings window. Always handles the click.
pub fn click(fs: &mut dyn FileSystem, wx: usize, wy: usize, ww: usize, wh: usize, mx: usize, my: usize) -> bool {
    if let Some(i) = (0..NAV.len()).find(|&i| nav_row(wx, wy, i).contains(mx, my)) {
        show(i);
        refresh(fs);
        return true;
    }
    let items = build(sec());
    let ev = form::click(form_area(wx, wy, ww, wh), &items, &mut FORM.lock(), mx, my);
    if let Some(ev) = ev {
        apply(fs, ev);
    }
    refresh(fs);
    true
}

/// A key press while the settings window has the focus.
pub fn key(fs: &mut dyn FileSystem, wx: usize, wy: usize, ww: usize, wh: usize, k: Key) {
    if k == Key::Tab && crate::ps2::alt_down() {
        return; // Alt+Tab belongs to the window switcher
    }
    let items = build(sec());
    let ev = {
        let mut st = FORM.lock();
        let idle = st.focus.is_none() && !st.popup_open() && !st.editing();
        if idle {
            let cur = SECTION.load(Ordering::Relaxed);
            match k {
                // With nothing focused, Up/Down walk the section list.
                Key::Up | Key::Down => {
                    drop(st);
                    show(if k == Key::Up { cur.saturating_sub(1) } else { (cur + 1).min(NAV.len() - 1) });
                    refresh(fs);
                    return;
                }
                // Typing in EuroGuard goes straight to the "block a domain" field.
                Key::Char(c) if sec() == Sec::EuroGuard && !c.is_whitespace() => st.focus = Some(ID_DOMAIN),
                _ => {}
            }
        }
        form::key(form_area(wx, wy, ww, wh), &items, &mut st, k)
    };
    if let Some(ev) = ev {
        if !matches!(ev, Event::Edited(_)) {
            apply(fs, ev);
            refresh(fs);
        }
    }
}

/// Scroll the form by `delta` rows (for a wheel; the kernel has no wheel
/// events yet, PageUp/PageDown and the scrollbar do the same).
#[allow(dead_code)]
pub fn scroll(wx: usize, wy: usize, ww: usize, wh: usize, delta: i32) {
    let items = build(sec());
    form::scroll(form_area(wx, wy, ww, wh), &items, &mut FORM.lock(), delta);
}

/// Once per desktop-loop iteration: save EuroGuard rule changes made anywhere
/// (panel or shell), and keep the snapshot fresh while the panel is shown.
pub fn tick(fs: &mut dyn FileSystem) {
    crate::euroguard::persist_tick(fs);
    let now = crate::interrupts::ticks();
    let shown = now.saturating_sub(LAST_RENDER.load(Ordering::Relaxed)) < 500;
    let stale = now.saturating_sub(LAST_REFRESH.load(Ordering::Relaxed)) > 300;
    if !SNAP.lock().loaded || (shown && stale) {
        refresh(fs);
    }
}

/// Render the panel body (live kernel state).
pub fn render(fb: &FrameBuffer, win_x: usize, win_y: usize, win_w: usize, win_h: usize) {
    SCREEN_W.store(fb.width(), Ordering::Relaxed);
    SCREEN_H.store(fb.height(), Ordering::Relaxed);
    LAST_RENDER.store(crate::interrupts::ticks(), Ordering::Relaxed);
    let x = win_x;
    let y = win_y + TITLEBAR_H;
    let h = win_h.saturating_sub(TITLEBAR_H);
    let cur = SECTION.load(Ordering::Relaxed) % NAV.len();

    // Background + navigation column.
    fb.fill_rect(x, y, win_w, h, Color::SURFACE);
    fb.fill_rect(x, y, NAV_W, h, Color::SURFACE_3);
    fb.fill_rect(x + NAV_W, y, 1, h, Color::BORDER);
    text::draw_px(fb, x + 18, y + 14, "Settings", Color::INK, 16.0);
    for (i, (_, name, _)) in NAV.iter().enumerate() {
        let r = nav_row(win_x, win_y, i);
        if r.y + r.h > y + h {
            break;
        }
        if i == cur {
            fb.fill_rounded_rect(r.x, r.y, r.w, r.h, crate::eds::RADIUS_S, Color::ACCENT_SOFT);
            fb.fill_rounded_rect(r.x.saturating_sub(4), r.y + 6, 3, r.h.saturating_sub(12), 1, Color::ACCENT);
        }
        let c = if i == cur { Color::ACCENT } else { Color::INK };
        text::draw_px(fb, r.x + 12, crate::euroui::draw::text_y(r.y, r.h, 13.5), name, c, 13.5);
    }

    // Section title + description, then the form.
    let cx = x + NAV_W + 20;
    text::draw_px(fb, cx, y + 16, NAV[cur].1, Color::INK, 20.0);
    text::draw_px(fb, cx, y + 44, NAV[cur].2, Color::TEXT_SEC, 12.5);
    let items = build(NAV[cur].0);
    let area = form_area(win_x, win_y, win_w, win_h);
    // The result of the last action, under the form (so it is visible however
    // far the form is scrolled).
    if let Some((i, msg)) = STATUS.lock().as_ref() {
        if *i == cur {
            let fy = area.y + area.h + 4;
            fb.fill_rect(area.x, fy, area.w.saturating_sub(14), 1, Color::BORDER);
            for (n, line) in form::wrap(msg, 12.5, area.w.saturating_sub(18)).iter().take(2).enumerate() {
                text::draw_px(fb, area.x + 2, fy + 5 + n * 16, line, Color::INK, 12.5);
            }
        }
    }
    form::draw(fb, area, &items, &FORM.lock());
}

// ── Boot self-tests ────────────────────────────────────────────────────────

/// `[7appgui]` boot self-test — the desktop per-app control screen's action
/// handlers do REAL kernel work (not just draw): selecting an app, cutting its
/// network with the switch, and revoking its permissions all change live state.
pub fn selftest() {
    let r = roster();
    if r.is_empty() {
        crate::serial_println!("[7appgui] app-control panel: no programs registered — SKIPPED");
        return;
    }
    let name = r[0].0.clone();

    // (1) Selecting a row targets that app.
    select_app(0);
    let selected_ok = selected_app_name().as_deref() == Some(name.as_str());

    // (2) The "Block all network" switch cuts the app off for real.
    let before = crate::euroguard::app_is_blocked(&name);
    let now = toggle_app_net();
    let blocked_ok = now && crate::euroguard::app_is_blocked(&name);

    // (3) Toggling back restores it (leave the system untouched).
    let restored = !toggle_app_net() && !crate::euroguard::app_is_blocked(&name);

    // (4) Revoke runs without panicking (0 grants is a valid outcome).
    let _revoked = revoke_app_perms();

    let ok = selected_ok && blocked_ok && restored && !before;
    crate::serial_println!(
        "[7appgui] Desktop app-control (EuroBeheer › Apps): select-app={selected_ok}, block-switch-cuts-network={blocked_ok}, toggle-restores={restored}, revoke-runs=true → {}",
        if ok { "OK (rights + network controllable per app from the desktop) ✓" } else { "FAILED ✗" }
    );
}

/// `[uikit]` `[set-keymap]` `[set-time]` `[set-host]` `[set-guard]` boot
/// self-tests: the toolkit logic, and the settings panel changing REAL state
/// through the same click/key path the desktop uses, saving it, and reading it
/// back. Every setting and file is restored afterwards.
pub fn state_selftest(fs: &mut dyn FileSystem) {
    form::selftest();

    let (wx, wy, ww, wh) = (300usize, 120usize, 760usize, 560usize);
    let area = form_area(wx, wy, ww, wh);
    let read = |fs: &mut dyn FileSystem, p: &str| crate::sysctx::as_system(fs, |fs| fs.read_file(p)).ok();
    let restore = |fs: &mut dyn FileSystem, p: &str, old: Option<Vec<u8>>| match old {
        Some(bytes) => {
            let _ = prefs::write_system_file(fs, p, &bytes);
        }
        None => crate::sysctx::as_system(fs, |fs| {
            let _ = fs.remove_file(p);
        }),
    };
    let saved_section = SECTION.load(Ordering::Relaxed);
    refresh(fs);

    // ── Keyboard layout: pick German from the dropdown with the mouse, then
    // step to French with the keyboard. The decoder must follow each time.
    let old_layout = crate::ps2::layout();
    let old_keymap = read(fs, "/etc/keymap");
    show_sec(Sec::Language);
    crate::ps2::set_layout(Layout::UsQwerty);
    let us_z = eurokeymap::translate(crate::ps2::layout(), 0x2C, false);
    let items = build(Sec::Language);
    let (cx, cy) = form::rect_of(area, &items, &FORM.lock(), ID_KEYMAP).map(|(_, c)| c.center()).unwrap_or((0, 0));
    click(fs, wx, wy, ww, wh, cx, cy);
    let opened = FORM.lock().popup_open();
    let (ox, oy) = form::popup_option_center(area, &items, &FORM.lock(), ID_KEYMAP, 3).unwrap_or((0, 0));
    click(fs, wx, wy, ww, wh, ox, oy);
    let de = crate::ps2::layout() == Layout::DeQwertz;
    let de_z = eurokeymap::translate(crate::ps2::layout(), 0x2C, false);
    let de_file = read(fs, "/etc/keymap").as_deref() == Some(b"de-qwertz\n".as_slice());
    FORM.lock().focus = Some(ID_KEYMAP);
    key(fs, wx, wy, ww, wh, Key::Left);
    let fr = crate::ps2::layout() == Layout::FrAzerty;
    let fr_q = eurokeymap::translate(crate::ps2::layout(), 0x10, false);
    let fr_file = read(fs, "/etc/keymap").as_deref() == Some(b"fr-azerty\n".as_slice());
    let shown = matches!(build(Sec::Language).iter().find(|i| i.id == ID_KEYMAP).map(|i| &i.kind), Some(form::Kind::Choice { selected: 2, .. }));
    crate::ps2::set_layout(old_layout);
    restore(fs, "/etc/keymap", old_keymap);
    let km_ok = opened && us_z == Some('z') && de && de_z == Some('y') && de_file && fr && fr_q == Some('a') && fr_file && shown;
    crate::serial_println!(
        "[set-keymap] Settings › Language & keyboard: dropdown-opens={opened}, click-picks-German={de} (Z-key: {us_z:?} → {de_z:?}), /etc/keymap=de-qwertz={de_file}, arrow-key-steps-to-French={fr} (Q-key → {fr_q:?}), /etc/keymap=fr-azerty={fr_file}, panel-shows-it={shown} → {}",
        if km_ok { "OK (layout switches live and is saved) ✓" } else { "FAILED ✗" }
    );

    // ── Time zone + clock format: applied to the clock string, saved, re-read.
    let old_zone = prefs::zone_index();
    let old_24 = prefs::hour24();
    let old_settings = read(fs, "/etc/euroos/settings.conf");
    let desktop_path = crate::session::active().and_then(|s| prefs::user_path(&s.name, prefs::DESKTOP));
    let old_desktop = desktop_path.as_ref().and_then(|p| read(fs, p));
    let bru = tz::index_of("Europe/Brussels").unwrap_or(0);
    let utc = crate::rtc::epoch();
    apply(fs, Event::Chosen(ID_ZONE, bru));
    let zone_set = prefs::zone().id == "Europe/Brussels";
    let shifted = prefs::local_time(utc) == utc + 60 * tz::offset_minutes(&tz::ZONES[bru], utc) as u64 && prefs::local_time(utc) > utc;
    let zone_saved = prefs::open_system(fs, prefs::SETTINGS).get("time.zone") == Some("Europe/Brussels");
    apply(fs, Event::Toggled(ID_24H, false));
    let c12 = crate::rtc::clock_string();
    let twelve = c12.ends_with("AM") || c12.ends_with("PM");
    let user_saved = desktop_path.is_none() || !prefs::open_user(fs, prefs::DESKTOP).get_bool("clock.24h", true);
    apply(fs, Event::Toggled(ID_24H, true));
    let c24 = crate::rtc::clock_string();
    let twentyfour = c24.len() == 5 && !c24.ends_with('M');
    // A reboot in miniature: forget the live value, load it from the file.
    prefs::set_zone_index(0);
    let reloaded = prefs::open_system(fs, prefs::SETTINGS).get("time.zone").and_then(tz::index_of) == Some(bru);
    prefs::set_zone_index(old_zone);
    prefs::set_hour24(old_24);
    restore(fs, "/etc/euroos/settings.conf", old_settings);
    if let Some(p) = &desktop_path {
        match old_desktop {
            Some(bytes) => {
                let _ = prefs::write_system_file(fs, p, &bytes);
            }
            None => crate::sysctx::as_system(fs, |fs| {
                let _ = fs.remove_file(p);
            }),
        }
    }
    let time_ok = zone_set && shifted && zone_saved && twelve && user_saved && twentyfour && reloaded;
    crate::serial_println!(
        "[set-time] Settings › Date & time: zone-applied={zone_set}, clock-shifted={shifted}, saved-in-/etc/euroos/settings.conf={zone_saved}, 12h-clock={twelve} ({c12}), per-user-saved={user_saved}, 24h-clock={twentyfour} ({c24}), reloaded-from-file={reloaded} → {}",
        if time_ok { "OK (time zone + clock format live and saved) ✓" } else { "FAILED ✗" }
    );

    // ── Host name: typed into the field, written through the immutability gate.
    let old_host = read(fs, "/etc/hostname");
    let old_flags = fs.get_flags("/etc/hostname").unwrap_or(0);
    show_sec(Sec::System);
    let items = build(Sec::System);
    let (hx, hy) = form::rect_of(area, &items, &FORM.lock(), ID_HOSTNAME).map(|(_, c)| c.center()).unwrap_or((0, 0));
    click(fs, wx, wy, ww, wh, hx, hy);
    let editing_host = FORM.lock().editing();
    for _ in 0..70 {
        key(fs, wx, wy, ww, wh, Key::Backspace);
    }
    for c in "euro-test-7".chars() {
        key(fs, wx, wy, ww, wh, Key::Char(c));
    }
    key(fs, wx, wy, ww, wh, Key::Enter);
    let host_set = read(fs, "/etc/hostname").as_deref() == Some(b"euro-test-7\n".as_slice());
    let host_shown = SNAP.lock().hostname == "euro-test-7";
    let flags_kept = fs.get_flags("/etc/hostname").unwrap_or(0) == old_flags;
    apply(fs, Event::Submitted(ID_HOSTNAME, String::from("not a valid name!")));
    let bad_refused = read(fs, "/etc/hostname").as_deref() == Some(b"euro-test-7\n".as_slice());
    restore(fs, "/etc/hostname", old_host);
    let host_ok = editing_host && host_set && host_shown && flags_kept && bad_refused;
    crate::serial_println!(
        "[set-host] Settings › System: field-takes-typing={editing_host}, /etc/hostname-written={host_set}, panel-shows-it={host_shown}, protection-flag-kept={flags_kept} (flags {old_flags:#x}), invalid-name-refused={bad_refused} → {}",
        if host_ok { "OK (computer name changeable from the desktop) ✓" } else { "FAILED ✗" }
    );

    // ── EuroGuard rules survive a "restart" (save → forget → load).
    crate::euroguard::persist_selftest(fs);

    FORM.lock().reset();
    *STATUS.lock() = None;
    SECTION.store(saved_section, Ordering::Relaxed);
    refresh(fs);
}
