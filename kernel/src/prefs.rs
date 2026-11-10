//! **Settings storage** — where EuroOS keeps what the user configured.
//!
//! Files are plain `key=value` text ([`europrefs::Conf`], host-tested), one per
//! scope and app:
//!
//! - system-wide: `/etc/euroos/<app>.conf` — [`open_system`]. The settings
//!   panel's own file is `/etc/euroos/settings.conf` ([`SETTINGS`]).
//! - per user: `/home/<user>/.config/euroos/<app>.conf` — [`open_user`], for
//!   the user of the active session (`session.rs`). Owned by that user.
//!
//! An app reads and writes its own file through a [`Prefs`] handle:
//!
//! ```ignore
//! let mut p = prefs::open_user(fs, "notes");
//! let wrap = p.get_bool("editor.wrap", true);      // default when unset/broken
//! p.set_bool("editor.wrap", !wrap);
//! let _ = p.save(fs);                               // atomic; Err is reportable
//! ```
//!
//! Guarantees: opening never fails (a missing or damaged file yields the
//! defaults, broken lines are skipped), nothing here panics, and [`Prefs::save`]
//! writes a temporary file and renames it over the old one, so a crash leaves
//! either the old or the new file. Where the filesystem has no rename the file
//! is written in place and the result says so ([`Saved::Direct`]).
//!
//! On the live image the root lives in RAM: settings work for the session and
//! are gone at shutdown. On an installed system they survive a restart.
//!
//! This module also holds the few settings the kernel consults on every frame
//! (time zone, 12/24-hour clock, region) as atomics, loaded at boot
//! ([`boot_apply`]) and when a desktop session starts ([`session_apply`]).
//! `docs/SETTINGS.md` lists every file and key.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use eurofs::{FileSystem, FsError, FLAG_APPEND_ONLY, FLAG_IMMUTABLE};
use eurolocale::Lang;
pub use europrefs::Conf;
use europrefs::{tz, validate};

/// Directory of the system-wide settings files.
pub const SYSTEM_DIR: &str = "/etc/euroos";
/// App name of the settings panel's system-wide file.
pub const SETTINGS: &str = "settings";
/// App name of the per-user desktop preferences.
pub const DESKTOP: &str = "desktop";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefsError {
    /// The app or user name cannot be a file name.
    BadName,
    /// Per-user settings were asked for, but no session is open.
    NoSession,
    /// The filesystem refused the write.
    Fs(FsError),
}

/// How a save reached the disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Saved {
    /// Temporary file + rename: the old file was replaced in one step.
    Atomic,
    /// Written in place (the filesystem has no rename).
    Direct,
}

/// One settings file, opened. Dereferences to [`Conf`] for the typed getters
/// and setters; [`Prefs::save`] writes it back.
#[derive(Clone)]
pub struct Prefs {
    conf: Conf,
    path: String,
    /// Owner of the file: 0 = system, else the user's uid.
    owner: u32,
    /// Why this handle cannot be saved (bad name / no session), if so.
    unusable: Option<PrefsError>,
}

impl Deref for Prefs {
    type Target = Conf;
    fn deref(&self) -> &Conf {
        &self.conf
    }
}

impl DerefMut for Prefs {
    fn deref_mut(&mut self) -> &mut Conf {
        &mut self.conf
    }
}

impl Prefs {
    fn unusable(why: PrefsError) -> Self {
        Prefs { conf: Conf::new(), path: String::new(), owner: 0, unusable: Some(why) }
    }

    /// The file this handle reads and writes (empty if it is unusable).
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Write the settings back. Creates the directories as needed.
    pub fn save(&self, fs: &mut dyn FileSystem) -> Result<Saved, PrefsError> {
        if let Some(e) = self.unusable {
            return Err(e);
        }
        let text = self.conf.to_text();
        let (path, owner) = (self.path.as_str(), self.owner);
        crate::sysctx::as_system(fs, |fs| {
            if let Some(slash) = path.rfind('/') {
                mkdir_p(fs, &path[..slash], owner);
            }
            let r = write_atomic(fs, path, text.as_bytes());
            if r.is_ok() && owner != 0 {
                let _ = fs.chown(path, owner);
            }
            r
        })
        .map_err(PrefsError::Fs)
    }
}

