//! Kernel-zijde van **EuroID** (Sprint K1 + P3): soeverein gebruikersbeheer.
//!
//! Bij boot bouwen we de identiteitsopslag (de ingebouwde groepen + een paar demo-
//! accounts), en bewijzen we de héle keten end-to-end: een gebruiker aanmaken
//! (Argon2id-gehasht met TPM-RNG-zout) → aanmelden met timing-aanval-preventie →
//! mislukte pogingen die het account vergrendelen → een onbekende gebruiker die
//! ononderscheidbaar faalt → een soft delete → en een **hash-chain audit-log** dat
//! elke actie onomkeerbaar vastlegt en knoeien detecteert. Host-geteste kern:
//! [`euroid`] (24 tests, incl. het officiële RFC 9106 Argon2id-testvector).
//!
//! De Argon2id-parameters zijn bij boot bewust verlaagd (geheugen/iteraties) zodat
//! de zelftest snel is onder TCG; de échte soevereine parameters (64 MiB/t=3/p=4) en
//! het RFC-testvector worden native in de host-tests geverifieerd.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use spin::Mutex;

use euroid::argon2::Params;
use euroid::persist::{deserialize_db, serialize_db};
use euroid::{
    authenticate, cap_names, effective_caps, validate_password, validate_username, Argon2idHash,
    AuditEvent, AuditLog, AuthError, Credential, Group, GroupDb, GroupId, LockReason,
    PasswordPolicy, PasswordRecord, Timestamp, User, UserDb, UserError, UserState, ALLOW_ALL,
    CAP_FILE, CAP_NET, GROUP_NET, GROUP_USERS, GROUP_WHEEL,
};

/// Het persistente gebruikersbestand op EuroFS (overleeft een herstart).
const USERS_DB: &str = "/etc/euroid/users.db";

/// Bewust verlaagde Argon2id-parameters voor de boot-zelftest/runtime onder TCG.
/// (De soevereine 64 MiB/t=3/p=4 + RFC-vector worden in de host-tests bewezen.)
const BOOT_PARAMS: Params = Params { m_cost: 256, t_cost: 1, p_cost: 1, tag_len: 32 };

/// De levende identiteitsopslag.
struct State {
    users: UserDb,
    groups: GroupDb,
    audit: AuditLog,
    dummy: Argon2idHash,
    policy: PasswordPolicy,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Genereer `n` willekeurige bytes — TPM-RNG indien beschikbaar, anders een
/// functionele tick/RDTSC-mix (de zelftest blijft geldig; productie gebruikt TPM).
fn rand_bytes(n: usize) -> Vec<u8> {
    if let Some(b) = crate::tpm::get_random(n as u16) {
        if b.len() >= n {
            return b[..n].to_vec();
        }
    }
    let mut out = Vec::with_capacity(n);
    let mut x = crate::interrupts::ticks().wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(0x1234_5678);
    for _ in 0..n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.push((x >> 24) as u8);
    }
    out
}

fn salt32() -> Vec<u8> {
    rand_bytes(euroid::SALT_LEN)
}

fn now() -> Timestamp {
    Timestamp(crate::rtc::epoch())
}

/// Maak een gebruikersrecord (de useradd-orkestratie uit de spec).
#[allow(clippy::too_many_arguments)]
fn build_user(
    uid: u32,
    name: &str,
    display: &str,
    primary: GroupId,
    groups: &[GroupId],
    own_caps: u64,
    password: &str,
    created_by: u32,
) -> User {
    let salt = salt32();
    let password = PasswordRecord::hash_password(password.as_bytes(), &salt, &BOOT_PARAMS, now());
    User {
        uid: euroid::UserId(uid),
        username: name.to_string(),
        display_name: display.to_string(),
        primary_gid: primary,
        groups: groups.to_vec(),
        home: format!("/home/{name}"),
        shell: "/bin/eurosh".to_string(),
        state: UserState::Active,
        caps: own_caps,
        created_at: now(),
        created_by: euroid::UserId(created_by),
        password,
        tpm_enrolled: false,
        failed_logins: 0,
    }
}

