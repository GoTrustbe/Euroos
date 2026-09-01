//! User and group model + the in-memory stores (`users.db` / `groups.db`).

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::cred::PasswordRecord;
use crate::{
    Caps, GroupId, Timestamp, UserId, ALLOW_ALL, CAP_ALL, CAP_AGENT_SPAWN, CAP_AUDIT_READ, CAP_NET,
    CAP_VAULT, GROUP_AGENT, GROUP_AUDIT, GROUP_NET, GROUP_USERS, GROUP_VAULT, GROUP_WHEEL,
};

/// The lifecycle state of an account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserState {
    Active,
    Locked { reason: LockReason, locked_at: Timestamp, locked_by: UserId },
    Expired { expired_at: Timestamp },
    /// Deleted records are NEVER erased — audit requirement (soft delete).
    Deleted { deleted_at: Timestamp, deleted_by: UserId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockReason {
    AdminLock,
    FailedLoginThreshold,
    PasswordExpired,
    InactivityTimeout,
}

impl LockReason {
    pub fn tag(self) -> &'static str {
        match self {
            LockReason::AdminLock => "admin-lock",
            LockReason::FailedLoginThreshold => "failed-login-threshold",
            LockReason::PasswordExpired => "password-expired",
            LockReason::InactivityTimeout => "inactivity-timeout",
        }
    }
}

/// A EuroOS user (record in `users.db`).
#[derive(Clone, Debug)]
pub struct User {
    pub uid: UserId,
    pub username: String,
    pub display_name: String,
    pub primary_gid: GroupId,
    pub groups: Vec<GroupId>,
    pub home: String,
    pub shell: String,
    pub state: UserState,
    pub caps: Caps,
    pub created_at: Timestamp,
    pub created_by: UserId,
    pub password: PasswordRecord,
    pub tpm_enrolled: bool,
    /// Number of consecutive failed logins (reset on success).
    pub failed_logins: u32,
}

impl User {
    pub fn is_active(&self) -> bool {
        matches!(self.state, UserState::Active)
    }
}

/// A group (record in `groups.db`).
#[derive(Clone, Debug)]
pub struct Group {
    pub gid: GroupId,
    pub name: String,
    pub members: Vec<UserId>,
    pub caps: Caps,
    pub created_at: Timestamp,
    pub created_by: UserId,
    /// Built-in groups cannot be deleted.
    pub builtin: bool,
}

/// The group store.
#[derive(Clone, Debug, Default)]
pub struct GroupDb {
    groups: Vec<Group>,
}

impl GroupDb {
    pub fn new() -> Self {
        GroupDb { groups: Vec::new() }
    }

    /// Create the group store with the built-in groups (system init).
    pub fn with_builtins() -> Self {
        let t = Timestamp(0);
        let mk = |gid: GroupId, name: &str, caps: Caps| Group {
            gid,
            name: name.to_string(),
            members: Vec::new(),
            caps,
            created_at: t,
            created_by: UserId::SYSTEM,
            builtin: true,
        };
        GroupDb {
            groups: alloc::vec![
                mk(GROUP_WHEEL, "wheel", CAP_ALL),
                mk(GROUP_AUDIT, "audit", crate::CAP_LOGIN | CAP_AUDIT_READ),
                mk(GROUP_NET, "net", crate::CAP_LOGIN | CAP_NET),
                mk(GROUP_VAULT, "vault", crate::CAP_LOGIN | CAP_VAULT),
                mk(GROUP_AGENT, "agent", crate::CAP_LOGIN | CAP_AGENT_SPAWN),
                mk(
                    GROUP_USERS,
                    "users",
                    crate::CAP_LOGIN | crate::CAP_FILE | crate::CAP_DISPLAY,
                ),
            ],
        }
    }

    pub fn get(&self, gid: GroupId) -> Option<&Group> {
        self.groups.iter().find(|g| g.gid == gid)
    }

    pub fn get_mut(&mut self, gid: GroupId) -> Option<&mut Group> {
        self.groups.iter_mut().find(|g| g.gid == gid)
    }