/// Create `dir` and its missing parents. Directories below `/home/<user>` that
/// this creates are handed to `owner`.
fn mkdir_p(fs: &mut dyn FileSystem, dir: &str, owner: u32) {
    let mut cur = String::new();
    for (depth, part) in dir.split('/').filter(|p| !p.is_empty()).enumerate() {
        cur.push('/');
        cur.push_str(part);
        if !fs.exists(&cur) && fs.create_dir(&cur).is_ok() && owner != 0 && depth >= 1 {
            let _ = fs.chown(&cur, owner);
        }
    }
}

/// Replace `path` with `data` in one step where the filesystem can: write
/// `<path>.tmp`, then rename it over `path`. Falls back to writing in place.
pub fn write_atomic(fs: &mut dyn FileSystem, path: &str, data: &[u8]) -> Result<Saved, FsError> {
    let tmp = alloc::format!("{path}.tmp");
    if fs.write_file(&tmp, data).is_ok() {
        if fs.rename(&tmp, path).is_ok() {
            return Ok(Saved::Atomic);
        }
        let _ = fs.remove_file(&tmp);
    }
    fs.write_file(path, data).map(|_| Saved::Direct)
}

/// Write a system configuration file (`/etc/...`) on behalf of the settings
/// panel: in system context, atomically, and through the immutability gate if
/// the file is protected (`/etc/hostname` is). The flag is cleared for the
/// write and set again, both audited by `immutable::set_protected`.
pub fn write_system_file(fs: &mut dyn FileSystem, path: &str, data: &[u8]) -> Result<Saved, FsError> {
    crate::sysctx::as_system(fs, |fs| {
        if let Some(slash) = path.rfind('/') {
            mkdir_p(fs, &path[..slash], 0);
        }
        let flags = fs.get_flags(path).unwrap_or(0);
        let locked = flags & (FLAG_IMMUTABLE | FLAG_APPEND_ONLY) != 0;
        if locked {
            crate::immutable::set_protected(fs, path, flags & !(FLAG_IMMUTABLE | FLAG_APPEND_ONLY), crate::ring3::CAP_IMMUTABLE_ADMIN)?;
        }
        let r = write_atomic(fs, path, data);
        if locked {
            let _ = crate::immutable::set_protected(fs, path, flags, crate::ring3::CAP_IMMUTABLE_ADMIN);
        }
        r
    })
}

fn open_at(fs: &mut dyn FileSystem, path: String, owner: u32) -> Prefs {
    let conf = match crate::sysctx::as_system(fs, |fs| fs.read_file(&path)) {
        Ok(bytes) => Conf::parse_bytes(&bytes),
        Err(_) => Conf::new(),
    };
    Prefs { conf, path, owner, unusable: None }
}

/// Path of the system-wide settings file of `app`.
pub fn system_path(app: &str) -> Option<String> {
    validate::valid_app_name(app).then(|| alloc::format!("{SYSTEM_DIR}/{app}.conf"))
}

/// Path of `user`'s settings file of `app`.
pub fn user_path(user: &str, app: &str) -> Option<String> {
    (validate::valid_app_name(app) && validate::valid_user_name(user)).then(|| alloc::format!("/home/{user}/.config/euroos/{app}.conf"))
}

/// Open the system-wide settings of `app` (`/etc/euroos/<app>.conf`).
pub fn open_system(fs: &mut dyn FileSystem, app: &str) -> Prefs {
    match system_path(app) {
        Some(path) => open_at(fs, path, 0),
        None => Prefs::unusable(PrefsError::BadName),
    }
}

