//! **Per-app policy from the outside** (2026-10-09): a plain Linux program or an
//! installed app runs inside a boundary it cannot change: which paths it may
//! read or write, which it may never touch, which domains it may resolve. The
//! file lives at `/etc/euroapps/<app>.policy.toml` (system-owned), keyed by the
//! app's name (argv[0] basename). No file = behaviour unchanged (the profile's
//! own rules only). With a policy, the Linux-ABI path syscalls and DNS are
//! checked against it; a refusal is EACCES / a dropped query, never a kill.
//!
//! Learning (`app policy learn <app>`): the program runs under an EMPTY policy
//! with enforcement ON; every refusal is recorded as a want; `app policy
//! propose` shows the policy that would allow it (never the hard-denied
//! system paths); `app policy approve` installs it, owner only, audited.
//! The same model Microsoft Execution Containers ship for Windows, enforced
//! here in the kernel's syscall path.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use eurofs::FileSystem;
use spin::Mutex;

pub const SCHEMA: u32 = 1;

/// Paths no policy can open up (the identity store, secrets, policies themselves).
const HARD_DENY: &[&str] = &["/etc/euroid", "/etc/shadow", "/etc/fde", "/etc/euroguard", "/etc/vault", "/etc/euroapps", "/etc/euroagent"];
/// What every program may use regardless of policy: libraries, fonts, devices, scratch.
const BASE_READ: &[&str] = &["/lib", "/usr/", "/pack/", "/bin/", "/dev/", "/proc/", "/sys/", "/run/", "/etc/fonts", "/etc/ld.so", "/etc/localtime", "/etc/keymap", "/pkg/"];
const BASE_WRITE: &[&str] = &["/tmp/", "/dev/"];

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Policy {
    pub schema: u32,
    pub app: String,
    pub read: Vec<String>,
    pub write: Vec<String>,
    pub deny: Vec<String>,
    pub domains: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Want {
    Read(String),
    Write(String),
    Domain(String),
}

/// The policy in force for the running app (set at launch, cleared at exit).
static ACTIVE: Mutex<Option<Policy>> = Mutex::new(None);
/// Learning session: (app, wants). Enforcement stays on.
static LEARN: Mutex<Option<(String, Vec<Want>)>> = Mutex::new(None);
/// The last proposal (base, proposed).
static PROPOSAL: Mutex<Option<(Policy, Policy)>> = Mutex::new(None);

pub fn key(argv0: &str) -> String {
    argv0.rsplit('/').next().unwrap_or(argv0).to_string()
}
pub fn path(app: &str) -> String {
    alloc::format!("/etc/euroapps/{app}.policy.toml")
}

fn starts(path: &str, prefix: &str) -> bool {
    path.starts_with(prefix) || (prefix.ends_with('/') && path == &prefix[..prefix.len() - 1])
}

impl Policy {
    pub fn empty(app: &str) -> Policy {
        Policy { schema: SCHEMA, app: app.to_string(), ..Default::default() }
    }
    /// Is `p` usable for `write`? Hard denies first, then the policy's own
    /// denies, then its allow lists, then the base system paths.
    pub fn allows(&self, p: &str, write: bool) -> bool {
        if HARD_DENY.iter().any(|d| starts(p, d)) { return false; }
        if self.deny.iter().any(|d| starts(p, d)) { return false; }
        if !p.starts_with('/') { return true; } // relative to the cwd scratch: the profile decides
        if write {
            self.write.iter().any(|a| starts(p, a)) || BASE_WRITE.iter().any(|a| starts(p, a))
        } else {
            self.read.iter().any(|a| starts(p, a)) || self.write.iter().any(|a| starts(p, a))
                || BASE_READ.iter().any(|a| starts(p, a))
        }
    }
    pub fn to_toml(&self) -> String {
        fn arr(v: &[String]) -> String {
            let mut o = String::from("[");
            for (i, s) in v.iter().enumerate() { if i > 0 { o.push_str(", "); } o.push('"'); o.push_str(s); o.push('"'); }
            o.push(']'); o
        }
        alloc::format!("[policy]\nschema = {}\napp = \"{}\"\n\n[paths]\nread = {}\nwrite = {}\ndeny = {}\n\n[net]\ndomains = {}\n",
            self.schema, self.app, arr(&self.read), arr(&self.write), arr(&self.deny), arr(&self.domains))
    }
    /// Parse the TOML dialect written by `to_toml`. A newer schema is refused.
    pub fn from_toml(t: &str) -> Result<Policy, String> {
        let mut p = Policy::default();
        let mut section = String::new();
        for raw in t.lines() {
            let l = raw.trim();
            if l.is_empty() || l.starts_with('#') { continue; }
            if l.starts_with('[') { section = l.trim_matches(|c| c == '[' || c == ']').to_string(); continue; }
            let Some((k, v)) = l.split_once('=') else { return Err(alloc::format!("bad line: {l}")) };
            let (k, v) = (k.trim(), v.trim());
            let list = |v: &str| -> Result<Vec<String>, String> {
                let inner = v.strip_prefix('[').and_then(|x| x.strip_suffix(']')).ok_or_else(|| alloc::format!("{k}: not a list"))?;
                Ok(inner.split(',').map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty()).collect())
            };
            match (section.as_str(), k) {
                ("policy", "schema") => { p.schema = v.parse().map_err(|_| String::from("schema: not a number"))?; }
                ("policy", "app") => p.app = v.trim_matches('"').to_string(),
                ("paths", "read") => p.read = list(v)?,
                ("paths", "write") => p.write = list(v)?,
                ("paths", "deny") => p.deny = list(v)?,
                ("net", "domains") => p.domains = list(v)?,
                _ => return Err(alloc::format!("unknown field {section}.{k}")),
            }
        }
        if p.schema == 0 { p.schema = 1; }
        if p.schema > SCHEMA { return Err(alloc::format!("policy schema {} is newer than this kernel supports (max {SCHEMA})", p.schema)); }
        if p.app.is_empty() { return Err(String::from("policy.app missing")); }
        Ok(p)
    }
}