/// Bouw de opslag met de ingebouwde groepen + twee demo-accounts (alice, admin-bob).
fn build_state() -> State {
    let groups = GroupDb::with_builtins();
    let mut users = UserDb::new();
    let mut audit = AuditLog::new();
    audit.append(&AuditEvent::SystemInit, now());

    // root: systeem-admin (wheel). Net als /etc/shadow `root:*` is interactieve
    // login vergrendeld — root-toegang loopt via sudo, niet via een wachtwoord.
    let mut root = build_user(0, "root", "System Administrator", GROUP_WHEEL, &[], 0, "*locked*", 0);
    root.state = UserState::Locked { reason: LockReason::AdminLock, locked_at: now(), locked_by: euroid::UserId::ROOT };
    users.insert(root).ok();

    // euro: het ECHTE desktop-account (uid 1000, /etc/passwd-canoniek). Hiermee
    // logt de shell in via EuroID-Argon2id (geen SHA-256 meer). Demo-wachtwoord "euro".
    let euro = build_user(1000, "euro", "Euro User", GROUP_USERS, &[GROUP_NET], CAP_FILE, "euro", 0);
    audit.append(
        &AuditEvent::UserCreated {
            uid: euro.uid,
            username: euro.username.clone(),
            created_by: euro.created_by,
            groups: euro.groups.clone(),
            caps: effective_caps(&euro, &groups, ALLOW_ALL),
        },
        now(),
    );
    users.insert(euro).ok();

    // alice: gewone gebruiker (groepen users+net, eigen CAP_FILE) — K1-demo-account.
    let alice = build_user(1002, "alice", "Alice Vermeersch", GROUP_USERS, &[GROUP_NET], CAP_FILE, "Correct-Horse-9!", 0);
    audit.append(
        &AuditEvent::UserCreated {
            uid: alice.uid,
            username: alice.username.clone(),
            created_by: alice.created_by,
            groups: alice.groups.clone(),
            caps: effective_caps(&alice, &groups, ALLOW_ALL),
        },
        now(),
    );
    users.insert(alice).ok();

    // bob: admin (wheel) — moet wachtwoord wijzigen bij eerste login.
    let mut bob = build_user(1003, "bob", "Bob De Smedt", GROUP_WHEEL, &[], 0, "Initial-Admin-1!", 0);
    bob.password.must_change = true;
    audit.append(
        &AuditEvent::UserCreated {
            uid: bob.uid,
            username: bob.username.clone(),
            created_by: bob.created_by,
            groups: alloc::vec![GROUP_WHEEL],
            caps: effective_caps(&bob, &groups, ALLOW_ALL),
        },
        now(),
    );
    users.insert(bob).ok();

    // De dummy-hash (zelfde params als echte accounts) voor timing-aanval-preventie.
    let dummy = Argon2idHash::create(b"*invalid*", &salt32(), &BOOT_PARAMS);

    State { users, groups, audit, dummy, policy: PasswordPolicy::default() }
}