/// Open the settings of `app` for the user of the active session
/// (`/home/<user>/.config/euroos/<app>.conf`). Without a session the handle
/// holds only defaults and `save` returns [`PrefsError::NoSession`].
pub fn open_user(fs: &mut dyn FileSystem, app: &str) -> Prefs {
    match crate::session::active() {
        Some(s) if s.uid != 0 => open_for(fs, &s.name, s.uid, app),
        _ => Prefs::unusable(PrefsError::NoSession),
    }
}

/// Open the settings of `app` for a named user (an admin tool acting for
/// someone; apps use [`open_user`]).
pub fn open_for(fs: &mut dyn FileSystem, user: &str, uid: u32, app: &str) -> Prefs {
    match user_path(user, app) {
        Some(path) => open_at(fs, path, uid),
        None => Prefs::unusable(PrefsError::BadName),
    }
}

// ── Settings the kernel consults live ──────────────────────────────────────

static ZONE: AtomicUsize = AtomicUsize::new(0);
static HOUR24: AtomicBool = AtomicBool::new(true);
/// Index into `Lang::ALL`; 0xFF = not set (English formats).
static REGION: AtomicU8 = AtomicU8::new(0xFF);

/// The configured time zone.
pub fn zone() -> &'static tz::Zone {
    &tz::ZONES[ZONE.load(Ordering::Relaxed) % tz::ZONES.len()]
}
pub fn zone_index() -> usize {
    ZONE.load(Ordering::Relaxed) % tz::ZONES.len()
}
pub fn set_zone_index(i: usize) {
    ZONE.store(i % tz::ZONES.len(), Ordering::Relaxed);
}
/// Local wall-clock seconds for a UTC instant, in the configured zone.
pub fn local_time(utc_epoch: u64) -> u64 {
    tz::local_epoch(zone(), utc_epoch)
}
/// 24-hour clock (true) or 12-hour with AM/PM.
pub fn hour24() -> bool {
    HOUR24.load(Ordering::Relaxed)
}
pub fn set_hour24(v: bool) {
    HOUR24.store(v, Ordering::Relaxed);
}
/// `"14:05"` / `"2:05 PM"` per the user's clock setting.
pub fn format_clock(hour: u8, min: u8) -> String {
    tz::format_clock(hour as u32, min as u32, hour24())
}
/// The configured region (number/date formats); English when unset.
pub fn region() -> Lang {
    Lang::ALL.get(REGION.load(Ordering::Relaxed) as usize).copied().unwrap_or(Lang::En)
}
pub fn set_region(lang: Lang) {
    let i = Lang::ALL.iter().position(|&l| l == lang).unwrap_or(0xFF);
    REGION.store(i as u8, Ordering::Relaxed);
}

/// The language of `/etc/locale.conf` (`LANG=nl-BE` → Dutch), if it has one.
pub fn read_locale_conf(fs: &mut dyn FileSystem) -> Option<Lang> {
    let bytes = crate::sysctx::as_system(fs, |fs| fs.read_file("/etc/locale.conf")).ok()?;
    let conf = Conf::parse_bytes(&bytes);
    conf.get("LANG").and_then(|v| Lang::parse(v.split('.').next().unwrap_or(v)))
}

/// Boot: load the system-wide settings and apply them (time zone, region, the
/// saved EuroGuard rules). Called once from `main.rs` after the system policy
/// is loaded. The keyboard layout is applied by `main.rs` itself from
/// `/etc/keymap`.
pub fn boot_apply(fs: &mut dyn FileSystem) {
    let sys = open_system(fs, SETTINGS);
    if let Some(i) = sys.get("time.zone").and_then(tz::index_of) {
        set_zone_index(i);
    }
    if let Some(lang) = read_locale_conf(fs) {
        set_region(lang);
    }
    let (domains, apps) = crate::euroguard::load_saved(fs);
    crate::serial_println!(
        "[prefs] system settings applied: {} ({} key(s), {} broken line(s) skipped), time zone {}, region {}, EuroGuard saved rules: {} domain change(s), {} app rule(s)",
        sys.path(),
        sys.len(),
        sys.rejected(),
        zone().id,
        region().code(),
        domains,
        apps
    );
}

