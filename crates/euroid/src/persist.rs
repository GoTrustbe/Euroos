//! **EuroID-persistentie** (Sprint AE-e2e): serialiseer de [`UserDb`] naar een
//! tekstformaat dat op EuroFS bewaard kan worden, en lees het terug — zodat
//! gebruikers, wachtwoord-hashes en accountstaat een herstart overleven i.p.v.
//! elke boot opnieuw opgebouwd te worden.
//!
//! Formaat: regelgebaseerd, versiekop `EUROID-DB-v1`, daarna één gebruiker per
//! regel met TAB-gescheiden velden. De wachtwoord-hash hergebruikt de PHC-codering
//! ([`Argon2idHash::encode`]). Pure `no_std`-logica → host-getest.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::cred::{Argon2idHash, PasswordRecord};
use crate::model::{LockReason, User, UserDb, UserState};
use crate::{GroupId, Timestamp, UserId};

const HEADER: &str = "EUROID-DB-v1";

#[derive(Debug, PartialEq, Eq)]
pub enum PersistError {
    BadHeader,
    BadField,
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len() / 2);
    let mut i = 0;
    while i < b.len() {
        let hi = (b[i] as char).to_digit(16)?;
        let lo = (b[i + 1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
        i += 2;
    }
    Some(out)
}

/// Decodeer een PHC-achtige Argon2id-string (`$argon2id$m=..,t=..,p=..$salt$tag`).
/// Lege salt/tag (een vergrendeld record) is toegestaan.
pub fn decode_hash(s: &str) -> Option<Argon2idHash> {
    // delen: ["", "argon2id", "m=..,t=..,p=..", "<salt-hex>", "<tag-hex>"]
    let parts: Vec<&str> = s.split('$').collect();
    if parts.len() != 5 || parts[1] != "argon2id" {
        return None;
    }
    let (mut m, mut t, mut p) = (0u32, 0u32, 0u32);
    for kv in parts[2].split(',') {
        let (k, v) = kv.split_once('=')?;
        let n: u32 = v.parse().ok()?;
        match k {
            "m" => m = n,
            "t" => t = n,
            "p" => p = n,
            _ => return None,
        }
    }
    Some(Argon2idHash { salt: unhex(parts[3])?, tag: unhex(parts[4])?, m_cost: m, t_cost: t, p_cost: p })
}

fn encode_state(s: &UserState) -> String {
    match s {
        UserState::Active => "active".to_string(),
        UserState::Locked { reason, locked_at, locked_by } => {
            alloc::format!("locked:{}:{}:{}", reason.tag(), locked_at.0, locked_by.0)
        }
        UserState::Expired { expired_at } => alloc::format!("expired:{}", expired_at.0),
        UserState::Deleted { deleted_at, deleted_by } => alloc::format!("deleted:{}:{}", deleted_at.0, deleted_by.0),
    }
}

fn reason_from_tag(t: &str) -> Option<LockReason> {
    Some(match t {
        "admin-lock" => LockReason::AdminLock,
        "failed-login-threshold" => LockReason::FailedLoginThreshold,
        "password-expired" => LockReason::PasswordExpired,
        "inactivity-timeout" => LockReason::InactivityTimeout,
        _ => return None,
    })
}

fn decode_state(s: &str) -> Option<UserState> {
    let mut it = s.split(':');
    match it.next()? {
        "active" => Some(UserState::Active),
        "locked" => {
            let reason = reason_from_tag(it.next()?)?;
            let locked_at = Timestamp(it.next()?.parse().ok()?);
            let locked_by = UserId(it.next()?.parse().ok()?);
            Some(UserState::Locked { reason, locked_at, locked_by })
        }
        "expired" => Some(UserState::Expired { expired_at: Timestamp(it.next()?.parse().ok()?) }),
        "deleted" => {
            let deleted_at = Timestamp(it.next()?.parse().ok()?);
            let deleted_by = UserId(it.next()?.parse().ok()?);
            Some(UserState::Deleted { deleted_at, deleted_by })
        }
        _ => None,
    }
}

// PasswordRecord ⇄ veld: `changed_at;expires|-;mustchange;locked;<curPHC>;<histPHC spatie-gescheiden>`.
// (PHC bevat geen `;` of spatie, dus deze scheidingstekens zijn veilig.)
fn encode_pw(p: &PasswordRecord) -> String {
    let expires = p.expires_at.map(|t| t.0.to_string()).unwrap_or_else(|| "-".to_string());
    let hist: Vec<String> = p.history.iter().map(|h| h.encode()).collect();
    alloc::format!(
        "{};{};{};{};{};{}",
        p.changed_at.0,
        expires,
        p.must_change as u8,
        p.locked as u8,
        p.hash.encode(),
        hist.join(" ")
    )
}

fn decode_pw(s: &str) -> Option<PasswordRecord> {
    let f: Vec<&str> = s.splitn(6, ';').collect();
    if f.len() != 6 {
        return None;
    }
    let changed_at = Timestamp(f[0].parse().ok()?);
    let expires_at = if f[1] == "-" { None } else { Some(Timestamp(f[1].parse().ok()?)) };
    let must_change = f[2] == "1";
    let locked = f[3] == "1";
    let hash = decode_hash(f[4])?;
    let mut history = Vec::new();
    for h in f[5].split(' ').filter(|x| !x.is_empty()) {
        history.push(decode_hash(h)?);
    }
    Some(PasswordRecord { hash, changed_at, expires_at, must_change, history, locked })
}

/// Serialiseer de hele [`UserDb`] naar het persistente tekstformaat.
pub fn serialize_db(db: &UserDb) -> String {
    let mut out = String::from(HEADER);
    out.push('\n');
    for u in db.all() {
        let groups: Vec<String> = u.groups.iter().map(|g| g.0.to_string()).collect();
        out.push_str(&alloc::format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            u.uid.0,
            u.username,
            u.display_name,
            u.primary_gid.0,
            groups.join(","),
            u.home,
            u.shell,
            encode_state(&u.state),
            u.caps,
            u.created_at.0,
            u.created_by.0,
            u.tpm_enrolled as u8,
            u.failed_logins,
            encode_pw(&u.password),
        ));
    }
    out
}

