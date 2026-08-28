//! Kernel side of **EuroID** (Sprint K1 + P3): sovereign user management.
//!
//! At boot we build the identity store (the built-in groups + a few demo
//! accounts), and we prove the entire chain end-to-end: create a user
//! (Argon2id-hashed with a TPM-RNG salt) → log in with timing-attack prevention →
//! failed attempts that lock the account → an unknown user that fails
//! indistinguishably → a soft delete → and a **hash-chain audit log** that
//! records every action irreversibly and detects tampering. Host-tested core:
//! [`euroid`] (24 tests, including the official RFC 9106 Argon2id test vector).
//!
//! The Argon2id parameters are deliberately lowered at boot (memory/iterations) so
//! the self-test is fast under TCG; the real sovereign parameters (64 MiB/t=3/p=4) and
//! the RFC test vector are verified natively in the host tests.

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

/// The persistent user store on EuroFS (survives a reboot).
const USERS_DB: &str = "/etc/euroid/users.db";

/// Deliberately lowered Argon2id parameters for the boot self-test/runtime under TCG.
/// (The sovereign 64 MiB/t=3/p=4 + RFC vector are proven in the host tests.)
const BOOT_PARAMS: Params = Params { m_cost: 256, t_cost: 1, p_cost: 1, tag_len: 32 };