    pub fn by_name(&self, name: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.name == name)
    }

    pub fn all(&self) -> &[Group] {
        &self.groups
    }

    /// Add a group; fails if the name/gid already exists.
    pub fn add(&mut self, group: Group) -> Result<(), UserError> {
        if self.groups.iter().any(|g| g.name == group.name) {
            return Err(UserError::AlreadyExists(group.name.clone()));
        }
        if self.groups.iter().any(|g| g.gid == group.gid) {
            return Err(UserError::AlreadyExists(alloc::format!("gid {}", group.gid.0)));
        }
        self.groups.push(group);
        Ok(())
    }

    /// Next free gid above 100 (for new, non-built-in groups).
    pub fn next_gid(&self) -> GroupId {
        let max = self.groups.iter().map(|g| g.gid.0).filter(|&g| g >= 1000).max().unwrap_or(999);
        GroupId(max + 1)
    }
}

/// Error categories for user management.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserError {
    AlreadyExists(String),
    NotFound(String),
    Unauthorized,
    InvalidName(String),
    InvalidGroup(String),
}

/// The user store.
#[derive(Clone, Debug, Default)]
pub struct UserDb {
    users: Vec<User>,
}

impl UserDb {
    pub fn new() -> Self {
        UserDb { users: Vec::new() }
    }

    pub fn exists(&self, username: &str) -> bool {
        self.users.iter().any(|u| u.username == username)
    }

    pub fn get(&self, uid: UserId) -> Option<&User> {
        self.users.iter().find(|u| u.uid == uid)
    }

    pub fn get_mut(&mut self, uid: UserId) -> Option<&mut User> {
        self.users.iter_mut().find(|u| u.uid == uid)
    }

    pub fn get_by_name(&self, username: &str) -> Option<&User> {
        self.users.iter().find(|u| u.username == username)
    }

    pub fn get_by_name_mut(&mut self, username: &str) -> Option<&mut User> {
        self.users.iter_mut().find(|u| u.username == username)
    }

    pub fn all(&self) -> &[User] {
        &self.users
    }

    /// Next free uid. System accounts from 100, regular users from 1000.
    pub fn next_uid(&self, is_system: bool) -> UserId {
        let (floor, ceil) = if is_system {
            (UserId::FIRST_SYSTEM, UserId::FIRST_REGULAR - 1)
        } else {
            (UserId::FIRST_REGULAR, u32::MAX)
        };
        let max = self
            .users
            .iter()
            .map(|u| u.uid.0)
            .filter(|&x| x >= floor && x <= ceil)
            .max();
        UserId(max.map(|m| m + 1).unwrap_or(floor))
    }

    /// Add a user (fails if the name already exists).
    pub fn insert(&mut self, user: User) -> Result<(), UserError> {
        if self.exists(&user.username) {
            return Err(UserError::AlreadyExists(user.username.clone()));
        }
        self.users.push(user);
        Ok(())
    }

    pub fn lock(&mut self, uid: UserId, reason: LockReason, by: UserId, now: Timestamp) -> Result<(), UserError> {
        let u = self.get_mut(uid).ok_or(UserError::NotFound(alloc::format!("uid {}", uid.0)))?;
        u.state = UserState::Locked { reason, locked_at: now, locked_by: by };
        Ok(())
    }

    pub fn unlock(&mut self, uid: UserId) -> Result<(), UserError> {
        let u = self.get_mut(uid).ok_or(UserError::NotFound(alloc::format!("uid {}", uid.0)))?;
        u.state = UserState::Active;
        u.failed_logins = 0;
        Ok(())
    }

    pub fn record_failed_login(&mut self, uid: UserId) -> u32 {
        if let Some(u) = self.get_mut(uid) {
            u.failed_logins += 1;
            u.failed_logins
        } else {
            0
        }
    }

    pub fn reset_failed_logins(&mut self, uid: UserId) {
        if let Some(u) = self.get_mut(uid) {
            u.failed_logins = 0;
        }
    }

    pub fn set_must_change(&mut self, uid: UserId, value: bool) {
        if let Some(u) = self.get_mut(uid) {
            u.password.must_change = value;
        }
    }

    /// Soft delete: the record remains (audit), but the state becomes `Deleted`.
    pub fn soft_delete(&mut self, uid: UserId, by: UserId, now: Timestamp) -> Result<(), UserError> {
        let u = self.get_mut(uid).ok_or(UserError::NotFound(alloc::format!("uid {}", uid.0)))?;
        u.state = UserState::Deleted { deleted_at: now, deleted_by: by };
        Ok(())
    }
}