pub fn load(fs: &mut dyn FileSystem, app: &str) -> Option<Policy> {
    let b = fs.read_file(&path(app)).ok()?;
    Policy::from_toml(core::str::from_utf8(&b).ok()?).ok()
}

/// Called at launch: the policy in force for `argv0`'s app (None = none).
pub fn activate(fs: &mut dyn FileSystem, argv0: &str) -> Option<Policy> {
    let app = key(argv0);
    let learning = LEARN.lock().as_ref().map(|(a, _)| a == &app).unwrap_or(false);
    let p = load(fs, &app).or_else(|| if learning { Some(Policy::empty(&app)) } else { None });
    *ACTIVE.lock() = p.clone();
    p
}
pub fn deactivate() { *ACTIVE.lock() = None; }
pub fn active() -> bool { ACTIVE.lock().is_some() }

/// The syscall gate: Some(false) = refuse (EACCES), Some(true) = fine, None = no policy.
pub fn check_path(app_argv0: &str, p: &str, write: bool) -> Option<bool> {
    let g = ACTIVE.lock();
    let pol = g.as_ref()?;
    if pol.app != key(app_argv0) { return None; }
    let ok = pol.allows(p, write);
    if !ok {
        if let Some((a, wants)) = LEARN.lock().as_mut() {
            if *a == pol.app {
                let w = if write { Want::Write(p.to_string()) } else { Want::Read(p.to_string()) };
                if !wants.contains(&w) { wants.push(w); }
            }
        }
    }
    Some(ok)
}
/// DNS gate: Some(false) = drop the query, None = no policy / no domain list.
pub fn check_dns(app_argv0: &str, name: &str) -> Option<bool> {
    let g = ACTIVE.lock();
    let pol = g.as_ref()?;
    if pol.app != key(app_argv0) { return None; }
    let n = name.trim_end_matches('.');
    let ok = pol.domains.iter().any(|d| n == d || n.ends_with(&alloc::format!(".{d}")));
    if !ok {
        if let Some((a, wants)) = LEARN.lock().as_mut() {
            if *a == pol.app { let w = Want::Domain(n.to_string()); if !wants.contains(&w) { wants.push(w); } }
        }
    }
    Some(ok)
}