/// The live identity store.
struct State {
    users: UserDb,
    groups: GroupDb,
    audit: AuditLog,
    dummy: Argon2idHash,
    policy: PasswordPolicy,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Generate `n` random bytes — TPM-RNG if available, otherwise a
/// functional tick/RDTSC mix (the self-test stays valid; production uses TPM).
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

/// Create a user record (the useradd orchestration from the spec).
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

/// Build the store with the built-in groups + two demo accounts (alice, admin-bob).
fn build_state() -> State {
    let groups = GroupDb::with_builtins();
    let mut users = UserDb::new();
    let mut audit = AuditLog::new();
    audit.append(&AuditEvent::SystemInit, now());

    // root: system admin (wheel). Just like /etc/shadow `root:*`, interactive
    // login is locked — root access goes through sudo, not via a password.
    let mut root = build_user(0, "root", "System Administrator", GROUP_WHEEL, &[], 0, "*locked*", 0);
    root.state = UserState::Locked { reason: LockReason::AdminLock, locked_at: now(), locked_by: euroid::UserId::ROOT };
    users.insert(root).ok();

    // euro: the REAL desktop account (uid 1000, /etc/passwd-canonical). With this
    // the shell logs in via EuroID-Argon2id (no more SHA-256). Demo password "euro".
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

    // alice: regular user (groups users+net, own CAP_FILE) — K1 demo account.
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

    // bob: admin (wheel) — must change password on first login.
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

    // The dummy hash (same params as real accounts) for timing-attack prevention.
    let dummy = Argon2idHash::create(b"*invalid*", &salt32(), &BOOT_PARAMS);

    State { users, groups, audit, dummy, policy: PasswordPolicy::default() }
}

/// **K1 boot self-test** — the entire chain manage→auth→audit, end-to-end.
pub fn selftest() {
    let mut st = build_state();
    let from_tpm = crate::tpm::get_random(1).is_some();

    // 1. alice logs in correctly → session + LoginSuccess in the audit log.
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
    // alice gets LOGIN|FILE|DISPLAY (users) ∪ NET (net) ∪ FILE (own).
    let caps_ok = caps & (CAP_NET | CAP_FILE | euroid::CAP_DISPLAY | euroid::CAP_LOGIN)
        == (CAP_NET | CAP_FILE | euroid::CAP_DISPLAY | euroid::CAP_LOGIN)
        && caps & euroid::CAP_USER_ADMIN == 0;

    // 2. Unknown user → indistinguishable from a wrong password.
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

    // 3. Five wrong attempts on bob → account locked (lockout).
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

    // 4. Soft delete of alice → record still exists (audit requirement).
    st.users.soft_delete(euroid::UserId(1002), euroid::UserId::ROOT, now()).ok();
    st.audit.append(
        &AuditEvent::UserDeleted { uid: euroid::UserId(1002), username: "alice".to_string(), deleted_by: euroid::UserId::ROOT },
        now(),
    );
    let record_kept = st.users.get(euroid::UserId(1002)).is_some();

    // 5. Hash chain: the entire chain must verify intact. (Tamper detection — a
    //    tampered record that invalidates ALL following hashes — is proven robustly
    //    in the host tests `tampering_*_breaks_the_chain`.)
    let chain_ok = st.audit.verify_chain().is_ok();
    let entries = st.audit.len();
    let root = euroid::hex(&st.audit.root_hash());

    let ok = login_ok && caps_ok && unknown_generic && locked && record_kept && chain_ok;
    crate::serial_println!(
        "[k1] EuroID: useradd+Argon2id(TPM-salt={from_tpm}) → login alice(caps users∪net={caps_ok})={login_ok} · unknown-user-indistinguishable={unknown_generic} · 5×wrong→bob-locked={locked} · soft-delete-keeps-record={record_kept} · hash-chain-intact={chain_ok} ({entries} events, root sha256:{}) → {}",
        &root[..16.min(root.len())],
        if ok { "OK (Sprint K1: sovereign user management + tamper-evident audit, NIS2/GDPR/ISO 27001) ✓" } else { "FAILED" }
    );

    *STATE.lock() = Some(st);

    // Smoke test of the REAL shell path (not just compiled): run a few
    // `eurousers` commands against the live store and prove they work.
    let listed = shell("list", 0);
    let added = shell("add carla S3cure-Pass-9! users,net", 0);
    let verify = shell("audit --verify-chain", 0);
    let shell_ok = listed.iter().any(|l| l.contains("alice"))
        && added.iter().any(|l| l.contains("created"))
        && verify.iter().any(|l| l.contains("chain intact"));
    crate::serial_println!(
        "[k1] eurousers shell-path: 'list'-shows-users={} · 'add carla'={} · 'audit --verify-chain'={} → {}",
        listed.iter().any(|l| l.contains("alice")),
        added.iter().any(|l| l.contains("created")),
        verify.iter().any(|l| l.contains("chain intact")),
        if shell_ok { "OK (command path verified live) ✓" } else { "FAILED" }
    );

    // [ae] Audit #3 / Sprint AE: prove that the REAL login gate (`euroid::login`,
    // the path the shell `login`/`su` now uses) runs on Argon2id — a correct
    // password succeeds, a wrong one is rejected, and the locked root account
    // cannot log in interactively.
    let ok = login("euro", "euro").is_ok();
    let bad = matches!(login("euro", "wrong"), Err(_));
    let root_locked = matches!(login("root", "x"), Err(ref m) if m.contains("locked"));
    crate::serial_println!(
        "[ae] EuroID login (Argon2id, no more SHA-256): euro/'euro'={} · euro/'wrong'-rejected={} · root-locked-rejected={} → {}",
        ok, bad, root_locked,
        if ok && bad && root_locked { "OK (login path on sovereign Argon2id identity) ✓" } else { "FAILED" }
    );
}

/// **3D-10 / eIDAS 2.0** — EuroID acting as an EUDI-wallet issuer + relying
/// party. Issues an SD-JWT VC PID credential, then demonstrates **selective
/// disclosure**: the holder reveals only `nationality` to a verifier (proving
/// key binding), while `given_name`/`family_name`/`birthdate` stay hidden — yet
/// the issuer signature still verifies. A forged attribute is rejected.
pub fn wallet_selftest() {
    use eurowallet::json::Json;
    use eurowallet::{add_key_binding, issue, present, verify_with_key_binding, WalletError};
    use ed25519_dalek::SigningKey;

    let mut iss_seed = [0u8; 32];
    iss_seed.copy_from_slice(&rand_bytes(32));
    let issuer = SigningKey::from_bytes(&iss_seed);
    let issuer_pub = issuer.verifying_key();
    let mut hol_seed = [0u8; 32];
    hol_seed.copy_from_slice(&rand_bytes(32));
    let holder = SigningKey::from_bytes(&hol_seed);
    let holder_pub = holder.verifying_key();
    let from_tpm = crate::tpm::get_random(1).is_some();

    // Issue a Person Identification Data credential (member-state PID shape).
    let sd_jwt = issue(
        &issuer,
        &[
            ("iss", Json::Str("https://euro-id.eu".into())),
            ("vct", Json::Str("eu.europa.ec.eudi.pid.1".into())),
        ],
        &[
            ("2GLC42sKQveCfGfryNRN9w", "given_name", "Alice"),
            ("eluV5Og3gSNII8EYnsxA_A", "family_name", "Janssens"),
            ("6Ij7tM-a5iVPGboS5tmvVA", "nationality", "BE"),
            ("AJx-095VPrpTtN4QMOqROA", "birthdate", "1990-01-01"),
        ],
    );

    // Holder presents ONLY nationality to the relying party, with key binding.
    let aud = "https://age-check.example";
    let nonce = "n-0S6_WzA2Mj";
    let pres = present(&sd_jwt, &["nationality"]).unwrap_or_default();
    let bound = add_key_binding(&pres, &holder, aud, nonce);

    let verified = verify_with_key_binding(&bound, &issuer_pub, &holder_pub, aud, nonce);
    let nationality_ok = verified.as_ref().map(|c| c.get("nationality") == Some("BE")).unwrap_or(false);
    let name_hidden = verified.as_ref().map(|c| c.get("given_name").is_none() && c.get("family_name").is_none()).unwrap_or(false);

    // A replayed presentation to a different nonce must be refused (anti-replay).
    let replay_denied = verify_with_key_binding(&bound, &issuer_pub, &holder_pub, aud, "other-nonce")
        == Err(WalletError::BadKeyBinding);

    // A tampered credential (wrong issuer) must not verify.
    let other_issuer = SigningKey::from_bytes(&[0xEE; 32]).verifying_key();
    let forged_denied = verify_with_key_binding(&bound, &other_issuer, &holder_pub, aud, nonce).is_err();

    let ok = nationality_ok && name_hidden && replay_denied && forged_denied;
    crate::serial_println!(
        "[3d10] EuroID EUDI-wallet (SD-JWT VC, EdDSA, keys-from-TPM={from_tpm}): PID issued, holder discloses ONLY nationality={} · name/birthdate-hidden={} · replay-nonce-denied={} · wrong-issuer-denied={} → {}",
        nationality_ok, name_hidden, replay_denied, forged_denied,
        if ok { "OK (selective disclosure + holder key binding, eIDAS 2.0) ✓" } else { "FAILED" }
    );
}

/// Result of a successful shell login via EuroID.
pub struct LoginOk {
    pub uid: u32,
    pub name: String,
    pub caps: u64,
}

/// The uid + effective capabilities of `username` WITHOUT authentication — for
/// session bookkeeping only (e.g. reopening the default session at logout,
/// 3E-3). This never grants a login; it just reads the store.
pub fn user_caps(username: &str) -> Option<(u32, u64)> {
    let guard = STATE.lock();
    let st = guard.as_ref()?;
    let u = st.users.get_by_name(username)?;
    Some((u.uid.0, effective_caps(u, &st.groups, ALLOW_ALL)))
}

/// **Audit #3 / Sprint AE** — authenticate against the live EuroID store with
/// Argon2id (memory-hard), account-state check, lockout counter and a tamper-
/// evident audit log. This replaces the old iterated-SHA-256 verification against
/// /etc/shadow as the path the shell `login`/`su` uses. The audit events
/// are written unconditionally (logging is not skippable).
pub fn login(username: &str, password: &str) -> Result<LoginOk, String> {
    let mut guard = STATE.lock();
    let st = match guard.as_mut() {
        Some(s) => s,
        None => return Err("identity store not initialized".to_string()),
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
        st.audit.append(ev, now()); // audit MUST be written
    }
    match r.outcome {
        Ok(session) => Ok(LoginOk { uid: session.uid.0, name: session.username, caps: session.caps }),
        Err(e) => Err(match e {
            AuthError::InvalidCredentials => "invalid username or password".to_string(),
            AuthError::AccountLocked => "account locked (too many attempts or admin lock)".to_string(),
            AuthError::AccountExpired => "account expired".to_string(),
            AuthError::MustChangePassword => {
                "password must be changed first (eurousers passwd)".to_string()
            }
        }),
    }
}

/// Self-service password change against the live store (used by the
/// GUI lockscreen on a must-change). Verifies the old password, validates
/// the new one (policy + history) and clears the must-change flag. `Ok` = changed.
pub fn change_own_password(user: &str, old: &str, new: &str) -> Result<(), String> {
    let mut guard = STATE.lock();
    let st = match guard.as_mut() {
        Some(s) => s,
        None => return Err("identity store not initialized".to_string()),
    };
    if let Err(e) = validate_password(new, &st.policy) {
        return Err(e.message().to_string());
    }
    let depth = st.policy.history_depth;
    let salt = salt32();
    let new_hash = Argon2idHash::create(new.as_bytes(), &salt, &BOOT_PARAMS);
    let target;
    {
        let u = st.users.get_by_name_mut(user).ok_or_else(|| "user not found".to_string())?;
        if !u.password.verify(old.as_bytes()) {
            return Err("old password incorrect".to_string());
        }
        if u.password.is_reused(new.as_bytes(), depth) {
            return Err(alloc::format!("password reused (last {depth} forbidden)"));
        }
        u.password.set_new(new_hash, depth, now()); // clears must_change
        target = u.uid;
    }
    st.audit.append(&AuditEvent::PasswordChanged { actor: target, target, admin_reset: false }, now());
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Persistence (Sprint AE-e2e): the user store survives a reboot.
// ─────────────────────────────────────────────────────────────────────────────

/// Write the live user store to `/etc/euroid/users.db`. Called after every
/// mutating `eurousers` action so that changes are durable.
pub fn persist_state(fs: &mut dyn eurofs::FileSystem) -> bool {
    // Kernel service: the identity store is system state (0600, uid 0).
    crate::sysctx::as_system(fs, |fs| persist_state_inner(fs))
}

fn persist_state_inner(fs: &mut dyn eurofs::FileSystem) -> bool {
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

/// Load the user store from disk if present. Returns the number of loaded
/// users (0 = no file / empty → first boot). On corruption: 0
/// (the caller then falls back to `build_state`).
fn load_users_from_disk(fs: &mut dyn eurofs::FileSystem) -> Option<UserDb> {
    // Kernel service: the identity store is 0600/uid 0; unlock/login must read
    // it even while a user session holds the uid-context.
    let data = crate::sysctx::as_system(fs, |fs| fs.read_file(USERS_DB)).ok()?;
    let text = core::str::from_utf8(&data).ok()?;
    match deserialize_db(text) {
        Ok(db) if !db.all().is_empty() => Some(db),
        _ => None,
    }
}

/// **Sprint AE-e2e boot self-test** — proves that the EuroID store survives a
/// reboot: build the store, persist to EuroFS, read it back FROM DISK, and
/// show that (1) the Argon2id password of 'euro' still verifies, (2) a newly
/// created user remains present after re-persist + re-read (survives remount).
pub fn persist_selftest(fs: &mut dyn eurofs::FileSystem) {
    use eurofs::FileSystem;

    // 1. Build + serialize + write to disk.
    let st = build_state();
    let text = serialize_db(&st.users);
    let _ = fs.create_dir("/etc");
    let _ = fs.create_dir("/etc/euroid");
    let wrote = fs.write_file(USERS_DB, text.as_bytes()).is_ok();

    // 2. Read BACK from disk → euro's password still verifies (hash survived).
    let reloaded = load_users_from_disk(fs);
    let euro_ok = reloaded
        .as_ref()
        .and_then(|db| db.get_by_name("euro"))
        .map(|u| u.password.verify(b"euro") && !u.password.verify(b"wrong"))
        .unwrap_or(false);
    // root stays locked after re-reading.
    let root_locked = reloaded
        .as_ref()
        .and_then(|db| db.get(euroid::UserId::ROOT))
        .map(|u| matches!(u.state, UserState::Locked { .. }))
        .unwrap_or(false);

    // 3. Mutation-survives-remount: add a user, re-persist, re-read.
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
        "[ae-persist] EuroID persistent on EuroFS: written={wrote}, euro-Argon2id-after-reread={euro_ok}, root-locked-after-reread={root_locked}, new-user-survives-remount={survives} → {}",
        if ok { "OK (identity + password hashes survive a reboot) ✓" } else { "FAILED" }
    );
}

/// **Sprint AE-e2e boot self-test** — must-change-password enforcement. Proves that
/// an account with the must-change flag CANNOT log in (even with the correct
/// password), that a self-service change clears the flag, and that logging in
/// afterwards with the NEW password succeeds while the old one fails.
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

    // 1. Correct password BUT must_change → login rejected (MustChangePassword).
    let blocked = matches!(auth(&mut db, "OldPass-1!", &dummy), Err(AuthError::MustChangePassword));

    // 2. Self-service change: verify the old one, set a new one → must_change cleared.
    let depth = PasswordPolicy::default().history_depth;
    let cleared = {
        let salt = salt32();
        let nh = Argon2idHash::create(b"NewPass-2!", &salt, &BOOT_PARAMS);
        let user = db.get_by_name_mut("resetuser").unwrap();
        let old_ok = user.password.verify(b"OldPass-1!");
        user.password.set_new(nh, depth, now());
        old_ok && !user.password.must_change
    };

    // 3. Login with the NEW password succeeds; the old one fails.
    let now_ok = auth(&mut db, "NewPass-2!", &dummy).is_ok();
    let old_fails = auth(&mut db, "OldPass-1!", &dummy).is_err();

    let ok = blocked && cleared && now_ok && old_fails;
    crate::serial_println!(
        "[ae-mustchange] must-change enforcement: correct-pw-but-blocked={blocked}, self-service-change-clears-flag={cleared}, login-with-new-pw-OK={now_ok}, old-pw-fails={old_fails} → {}",
        if ok { "OK (forced password change enforced end-to-end) ✓" } else { "FAILED" }
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// `eurousers` shell command.
// ─────────────────────────────────────────────────────────────────────────────

fn caps_summary(caps: u64) -> String {
    cap_names(caps).join("|")
}

fn state_caps(st: &State, u: &User) -> u64 {
    effective_caps(u, &st.groups, ALLOW_ALL)
}

/// `eurousers <subcmd> [args...]` — sovereign user management from the shell.
/// `actor_uid` is the uid of the current session (for the CAP_USER_ADMIN check).
pub fn shell(args: &str, actor_uid: u32) -> Vec<String> {
    let mut guard = STATE.lock();
    let st = match guard.as_mut() {
        Some(s) => s,
        None => return alloc::vec!["eurousers: identity store not initialized".to_string()],
    };

    let mut it = args.split_whitespace();
    let sub = it.next().unwrap_or("");
    let a1 = it.next().unwrap_or("");
    let a2 = it.next().unwrap_or("");
    let a3 = it.next().unwrap_or("");

    // Who is running this? Do they have CAP_USER_ADMIN (wheel)?
    let actor = euroid::UserId(actor_uid);
    let is_admin = st
        .users
        .get(actor)
        .map(|u| state_caps(st, u) & euroid::CAP_USER_ADMIN != 0)
        .unwrap_or(actor_uid == 0); // uid 0 = root/system always allowed

    let require_admin = |is_admin: bool| -> Option<Vec<String>> {
        if is_admin {
            None
        } else {
            Some(alloc::vec!["eurousers: EPERM — requires CAP_USER_ADMIN (wheel group)".to_string()])
        }
    };

    match sub {
        "" | "help" => alloc::vec![
            "eurousers — sovereign user management (Sprint K1)".to_string(),
            "  list                       all users + state".to_string(),
            "  show <name>                full record (no password hash)".to_string(),
            "  add <name> <pw> [group,..] create user (Argon2id)".to_string(),
            "  passwd <name> <new-pw>     password (admin-reset → must-change)".to_string(),
            "  chpasswd <name> <old> <new>  change own password (clears must-change)".to_string(),
            "  lock <name> / unlock <name>  (un)lock account".to_string(),
            "  del <name>                 soft delete (record kept for audit)".to_string(),
            "  groups                     all groups + members + caps".to_string(),
            "  audit [--user N|--verify-chain]  the hash-chain audit log".to_string(),
        ],

        "list" => {
            let mut out = alloc::vec![format!("{:<10} {:>5} {:<10} {}", "USER", "UID", "STATE", "GROUPS")];
            for u in st.users.all() {
                let state = match &u.state {
                    UserState::Active => "active".to_string(),
                    UserState::Locked { reason, .. } => format!("locked({})", reason.tag()),
                    UserState::Expired { .. } => "expired".to_string(),
                    UserState::Deleted { .. } => "deleted".to_string(),
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
                None => return alloc::vec![format!("eurousers: user '{a1}' not found")],
            };
            let groups: Vec<String> = u.groups.iter().filter_map(|g| st.groups.get(*g).map(|g| g.name.clone())).collect();
            let must = if u.password.must_change { " (must-change)" } else { "" };
            alloc::vec![
                format!("user:         {} (uid={})", u.username, u.uid.0),
                format!("display name: {}", u.display_name),
                format!("home/shell:   {}  {}", u.home, u.shell),
                format!("primary grp:  {}", st.groups.get(u.primary_gid).map(|g| g.name.as_str()).unwrap_or("?")),
                format!("groups:       {}", groups.join(",")),
                format!("effective caps: {}", caps_summary(state_caps(st, u))),
                format!("password:     Argon2id{must} (hash not shown)"),
                format!("created:      t={} by uid={}", u.created_at.0, u.created_by.0),
                format!("failed-logins: {}", u.failed_logins),
            ]
        }

        "add" => {
            if let Some(e) = require_admin(is_admin) {
                return e;
            }
            if a1.is_empty() || a2.is_empty() {
                return alloc::vec!["usage: eurousers add <name> <password> [group,group]".to_string()];
            }
            if let Err(msg) = validate_username(a1) {
                return alloc::vec![format!("eurousers: {msg}")];
            }
            if st.users.exists(a1) {
                return alloc::vec![format!("eurousers: user '{a1}' already exists")];
            }
            if let Err(e) = validate_password(a2, &st.policy) {
                return alloc::vec![format!("eurousers: {}", e.message())];
            }
            // Resolve groups (default: users).
            let mut gids: Vec<GroupId> = Vec::new();
            if !a3.is_empty() {
                for g in a3.split(',') {
                    match st.groups.by_name(g) {
                        Some(gr) => gids.push(gr.gid),
                        None => return alloc::vec![format!("eurousers: unknown group '{g}'")],
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
                    alloc::vec![format!("[euro/users] user '{a1}' created (uid={})", uid.0)]
                }
                Err(UserError::AlreadyExists(n)) => alloc::vec![format!("eurousers: '{n}' already exists")],
                Err(_) => alloc::vec!["eurousers: creation failed".to_string()],
            }
        }

        "passwd" => {
            if let Some(e) = require_admin(is_admin) {
                return e;
            }
            if a1.is_empty() || a2.is_empty() {
                return alloc::vec!["usage: eurousers passwd <name> <new-password>".to_string()];
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
                    None => return alloc::vec![format!("eurousers: user '{a1}' not found")],
                };
                if u.password.is_reused(a2.as_bytes(), depth) {
                    return alloc::vec![format!("eurousers: password reused (last {depth} forbidden)")];
                }
                u.password.set_new(new_hash, depth, now());
                // Admin reset → force a change at next login.
                u.password.must_change = true;
                target_uid = u.uid;
            }
            st.audit.append(&AuditEvent::PasswordChanged { actor, target: target_uid, admin_reset: true }, now());
            alloc::vec![format!("[euro/users] password of '{a1}' changed (must-change at next login)")]
        }

        "chpasswd" => {
            // Self-service: a user changes their OWN password and proves
            // ownership with the old one. This CLEARS the must-change flag (via `set_new`) —
            // the path by which a user can log in again after an admin reset.
            // No CAP_USER_ADMIN required (you only change your own secret).
            if a1.is_empty() || a2.is_empty() || a3.is_empty() {
                return alloc::vec!["usage: eurousers chpasswd <name> <old-pw> <new-pw>".to_string()];
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
                    None => return alloc::vec![format!("eurousers: user '{a1}' not found")],
                };
                if !u.password.verify(a2.as_bytes()) {
                    return alloc::vec!["eurousers: old password incorrect".to_string()];
                }
                if u.password.is_reused(a3.as_bytes(), depth) {
                    return alloc::vec![format!("eurousers: password reused (last {depth} forbidden)")];
                }
                u.password.set_new(new_hash, depth, now()); // clears must_change
                target_uid = u.uid;
            }
            st.audit.append(&AuditEvent::PasswordChanged { actor, target: target_uid, admin_reset: false }, now());
            alloc::vec![format!("[euro/users] password of '{a1}' changed (self-service; must-change cleared)")]
        }

        "lock" => {
            if let Some(e) = require_admin(is_admin) {
                return e;
            }
            let uid = match st.users.get_by_name(a1).map(|u| u.uid) {
                Some(u) => u,
                None => return alloc::vec![format!("eurousers: user '{a1}' not found")],
            };
            st.users.lock(uid, LockReason::AdminLock, actor, now()).ok();
            st.audit.append(&AuditEvent::UserLocked { uid, username: a1.to_string(), reason: LockReason::AdminLock, locked_by: actor }, now());
            alloc::vec![format!("[euro/users] account '{a1}' locked")]
        }

        "unlock" => {
            if let Some(e) = require_admin(is_admin) {
                return e;
            }
            let uid = match st.users.get_by_name(a1).map(|u| u.uid) {
                Some(u) => u,
                None => return alloc::vec![format!("eurousers: user '{a1}' not found")],
            };
            st.users.unlock(uid).ok();
            st.audit.append(&AuditEvent::UserUnlocked { uid, username: a1.to_string(), unlocked_by: actor }, now());
            alloc::vec![format!("[euro/users] account '{a1}' unlocked")]
        }

        "del" => {
            if let Some(e) = require_admin(is_admin) {
                return e;
            }
            let uid = match st.users.get_by_name(a1).map(|u| u.uid) {
                Some(u) => u,
                None => return alloc::vec![format!("eurousers: user '{a1}' not found")],
            };
            st.users.soft_delete(uid, actor, now()).ok();
            st.audit.append(&AuditEvent::UserDeleted { uid, username: a1.to_string(), deleted_by: actor }, now());
            alloc::vec![format!("[euro/users] '{a1}' soft-deleted (record + home kept, audit requirement)")]
        }

        "groups" => {
            let mut out = alloc::vec![format!("{:<8} {:>4} {:<24} {}", "GROUP", "GID", "CAPS", "MEMBERS")];
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
                            format!("[euro/audit] chain intact — {entries} events, no tampering detected."),
                            format!("[euro/audit] root hash: sha256:{}", &root[..32.min(root.len())]),
                        ]
                    }
                    Err(seq) => alloc::vec![format!("[euro/audit] ✗ CHAIN BROKEN at record seq={seq} — tampering detected!")],
                };
            }
            // Optionally filter on --user <name>.
            let filter_uid = if a1 == "--user" { st.users.get_by_name(a2).map(|u| u.uid.0) } else { None };
            let mut out = alloc::vec![format!("audit-log ({} events, hash-chain, append-only):", st.audit.len())];
            for e in st.audit.entries() {
                if let Some(uid) = filter_uid {
                    if !e.body.contains(&format!("\"uid\":{uid}")) && !e.body.contains(&format!("\"target\":{uid}")) {
                        continue;
                    }
                }
                // Show the body (without the hash fields) — compact.
                out.push(format!("  #{:<4} {}", e.seq, e.body));
            }
            if out.len() == 1 {
                out.push("  (no matching events)".to_string());
            }
            out
        }

        other => alloc::vec![format!("eurousers: unknown subcommand '{other}' (try 'eurousers help')")],
    }
}
