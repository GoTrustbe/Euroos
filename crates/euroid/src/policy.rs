//! Wachtwoord- en gebruikersnaambeleid (afdwingbaar, ISO 27001 A.9.4).

use alloc::string::String;

/// Systeembreed wachtwoord-/sessiebeleid (uit `/etc/euro/policy.toml`).
#[derive(Clone, Copy, Debug)]
pub struct PasswordPolicy {
    pub min_length: usize,
    pub max_length: usize,
    pub require_uppercase: bool,
    pub require_lowercase: bool,
    pub require_digit: bool,
    pub require_special: bool,
    pub history_depth: usize,
    pub max_age_days: u32,
    pub min_age_days: u32,
    pub warn_before_days: u32,
    pub max_failed_logins: u32,
    pub lockout_duration_secs: u32,
}

impl Default for PasswordPolicy {
    /// De soevereine standaard (`/etc/euro/policy.toml`).
    fn default() -> Self {
        PasswordPolicy {
            min_length: 12,
            max_length: 128,
            require_uppercase: true,
            require_lowercase: true,
            require_digit: true,
            require_special: true,
            history_depth: 12,
            max_age_days: 90,
            min_age_days: 1,
            warn_before_days: 14,
            max_failed_logins: 5,
            lockout_duration_secs: 900,
        }
    }
}

/// Waarom een wachtwoord het beleid niet haalt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyError {
    TooShort { min: usize },
    TooLong { max: usize },
    MissingUppercase,
    MissingLowercase,
    MissingDigit,
    MissingSpecial,
}

impl PolicyError {
    pub fn message(self) -> String {
        match self {
            PolicyError::TooShort { min } => alloc::format!("wachtwoord te kort (minimaal {min} tekens)"),
            PolicyError::TooLong { max } => alloc::format!("wachtwoord te lang (maximaal {max} tekens)"),
            PolicyError::MissingUppercase => String::from("wachtwoord mist een hoofdletter"),
            PolicyError::MissingLowercase => String::from("wachtwoord mist een kleine letter"),
            PolicyError::MissingDigit => String::from("wachtwoord mist een cijfer"),
            PolicyError::MissingSpecial => String::from("wachtwoord mist een speciaal teken"),
        }
    }
}

/// Toets een wachtwoord aan het beleid. De lengte wordt in Unicode-tekens geteld.
pub fn validate_password(pw: &str, policy: &PasswordPolicy) -> Result<(), PolicyError> {
    let len = pw.chars().count();
    if len < policy.min_length {
        return Err(PolicyError::TooShort { min: policy.min_length });
    }
    if len > policy.max_length {
        return Err(PolicyError::TooLong { max: policy.max_length });
    }
    if policy.require_uppercase && !pw.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(PolicyError::MissingUppercase);
    }
    if policy.require_lowercase && !pw.chars().any(|c| c.is_ascii_lowercase()) {
        return Err(PolicyError::MissingLowercase);
    }
    if policy.require_digit && !pw.chars().any(|c| c.is_ascii_digit()) {
        return Err(PolicyError::MissingDigit);
    }
    if policy.require_special && !pw.chars().any(|c| !c.is_ascii_alphanumeric()) {
        return Err(PolicyError::MissingSpecial);
    }
    Ok(())
}

/// Valideer een gebruikersnaam: 1–32 tekens uit `[a-z0-9_-]`, begint met `[a-z_]`.
pub fn validate_username(name: &str) -> Result<(), String> {
    let len = name.len();
    if len == 0 {
        return Err(String::from("gebruikersnaam mag niet leeg zijn"));
    }
    if len > 32 {
        return Err(String::from("gebruikersnaam te lang (maximaal 32 tekens)"));
    }
    let first = name.as_bytes()[0];
    if !(first.is_ascii_lowercase() || first == b'_') {
        return Err(String::from("gebruikersnaam moet beginnen met een kleine letter of '_'"));
    }
    for &b in name.as_bytes() {
        let ok = b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-';
        if !ok {
            return Err(alloc::format!(
                "ongeldig teken in gebruikersnaam: alleen [a-z0-9_-] toegestaan"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_weak_accepts_strong() {
        let p = PasswordPolicy::default();
        assert_eq!(validate_password("short", &p), Err(PolicyError::TooShort { min: 12 }));
        assert_eq!(validate_password("alllowercase1!", &p), Err(PolicyError::MissingUppercase));
        assert_eq!(validate_password("ALLUPPERCASE1!", &p), Err(PolicyError::MissingLowercase));
        assert_eq!(validate_password("NoDigitsHere!!", &p), Err(PolicyError::MissingDigit));
        assert_eq!(validate_password("NoSpecial1234A", &p), Err(PolicyError::MissingSpecial));
        // Sterk: lengte, hoofd-/kleine letter, cijfer, speciaal teken.
        assert_eq!(validate_password("Correct-Horse-9!", &p), Ok(()));
    }

    #[test]
    fn max_length_dos_guard() {
        let p = PasswordPolicy::default();
        let huge: String = core::iter::repeat('A').take(200).collect();
        assert_eq!(validate_password(&huge, &p), Err(PolicyError::TooLong { max: 128 }));
    }

    #[test]
    fn username_rules() {
        assert!(validate_username("alice").is_ok());
        assert!(validate_username("svc-backup").is_ok());
        assert!(validate_username("_system").is_ok());
        assert!(validate_username("a1_b-c").is_ok());
        assert!(validate_username("").is_err());
        assert!(validate_username("1alice").is_err()); // begint met cijfer
        assert!(validate_username("-alice").is_err()); // begint met koppelteken
        assert!(validate_username("Alice").is_err()); // hoofdletter
        assert!(validate_username("al ice").is_err()); // spatie
        assert!(validate_username("al/ice").is_err()); // slash
        let toolong: String = core::iter::repeat('a').take(33).collect();
        assert!(validate_username(&toolong).is_err());
    }
}
