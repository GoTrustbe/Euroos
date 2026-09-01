//! EuroAML — a **minimal ACPI-AML-bytecode interpreter** (plan I3).
//!
//! ACPI's DSDT/SSDT tables contain no fixed fields but **AML bytecode**: a
//! small bytecode language in which the firmware expresses control-methods such as `_STA`
//! (status), `_TMP` (thermal-zone temperature), `_BST`/`_BIF` (battery) and `_PSR`
//! (mains power). To read these an OS must *interpret* the AML. [`euroacpi`]
//! provides the table parser; this crate is the bytecode layer on top.
//!
//! It is deliberately a **subset** — enough for the common read-out methods
//! (constants, packages, buffers, simple arithmetic `Return` expressions and
//! the namespace build-up via `Scope`/`Name`/`Method`) — not a full AML2.0 machine
//! (no OperationRegion/Field side-effects, no control-flow). Pure `no_std` logic
//! → the offset- and length-sensitive bytecode parsing is fully tested on the host.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

// ── AML opcodes (subset) ────────────────────────────────────────────────────
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
// Extended opcodes (after 0x5B).
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

/// An evaluated AML value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmlValue {
    Integer(u64),
    Buffer(Vec<u8>),
    Package(Vec<AmlValue>),
}

impl AmlValue {
    /// Return the integer value (None if it is not an integer).
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

/// A stored namespace object: either a data value (`Name`) or a
/// control-method (`Method`, with its raw body bytes to run later).
#[derive(Debug, Clone)]
enum Object {
    Value(AmlValue),
    Method { body: Vec<u8> },
}

/// Decoded ACPI battery status (from a `_BST` package), plus a percentage when
/// a full/design capacity is known (from `_BIX`/`_BIF`). Metal M5-1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatteryStatus {
    pub charging: bool,       // _BST[0] bit1
    pub discharging: bool,    // _BST[0] bit0
    pub rate: u32,            // present rate (mW or mA, per _BIF power-unit)
    pub remaining: u32,       // remaining capacity (mWh or mAh)
    pub voltage_mv: u32,      // present voltage (mV)
    pub percent: Option<u8>,  // remaining / last-full, if last-full is known
}

impl BatteryStatus {
    /// Decode a `_BST` package `[state, present-rate, remaining, voltage]`.
    /// `full` is the last-full capacity from `_BIX`/`_BIF` for the percentage.
    pub fn from_bst(bst: &[AmlValue], full: Option<u64>) -> Option<BatteryStatus> {
        if bst.len() < 4 {
            return None;
        }
        let f = |i: usize| bst[i].as_int().unwrap_or(0);
        let state = f(0);
        let remaining = f(2) as u32;
        // 0xFFFFFFFF = "unknown" per ACPI; treat as no percentage.
        let percent = match full {
            Some(fu) if fu != 0 && fu != 0xFFFF_FFFF && remaining != 0xFFFF_FFFF => {
                Some(((remaining as u64 * 100 / fu).min(100)) as u8)
            }
            _ => None,
        };
        Some(BatteryStatus {
            charging: state & 0b10 != 0,
            discharging: state & 0b01 != 0,
            rate: f(1) as u32,
            remaining,
            voltage_mv: f(3) as u32,
            percent,
        })
    }
}

/// The parsed AML namespace: a flat map of 4-character names → object. (We
/// ignore the scope hierarchy for the lookup; the last NameSeg is the key —
/// enough for looking up methods like `_STA`/`_TMP` by name.)
pub struct AmlNamespace {
    objects: BTreeMap<String, Object>,
}

impl AmlNamespace {
    /// Parse an AML byte block (the body of a DSDT/SSDT after the 36-byte SDT header)
    /// into a namespace.
    pub fn parse(aml: &[u8]) -> AmlNamespace {
        let mut ns = AmlNamespace { objects: BTreeMap::new() };
        let mut p = Parser { b: aml, pos: 0 };
        p.parse_term_list(&mut ns, aml.len());
        ns
    }

    /// How many objects were discovered?
    pub fn len(&self) -> usize {
        self.objects.len()
    }
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Is there an object (Name or Method) with this (last-NameSeg) name?
    pub fn contains(&self, name: &str) -> bool {
        self.objects.contains_key(&seg_key(name))
    }

    /// Evaluate a name: a `Name` returns its value; a `Method` is
    /// executed (subset: a single `Return(expr)`). None if unknown/not
    /// evaluable.
    pub fn evaluate(&self, name: &str) -> Option<AmlValue> {
        match self.objects.get(&seg_key(name))? {
            Object::Value(v) => Some(v.clone()),
            Object::Method { body } => {
                let mut p = Parser { b: body, pos: 0 };
                p.run_method(self)
            }
        }
    }

    // ── M5-1: ACPI power/battery helpers ──────────────────────────────────────