/// Lees een [`UserDb`] terug uit het persistente tekstformaat. Regels die niet
/// parsen worden afgewezen (corruptie) i.p.v. stil overgeslagen.
pub fn deserialize_db(data: &str) -> Result<UserDb, PersistError> {
    let mut lines = data.lines();
    if lines.next() != Some(HEADER) {
        return Err(PersistError::BadHeader);
    }
    let mut db = UserDb::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 14 {
            return Err(PersistError::BadField);
        }
        let parse_u32 = |s: &str| s.parse::<u32>().map_err(|_| PersistError::BadField);
        let parse_u64 = |s: &str| s.parse::<u64>().map_err(|_| PersistError::BadField);
        let mut groups = Vec::new();
        for g in f[4].split(',').filter(|x| !x.is_empty()) {
            groups.push(GroupId(parse_u32(g)?));
        }
        let user = User {
            uid: UserId(parse_u32(f[0])?),
            username: f[1].to_string(),
            display_name: f[2].to_string(),
            primary_gid: GroupId(parse_u32(f[3])?),
            groups,
            home: f[5].to_string(),
            shell: f[6].to_string(),
            state: decode_state(f[7]).ok_or(PersistError::BadField)?,
            caps: parse_u64(f[8])?,
            created_at: Timestamp(parse_u64(f[9])?),
            created_by: UserId(parse_u32(f[10])?),
            tpm_enrolled: f[11] == "1",
            failed_logins: parse_u32(f[12])?,
            password: decode_pw(f[13]).ok_or(PersistError::BadField)?,
        };
        db.insert(user).map_err(|_| PersistError::BadField)?;
    }
    Ok(db)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::argon2::Params;

    const P: Params = Params { m_cost: 256, t_cost: 1, p_cost: 1, tag_len: 32 };

    fn sample_db() -> UserDb {
        let mut db = UserDb::new();
        // root: vergrendeld systeem-account.
        let mut root = User {
            uid: UserId(0),
            username: "root".to_string(),
            display_name: "System Administrator".to_string(),
            primary_gid: GroupId(0),
            groups: Vec::new(),
            home: "/root".to_string(),
            shell: "/bin/eurosh".to_string(),
            state: UserState::Locked { reason: LockReason::AdminLock, locked_at: Timestamp(5), locked_by: UserId::ROOT },
            caps: 0,
            created_at: Timestamp(1),
            created_by: UserId(0),
            password: PasswordRecord::locked(),
            tpm_enrolled: false,
            failed_logins: 0,
        };
        root.failed_logins = 0;
        db.insert(root).unwrap();
        // euro: gewone gebruiker met must_change + een history-entry.
        let mut euro = User {
            uid: UserId(1000),
            username: "euro".to_string(),
            display_name: "Euro User".to_string(),
            primary_gid: GroupId(100),
            groups: alloc::vec![GroupId(2)],
            home: "/home/euro".to_string(),
            shell: "/bin/eurosh".to_string(),
            state: UserState::Active,
            caps: 0b1010,
            created_at: Timestamp(2),
            created_by: UserId(0),
            password: PasswordRecord::hash_password(b"euro", b"saltsalt", &P, Timestamp(2)),
            tpm_enrolled: true,
            failed_logins: 3,
        };
        euro.password.must_change = true;
        euro.password.history.push(Argon2idHash::create(b"old", b"oldsalt0", &P));
        db.insert(euro).unwrap();
        db
    }

    #[test]
    fn roundtrip_preserves_all_fields_and_verifies_password() {
        let db = sample_db();
        let text = serialize_db(&db);
        let back = deserialize_db(&text).unwrap();

        // Zelfde aantal gebruikers, en het wachtwoord verifieert nog (hash intact).
        assert_eq!(back.all().len(), 2);
        let euro = back.get_by_name("euro").unwrap();
        assert!(euro.password.hash.verify(b"euro"));
        assert!(!euro.password.hash.verify(b"fout"));
        assert!(euro.password.must_change);
        assert_eq!(euro.failed_logins, 3);
        assert_eq!(euro.groups, alloc::vec![GroupId(2)]);
        assert!(euro.tpm_enrolled);
        assert_eq!(euro.caps, 0b1010);

        // root blijft vergrendeld met de juiste reden + lege hash.
        let root = back.get(UserId::ROOT).unwrap();
        assert!(matches!(root.state, UserState::Locked { reason: LockReason::AdminLock, .. }));
        assert!(root.password.locked);
        // history-entry overleeft en verifieert.
        assert_eq!(euro.password.history.len(), 1);
        assert!(euro.password.history[0].verify(b"old"));
    }

    #[test]
    fn bad_header_rejected() {
        assert_eq!(deserialize_db("WRONG\n").err(), Some(PersistError::BadHeader));
    }

    #[test]
    fn corrupt_line_rejected_not_skipped() {
        let mut text = serialize_db(&sample_db());
        text.push_str("0\t1\t2\n"); // te weinig velden
        assert_eq!(deserialize_db(&text).err(), Some(PersistError::BadField));
    }

    #[test]
    fn decode_hash_handles_phc_and_empty() {
        let h = Argon2idHash::create(b"pw", b"saltsalt", &P);
        assert_eq!(decode_hash(&h.encode()), Some(h));
        // Vergrendeld record: lege salt/tag.
        let locked = Argon2idHash { salt: Vec::new(), tag: Vec::new(), m_cost: 0, t_cost: 0, p_cost: 0 };
        assert_eq!(decode_hash(&locked.encode()), Some(locked));
    }
}