/// **K1 boot-zelftest** — de hele keten manage→auth→audit, end-to-end.
pub fn selftest() {
    let mut st = build_state();
    let from_tpm = crate::tpm::get_random(1).is_some();

    // 1. alice logt correct in → sessie + LoginSuccess in het audit-log.
    let sid = {
        let mut s = [0u8; 32];
        s.copy_from_slice(&rand_bytes(32));
        s
    };
    let r = authenticate(
        &mut st.users,
        &st.groups,
        "alice",
        Credential::Password("Correct-Horse-9!".to_string()),
        now(),
        sid,
        ALLOW_ALL,
        "/dev/tty1",
        &st.dummy,
    );
    for ev in &r.events {
        st.audit.append(ev, now());
    }
    let login_ok = r.outcome.is_ok();
    let caps = r.outcome.as_ref().map(|s| s.caps).unwrap_or(0);
    // alice krijgt LOGIN|FILE|DISPLAY (users) ∪ NET (net) ∪ FILE (eigen).
    let caps_ok = caps & (CAP_NET | CAP_FILE | euroid::CAP_DISPLAY | euroid::CAP_LOGIN)
        == (CAP_NET | CAP_FILE | euroid::CAP_DISPLAY | euroid::CAP_LOGIN)
        && caps & euroid::CAP_USER_ADMIN == 0;

    // 2. Onbekende gebruiker → ononderscheidbaar van verkeerd wachtwoord.
    let r_unknown = authenticate(
        &mut st.users,
        &st.groups,
        "mallory",
        Credential::Password("guess".to_string()),
        now(),
        [0u8; 32],
        ALLOW_ALL,
        "/dev/tty1",
        &st.dummy,
    );
    for ev in &r_unknown.events {
        st.audit.append(ev, now());
    }
    let unknown_generic = r_unknown.outcome == Err(AuthError::InvalidCredentials);

    // 3. Vijf foute pogingen op bob → account vergrendeld (lockout).
    for _ in 0..5 {
        let rb = authenticate(
            &mut st.users,
            &st.groups,
            "bob",
            Credential::Password("nope".to_string()),
            now(),
            [0u8; 32],
            ALLOW_ALL,
            "/dev/tty1",
            &st.dummy,
        );
        for ev in &rb.events {
            st.audit.append(ev, now());
        }
    }
    let locked = matches!(st.users.get(euroid::UserId(1003)).map(|u| &u.state), Some(UserState::Locked { .. }));

    // 4. Soft delete van alice → record blijft bestaan (audit-vereiste).
    st.users.soft_delete(euroid::UserId(1002), euroid::UserId::ROOT, now()).ok();
    st.audit.append(
        &AuditEvent::UserDeleted { uid: euroid::UserId(1002), username: "alice".to_string(), deleted_by: euroid::UserId::ROOT },
        now(),
    );
    let record_kept = st.users.get(euroid::UserId(1002)).is_some();

    // 5. Hash-chain: de hele keten moet intact verifiëren. (Tamper-detectie — een
    //    geknoeid record dat álle volgende hashes ongeldig maakt — wordt robuust
    //    bewezen in de host-tests `tampering_*_breaks_the_chain`.)
    let chain_ok = st.audit.verify_chain().is_ok();
    let entries = st.audit.len();
    let root = euroid::hex(&st.audit.root_hash());

    let ok = login_ok && caps_ok && unknown_generic && locked && record_kept && chain_ok;
    crate::serial_println!(
        "[k1] EuroID: useradd+Argon2id(TPM-zout={from_tpm}) → login alice(caps users∪net={caps_ok})={login_ok} · onbekende-gebruiker-ononderscheidbaar={unknown_generic} · 5×fout→bob-vergrendeld={locked} · soft-delete-bewaart-record={record_kept} · hash-chain-intact={chain_ok} ({entries} events, root sha256:{}) → {}",
        &root[..16.min(root.len())],
        if ok { "OK (Sprint K1: soeverein gebruikersbeheer + tamper-evident audit, NIS2/GDPR/ISO 27001) ✓" } else { "MISLUKT" }
    );

    *STATE.lock() = Some(st);

    // Rookproef van het ECHTE shell-pad (niet alleen gecompileerd): draai een paar
    // `eurousers`-commando's tegen de levende opslag en bewijs dat ze werken.
    let listed = shell("list", 0);
    let added = shell("add carla S3cure-Pass-9! users,net", 0);
    let verify = shell("audit --verify-chain", 0);
    let shell_ok = listed.iter().any(|l| l.contains("alice"))
        && added.iter().any(|l| l.contains("aangemaakt"))
        && verify.iter().any(|l| l.contains("keten intact"));
    crate::serial_println!(
        "[k1] eurousers shell-pad: 'list'-toont-gebruikers={} · 'add carla'={} · 'audit --verify-chain'={} → {}",
        listed.iter().any(|l| l.contains("alice")),
        added.iter().any(|l| l.contains("aangemaakt")),
        verify.iter().any(|l| l.contains("keten intact")),
        if shell_ok { "OK (commandopad live geverifieerd) ✓" } else { "MISLUKT" }
    );

    // [ae] Audit #3 / Sprint AE: bewijs dat de ECHTE login-poort (`euroid::login`,
    // het pad dat de shell `login`/`su` nu gebruikt) op Argon2id draait — juist
    // wachtwoord lukt, fout wordt geweigerd, en het vergrendelde root-account kan
    // niet interactief inloggen.
    let ok = login("euro", "euro").is_ok();
    let bad = matches!(login("euro", "fout"), Err(_));
    let root_locked = matches!(login("root", "x"), Err(ref m) if m.contains("vergrendeld"));
    crate::serial_println!(
        "[ae] EuroID-login (Argon2id, geen SHA-256 meer): euro/'euro'={} · euro/'fout'-geweigerd={} · root-locked-geweigerd={} → {}",
        ok, bad, root_locked,
        if ok && bad && root_locked { "OK (login-pad op soevereine Argon2id-identiteit) ✓" } else { "MISLUKT" }
    );
}

/// Resultaat van een geslaagde shell-login via EuroID.
pub struct LoginOk {
    pub uid: u32,
    pub name: String,
    pub caps: u64,
}