    /// True when the DSDT declares an ACPI battery (a `_BST` method exists).
    pub fn has_battery(&self) -> bool {
        self.contains("_BST")
    }

    /// True when the DSDT declares an AC adapter (a `_PSR` method exists).
    pub fn has_ac_adapter(&self) -> bool {
        self.contains("_PSR")
    }

    /// True when the DSDT declares a lid switch (`_LID` method exists).
    pub fn has_lid(&self) -> bool {
        self.contains("_LID")
    }

    /// AC online? Evaluates `_PSR` (0 = on battery, 1 = on AC). `None` when the
    /// method is absent or reads a value we can't evaluate statically (an
    /// EC-backed `_PSR` needs an Embedded-Controller driver — deferred).
    pub fn ac_online(&self) -> Option<bool> {
        Some(self.evaluate("_PSR")?.as_int()? != 0)
    }

    /// Last-full (or design) battery capacity from `_BIX`/`_BIF`, for the
    /// percentage. `_BIX` package: [rev, power-unit, design-cap, last-full, …];
    /// `_BIF`: [power-unit, design-cap, last-full, …]. Prefer last-full.
    pub fn battery_full_capacity(&self) -> Option<u64> {
        if let Some(bix) = self.evaluate("_BIX").as_ref().and_then(|v| v.as_package().map(|p| p.to_vec())) {
            // _BIX: last-full-charge capacity is field index 3.
            return bix.get(3).and_then(|v| v.as_int()).filter(|&c| c != 0 && c != 0xFFFF_FFFF);
        }
        if let Some(bif) = self.evaluate("_BIF").as_ref().and_then(|v| v.as_package().map(|p| p.to_vec())) {
            // _BIF: last-full-charge capacity is field index 2.
            return bif.get(2).and_then(|v| v.as_int()).filter(|&c| c != 0 && c != 0xFFFF_FFFF);
        }
        None
    }

    /// Decoded battery status from `_BST` (+ `_BIX`/`_BIF` for the percentage).
    /// `None` when `_BST` is absent or not statically evaluable (EC-backed
    /// `_BST` reads Embedded-Controller fields — that needs an EC driver, which
    /// is deferred; this decodes literal/computed `_BST` packages).
    pub fn battery_status(&self) -> Option<BatteryStatus> {
        let bst = self.evaluate("_BST")?;
        BatteryStatus::from_bst(bst.as_package()?, self.battery_full_capacity())
    }
}

/// The key under which we store/look up: the last 4-character NameSeg, padded.
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

    /// AML PkgLength: the first byte encodes how many following bytes come + the low
    /// bits of the length. Returns (total-length-incl-pkglength-bytes, bytes-read).
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
        // `len` counts from the FIRST pkglength byte. Subtract what we already read.
        let consumed = self.pos - start;
        len = len.saturating_sub(consumed);
        len
    }

    /// Read a NameString and return the last NameSeg key.
    fn name_string(&mut self) -> String {
        // Skip prefixes (root/parent).
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
                
                self.byte().unwrap_or(0) as usize
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

    /// Parse a TermList up to `end` (absolute byte offset in `self.b`).
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
                    // The contents of the scope is itself a TermList.
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
                        break; // unknown extended op → stop safely
                    }
                }
                _ => {
                    // Unknown/unsupported top-level term: stop safely (we
                    // have no full grammar; skipping further would derail the
                    // stream). The already-discovered objects remain valid.
                    break;
                }
            }
        }
    }

    /// Process an extended term (after 0x5B). Container objects (Device/ThermalZone/
    /// PowerResource/Processor) we recurse into so we find the methods inside (e.g. `_TMP`,
    /// `_BST`, `_STA`); non-container ext-ops we skip correctly. Returns
    /// false if the sub-op is unknown (then the caller stops safely).
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
                // Field/IndexField: PkgLength covers the entire definition → skip.
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

    /// Parse a DataObject / constant expression → AmlValue.
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
                let _size = self.parse_data_object(); // buffer size (ignored)
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
                // Skip the Target (where the result goes): usually Zero/0x00.
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

    /// Execute a method body (subset): find the first `Return(expr)` and evaluate.
    /// A body without Return returns `Integer(0)` (implicit zero).
    fn run_method(&mut self, _ns: &AmlNamespace) -> Option<AmlValue> {
        while let Some(op) = self.peek() {
            if op == RETURN_OP {
                self.pos += 1;
                return self.parse_data_object();
            }
            // Skip uncomprehended statements by searching for the next Return.
            self.pos += 1;
        }
        Some(AmlValue::Integer(0))
    }
}

/// AML encoder helpers — build valid AML byte blocks for tests (and for
/// generating simple tables). Mirrors the parser.
pub mod enc {
    use super::*;