/// Names of the selectable wallpapers, in `compositor` order; the stored value
/// of `display.wallpaper`.
pub const WALLPAPERS: [&str; 3] = ["euro", "sand", "mist"];

/// Desktop session start: load the user's desktop preferences (clock format,
/// wallpaper, text size) and the system-wide HTTP-server switch. Called from
/// `main.rs` right after `session::open`.
pub fn session_apply(fs: &mut dyn FileSystem) {
    let user = open_user(fs, DESKTOP);
    set_hour24(user.get_bool("clock.24h", true));
    let wp = user.get_choice("display.wallpaper", &WALLPAPERS, 0);
    crate::compositor::set_wallpaper(wp);
    crate::euroui::set_text_scale(user.get_i64_in("ui.text_scale", 100, crate::euroui::TEXT_SCALE_MIN as i64, crate::euroui::TEXT_SCALE_MAX as i64) as u32);
    // The HTTP server is off after boot; turn it on if the user left it on.
    let sys = open_system(fs, SETTINGS);
    if sys.get_bool("net.httpd", false) && !crate::net::httpd_status().0 {
        crate::net::httpd_toggle();
    }
    crate::serial_println!(
        "[prefs] session settings applied: {} ({} key(s)), clock {}, wallpaper {}, text scale {}%",
        if user.path().is_empty() { "no user session" } else { user.path() },
        user.len(),
        if hour24() { "24h" } else { "12h" },
        WALLPAPERS[wp],
        crate::euroui::text_scale()
    );
}

