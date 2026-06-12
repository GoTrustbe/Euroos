//! EuroAML — een **minimale ACPI-AML-bytecode-interpreter** (plan I3).
//!
//! ACPI's DSDT/SSDT-tabellen bevatten geen vaste velden maar **AML-bytecode**: een
//! kleine bytecode-taal waarin de firmware control-methods uitdrukt zoals `_STA`
//! (status), `_TMP` (thermal-zone-temperatuur), `_BST`/`_BIF` (batterij) en `_PSR`
//! (netstroom). Om die te lezen moet een OS de AML *interpreteren*. [`euroacpi`]
//! levert de tabel-parser; deze crate is de bytecode-laag erboven.
//!
//! Het is bewust een **subset** — genoeg voor de veelvoorkomende read-out-methods
//! (constanten, packages, buffers, eenvoudige rekenkundige `Return`-expressies en
//! de naamruimte-opbouw via `Scope`/`Name`/`Method`) — geen volledige AML2.0-machine
//! (geen OperationRegion/Field-side-effects, geen control-flow). Pure `no_std`-logica
//! → de offset- en lengte-gevoelige bytecode-parsing is volledig op de host getest.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

// ── AML-opcodes (subset) ────────────────────────────────────────────────────
const ZERO_OP: u8 = 0x00;
const ONE_OP: u8 = 0x01;
const NAME_OP: u8 = 0x08;
const BYTE_PREFIX: u8 = 0x0A;
const WORD_PREFIX: u8 = 0x0B;
const DWORD_PREFIX: u8 = 0x0C;
const STRING_PREFIX: u8 = 0x0D;
const QWORD_PREFIX: u8 = 0x0E;
const SCOPE_OP: u8 = 0x10;
const BUFFER_OP: u8 = 0x11;
const PACKAGE_OP: u8 = 0x12;
const METHOD_OP: u8 = 0x14;
const DUAL_NAME_PREFIX: u8 = 0x2E;
const MULTI_NAME_PREFIX: u8 = 0x2F;
const ROOT_CHAR: u8 = 0x5C; // '\'
const PARENT_PREFIX: u8 = 0x5E; // '^'
const RETURN_OP: u8 = 0xA4;
const EXT_OP_PREFIX: u8 = 0x5B;
// Extended-opcodes (na 0x5B).
const EXT_MUTEX: u8 = 0x01;
const EXT_EVENT: u8 = 0x02;
const EXT_OP_REGION: u8 = 0x80;
const EXT_FIELD: u8 = 0x81;
const EXT_DEVICE: u8 = 0x82;
const EXT_PROCESSOR: u8 = 0x83;
const EXT_POWER_RES: u8 = 0x84;
const EXT_THERMAL_ZONE: u8 = 0x85;
const EXT_INDEX_FIELD: u8 = 0x86;
const ADD_OP: u8 = 0x72;
const SUBTRACT_OP: u8 = 0x74;
const MULTIPLY_OP: u8 = 0x77;
const ONES_OP: u8 = 0xFF;

/// Een geëvalueerde AML-waarde.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmlValue {
    Integer(u64),
    Buffer(Vec<u8>),
    Package(Vec<AmlValue>),
}

impl AmlValue {
    /// Geef de integer-waarde (None als het geen integer is).
    pub fn as_int(&self) -> Option<u64> {
        match self {
            AmlValue::Integer(v) => Some(*v),
            _ => None,
        }
    }
    pub fn as_package(&self) -> Option<&[AmlValue]> {
        match self {
            AmlValue::Package(p) => Some(p),
            _ => None,
        }
    }
}

/// Een opgeslagen naamruimte-object: ofwel een data-waarde (`Name`) of een
/// control-method (`Method`, met z'n ruwe body-bytes om later te draaien).
#[derive(Debug, Clone)]
enum Object {
    Value(AmlValue),
    Method { body: Vec<u8> },
}

/// De geparste AML-naamruimte: een platte map van 4-teken-namen → object. (We
/// negeren de scope-hiërarchie voor de lookup; de laatste NameSeg is de sleutel —
/// genoeg voor het opzoeken van methods als `_STA`/`_TMP` op naam.)
pub struct AmlNamespace {
    objects: BTreeMap<String, Object>,
}

impl AmlNamespace {
    /// Parse een AML-byteblok (de body van een DSDT/SSDT na de 36-byte SDT-header)
    /// tot een naamruimte.
    pub fn parse(aml: &[u8]) -> AmlNamespace {
        let mut ns = AmlNamespace { objects: BTreeMap::new() };
        let mut p = Parser { b: aml, pos: 0 };
        p.parse_term_list(&mut ns, aml.len());
        ns
    }

