//! EuroContacts — the address book of EuroOS (Sprint AC-3).
//!
//! Reads and writes **vCard 3.0** (the standard that calendars, mail clients and
//! phones exchange): `FN`, `N`, `EMAIL`, `TEL`, `ORG`, with `TYPE` parameters
//! (`work`/`home`/`cell`). On top of that an [`AddressBook`] with search, sort and
//! groups. CardDAV synchronization comes later via a dedicated server (sovereign,
//! no Google Contacts). Pure `no_std` logic, host-tested.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// A typed value (e.g. an email "work" → "jan@x.eu").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Typed {
    pub typ: String,
    pub value: String,
}

/// A single contact.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Contact {
    /// Full display name (`FN`).
    pub full_name: String,
    /// Family name (`N` field 1).
    pub family: String,
    /// Given name (`N` field 2).
    pub given: String,
    pub org: String,
    pub emails: Vec<Typed>,
    pub phones: Vec<Typed>,
    /// Free-form groups/categories (`CATEGORIES`).
    pub groups: Vec<String>,
}

impl Contact {
    pub fn new(full_name: &str) -> Self {
        Contact { full_name: full_name.to_string(), ..Default::default() }
    }
    /// Key to sort on: family name, otherwise full name.
    pub fn sort_key(&self) -> String {
        if !self.family.is_empty() {
            alloc::format!("{} {}", self.family, self.given).to_lowercase()
        } else {
            self.full_name.to_lowercase()
        }
    }
    pub fn primary_email(&self) -> Option<&str> {
        self.emails.first().map(|t| t.value.as_str())
    }
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(',') => out.push(','),
                Some(';') => out.push(';'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// Split a property line into (name, params, value).
/// E.g. `EMAIL;TYPE=work:jan@x.eu` → ("EMAIL", ["TYPE=work"], "jan@x.eu").
fn split_prop(line: &str) -> Option<(String, Vec<String>, String)> {
    let colon = line.find(':')?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.to_ascii_uppercase();
    let params: Vec<String> = parts.map(|p| p.to_string()).collect();
    Some((name, params, value.to_string()))
}

fn type_of(params: &[String]) -> String {
    for p in params {
        let up = p.to_ascii_uppercase();
        if let Some(rest) = up.strip_prefix("TYPE=") {
            return rest.to_lowercase();
        }
        // vCard also allows bare type tokens (e.g. `EMAIL;WORK:`).
        if matches!(up.as_str(), "WORK" | "HOME" | "CELL" | "VOICE" | "FAX") {
            return up.to_lowercase();
        }
    }
    String::new()
}

/// Parse one or more vCards from a text. Unknown properties are ignored.
pub fn parse(text: &str) -> Vec<Contact> {
    let mut out = Vec::new();
    let mut cur: Option<Contact> = None;
    for raw in text.lines() {
        let line = raw.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            continue;
        }
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("BEGIN:VCARD") {
            cur = Some(Contact::default());
            continue;
        }
        if upper.starts_with("END:VCARD") {
            if let Some(c) = cur.take() {
                out.push(c);
            }
            continue;
        }
        let c = match cur.as_mut() {
            Some(c) => c,
            None => continue,
        };
        let (name, params, value) = match split_prop(line) {
            Some(v) => v,
            None => continue,
        };
        match name.as_str() {
            "FN" => c.full_name = unescape(&value),
            "N" => {
                let f: Vec<&str> = value.split(';').collect();
                c.family = unescape(f.first().copied().unwrap_or(""));
                c.given = unescape(f.get(1).copied().unwrap_or(""));
            }
            // ORG can be "Company;Department", but splitting before unescape breaks
            // escaped semicolons; we keep the full (unescaped) value.
            "ORG" => c.org = unescape(&value),
            "EMAIL" => c.emails.push(Typed { typ: type_of(&params), value: unescape(&value) }),
            "TEL" => c.phones.push(Typed { typ: type_of(&params), value: unescape(&value) }),
            "CATEGORIES" => {
                for g in value.split(',') {
                    let g = unescape(g.trim());
                    if !g.is_empty() {
                        c.groups.push(g);
                    }
                }
            }
            _ => {}
        }
        // If FN is missing but N is present, derive FN (at END).
        if c.full_name.is_empty() && !c.given.is_empty() {
            c.full_name = alloc::format!("{} {}", c.given, c.family).trim().to_string();
        }
    }
    out
}

/// Serialize a contact to vCard 3.0.
pub fn to_vcard(c: &Contact) -> String {
    let mut s = String::new();
    s.push_str("BEGIN:VCARD\r\n");
    s.push_str("VERSION:3.0\r\n");
    s.push_str(&alloc::format!("N:{};{};;;\r\n", escape(&c.family), escape(&c.given)));
    s.push_str(&alloc::format!("FN:{}\r\n", escape(&c.full_name)));
    if !c.org.is_empty() {
        s.push_str(&alloc::format!("ORG:{}\r\n", escape(&c.org)));
    }
    for e in &c.emails {
        if e.typ.is_empty() {
            s.push_str(&alloc::format!("EMAIL:{}\r\n", escape(&e.value)));
        } else {
            s.push_str(&alloc::format!("EMAIL;TYPE={}:{}\r\n", e.typ, escape(&e.value)));
        }
    }
    for p in &c.phones {
        if p.typ.is_empty() {
            s.push_str(&alloc::format!("TEL:{}\r\n", escape(&p.value)));
        } else {
            s.push_str(&alloc::format!("TEL;TYPE={}:{}\r\n", p.typ, escape(&p.value)));
        }
    }
    if !c.groups.is_empty() {
        let g: Vec<String> = c.groups.iter().map(|x| escape(x)).collect();
        s.push_str(&alloc::format!("CATEGORIES:{}\r\n", g.join(",")));
    }
    s.push_str("END:VCARD\r\n");
    s
}

/// An address book with search/sort/groups.
#[derive(Debug, Clone, Default)]
pub struct AddressBook {
    pub contacts: Vec<Contact>,
}

impl AddressBook {
    pub fn from_vcards(text: &str) -> Self {
        AddressBook { contacts: parse(text) }
    }
    /// Sort by family name (then given name/full name).
    pub fn sort(&mut self) {
        self.contacts.sort_by_key(|a| a.sort_key());
    }
    /// Search by name/email/organization (substring, case-insensitive).
    pub fn search(&self, query: &str) -> Vec<&Contact> {
        let q = query.to_lowercase();
        self.contacts
            .iter()
            .filter(|c| {
                q.is_empty()
                    || c.full_name.to_lowercase().contains(&q)
                    || c.org.to_lowercase().contains(&q)
                    || c.emails.iter().any(|e| e.value.to_lowercase().contains(&q))
            })
            .collect()
    }
    /// Contacts in a group/category.
    pub fn in_group<'a>(&'a self, group: &str) -> Vec<&'a Contact> {
        self.contacts.iter().filter(|c| c.groups.iter().any(|g| g == group)).collect()
    }
    /// Export the entire address book to a single vCard file.
    pub fn export(&self) -> String {
        self.contacts.iter().map(to_vcard).collect::<Vec<_>>().join("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Vandenberg;Jan;;;\r\nFN:Jan Vandenberg\r\nORG:EuroOS\r\nEMAIL;TYPE=work:jan@euro-os.eu\r\nEMAIL;TYPE=home:jan@thuis.be\r\nTEL;TYPE=cell:+32470123456\r\nCATEGORIES:Werk,Kernteam\r\nEND:VCARD\r\n";

    #[test]
    fn parse_single() {
        let v = parse(SAMPLE);
        assert_eq!(v.len(), 1);
        let c = &v[0];
        assert_eq!(c.full_name, "Jan Vandenberg");
        assert_eq!(c.family, "Vandenberg");
        assert_eq!(c.given, "Jan");
        assert_eq!(c.org, "EuroOS");
        assert_eq!(c.emails.len(), 2);
        assert_eq!(c.emails[0], Typed { typ: "work".into(), value: "jan@euro-os.eu".into() });
        assert_eq!(c.phones[0].value, "+32470123456");
        assert_eq!(c.groups, alloc::vec!["Werk".to_string(), "Kernteam".to_string()]);
    }

    #[test]
    fn roundtrip() {
        let c = &parse(SAMPLE)[0];
        let re = &parse(&to_vcard(c))[0];
        assert_eq!(c, re);
    }

    #[test]
    fn parse_multiple_and_sort() {
        let text = alloc::format!(
            "{}BEGIN:VCARD\r\nFN:Anna Bakker\r\nN:Bakker;Anna;;;\r\nEND:VCARD\r\n",
            SAMPLE
        );
        let mut ab = AddressBook::from_vcards(&text);
        assert_eq!(ab.contacts.len(), 2);
        ab.sort();
        // Bakker < Vandenberg.
        assert_eq!(ab.contacts[0].family, "Bakker");
    }

    #[test]
    fn search_and_groups() {
        let ab = AddressBook::from_vcards(SAMPLE);
        assert_eq!(ab.search("euro").len(), 1); // ORG + email match
        assert_eq!(ab.search("jan@thuis").len(), 1);
        assert_eq!(ab.search("zzz").len(), 0);
        assert_eq!(ab.in_group("Kernteam").len(), 1);
    }

    #[test]
    fn escaping_special_chars() {
        let mut c = Contact::new("Bedrijf, N.V.");
        c.org = "A;B\\C".to_string();
        let card = to_vcard(&c);
        let back = &parse(&card)[0];
        assert_eq!(back.full_name, "Bedrijf, N.V.");
        assert_eq!(back.org, "A;B\\C");
    }

    #[test]
    fn fn_derived_from_n_when_missing() {
        let v = parse("BEGIN:VCARD\r\nN:Jansen;Piet;;;\r\nEND:VCARD\r\n");
        assert_eq!(v[0].full_name, "Piet Jansen");
    }
}
