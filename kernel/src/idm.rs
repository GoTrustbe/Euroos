//! Kernel side of **EuroIDM** (plan V): sovereign enterprise identity. At boot
//! we set up a local IDM (users + group→capability rules), issue a signed
//! token, derive the effective capabilities, and prove that a privilege
//! escalation (adding a group after signing) fails. Host-tested
//! core: [`euroidm`].

use alloc::string::String;
use alloc::vec::Vec;

use euroidm::{Idm, TokenError, CAP_FS_WRITE, CAP_LOGIN, CAP_NET, CAP_USER_ADMIN};
use spin::Mutex;

static IDM_PUB: Mutex<Option<[u8; 32]>> = Mutex::new(None);

fn build_idm(seed: [u8; 32]) -> Idm {
    let mut idm = Idm::new(seed);
    idm.set_group_caps("users", CAP_LOGIN | CAP_NET | euroidm::CAP_FS_READ);
    idm.set_group_caps(
        "admins",
        CAP_LOGIN | CAP_NET | euroidm::CAP_FS_READ | CAP_FS_WRITE | CAP_USER_ADMIN | euroidm::CAP_SHUTDOWN | euroidm::CAP_IMMUTABLE_ADMIN,
    );
    idm.set_group_caps("auditors", CAP_LOGIN | euroidm::CAP_AUDIT_READ);
    idm.add_user(1000, "anke", &["users"]);
    idm.add_user(0, "root", &["admins"]);
    idm.add_user(1001, "controle", &["users", "auditors"]);
    idm
}

/// Boot self-test: issue token + verify + derive caps + deny escalation.
pub fn selftest(seed: [u8; 32], from_tpm: bool, now: u64) {
    let idm = build_idm(seed);
    *IDM_PUB.lock() = Some(idm.public_key());

    // Give 'anke' (group users) a token valid for 1 hour.
    let tok = idm.issue_token("anke", now, 3600);
    let verified = tok.as_ref().map(|t| t.verify(&idm.public_key(), now + 60).is_ok()).unwrap_or(false);
    let caps = tok.as_ref().map(|t| idm.caps_for_groups(&t.groups)).unwrap_or(0);
    // 'anke' may read + network, but NOT write or user-admin.
    let caps_ok = caps & (CAP_LOGIN | CAP_NET) == (CAP_LOGIN | CAP_NET) && caps & (CAP_FS_WRITE | CAP_USER_ADMIN) == 0;

    // Privilege escalation: add 'admins' after signing → signature must fail.
    let escalation_blocked = match tok {
        Some(mut t) => {
            t.groups.push(String::from("admins"));
            matches!(t.verify(&idm.public_key(), now + 60), Err(TokenError::BadSignature))
        }
        None => false,
    };

    let ok = verified && caps_ok && escalation_blocked;
    crate::serial_println!(
        "[v] EuroIDM: token 'anke'(users) issued+verified={verified} (IDM-seed-from-TPM={from_tpm}), caps=read+net-no-write={caps_ok}, escalation(add-group)-denied={escalation_blocked} → {}",
        if ok { "OK (identity→capabilities, signed tokens, locally sovereign) ✓" } else { "FAILED" }
    );
}

/// `euroidm` shell command: show the identity store + group→cap rules.
pub fn shell() -> Vec<String> {
    // Show a dry run with a fixed seed (the real IDM state lives in a daemon).
    let idm = build_idm([0x1d; 32]);
    let mut out = alloc::vec![
        String::from("EuroIDM — sovereign enterprise identity (local; bridge to LDAP/OIDC optional)"),
        String::from("  identity → capabilities via group membership; signed OIDC-like tokens (Ed25519)"),
    ];
    if let Some(pk) = &*IDM_PUB.lock() {
        let hex: String = pk.iter().take(8).map(|b| alloc::format!("{b:02x}")).collect();
        out.push(alloc::format!("  IDM verification key: {hex}…"));
    }
    for name in ["anke", "root", "controle"] {
        if let Some(u) = idm.lookup(name) {
            let caps = idm.caps_for_groups(&u.groups);
            out.push(alloc::format!("  {:<9} uid={:<5} groups={:?}  caps=0b{:08b}", u.name, u.uid, u.groups, caps));
        }
    }
    out
}
