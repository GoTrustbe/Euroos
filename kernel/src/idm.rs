//! Kernel-zijde van **EuroIDM** (plan V): soevereine bedrijfsidentiteit. Bij boot
//! zetten we een lokale IDM op (gebruikers + groep→capability-regels), geven we een
//! getekend token uit, leiden we de effectieve capabilities af, en bewijzen we dat
//! een privilege-escalatie (groep toevoegen ná ondertekening) faalt. Host-geteste
//! kern: [`euroidm`].

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

/// Boot-zelftest: token uitgeven + verifiëren + caps afleiden + escalatie weigeren.
pub fn selftest(seed: [u8; 32], from_tpm: bool, now: u64) {
    let idm = build_idm(seed);
    *IDM_PUB.lock() = Some(idm.public_key());

    // Geef 'anke' (groep users) een token van 1 uur.
    let tok = idm.issue_token("anke", now, 3600);
    let verified = tok.as_ref().map(|t| t.verify(&idm.public_key(), now + 60).is_ok()).unwrap_or(false);
    let caps = tok.as_ref().map(|t| idm.caps_for_groups(&t.groups)).unwrap_or(0);
    // 'anke' mag lezen + netwerk, maar NIET schrijven of user-admin.
    let caps_ok = caps & (CAP_LOGIN | CAP_NET) == (CAP_LOGIN | CAP_NET) && caps & (CAP_FS_WRITE | CAP_USER_ADMIN) == 0;

    // Privilege-escalatie: voeg 'admins' toe ná ondertekening → handtekening moet falen.
    let escalation_blocked = match tok {
        Some(mut t) => {
            t.groups.push(String::from("admins"));
            matches!(t.verify(&idm.public_key(), now + 60), Err(TokenError::BadSignature))
        }
        None => false,
    };

    let ok = verified && caps_ok && escalation_blocked;
    crate::serial_println!(
        "[v] EuroIDM: token 'anke'(users) uitgegeven+geverifieerd={verified} (IDM-seed-van-TPM={from_tpm}), caps=lezen+net-geen-schrijven={caps_ok}, escalatie(groep-toevoegen)-geweigerd={escalation_blocked} → {}",
        if ok { "OK (identiteit→capabilities, getekende tokens, lokaal soeverein) ✓" } else { "MISLUKT" }
    );
}

/// `euroidm`-shellcommando: toon de identiteitsopslag + groep→cap-regels.
pub fn shell() -> Vec<String> {
    // Toon een dry-run met een vaste seed (de echte IDM-staat leeft in een daemon).
    let idm = build_idm([0x1d; 32]);
    let mut out = alloc::vec![
        String::from("EuroIDM — soevereine bedrijfsidentiteit (lokaal; brug naar LDAP/OIDC optioneel)"),
        String::from("  identiteit → capabilities via groepslidmaatschap; getekende OIDC-achtige tokens (Ed25519)"),
    ];
    if let Some(pk) = &*IDM_PUB.lock() {
        let hex: String = pk.iter().take(8).map(|b| alloc::format!("{b:02x}")).collect();
        out.push(alloc::format!("  IDM-verificatiesleutel: {hex}…"));
    }
    for name in ["anke", "root", "controle"] {
        if let Some(u) = idm.lookup(name) {
            let caps = idm.caps_for_groups(&u.groups);
            out.push(alloc::format!("  {:<9} uid={:<5} groepen={:?}  caps=0b{:08b}", u.name, u.uid, u.groups, caps));
        }
    }
    out
}