/// `[prefs-rt]` `[prefs-bad]` `[prefs-user]` boot self-tests on the live root
/// FS: a typed round trip through an atomic save; a damaged file that still
/// yields its intact lines; per-user files that are separate and user-owned.
/// Everything it creates is removed again.
pub fn selftest(fs: &mut dyn FileSystem) {
    // (1) Round trip.
    let mut p = open_system(fs, "selftest");
    let fresh = p.is_empty() && p.get_bool("flag", true) && p.get_i64("n", 7) == 7;
    p.set("name", "Euro PC \u{20ac}");
    p.set_bool("flag", false);
    p.set_i64("n", -42);
    p.set_list("list", &["a.example", "b.example"]);
    p.set("multi", "line one\nline two");
    let saved = p.save(fs);
    let q = open_system(fs, "selftest");
    let same = q.get("name") == Some("Euro PC \u{20ac}")
        && !q.get_bool("flag", true)
        && q.get_i64("n", 0) == -42
        && q.get_list("list") == ["a.example", "b.example"]
        && q.get("multi") == Some("line one\nline two")
        && q.rejected() == 0;
    let tmp_gone = !fs.exists("/etc/euroos/selftest.conf.tmp");
    // A second save replaces the first (the rename runs over an existing file).
    let mut q2 = q.clone();
    q2.set_i64("n", 43);
    let resaved = q2.save(fs).is_ok() && open_system(fs, "selftest").get_i64("n", 0) == 43;
    let rt_ok = fresh && saved.is_ok() && same && tmp_gone && resaved;
    crate::serial_println!(
        "[prefs-rt] settings round trip (/etc/euroos/selftest.conf): defaults-when-missing={fresh}, save={saved:?}, typed-values-read-back={same}, temp-file-gone={tmp_gone}, overwrite={resaved} → {}",
        if rt_ok { "OK (typed key=value store, atomic save) ✓" } else { "FAILED ✗" }
    );

    // (2) A damaged file: broken lines are skipped, intact ones still count,
    // nothing panics, and the next save writes a clean file.
    let damaged: &[u8] = b"good=1\n\xff\xfe garbage \x00\x01\nno equals sign\n=x\nui.text_scale=120\n[section]\nlast = ok \n";
    let _ = write_system_file(fs, "/etc/euroos/selftest.conf", damaged);
    let d = open_system(fs, "selftest");
    let kept = d.get("good") == Some("1") && d.get_i64("ui.text_scale", 100) == 120 && d.get("last") == Some("ok");
    let skipped = d.rejected();
    let binary: Vec<u8> = (0..=255u8).cycle().take(2048).collect();
    let _ = write_system_file(fs, "/etc/euroos/selftest.conf", &binary);
    let mut b = open_system(fs, "selftest");
    let binary_ok = b.get_bool("anything", true) && b.get_i64("n", 5) == 5;
    b.set("recovered", "yes");
    let healed = b.save(fs).is_ok() && {
        let c = open_system(fs, "selftest");
        c.rejected() == 0 && c.get("recovered") == Some("yes")
    };
    let bad_ok = kept && skipped == 4 && binary_ok && healed;
    crate::serial_println!(
        "[prefs-bad] damaged settings file: intact-lines-kept={kept}, broken-lines-skipped={skipped} (4 expected), binary-file-gives-defaults={binary_ok}, next-save-is-clean={healed} → {}",
        if bad_ok { "OK (tolerant parser, no panic) ✓" } else { "FAILED ✗" }
    );
    crate::sysctx::as_system(fs, |fs| {
        let _ = fs.remove_file("/etc/euroos/selftest.conf");
    });

    // (3) Per-user files: separate per user, owned by the user, and names that
    // would leave the settings directory are refused.
    let users = [("prefs-t1", 4301u32), ("prefs-t2", 4302u32)];
    let mut written = true;
    for (name, uid) in users {
        let mut u = open_for(fs, name, uid, "demo");
        u.set("who", name);
        written &= u.save(fs).is_ok();
    }
    let separate = users.iter().all(|(name, uid)| open_for(fs, name, *uid, "demo").get("who") == Some(*name));
    let path_ok = user_path("prefs-t1", "demo").as_deref() == Some("/home/prefs-t1/.config/euroos/demo.conf");
    let owned = users.iter().all(|(name, uid)| {
        let path = user_path(name, "demo").unwrap_or_default();
        fs.owner(&path).ok() == Some(*uid) && fs.owner(&alloc::format!("/home/{name}/.config/euroos")).ok() == Some(*uid)
    });
    let refused = open_for(fs, "../etc", 4301, "demo").save(fs) == Err(PrefsError::BadName)
        && open_for(fs, "prefs-t1", 4301, "../../etc/passwd").save(fs) == Err(PrefsError::BadName)
        && open_system(fs, "a/b").save(fs) == Err(PrefsError::BadName);
    crate::sysctx::as_system(fs, |fs| {
        for (name, _) in users {
            let _ = fs.remove_file(&alloc::format!("/home/{name}/.config/euroos/demo.conf"));
            let _ = fs.remove_dir(&alloc::format!("/home/{name}/.config/euroos"));
            let _ = fs.remove_dir(&alloc::format!("/home/{name}/.config"));
            let _ = fs.remove_dir(&alloc::format!("/home/{name}"));
        }
    });
    let cleaned = !fs.exists("/home/prefs-t1") && !fs.exists("/home/prefs-t2");
    let user_ok = written && separate && path_ok && owned && refused && cleaned;
    crate::serial_println!(
        "[prefs-user] per-user settings (/home/<user>/.config/euroos/<app>.conf): saved={written}, separate-per-user={separate}, owned-by-user={owned}, path-escape-refused={refused}, cleaned-up={cleaned} → {}",
        if user_ok { "OK (per-user store on the live root FS) ✓" } else { "FAILED ✗" }
    );
}