    /// Hoeveel objecten zijn er ontdekt?
    pub fn len(&self) -> usize {
        self.objects.len()
    }
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Is er een object (Name of Method) met deze (laatste-NameSeg-)naam?
    pub fn contains(&self, name: &str) -> bool {
        self.objects.contains_key(&seg_key(name))
    }

    /// Evalueer een naam: een `Name` geeft z'n waarde terug; een `Method` wordt
    /// uitgevoerd (subset: een enkele `Return(expr)`). None als onbekend/niet te
    /// evalueren.
    pub fn evaluate(&self, name: &str) -> Option<AmlValue> {
        match self.objects.get(&seg_key(name))? {
            Object::Value(v) => Some(v.clone()),
            Object::Method { body } => {
                let mut p = Parser { b: body, pos: 0 };
                p.run_method(self)
            }
        }
    }
}

/// De sleutel waaronder we opslaan/opzoeken: de laatste 4-teken-NameSeg, opgevuld.
fn seg_key(name: &str) -> String {
    let last = name.trim_start_matches(['\\', '^']).rsplit('.').next().unwrap_or(name);
    let mut s = String::new();
    for c in last.chars().take(4) {
        s.push(c);
    }
    while s.len() < 4 && !s.is_empty() {
        s.push('_');
    }
    s
}

struct Parser<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn byte(&mut self) -> Option<u8> {
        let v = *self.b.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    /// AML PkgLength: het eerste byte codeert hoeveel vervolgbytes volgen + de lage
    /// bits van de lengte. Geeft (totale-lengte-incl-pkglength-bytes, bytes-gelezen).
    fn pkg_length(&mut self) -> usize {
        let start = self.pos;
        let lead = self.byte().unwrap_or(0);
        let extra = (lead >> 6) & 0x3;
        let mut len = if extra == 0 {
            (lead & 0x3F) as usize
        } else {
            let mut v = (lead & 0x0F) as usize;
            for i in 0..extra {
                let nb = self.byte().unwrap_or(0) as usize;
                v |= nb << (4 + 8 * i as usize);
            }
            v
        };
        // `len` telt vanaf het EERSTE pkglength-byte. Trek af wat we al lazen.
        let consumed = self.pos - start;
        len = len.saturating_sub(consumed);
        len
    }

    /// Lees een NameString en geef de laatste NameSeg-sleutel terug.
    fn name_string(&mut self) -> String {
        // Voorvoegsels (root/parent) overslaan.
        while matches!(self.peek(), Some(ROOT_CHAR) | Some(PARENT_PREFIX)) {
            self.pos += 1;
        }
        let segs: usize = match self.peek() {
            Some(0x00) => {
                self.pos += 1; // NullName
                0
            }
            Some(DUAL_NAME_PREFIX) => {
                self.pos += 1;
                2
            }
            Some(MULTI_NAME_PREFIX) => {
                self.pos += 1;
                let n = self.byte().unwrap_or(0) as usize;
                n
            }
            _ => 1,
        };
        let mut last = String::new();
        for _ in 0..segs {
            last.clear();
            for _ in 0..4 {
                if let Some(c) = self.byte() {
                    last.push(c as char);
                }
            }
        }
        if last.is_empty() {
            last.push_str("____");
        }
        last
    }

    /// Parse een TermList tot `end` (absolute byte-offset in `self.b`).
    fn parse_term_list(&mut self, ns: &mut AmlNamespace, end: usize) {
        while self.pos < end {
            let op = match self.byte() {
                Some(o) => o,
                None => break,
            };
            match op {
                NAME_OP => {
                    let name = self.name_string();
                    if let Some(v) = self.parse_data_object() {
                        ns.objects.insert(seg_key(&name), Object::Value(v));
                    }
                }
                SCOPE_OP => {
                    let len = self.pkg_length();
                    let body_end = self.pos + len;
                    let _scope = self.name_string();
                    // De inhoud van de scope is zelf een TermList.
                    self.parse_term_list(ns, body_end.min(self.b.len()));
                    self.pos = body_end.min(self.b.len());
                }
                METHOD_OP => {
                    let len = self.pkg_length();
                    let body_end = (self.pos + len).min(self.b.len());
                    let name = self.name_string();
                    let _flags = self.byte();
                    let body = self.b[self.pos..body_end].to_vec();
                    ns.objects.insert(seg_key(&name), Object::Method { body });
                    self.pos = body_end;
                }
                EXT_OP_PREFIX => {
                    if !self.parse_ext_term(ns, end) {
                        break; // onbekende extended-op → stop veilig
                    }
                }
                _ => {
                    // Onbekende/niet-ondersteunde top-level term: stop veilig (we
                    // hebben geen volledige grammatica; verder skippen zou de stream
                    // ontsporen). De al-ontdekte objecten blijven geldig.
                    break;
                }
            }
        }
    }