/// A user's effective capability set = own caps ∪ group caps, then
/// bounded by the EuroPol mask (policy can only remove, never add).
pub fn effective_caps(user: &User, db: &GroupDb, europol_allowed: Caps) -> Caps {
    let mut caps = user.caps;
    if let Some(g) = db.get(user.primary_gid) {
        caps |= g.caps;
    }
    for gid in &user.groups {
        if let Some(g) = db.get(*gid) {
            caps |= g.caps;
        }
    }
    caps & europol_allowed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cred::PasswordRecord;
    use crate::{CAP_DISPLAY, CAP_FILE, CAP_LOGIN, CAP_NET, CAP_USER_ADMIN};

    fn mk_user(uid: u32, name: &str, primary: GroupId, groups: &[GroupId], own: Caps) -> User {
        User {
            uid: UserId(uid),
            username: name.to_string(),
            display_name: name.to_string(),
            primary_gid: primary,
            groups: groups.to_vec(),
            home: alloc::format!("/home/{name}"),
            shell: "/bin/eurosh".to_string(),
            state: UserState::Active,
            caps: own,
            created_at: Timestamp(0),
            created_by: UserId::SYSTEM,
            password: PasswordRecord::locked(),
            tpm_enrolled: false,
            failed_logins: 0,
        }
    }

    #[test]
    fn effective_caps_union_of_user_and_groups() {
        let gdb = GroupDb::with_builtins();
        // alice: primary=users, supplementary=net, own caps = CAP_FILE.
        let alice = mk_user(1000, "alice", GROUP_USERS, &[GROUP_NET], CAP_FILE);
        let caps = effective_caps(&alice, &gdb, ALLOW_ALL);
        // users → LOGIN|FILE|DISPLAY ; net → LOGIN|NET ; own → FILE.
        assert_eq!(caps & CAP_LOGIN, CAP_LOGIN);
        assert_eq!(caps & CAP_NET, CAP_NET);
        assert_eq!(caps & CAP_DISPLAY, CAP_DISPLAY);
        assert_eq!(caps & CAP_FILE, CAP_FILE);
        // No user-admin: alice is not in wheel.
        assert_eq!(caps & CAP_USER_ADMIN, 0);
    }

    #[test]
    fn wheel_grants_all_but_policy_can_deny() {
        let gdb = GroupDb::with_builtins();
        let root = mk_user(0, "root", GROUP_WHEEL, &[], 0);
        let full = effective_caps(&root, &gdb, ALLOW_ALL);
        assert_eq!(full & CAP_USER_ADMIN, CAP_USER_ADMIN);
        // EuroPol denies CAP_NET to everyone → even wheel loses it.
        let denied = effective_caps(&root, &gdb, ALLOW_ALL & !CAP_NET);
        assert_eq!(denied & CAP_NET, 0);
        // But the rest remains.
        assert_eq!(denied & CAP_USER_ADMIN, CAP_USER_ADMIN);
    }

    #[test]
    fn next_uid_ranges() {
        let mut db = UserDb::new();
        assert_eq!(db.next_uid(false), UserId(1000));
        assert_eq!(db.next_uid(true), UserId(100));
        db.insert(mk_user(1000, "alice", GROUP_USERS, &[], 0)).unwrap();
        db.insert(mk_user(100, "svc", GROUP_USERS, &[], 0)).unwrap();
        assert_eq!(db.next_uid(false), UserId(1001));
        assert_eq!(db.next_uid(true), UserId(101));
    }

    #[test]
    fn duplicate_username_rejected() {
        let mut db = UserDb::new();
        db.insert(mk_user(1000, "alice", GROUP_USERS, &[], 0)).unwrap();
        let dup = mk_user(1001, "alice", GROUP_USERS, &[], 0);
        assert_eq!(db.insert(dup), Err(UserError::AlreadyExists("alice".to_string())));
    }

    #[test]
    fn soft_delete_keeps_record() {
        let mut db = UserDb::new();
        db.insert(mk_user(1000, "alice", GROUP_USERS, &[], 0)).unwrap();
        db.soft_delete(UserId(1000), UserId::ROOT, Timestamp(5)).unwrap();
        // The record still exists (audit), but is marked as deleted.
        let u = db.get(UserId(1000)).unwrap();
        assert!(matches!(u.state, UserState::Deleted { .. }));
    }
}