    /// Encode a PkgLength for a payload of `len` bytes (the length counts the
    /// pkglength bytes themselves, as AML requires).
    pub fn pkg_length(payload_len: usize) -> Vec<u8> {
        // Try 1-byte (≤ 63 incl. itself): total = payload + 1.
        let total1 = payload_len + 1;
        if total1 <= 0x3F {
            return alloc::vec![total1 as u8];
        }
        // 2-byte: lead bits[7:6]=01, low nibble + 1 following byte. total = payload + 2.
        let total2 = payload_len + 2;
        alloc::vec![0x40 | (total2 & 0x0F) as u8, (total2 >> 4) as u8]
    }

    /// Encode a NameString (single 4-character seg, padded with '_').
    pub fn name(seg: &str) -> Vec<u8> {
        let mut s: Vec<u8> = seg.bytes().take(4).collect();
        while s.len() < 4 {
            s.push(b'_');
        }
        s
    }

    /// Encode an integer constant as compactly as possible.
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

    /// `Name(seg, value)` — a data object under a name.
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
        inner.push(0); // method flags (0 args)
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
        // Method(_STA) { Return(0x0F) }  — device present+enabled.
        let body = enc::ret(&enc::int(0x0F));
        let aml = enc::method("_STA", &body);
        let ns = AmlNamespace::parse(&aml);
        assert_eq!(ns.evaluate("_STA").and_then(|v| v.as_int()), Some(0x0F));
    }

    #[test]
    fn method_returns_package() {
        // _BST-like: Return(Package { 0, 0x7D0, 0x2710, 0x2EE0 })
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
    fn battery_status_decode_discharging() {
        // A battery Device (M5-1): _BST returns [state=1(discharging), rate=2000,
        // remaining=8000, voltage=12000] and _BIF gives last-full=10000.
        // Device(BAT0) { Name(_BIF, Package{...}) Method(_BST){Return(Package{...})} }
        let bif = enc::name_def(
            "_BIF",
            &enc::package(&[
                enc::int(0),     // power unit
                enc::int(10000), // design capacity
                enc::int(10000), // last-full capacity (index 2)
                enc::int(1),     // technology
            ]),
        );
        let bst = enc::method(
            "_BST",
            &enc::ret(&enc::package(&[
                enc::int(1),     // state: discharging
                enc::int(2000),  // present rate
                enc::int(8000),  // remaining capacity
                enc::int(12000), // present voltage
            ])),
        );
        let mut inner = bif;
        inner.extend_from_slice(&bst);
        let mut dev = alloc::vec![EXT_OP_PREFIX, EXT_DEVICE];
        let mut body = enc::name("BAT0");
        body.extend_from_slice(&inner);
        dev.extend_from_slice(&enc::pkg_length(body.len()));
        dev.extend_from_slice(&body);
        let ns = AmlNamespace::parse(&dev);

        assert!(ns.has_battery());
        assert!(!ns.has_ac_adapter());
        assert_eq!(ns.battery_full_capacity(), Some(10000));
        let b = ns.battery_status().unwrap();
        assert!(b.discharging && !b.charging);
        assert_eq!(b.rate, 2000);
        assert_eq!(b.remaining, 8000);
        assert_eq!(b.voltage_mv, 12000);
        assert_eq!(b.percent, Some(80)); // 8000 / 10000
    }

    #[test]
    fn ac_adapter_online() {
        // Method(_PSR) { Return(1) } → AC online.
        let ns = AmlNamespace::parse(&enc::method("_PSR", &enc::ret(&enc::int(1))));
        assert!(ns.has_ac_adapter());
        assert_eq!(ns.ac_online(), Some(true));
        let off = AmlNamespace::parse(&enc::method("_PSR", &enc::ret(&enc::int(0))));
        assert_eq!(off.ac_online(), Some(false));
    }

    #[test]
    fn battery_unknown_capacity_no_percent() {
        // remaining = 0xFFFFFFFF (unknown) → no percentage, still decodes.
        let bst = enc::method(
            "_BST",
            &enc::ret(&enc::package(&[
                enc::int(2),          // charging
                enc::int(0),
                enc::int(0xFFFF_FFFF),
                enc::int(11100),
            ])),
        );
        let ns = AmlNamespace::parse(&bst);
        let b = ns.battery_status().unwrap();
        assert!(b.charging && !b.discharging);
        assert_eq!(b.percent, None);
        assert_eq!(b.voltage_mv, 11100);
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
        // Name(_HID, "PNP0C0A")  (battery HID) as string.
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
        // A package with enough items that the PkgLength needs 2 bytes (> 63).
        let items: Vec<Vec<u8>> = (0..40).map(|i| enc::int(i as u64 & 0xFF)).collect();
        let pkg = enc::package(&items);
        let aml = enc::name_def("BIG_", &pkg);
        let ns = AmlNamespace::parse(&aml);
        let v = ns.evaluate("BIG_").unwrap();
        assert_eq!(v.as_package().unwrap().len(), 40);
    }
}