pub fn learn_start(app: &str) { *LEARN.lock() = Some((app.to_string(), Vec::new())); }
/// End the learning session: build the proposal from the wants.
pub fn learn_finish(fs: &mut dyn FileSystem) -> Option<(Policy, Policy, Vec<Want>)> {
    let (app, wants) = LEARN.lock().take()?;
    let base = load(fs, &app).unwrap_or_else(|| Policy::empty(&app));
    let proposed = propose(&base, &wants);
    *PROPOSAL.lock() = Some((base.clone(), proposed.clone()));
    Some((base, proposed, wants))
}
/// The proposal never opens a hard-denied path and never removes anything.
pub fn propose(base: &Policy, wants: &[Want]) -> Policy {
    let mut p = base.clone();
    for w in wants {
        match w {
            Want::Read(x) => { if !HARD_DENY.iter().any(|d| starts(x, d)) && !p.read.contains(x) && !p.write.contains(x) { p.read.push(x.clone()); } }
            Want::Write(x) => { if !HARD_DENY.iter().any(|d| starts(x, d)) && !p.write.contains(x) { p.write.push(x.clone()); } }
            Want::Domain(d) => { if !p.domains.contains(d) { p.domains.push(d.clone()); } }
        }
    }
    p
}
pub fn diff(base: &Policy, p: &Policy) -> Vec<String> {
    let mut o = Vec::new();
    for x in &p.read { if !base.read.contains(x) { o.push(alloc::format!("+ read    {x}")); } }
    for x in &p.write { if !base.write.contains(x) { o.push(alloc::format!("+ write   {x}")); } }
    for x in &p.domains { if !base.domains.contains(x) { o.push(alloc::format!("+ domain  {x}")); } }
    if o.is_empty() { o.push(String::from("(nothing to add)")); }
    o
}
pub fn approve(fs: &mut dyn FileSystem, p: &Policy, is_owner: bool) -> Result<String, String> {
    if !is_owner { return Err(String::from("app policy approve: only the owner can change a program's boundary")); }
    let file = path(&p.app);
    let toml = p.to_toml();
    let ok = crate::sysctx::as_system(fs, |sys| { let _ = sys.create_dir("/etc/euroapps"); sys.write_file(&file, toml.as_bytes()).is_ok() });
    if !ok { return Err(alloc::format!("app policy approve: cannot write {file}")); }
    crate::audit::record(crate::audit::Event::PolicyApproved, &alloc::format!("app {} schema {}", p.app, p.schema));
    Ok(alloc::format!("app policy: boundary of '{}' installed ({file})", p.app))
}

/// `app policy <learn|propose|approve|show|export|import> ...` (the run itself
/// is the caller's: `learn` only arms the session for the next launch).
pub fn shell(fs: &mut dyn FileSystem, args: &[&str], is_owner: bool) -> Vec<String> {
    match args {
        ["learn", app] => { learn_start(app); alloc::vec![alloc::format!("app policy: learning armed for '{app}' (enforcement on): run it, then 'app policy propose'")] }
        ["propose"] | ["propose", _] => {
            let got = learn_finish(fs).or_else(|| PROPOSAL.lock().clone().map(|(b, p)| (b, p, Vec::new())));
            match got {
                None => alloc::vec![String::from("app policy propose: nothing learned (arm with 'app policy learn <app>' and run the program)")],
                Some((b, p, _)) => { let mut o = alloc::vec![alloc::format!("proposed policy for '{}':", p.app)]; o.extend(diff(&b, &p).into_iter().map(|l| alloc::format!("  {l}"))); o.extend(p.to_toml().lines().map(|l| alloc::format!("    {l}"))); o }
            }
        }
        ["approve"] | ["approve", _] => {
            let p = PROPOSAL.lock().as_ref().map(|(_, p)| p.clone());
            match p { None => alloc::vec![String::from("app policy approve: nothing proposed")], Some(p) => match approve(fs, &p, is_owner) { Ok(s) => { *PROPOSAL.lock() = None; alloc::vec![s] } Err(e) => alloc::vec![e] } }
        }
        ["show", app] => match load(fs, app) { Some(p) => p.to_toml().lines().map(|l| l.to_string()).collect(), None => alloc::vec![alloc::format!("app policy: no policy for '{app}' (the profile's rules only)")] },
        ["export", app, file] => match load(fs, app) { Some(p) => match fs.write_file(file, p.to_toml().as_bytes()) { Ok(_) => alloc::vec![alloc::format!("app policy: '{app}' exported to {file}")], Err(_) => alloc::vec![alloc::format!("app policy export: cannot write {file}")] }, None => alloc::vec![alloc::format!("app policy: no policy for '{app}'")] },
        ["import", file] => match fs.read_file(file).ok().and_then(|b| String::from_utf8(b).ok()) { None => alloc::vec![alloc::format!("app policy import: cannot read {file}")], Some(t) => match Policy::from_toml(&t) { Err(e) => alloc::vec![alloc::format!("app policy import: refused: {e}")], Ok(p) => match approve(fs, &p, is_owner) { Ok(s) => alloc::vec![s], Err(e) => alloc::vec![e] } } },
        _ => alloc::vec![String::from("usage: app policy learn <app> | propose | approve | show <app> | export <app> <file> | import <file>")],
    }
}