/// **Audit #3 / Sprint AE** — authenticeer tegen de levende EuroID-opslag met
/// Argon2id (memory-hard), accountstaat-controle, lockout-teller én een tamper-
/// evident audit-log. Dit vervangt de oude geïtereerde-SHA-256-verificatie tegen
/// /etc/shadow als het pad dat de shell `login`/`su` gebruikt. De audit-events
/// worden onvoorwaardelijk weggeschreven (loggen is niet overslaanbaar).
pub fn login(username: &str, password: &str) -> Result<LoginOk, String> {
    let mut guard = STATE.lock();
    let st = match guard.as_mut() {
        Some(s) => s,
        None => return Err("identiteitsopslag niet geïnitialiseerd".to_string()),
    };
    let sid = {
        let mut s = [0u8; 32];
        let r = rand_bytes(32);
        s.copy_from_slice(&r[..32]);
        s
    };
    let r = authenticate(
        &mut st.users,
        &st.groups,
        username,
        Credential::Password(password.to_string()),
        now(),
        sid,
        ALLOW_ALL,
        "/dev/console",
        &st.dummy,
    );
    for ev in &r.events {
        st.audit.append(ev, now()); // audit MOET geschreven worden
    }
    match r.outcome {
        Ok(session) => Ok(LoginOk { uid: session.uid.0, name: session.username, caps: session.caps }),
        Err(e) => Err(match e {
            AuthError::InvalidCredentials => "ongeldige gebruikersnaam of wachtwoord".to_string(),
            AuthError::AccountLocked => "account vergrendeld (te veel pogingen of admin-lock)".to_string(),
            AuthError::AccountExpired => "account verlopen".to_string(),
            AuthError::MustChangePassword => {
                "wachtwoord moet eerst gewijzigd worden (eurousers passwd)".to_string()
            }
        }),
    }
}

