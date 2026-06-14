//! EuroAuth — the login flow (PAM equivalent), with timing-attack prevention.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::audit::{AuditEvent, DenyReason};
use crate::cred::Argon2idHash;
use crate::model::{LockReason, UserDb, UserState};
use crate::{effective_caps, hex, Caps, GroupDb, Timestamp, UserId};

/// A login credential.
#[derive(Clone, Debug)]
pub enum Credential {
    Password(String),
    // TpmKey(..) — TPM-bound login is a later milestone.
}

/// Why a login fails. At the top level, `InvalidCredentials` is IDENTICAL for
/// an unknown user and a wrong password (no enumeration).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthError {
    InvalidCredentials,
    AccountLocked,
    AccountExpired,
    MustChangePassword,
}

/// An active session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: [u8; 32],
    pub uid: UserId,
    pub username: String,
    pub caps: Caps,
    pub started_at: Timestamp,
    pub last_active: Timestamp,
    pub tty: String,
}

impl Session {
    pub fn id_hex(&self) -> String {
        hex(&self.id)
    }
}

/// The result of [`authenticate`]: the outcome plus the audit events that the
/// caller MUST persist (logging cannot be skipped).
pub struct AuthResult {
    pub outcome: Result<Session, AuthError>,
    pub events: Vec<AuditEvent>,
}