/// `[app-policy]`: a plain Linux program under a policy from the outside.
pub fn selftest(falloc: &mut euromm::FrameAllocator, fs: &mut dyn FileSystem) {
    let caps = crate::ring3::CAP_CONSOLE | crate::ring3::CAP_FILE | crate::ring3::CAP_PROC_INFO;
    let run = |falloc: &mut euromm::FrameAllocator, fs: &mut dyn FileSystem| -> (String, u64) {
        activate(fs, "/bin/gpolicy");
        let r = crate::ring3::run_glibc(falloc, crate::ring3::gpolicy_bytes(), crate::ring3::ldlinux_bytes(), &[b"/bin/gpolicy"], &[b"PATH=/bin"], caps);
        deactivate();
        r
    };
    let _ = crate::sysctx::as_system(fs, |sys| sys.remove_file(&path("gpolicy")).is_ok());
    let _ = fs.remove_file("/home/euro/policy-demo.txt");
    // 1. no policy: the program behaves as before (hostname + home write ok, identity store refused by the system)
    let (_, e0) = run(falloc, fs);
    // 2. learning: empty policy, enforcement on → both the read and the write are refused and recorded
    learn_start("gpolicy");
    let (_, e1) = run(falloc, fs);
    let (base, proposed, wants) = learn_finish(fs).unwrap_or((Policy::empty("gpolicy"), Policy::empty("gpolicy"), Vec::new()));
    let want_read = wants.contains(&Want::Read(String::from("/etc/hostname")));
    let want_write = wants.contains(&Want::Write(String::from("/home/euro/policy-demo.txt")));
    let identity_never_proposed = !proposed.read.iter().chain(proposed.write.iter()).any(|x| x.starts_with("/etc/euroid"));
    let proposes_both = proposed.read.iter().any(|x| x == "/etc/hostname") && proposed.write.iter().any(|x| x == "/home/euro/policy-demo.txt");
    // 3. export/import round trip + newer schema refused
    let round = Policy::from_toml(&proposed.to_toml()).ok() == Some(proposed.clone());
    let newer_refused = Policy::from_toml(&proposed.to_toml().replace("schema = 1", "schema = 2")).is_err();
    // 4. approve (owner) and run again: allowed now, identity store still refused
    let approved = approve(fs, &proposed, true).is_ok();
    let (_, e2) = run(falloc, fs);
    let not_owner = approve(fs, &proposed, false).is_err();
    let _ = base;
    // Without a policy the program's reach is whatever the caller's file context
    // allows (here, the boot context: the identity store is readable, exit 7);
    // the policy's hard denies close that regardless of file permissions.
    let store_open_without_policy = e0 & 4 != 0;
    let ok = e0 & 3 == 3 && e1 == 0 && want_read && want_write && proposes_both && identity_never_proposed && round && newer_refused && approved && e2 == 3 && not_owner;
    crate::serial_println!(
        "[app-policy] a Linux program under a policy from the outside: no-policy exit={e0} (hostname+home ok; identity store readable in this context={store_open_without_policy}) · learning(empty policy, enforcement on) exit={e1} (want 0) wants-read-hostname={want_read} wants-write-home={want_write} · proposal adds both={proposes_both}, identity store never proposed={identity_never_proposed} · export→import-equal={round} · newer-schema-refused={newer_refused} · approve-as-owner={approved} · approved run exit={e2} (want 3: hostname+home ok, identity store refused BY POLICY) · non-owner-refused={not_owner} → {}",
        if ok { "OK ✓ (the boundary is written by the owner, enforced in the syscall path)" } else { "FAILED" }
    );
    let _ = crate::sysctx::as_system(fs, |sys| sys.remove_file(&path("gpolicy")).is_ok());
    let _ = fs.remove_file("/home/euro/policy-demo.txt");
}