    /// Verwerk een extended-term (na 0x5B). Container-objecten (Device/ThermalZone/
    /// PowerResource/Processor) recursen we in zodat we de methods erin (bv. `_TMP`,
    /// `_BST`, `_STA`) vinden; niet-container ext-ops slaan we correct over. Geeft
    /// false als de sub-op onbekend is (dan stopt de caller veilig).
    fn parse_ext_term(&mut self, ns: &mut AmlNamespace, _outer_end: usize) -> bool {
        let sub = match self.byte() {
            Some(s) => s,
            None => return false,
        };
        match sub {
            EXT_DEVICE | EXT_THERMAL_ZONE => {
                let len = self.pkg_length();
                let body_end = (self.pos + len).min(self.b.len());
                let _name = self.name_string();
                self.parse_term_list(ns, body_end);
                self.pos = body_end;
                true
            }
            EXT_POWER_RES => {
                let len = self.pkg_length();
                let body_end = (self.pos + len).min(self.b.len());
                let _name = self.name_string();
                let _system_level = self.byte();
                let _resource_order = (self.byte(), self.byte());
                self.parse_term_list(ns, body_end);
                self.pos = body_end;
                true
            }
            EXT_PROCESSOR => {
                let len = self.pkg_length();
                let body_end = (self.pos + len).min(self.b.len());
                let _name = self.name_string();
                let _procid = self.byte();
                for _ in 0..4 {
                    self.byte(); // PblkAddr (dword)
                }
                let _pblklen = self.byte();
                self.parse_term_list(ns, body_end);
                self.pos = body_end;
                true
            }
            EXT_FIELD | EXT_INDEX_FIELD => {
                // Field/IndexField: PkgLength dekt de hele definitie → overslaan.
                let len = self.pkg_length();
                self.pos = (self.pos + len).min(self.b.len());
                true
            }
            EXT_OP_REGION => {
                let _name = self.name_string();
                let _space = self.byte();
                let _offset = self.parse_data_object();
                let _length = self.parse_data_object();
                true
            }
            EXT_MUTEX => {
                let _name = self.name_string();
                let _sync_flags = self.byte();
                true
            }
            EXT_EVENT => {
                let _name = self.name_string();
                true
            }
            _ => false,
        }
    }

    /// Parse een DataObject / constante expressie → AmlValue.
    fn parse_data_object(&mut self) -> Option<AmlValue> {
        let op = self.peek()?;
        match op {
            ZERO_OP => {
                self.pos += 1;
                Some(AmlValue::Integer(0))
            }
            ONE_OP => {
                self.pos += 1;
                Some(AmlValue::Integer(1))
            }
            ONES_OP => {
                self.pos += 1;
                Some(AmlValue::Integer(u64::MAX))
            }
            BYTE_PREFIX => {
                self.pos += 1;
                Some(AmlValue::Integer(self.byte()? as u64))
            }
            WORD_PREFIX => {
                self.pos += 1;
                let lo = self.byte()? as u64;
                let hi = self.byte()? as u64;
                Some(AmlValue::Integer(lo | (hi << 8)))
            }
            DWORD_PREFIX => {
                self.pos += 1;
                let mut v = 0u64;
                for i in 0..4 {
                    v |= (self.byte()? as u64) << (8 * i);
                }
                Some(AmlValue::Integer(v))
            }
            QWORD_PREFIX => {
                self.pos += 1;
                let mut v = 0u64;
                for i in 0..8 {
                    v |= (self.byte()? as u64) << (8 * i);
                }
                Some(AmlValue::Integer(v))
            }
            STRING_PREFIX => {
                self.pos += 1;
                let mut s = Vec::new();
                while let Some(c) = self.byte() {
                    if c == 0 {
                        break;
                    }
                    s.push(c);
                }
                Some(AmlValue::Buffer(s))
            }
            BUFFER_OP => {
                self.pos += 1;
                let len = self.pkg_length();
                let body_end = (self.pos + len).min(self.b.len());
                let _size = self.parse_data_object(); // buffer-grootte (genegeerd)
                let bytes = self.b[self.pos..body_end].to_vec();
                self.pos = body_end;
                Some(AmlValue::Buffer(bytes))
            }
            PACKAGE_OP => {
                self.pos += 1;
                let len = self.pkg_length();
                let body_end = (self.pos + len).min(self.b.len());
                let count = self.byte()? as usize;
                let mut items = Vec::new();
                for _ in 0..count {
                    if self.pos >= body_end {
                        break;
                    }
                    match self.parse_data_object() {
                        Some(v) => items.push(v),
                        None => break,
                    }
                }
                self.pos = body_end;
                Some(AmlValue::Package(items))
            }
            ADD_OP | SUBTRACT_OP | MULTIPLY_OP => {
                self.pos += 1;
                let a = self.parse_data_object()?.as_int()?;
                let b = self.parse_data_object()?.as_int()?;
                // Target (waar het resultaat heen gaat) overslaan: meestal Zero/0x00.
                let _target = self.byte();
                let r = match op {
                    ADD_OP => a.wrapping_add(b),
                    SUBTRACT_OP => a.wrapping_sub(b),
                    _ => a.wrapping_mul(b),
                };
                Some(AmlValue::Integer(r))
            }
            _ => None,
        }
    }