/// Full login sequence. `dummy` is a precomputed Argon2id hash with the
/// system parameters: for an unknown user we verify against it, so that the
/// running time equals that of an existing account with a wrong password
/// (no username enumeration via timing).
#[allow(clippy::too_many_arguments)]
pub fn authenticate(
    db: &mut UserDb,
    groups: &GroupDb,
    username: &str,
    cred: Credential,
    now: Timestamp,
    session_id: [u8; 32],
    europol_allowed: Caps,
    tty: &str,
    dummy: &Argon2idHash,
) -> AuthResult {
    let Credential::Password(password) = cred;
    let mut events = Vec::new();

    // 1. Look up the user.
    let found = db.get_by_name(username).cloned();

    // 2. Check the state BEFORE password verification.
    let user = match found {
        Some(u) => match &u.state {
            UserState::Locked { reason, .. } => {
                events.push(AuditEvent::LoginDenied {
                    username: username.to_string(),
                    reason: DenyReason::AccountLocked(*reason),
                });
                // Burn time anyway so that locked vs. unknown is not
                // distinguishable via timing.
                let _ = dummy.verify(password.as_bytes());
                return AuthResult { outcome: Err(AuthError::AccountLocked), events };
            }
            UserState::Expired { .. } => {
                events.push(AuditEvent::LoginDenied {
                    username: username.to_string(),
                    reason: DenyReason::AccountExpired,
                });
                let _ = dummy.verify(password.as_bytes());
                return AuthResult { outcome: Err(AuthError::AccountExpired), events };
            }
            UserState::Deleted { .. } => {
                // Treat as "unknown" — do not reveal the deletion.
                let _ = dummy.verify(password.as_bytes());
                events.push(AuditEvent::LoginDenied {
                    username: username.to_string(),
                    reason: DenyReason::AccountDeleted,
                });
                return AuthResult { outcome: Err(AuthError::InvalidCredentials), events };
            }
            UserState::Active => u,
        },
        None => {
            // Unknown user: do a dummy Argon2id verification (same
            // running time) and fail with the same generic error.
            let _ = dummy.verify(password.as_bytes());
            events.push(AuditEvent::LoginDenied {
                username: username.to_string(),
                reason: DenyReason::UnknownUser,
            });
            return AuthResult { outcome: Err(AuthError::InvalidCredentials), events };
        }
    };

    let uid = user.uid;
    let max_failed = 5u32; // policy.max_failed_logins (system policy)

    // 3. Verify the password.
    let verified = user.password.verify(password.as_bytes());

    if !verified {
        let attempts = db.record_failed_login(uid);
        events.push(AuditEvent::LoginFailed {
            uid,
            username: username.to_string(),
            attempt: attempts,
        });
        // Lock when the threshold is exceeded.
        if attempts >= max_failed {
            let _ = db.lock(uid, LockReason::FailedLoginThreshold, UserId::SYSTEM, now);
            events.push(AuditEvent::UserLocked {
                uid,
                username: username.to_string(),
                reason: LockReason::FailedLoginThreshold,
                locked_by: UserId::SYSTEM,
            });
        }
        return AuthResult { outcome: Err(AuthError::InvalidCredentials), events };
    }

    // 4. Reset the counter on success.
    db.reset_failed_logins(uid);

    // 5. Must the password be changed?
    let must_change = db.get(uid).map(|u| u.password.must_change).unwrap_or(false);
    if must_change {
        return AuthResult { outcome: Err(AuthError::MustChangePassword), events };
    }

    // 6. Derive the capability set for this session (fixed for the session's duration).
    let user = db.get(uid).unwrap();
    let caps = effective_caps(user, groups, europol_allowed);
    let uname = user.username.clone();

    // 7. Create the session.
    let session = Session {
        id: session_id,
        uid,
        username: uname.clone(),
        caps,
        started_at: now,
        last_active: now,
        tty: tty.to_string(),
    };

    // 8. Audit — successful login.
    events.push(AuditEvent::LoginSuccess {
        uid,
        username: uname,
        session: hex(&session_id),
        caps,
        tty: tty.to_string(),
    });

    AuthResult { outcome: Ok(session), events }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::argon2::Params;
    use crate::cred::PasswordRecord;
    use crate::model::{User, UserState};
    use crate::{ALLOW_ALL, GROUP_USERS};

    // Shared, fast-but-realistic params for timing parity (both the
    // real record and the dummy use them → the running time is equal by definition).
    fn params() -> Params {
        Params { m_cost: 2048, t_cost: 2, p_cost: 1, tag_len: 32 }
    }

    fn mk_db() -> (UserDb, GroupDb, Argon2idHash) {
        let gdb = GroupDb::with_builtins();
        let mut db = UserDb::new();
        let salt = [5u8; 32];
        let rec = PasswordRecord::hash_password(b"Correct-Horse-9!", &salt, &params(), Timestamp(1));
        db.insert(User {
            uid: UserId(1000),
            username: "alice".to_string(),
            display_name: "Alice".to_string(),
            primary_gid: GROUP_USERS,
            groups: alloc::vec![crate::GROUP_NET],
            home: "/home/alice".to_string(),
            shell: "/bin/eurosh".to_string(),
            state: UserState::Active,
            caps: crate::CAP_FILE,
            created_at: Timestamp(1),
            created_by: UserId::ROOT,
            password: rec,
            tpm_enrolled: false,
            failed_logins: 0,
        })
        .unwrap();
        // The dummy uses the same params as real accounts.
        let dummy = Argon2idHash::create(b"*", &[0u8; 32], &params());
        (db, gdb, dummy)
    }

    #[test]
    fn correct_password_logs_in() {
        let (mut db, gdb, dummy) = mk_db();
        let r = authenticate(
            &mut db,
            &gdb,
            "alice",
            Credential::Password("Correct-Horse-9!".to_string()),
            Timestamp(100),
            [9u8; 32],
            ALLOW_ALL,
            "/dev/tty1",
            &dummy,
        );
        let session = r.outcome.expect("login must succeed");
        assert_eq!(session.uid, UserId(1000));
        // Caps = own FILE ∪ users(LOGIN|FILE|DISPLAY) ∪ net(LOGIN|NET).
        assert_eq!(session.caps & crate::CAP_NET, crate::CAP_NET);
        assert_eq!(session.caps & crate::CAP_DISPLAY, crate::CAP_DISPLAY);
        assert!(r.events.iter().any(|e| e.name() == "LoginSuccess"));
    }

    #[test]
    fn wrong_password_and_unknown_user_are_indistinguishable() {
        let (mut db, gdb, dummy) = mk_db();
        // Wrong password for an existing user.
        let r1 = authenticate(
            &mut db,
            &gdb,
            "alice",
            Credential::Password("wrong".to_string()),
            Timestamp(100),
            [1u8; 32],
            ALLOW_ALL,
            "tty1",
            &dummy,
        );
        // Unknown user.
        let r2 = authenticate(
            &mut db,
            &gdb,
            "nemo",
            Credential::Password("whatever".to_string()),
            Timestamp(100),
            [2u8; 32],
            ALLOW_ALL,
            "tty1",
            &dummy,
        );
        // At the top level: exactly the same error (no enumeration).
        assert_eq!(r1.outcome, Err(AuthError::InvalidCredentials));
        assert_eq!(r2.outcome, Err(AuthError::InvalidCredentials));
    }

    #[test]
    fn timing_unknown_user_matches_wrong_password() {
        use std::time::Instant;
        let (mut db, gdb, dummy) = mk_db();
        // Measure wrong-password (existing user, real Argon2id verification).
        let t0 = Instant::now();
        let _ = authenticate(
            &mut db,
            &gdb,
            "alice",
            Credential::Password("wrong".to_string()),
            Timestamp(100),
            [1u8; 32],
            ALLOW_ALL,
            "tty1",
            &dummy,
        );
        let wrong = t0.elapsed().as_secs_f64();
        // Measure unknown-user (dummy Argon2id verification).
        let t1 = Instant::now();
        let _ = authenticate(
            &mut db,
            &gdb,
            "nemo",
            Credential::Password("whatever".to_string()),
            Timestamp(100),
            [2u8; 32],
            ALLOW_ALL,
            "tty1",
            &dummy,
        );
        let unknown = t1.elapsed().as_secs_f64();
        // Both do one Argon2id verification with the same params → comparable time.
        // Generous tolerance against scheduler noise, but enough to catch "0 vs. ~ms".
        let ratio = wrong.max(1e-9) / unknown.max(1e-9);
        assert!(
            (0.2..5.0).contains(&ratio),
            "timing must be comparable: wrong={wrong:.6}s unknown={unknown:.6}s ratio={ratio:.2}"
        );
    }

    #[test]
    fn five_failures_lock_the_account() {
        let (mut db, gdb, dummy) = mk_db();
        for _ in 0..5 {
            let r = authenticate(
                &mut db,
                &gdb,
                "alice",
                Credential::Password("nope".to_string()),
                Timestamp(100),
                [0u8; 32],
                ALLOW_ALL,
                "tty1",
                &dummy,
            );
            assert_eq!(r.outcome, Err(AuthError::InvalidCredentials));
        }
        // Account is now locked; even the correct password fails with Locked.
        let r = authenticate(
            &mut db,
            &gdb,
            "alice",
            Credential::Password("Correct-Horse-9!".to_string()),
            Timestamp(100),
            [0u8; 32],
            ALLOW_ALL,
            "tty1",
            &dummy,
        );
        assert_eq!(r.outcome, Err(AuthError::AccountLocked));
        assert!(matches!(db.get(UserId(1000)).unwrap().state, UserState::Locked { .. }));
    }

    #[test]
    fn deleted_user_treated_as_unknown() {
        let (mut db, gdb, dummy) = mk_db();
        db.soft_delete(UserId(1000), UserId::ROOT, Timestamp(50)).unwrap();
        let r = authenticate(
            &mut db,
            &gdb,
            "alice",
            Credential::Password("Correct-Horse-9!".to_string()),
            Timestamp(100),
            [0u8; 32],
            ALLOW_ALL,
            "tty1",
            &dummy,
        );
        // No revelation of deletion: generic InvalidCredentials.
        assert_eq!(r.outcome, Err(AuthError::InvalidCredentials));
    }
}
