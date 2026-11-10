//! Validation of names a user types into the settings panel, and of the app
//! names that become settings file names.

fn valid_label(l: &str) -> bool {
    !l.is_empty()
        && l.len() <= 63
        && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !l.starts_with('-')
        && !l.ends_with('-')
}

/// A host name: one RFC 1123 label (letters, digits, hyphens; 1..=63 chars;
/// no hyphen first or last).
pub fn valid_hostname(name: &str) -> bool {
    valid_label(name)
}

/// A DNS name to block: labels separated by dots, at most 253 chars, at least
/// one dot is not required (`localhost` is a valid name).
pub fn valid_domain(name: &str) -> bool {
    !name.is_empty() && name.len() <= 253 && name.split('.').all(valid_label)
}

/// An app name usable as a settings file name: 1..=32 chars of `a-z 0-9 _ -`,
/// starting with a letter or digit. No dots or slashes, so it cannot leave the
/// settings directory.
pub fn valid_app_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        && name.as_bytes()[0].is_ascii_alphanumeric()
}

/// A login name usable in `/home/<user>`: 1..=32 chars of `a-z A-Z 0-9 . _ -`,
/// not starting with a dot or hyphen, and not `.`/`..`.
pub fn valid_user_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
        && !name.starts_with('.')
        && !name.starts_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostnames() {
        for ok in ["euro-pc", "a", "PC1", "x1-y2-z3", &"a".repeat(63)] {
            assert!(valid_hostname(ok), "{ok}");
        }
        for bad in ["", "-pc", "pc-", "my pc", "pc.local", "pc_1", "caf\u{e9}", &"a".repeat(64), "a/b", "pc\n"] {
            assert!(!valid_hostname(bad), "{bad:?}");
        }
    }

    #[test]
    fn domains() {
        for ok in ["ads.example.com", "localhost", "a-b.c-d.example", "xn--bcher-kva.example"] {
            assert!(valid_domain(ok), "{ok}");
        }
        let too_long = alloc::format!("{}.com", "a.".repeat(126));
        for bad in ["", ".", "a..b", ".example.com", "example.com.", "exa mple.com", "a,b.com", "-a.com", too_long.as_str()] {
            assert!(!valid_domain(bad), "{bad:?}");
        }
    }

    #[test]
    fn app_and_user_names() {
        for ok in ["notes", "settings", "euro-guard", "app_2", "9lives"] {
            assert!(valid_app_name(ok), "{ok}");
        }
        for bad in ["", "..", "../etc", "a/b", "Notes", "a.b", "-x", "_x", &"a".repeat(33)] {
            assert!(!valid_app_name(bad), "{bad:?}");
        }
        for ok in ["euro", "alice", "j.doe", "User_1"] {
            assert!(valid_user_name(ok), "{ok}");
        }
        for bad in ["", ".", "..", ".hidden", "a/b", "-x", "a b", &"a".repeat(33)] {
            assert!(!valid_user_name(bad), "{bad:?}");
        }
    }
}