    /// Voer een method-body uit (subset): zoek de eerste `Return(expr)` en evalueer.
    /// Een body zonder Return geeft `Integer(0)` (impliciete nul).
    fn run_method(&mut self, _ns: &AmlNamespace) -> Option<AmlValue> {
        while let Some(op) = self.peek() {
            if op == RETURN_OP {
                self.pos += 1;
                return self.parse_data_object();
            }
            // Sla onbegrepen statements over door naar het volgende Return te zoeken.
            self.pos += 1;
        }
        Some(AmlValue::Integer(0))
    }
}

/// AML-encoder-helpers — bouwen geldige AML-byteblokken voor tests (en voor het
/// genereren van eenvoudige tabellen). Spiegelt de parser.
pub mod enc {
    use super::*;

    /// Codeer een PkgLength voor een payload van `len` bytes (de lengte telt de
    /// pkglength-bytes zelf mee, zoals AML vereist).
    pub fn pkg_length(payload_len: usize) -> Vec<u8> {
        // Probeer 1-byte (≤ 63 incl. zichzelf): totaal = payload + 1.
        let total1 = payload_len + 1;
        if total1 <= 0x3F {
            return alloc::vec![total1 as u8];
        }
        // 2-byte: lead bits[7:6]=01, lage nibble + 1 vervolgbyte. totaal = payload + 2.
        let total2 = payload_len + 2;
        alloc::vec![0x40 | (total2 & 0x0F) as u8, (total2 >> 4) as u8]
    }

    /// Codeer een NameString (enkele 4-teken-seg, opgevuld met '_').
    pub fn name(seg: &str) -> Vec<u8> {
        let mut s: Vec<u8> = seg.bytes().take(4).collect();
        while s.len() < 4 {
            s.push(b'_');
        }
        s
    }

    /// Codeer een integer-constante zo compact mogelijk.
    pub fn int(v: u64) -> Vec<u8> {
        if v == 0 {
            alloc::vec![ZERO_OP]
        } else if v == 1 {
            alloc::vec![ONE_OP]
        } else if v <= 0xFF {
            alloc::vec![BYTE_PREFIX, v as u8]
        } else if v <= 0xFFFF {
            alloc::vec![WORD_PREFIX, v as u8, (v >> 8) as u8]
        } else if v <= 0xFFFF_FFFF {
            let mut o = alloc::vec![DWORD_PREFIX];
            o.extend_from_slice(&(v as u32).to_le_bytes());
            o
        } else {
            let mut o = alloc::vec![QWORD_PREFIX];
            o.extend_from_slice(&v.to_le_bytes());
            o
        }
    }

    /// `Name(seg, value)` — een data-object onder een naam.
    pub fn name_def(seg: &str, value: &[u8]) -> Vec<u8> {
        let mut o = alloc::vec![NAME_OP];
        o.extend_from_slice(&name(seg));
        o.extend_from_slice(value);
        o
    }

    /// `Package { items }`.
    pub fn package(items: &[Vec<u8>]) -> Vec<u8> {
        let mut body = alloc::vec![items.len() as u8];
        for it in items {
            body.extend_from_slice(it);
        }
        let mut o = alloc::vec![PACKAGE_OP];
        o.extend_from_slice(&pkg_length(body.len()));
        o.extend_from_slice(&body);
        o
    }

    /// `Return(expr)`.
    pub fn ret(expr: &[u8]) -> Vec<u8> {
        let mut o = alloc::vec![RETURN_OP];
        o.extend_from_slice(expr);
        o
    }