/// Self-service wachtwoordwijziging tegen de levende opslag (gebruikt door de
/// GUI-lockscreen bij een must-change). Verifieert het oude wachtwoord, valideert
/// het nieuwe (policy + history) en wist de must-change-vlag. `Ok` = gewijzigd.
pub fn change_own_password(user: &str, old: &str, new: &str) -> Result<(), String> {
    let mut guard = STATE.lock();
    let st = match guard.as_mut() {
        Some(s) => s,
        None => return Err("identiteitsopslag niet geïnitialiseerd".to_string()),
    };
    if let Err(e) = validate_password(new, &st.policy) {
        return Err(e.message().to_string());
    }
    let depth = st.policy.history_depth;
    let salt = salt32();
    let new_hash = Argon2idHash::create(new.as_bytes(), &salt, &BOOT_PARAMS);
    let target;
    {
        let u = st.users.get_by_name_mut(user).ok_or_else(|| "gebruiker niet gevonden".to_string())?;
        if !u.password.verify(old.as_bytes()) {
            return Err("oud wachtwoord onjuist".to_string());
        }
        if u.password.is_reused(new.as_bytes(), depth) {
            return Err(alloc::format!("wachtwoord hergebruikt (laatste {depth} verboden)"));
        }
        u.password.set_new(new_hash, depth, now()); // wist must_change
        target = u.uid;
    }
    st.audit.append(&AuditEvent::PasswordChanged { actor: target, target, admin_reset: false }, now());
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Persistentie (Sprint AE-e2e): de gebruikersopslag overleeft een herstart.
// ─────────────────────────────────────────────────────────────────────────────

/// Schrijf de levende gebruikersopslag naar `/etc/euroid/users.db`. Wordt na élke
/// muterende `eurousers`-actie aangeroepen zodat wijzigingen duurzaam zijn.
pub fn persist_state(fs: &mut dyn eurofs::FileSystem) -> bool {
    let guard = STATE.lock();
    let st = match guard.as_ref() {
        Some(s) => s,
        None => return false,
    };
    let text = serialize_db(&st.users);
    let _ = fs.create_dir("/etc");
    let _ = fs.create_dir("/etc/euroid");
    fs.write_file(USERS_DB, text.as_bytes()).is_ok()
}

/// Laad de gebruikersopslag van schijf indien aanwezig. Geeft het aantal geladen
/// gebruikers terug (0 = geen bestand / leeg → eerste boot). Bij corruptie: 0
/// (de aanroeper valt dan terug op `build_state`).
fn load_users_from_disk(fs: &mut dyn eurofs::FileSystem) -> Option<UserDb> {
    let data = fs.read_file(USERS_DB).ok()?;
    let text = core::str::from_utf8(&data).ok()?;
    match deserialize_db(text) {
        Ok(db) if !db.all().is_empty() => Some(db),
        _ => None,
    }
}

/// **Sprint AE-e2e boot-zelftest** — bewijst dat de EuroID-opslag een herstart
/// overleeft: bouw de opslag, persisteer naar EuroFS, lees 'm VAN SCHIJF terug, en
/// toon dat (1) het Argon2id-wachtwoord van 'euro' nog verifieert, (2) een nieuw
/// aangemaakte gebruiker na her-persist + herlezen aanwezig blijft (overleeft remount).
pub fn persist_selftest(fs: &mut dyn eurofs::FileSystem) {
    use eurofs::FileSystem;

    // 1. Bouw + serialiseer + schrijf naar schijf.
    let st = build_state();
    let text = serialize_db(&st.users);
    let _ = fs.create_dir("/etc");
    let _ = fs.create_dir("/etc/euroid");
    let wrote = fs.write_file(USERS_DB, text.as_bytes()).is_ok();

    // 2. Lees TERUG van schijf → euro's wachtwoord verifieert nog (hash overleefde).
    let reloaded = load_users_from_disk(fs);
    let euro_ok = reloaded
        .as_ref()
        .and_then(|db| db.get_by_name("euro"))
        .map(|u| u.password.verify(b"euro") && !u.password.verify(b"fout"))
        .unwrap_or(false);
    // root blijft vergrendeld na herlezen.
    let root_locked = reloaded
        .as_ref()
        .and_then(|db| db.get(euroid::UserId::ROOT))
        .map(|u| matches!(u.state, UserState::Locked { .. }))
        .unwrap_or(false);

    // 3. Mutatie-overleeft-remount: voeg een gebruiker toe, her-persist, herlees.
    let mut db2 = reloaded.unwrap_or_else(|| build_state().users);
    let salt = salt32();
    let newrec = PasswordRecord::hash_password(b"Persist-Test-1!", &salt, &BOOT_PARAMS, now());
    let newuser = User {
        uid: db2.next_uid(false),
        username: "persisttest".to_string(),
        display_name: "Persist Test".to_string(),
        primary_gid: GROUP_USERS,
        groups: Vec::new(),
        home: "/home/persisttest".to_string(),
        shell: "/bin/eurosh".to_string(),
        state: UserState::Active,
        caps: 0,
        created_at: now(),
        created_by: euroid::UserId::ROOT,
        password: newrec,
        tpm_enrolled: false,
        failed_logins: 0,
    };
    db2.insert(newuser).ok();
    let _ = fs.write_file(USERS_DB, serialize_db(&db2).as_bytes());
    let survives = load_users_from_disk(fs)
        .and_then(|db| db.get_by_name("persisttest").map(|u| u.password.verify(b"Persist-Test-1!")))
        .unwrap_or(false);

    let ok = wrote && euro_ok && root_locked && survives;
    crate::serial_println!(
        "[ae-persist] EuroID persistent op EuroFS: weggeschreven={wrote}, euro-Argon2id-na-herlezen={euro_ok}, root-vergrendeld-na-herlezen={root_locked}, nieuwe-gebruiker-overleeft-remount={survives} → {}",
        if ok { "OK (identiteit + wachtwoord-hashes overleven een herstart) ✓" } else { "MISLUKT" }
    );
}

/// **Sprint AE-e2e boot-zelftest** — must-change-password-handhaving. Bewijst dat
/// een account met de must-change-vlag NIET kan inloggen (ook met het juiste
/// wachtwoord), dat een self-service wijziging de vlag wist, en dat inloggen daarna
/// met het NIEUWE wachtwoord lukt terwijl het oude faalt.
pub fn must_change_selftest() {
    let groups = GroupDb::with_builtins();
    let mut db = UserDb::new();
    let mut u = build_user(2000, "resetuser", "Reset User", GROUP_USERS, &[], 0, "OldPass-1!", 0);
    u.password.must_change = true;
    db.insert(u).ok();
    let dummy = Argon2idHash::create(b"*invalid*", &salt32(), &BOOT_PARAMS);

    let auth = |db: &mut UserDb, pw: &str, dummy: &Argon2idHash| {
        authenticate(
            db,
            &groups,
            "resetuser",
            Credential::Password(pw.to_string()),
            now(),
            [0u8; 32],
            ALLOW_ALL,
            "/dev/console",
            dummy,
        )
        .outcome
    };

    // 1. Juist wachtwoord MAAR must_change → login geweigerd (MustChangePassword).
    let blocked = matches!(auth(&mut db, "OldPass-1!", &dummy), Err(AuthError::MustChangePassword));

    // 2. Self-service wijziging: verifieer het oude, zet een nieuw → must_change gewist.
    let depth = PasswordPolicy::default().history_depth;
    let cleared = {
        let salt = salt32();
        let nh = Argon2idHash::create(b"NewPass-2!", &salt, &BOOT_PARAMS);
        let user = db.get_by_name_mut("resetuser").unwrap();
        let old_ok = user.password.verify(b"OldPass-1!");
        user.password.set_new(nh, depth, now());
        old_ok && !user.password.must_change
    };

    // 3. Login met het NIEUWE wachtwoord lukt; het oude faalt.
    let now_ok = auth(&mut db, "NewPass-2!", &dummy).is_ok();
    let old_fails = auth(&mut db, "OldPass-1!", &dummy).is_err();

    let ok = blocked && cleared && now_ok && old_fails;
    crate::serial_println!(
        "[ae-mustchange] must-change-handhaving: juist-pw-maar-geblokkeerd={blocked}, self-service-wijziging-wist-vlag={cleared}, login-met-nieuw-pw-OK={now_ok}, oud-pw-faalt={old_fails} → {}",
        if ok { "OK (gedwongen wachtwoordwijziging end-to-end afgedwongen) ✓" } else { "MISLUKT" }
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// `eurousers` shellcommando.
// ─────────────────────────────────────────────────────────────────────────────

fn caps_summary(caps: u64) -> String {
    cap_names(caps).join("|")
}

fn state_caps(st: &State, u: &User) -> u64 {
    effective_caps(u, &st.groups, ALLOW_ALL)
}

/// `eurousers <subcmd> [args...]` — soeverein gebruikersbeheer vanaf de shell.
/// `actor_uid` is de uid van de huidige sessie (voor de CAP_USER_ADMIN-check).
pub fn shell(args: &str, actor_uid: u32) -> Vec<String> {
    let mut guard = STATE.lock();
    let st = match guard.as_mut() {
        Some(s) => s,
        None => return alloc::vec!["eurousers: identiteitsopslag niet geïnitialiseerd".to_string()],
    };

    let mut it = args.split_whitespace();
    let sub = it.next().unwrap_or("");
    let a1 = it.next().unwrap_or("");
    let a2 = it.next().unwrap_or("");
    let a3 = it.next().unwrap_or("");

    // Wie voert dit uit? Heeft die CAP_USER_ADMIN (wheel)?
    let actor = euroid::UserId(actor_uid);
    let is_admin = st
        .users
        .get(actor)
        .map(|u| state_caps(st, u) & euroid::CAP_USER_ADMIN != 0)
        .unwrap_or(actor_uid == 0); // uid 0 = root/system mag altijd

    let require_admin = |is_admin: bool| -> Option<Vec<String>> {
        if is_admin {
            None
        } else {
            Some(alloc::vec!["eurousers: EPERM — vereist CAP_USER_ADMIN (wheel-groep)".to_string()])
        }
    };

    match sub {
        "" | "help" => alloc::vec![
            "eurousers — soeverein gebruikersbeheer (Sprint K1)".to_string(),
            "  list                       alle gebruikers + staat".to_string(),
            "  show <naam>                volledig record (geen wachtwoord-hash)".to_string(),
            "  add <naam> <pw> [groep,..] gebruiker aanmaken (Argon2id)".to_string(),
            "  passwd <naam> <nieuw-pw>   wachtwoord (admin-reset → must-change)".to_string(),
            "  chpasswd <naam> <oud> <nieuw>  eigen wachtwoord wijzigen (wist must-change)".to_string(),
            "  lock <naam> / unlock <naam>  account (ont)grendelen".to_string(),
            "  del <naam>                 soft delete (record blijft voor audit)".to_string(),
            "  groups                     alle groepen + leden + caps".to_string(),
            "  audit [--user N|--verify-chain]  het hash-chain audit-log".to_string(),
        ],

        "list" => {
            let mut out = alloc::vec![format!("{:<10} {:>5} {:<10} {}", "GEBRUIKER", "UID", "STAAT", "GROEPEN")];
            for u in st.users.all() {
                let state = match &u.state {
                    UserState::Active => "actief".to_string(),
                    UserState::Locked { reason, .. } => format!("vergr.({})", reason.tag()),
                    UserState::Expired { .. } => "verlopen".to_string(),
                    UserState::Deleted { .. } => "verwijderd".to_string(),
                };
                let groups: Vec<String> = core::iter::once(u.primary_gid)
                    .chain(u.groups.iter().copied())
                    .filter_map(|g| st.groups.get(g).map(|g| g.name.clone()))
                    .collect();
                out.push(format!("{:<10} {:>5} {:<10} {}", u.username, u.uid.0, state, groups.join(",")));
            }
            out
        }

        "show" => {
            let u = match st.users.get_by_name(a1) {
                Some(u) => u,
                None => return alloc::vec![format!("eurousers: gebruiker '{a1}' niet gevonden")],
            };
            let groups: Vec<String> = u.groups.iter().filter_map(|g| st.groups.get(*g).map(|g| g.name.clone())).collect();
            let must = if u.password.must_change { " (must-change)" } else { "" };
            alloc::vec![
                format!("gebruiker:    {} (uid={})", u.username, u.uid.0),
                format!("weergavenaam: {}", u.display_name),
                format!("home/shell:   {}  {}", u.home, u.shell),
                format!("primaire grp: {}", st.groups.get(u.primary_gid).map(|g| g.name.as_str()).unwrap_or("?")),
                format!("groepen:      {}", groups.join(",")),
                format!("effectieve caps: {}", caps_summary(state_caps(st, u))),
                format!("wachtwoord:   Argon2id{must} (hash niet getoond)"),
                format!("aangemaakt:   t={} door uid={}", u.created_at.0, u.created_by.0),
                format!("failed-logins: {}", u.failed_logins),
            ]
        }

        "add" => {
            if let Some(e) = require_admin(is_admin) {
                return e;
            }
            if a1.is_empty() || a2.is_empty() {
                return alloc::vec!["gebruik: eurousers add <naam> <wachtwoord> [groep,groep]".to_string()];
            }
            if let Err(msg) = validate_username(a1) {
                return alloc::vec![format!("eurousers: {msg}")];
            }
            if st.users.exists(a1) {
                return alloc::vec![format!("eurousers: gebruiker '{a1}' bestaat al")];
            }
            if let Err(e) = validate_password(a2, &st.policy) {
                return alloc::vec![format!("eurousers: {}", e.message())];
            }
            // Groepen oplossen (default: users).
            let mut gids: Vec<GroupId> = Vec::new();
            if !a3.is_empty() {
                for g in a3.split(',') {
                    match st.groups.by_name(g) {
                        Some(gr) => gids.push(gr.gid),
                        None => return alloc::vec![format!("eurousers: onbekende groep '{g}'")],
                    }
                }
            }
            let uid = st.users.next_uid(false);
            let salt = salt32();
            let rec = PasswordRecord::hash_password(a2.as_bytes(), &salt, &BOOT_PARAMS, now());
            let user = User {
                uid,
                username: a1.to_string(),
                display_name: a1.to_string(),
                primary_gid: GROUP_USERS,
                groups: gids.clone(),
                home: format!("/home/{a1}"),
                shell: "/bin/eurosh".to_string(),
                state: UserState::Active,
                caps: 0,
                created_at: now(),
                created_by: actor,
                password: rec,
                tpm_enrolled: false,
                failed_logins: 0,
            };
            let caps = state_caps(st, &user);
            match st.users.insert(user) {
                Ok(()) => {
                    st.audit.append(
                        &AuditEvent::UserCreated { uid, username: a1.to_string(), created_by: actor, groups: gids, caps },
                        now(),
                    );
                    alloc::vec![format!("[euro/users] gebruiker '{a1}' aangemaakt (uid={})", uid.0)]
                }
                Err(UserError::AlreadyExists(n)) => alloc::vec![format!("eurousers: '{n}' bestaat al")],
                Err(_) => alloc::vec!["eurousers: aanmaken mislukt".to_string()],
            }
        }

        "passwd" => {
            if let Some(e) = require_admin(is_admin) {
                return e;
            }
            if a1.is_empty() || a2.is_empty() {
                return alloc::vec!["gebruik: eurousers passwd <naam> <nieuw-wachtwoord>".to_string()];
            }
            if let Err(e) = validate_password(a2, &st.policy) {
                return alloc::vec![format!("eurousers: {}", e.message())];
            }
            let depth = st.policy.history_depth;
            let salt = salt32();
            let new_hash = Argon2idHash::create(a2.as_bytes(), &salt, &BOOT_PARAMS);
            let target_uid;
            {
                let u = match st.users.get_by_name_mut(a1) {
                    Some(u) => u,
                    None => return alloc::vec![format!("eurousers: gebruiker '{a1}' niet gevonden")],
                };
                if u.password.is_reused(a2.as_bytes(), depth) {
                    return alloc::vec![format!("eurousers: wachtwoord hergebruikt (laatste {depth} verboden)")];
                }
                u.password.set_new(new_hash, depth, now());
                // Admin-reset → forceer wijziging bij volgende login.
                u.password.must_change = true;
                target_uid = u.uid;
            }
            st.audit.append(&AuditEvent::PasswordChanged { actor, target: target_uid, admin_reset: true }, now());
            alloc::vec![format!("[euro/users] wachtwoord van '{a1}' gewijzigd (must-change bij volgende login)")]
        }

        "chpasswd" => {
            // Self-service: een gebruiker wijzigt zijn EIGEN wachtwoord en bewijst
            // eigendom met het oude. Dit WIST de must-change-vlag (via `set_new`) —
            // het pad waarmee een gebruiker na een admin-reset weer kan inloggen.
            // Geen CAP_USER_ADMIN nodig (je verandert enkel je eigen geheim).
            if a1.is_empty() || a2.is_empty() || a3.is_empty() {
                return alloc::vec!["gebruik: eurousers chpasswd <naam> <oud-pw> <nieuw-pw>".to_string()];
            }
            if let Err(e) = validate_password(a3, &st.policy) {
                return alloc::vec![format!("eurousers: {}", e.message())];
            }
            let depth = st.policy.history_depth;
            let salt = salt32();
            let new_hash = Argon2idHash::create(a3.as_bytes(), &salt, &BOOT_PARAMS);
            let target_uid;
            {
                let u = match st.users.get_by_name_mut(a1) {
                    Some(u) => u,
                    None => return alloc::vec![format!("eurousers: gebruiker '{a1}' niet gevonden")],
                };
                if !u.password.verify(a2.as_bytes()) {
                    return alloc::vec!["eurousers: oud wachtwoord onjuist".to_string()];
                }
                if u.password.is_reused(a3.as_bytes(), depth) {
                    return alloc::vec![format!("eurousers: wachtwoord hergebruikt (laatste {depth} verboden)")];
                }
                u.password.set_new(new_hash, depth, now()); // wist must_change
                target_uid = u.uid;
            }
            st.audit.append(&AuditEvent::PasswordChanged { actor, target: target_uid, admin_reset: false }, now());
            alloc::vec![format!("[euro/users] wachtwoord van '{a1}' gewijzigd (self-service; must-change gewist)")]
        }

        "lock" => {
            if let Some(e) = require_admin(is_admin) {
                return e;
            }
            let uid = match st.users.get_by_name(a1).map(|u| u.uid) {
                Some(u) => u,
                None => return alloc::vec![format!("eurousers: gebruiker '{a1}' niet gevonden")],
            };
            st.users.lock(uid, LockReason::AdminLock, actor, now()).ok();
            st.audit.append(&AuditEvent::UserLocked { uid, username: a1.to_string(), reason: LockReason::AdminLock, locked_by: actor }, now());
            alloc::vec![format!("[euro/users] account '{a1}' vergrendeld")]
        }

        "unlock" => {
            if let Some(e) = require_admin(is_admin) {
                return e;
            }
            let uid = match st.users.get_by_name(a1).map(|u| u.uid) {
                Some(u) => u,
                None => return alloc::vec![format!("eurousers: gebruiker '{a1}' niet gevonden")],
            };
            st.users.unlock(uid).ok();
            st.audit.append(&AuditEvent::UserUnlocked { uid, username: a1.to_string(), unlocked_by: actor }, now());
            alloc::vec![format!("[euro/users] account '{a1}' ontgrendeld")]
        }

        "del" => {
            if let Some(e) = require_admin(is_admin) {
                return e;
            }
            let uid = match st.users.get_by_name(a1).map(|u| u.uid) {
                Some(u) => u,
                None => return alloc::vec![format!("eurousers: gebruiker '{a1}' niet gevonden")],
            };
            st.users.soft_delete(uid, actor, now()).ok();
            st.audit.append(&AuditEvent::UserDeleted { uid, username: a1.to_string(), deleted_by: actor }, now());
            alloc::vec![format!("[euro/users] '{a1}' soft-deleted (record + home blijven, audit-vereiste)")]
        }

        "groups" => {
            let mut out = alloc::vec![format!("{:<8} {:>4} {:<24} {}", "GROEP", "GID", "CAPS", "LEDEN")];
            for g in st.groups.all() {
                let members: Vec<String> = st
                    .users
                    .all()
                    .iter()
                    .filter(|u| u.primary_gid == g.gid || u.groups.contains(&g.gid))
                    .map(|u| u.username.clone())
                    .collect();
                out.push(format!("{:<8} {:>4} {:<24} {}", g.name, g.gid.0, caps_summary(g.caps), members.join(",")));
            }
            out
        }

        "audit" => {
            if a1 == "--verify-chain" {
                let entries = st.audit.len();
                return match st.audit.verify_chain() {
                    Ok(()) => {
                        let root = euroid::hex(&st.audit.root_hash());
                        alloc::vec![
                            format!("[euro/audit] keten intact — {entries} events, geen knoeien gedetecteerd."),
                            format!("[euro/audit] root hash: sha256:{}", &root[..32.min(root.len())]),
                        ]
                    }
                    Err(seq) => alloc::vec![format!("[euro/audit] ✗ KETEN GEBROKEN bij record seq={seq} — knoeien gedetecteerd!")],
                };
            }
            // Filter optioneel op --user <naam>.
            let filter_uid = if a1 == "--user" { st.users.get_by_name(a2).map(|u| u.uid.0) } else { None };
            let mut out = alloc::vec![format!("audit-log ({} events, hash-chain, append-only):", st.audit.len())];
            for e in st.audit.entries() {
                if let Some(uid) = filter_uid {
                    if !e.body.contains(&format!("\"uid\":{uid}")) && !e.body.contains(&format!("\"target\":{uid}")) {
                        continue;
                    }
                }
                // Toon de body (zonder de hash-velden) — compact.
                out.push(format!("  #{:<4} {}", e.seq, e.body));
            }
            if out.len() == 1 {
                out.push("  (geen overeenkomende events)".to_string());
            }
            out
        }

        other => alloc::vec![format!("eurousers: onbekend subcommando '{other}' (probeer 'eurousers help')")],
    }
}