    /// `Method(seg, flags=0) { body }`.
    pub fn method(seg: &str, body: &[u8]) -> Vec<u8> {
        let mut inner = name(seg);
        inner.push(0); // method-flags (0 args)
        inner.extend_from_slice(body);
        let mut o = alloc::vec![METHOD_OP];
        o.extend_from_slice(&pkg_length(inner.len()));
        o.extend_from_slice(&inner);
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_integer() {
        // Name(_TMP, 0x0BB8)  (3000 = 30.00 °C in deci-Kelvin)
        let aml = enc::name_def("_TMP", &enc::int(0x0BB8));
        let ns = AmlNamespace::parse(&aml);
        assert_eq!(ns.evaluate("_TMP"), Some(AmlValue::Integer(0x0BB8)));
        assert!(ns.contains("_TMP"));
    }

    #[test]
    fn method_returns_constant() {
        // Method(_STA) { Return(0x0F) }  — apparaat aanwezig+ingeschakeld.
        let body = enc::ret(&enc::int(0x0F));
        let aml = enc::method("_STA", &body);
        let ns = AmlNamespace::parse(&aml);
        assert_eq!(ns.evaluate("_STA").and_then(|v| v.as_int()), Some(0x0F));
    }

    #[test]
    fn method_returns_package() {
        // _BST-achtig: Return(Package { 0, 0x7D0, 0x2710, 0x2EE0 })
        let body = enc::ret(&enc::package(&[
            enc::int(0),
            enc::int(0x7D0),
            enc::int(0x2710),
            enc::int(0x2EE0),
        ]));
        let aml = enc::method("_BST", &body);
        let ns = AmlNamespace::parse(&aml);
        let v = ns.evaluate("_BST").unwrap();
        let pkg = v.as_package().unwrap();
        assert_eq!(pkg.len(), 4);
        assert_eq!(pkg[1].as_int(), Some(0x7D0));
        assert_eq!(pkg[3].as_int(), Some(0x2EE0));
    }

    #[test]
    fn method_arithmetic_return() {
        // Method(_TMP) { Return(Add(0x0B00, 0xB8)) } = 0x0BB8
        let mut expr = alloc::vec![ADD_OP];
        expr.extend_from_slice(&enc::int(0x0B00));
        expr.extend_from_slice(&enc::int(0xB8));
        expr.push(0x00); // target = Zero
        let aml = enc::method("_TMP", &enc::ret(&expr));
        let ns = AmlNamespace::parse(&aml);
        assert_eq!(ns.evaluate("_TMP").and_then(|v| v.as_int()), Some(0x0BB8));
    }

    #[test]
    fn scope_groups_objects() {
        // Scope(_SB_) { Name(FOO_, 7) Name(BAR_, 9) }
        let mut inner = enc::name_def("FOO_", &enc::int(7));
        inner.extend_from_slice(&enc::name_def("BAR_", &enc::int(9)));
        let mut scope = alloc::vec![SCOPE_OP];
        let mut body = enc::name("_SB_");
        body.extend_from_slice(&inner);
        scope.extend_from_slice(&enc::pkg_length(body.len()));
        scope.extend_from_slice(&body);
        let ns = AmlNamespace::parse(&scope);
        assert_eq!(ns.evaluate("FOO_").and_then(|v| v.as_int()), Some(7));
        assert_eq!(ns.evaluate("BAR_").and_then(|v| v.as_int()), Some(9));
        assert_eq!(ns.len(), 2);
    }

    #[test]
    fn buffer_and_string() {
        // Name(_HID, "PNP0C0A")  (batterij-HID) als string.
        let mut val = alloc::vec![STRING_PREFIX];
        val.extend_from_slice(b"PNP0C0A\0");
        let aml = enc::name_def("_HID", &val);
        let ns = AmlNamespace::parse(&aml);
        assert_eq!(ns.evaluate("_HID"), Some(AmlValue::Buffer(b"PNP0C0A".to_vec())));
    }

    #[test]
    fn unknown_name_is_none() {
        let ns = AmlNamespace::parse(&enc::name_def("_STA", &enc::int(1)));
        assert!(ns.evaluate("_XYZ").is_none());
        assert!(!ns.contains("_XYZ"));
    }

    #[test]
    fn large_pkg_length_roundtrips() {
        // Een package met genoeg items dat de PkgLength 2 bytes nodig heeft (> 63).
        let items: Vec<Vec<u8>> = (0..40).map(|i| enc::int(i as u64 & 0xFF)).collect();
        let pkg = enc::package(&items);
        let aml = enc::name_def("BIG_", &pkg);
        let ns = AmlNamespace::parse(&aml);
        let v = ns.evaluate("BIG_").unwrap();
        assert_eq!(v.as_package().unwrap().len(), 40);
    }
}
