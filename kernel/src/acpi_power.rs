//! ACPI power sources (Metal M5-1): battery, AC adapter and lid, read from the
//! DSDT via the `euroaml` interpreter.
//!
//! Scope, honestly: a laptop's `_BST`/`_PSR` usually read Embedded-Controller
//! registers through AML `Field`/`OperationRegion`, which needs a live EC
//! driver — deferred. This module decodes the values that ARE statically
//! evaluable (literal/computed `_BST` packages and `_PSR`), reports which power
//! devices the firmware declares, and drives the `battery` shell command. On a
//! desktop or VM (QEMU q35 included) the DSDT declares no battery, and we say
//! so plainly rather than inventing a reading.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

/// A snapshot of what the DSDT told us at boot (the namespace isn't retained).
#[derive(Clone, Copy, Default)]
struct PowerInfo {
    has_battery: bool,
    has_ac: bool,
    has_lid: bool,
    ac_online: Option<bool>,
    // Decoded battery status when statically evaluable.
    battery: Option<Battery>,
}

#[derive(Clone, Copy)]
struct Battery {
    charging: bool,
    discharging: bool,
    rate: u32,
    remaining: u32,
    voltage_mv: u32,
    percent: Option<u8>,
}

static INFO: Mutex<PowerInfo> = Mutex::new(PowerInfo {
    has_battery: false,
    has_ac: false,
    has_lid: false,
    ac_online: None,
    battery: None,
});

/// Read the ACPI power sources from the parsed DSDT + log a boot summary.
pub fn report(ns: &euroaml::AmlNamespace) {
    let mut info = PowerInfo {
        has_battery: ns.has_battery(),
        has_ac: ns.has_ac_adapter(),
        has_lid: ns.has_lid(),
        ac_online: ns.ac_online(),
        battery: None,
    };
    if let Some(b) = ns.battery_status() {
        info.battery = Some(Battery {
            charging: b.charging,
            discharging: b.discharging,
            rate: b.rate,
            remaining: b.remaining,
            voltage_mv: b.voltage_mv,
            percent: b.percent,
        });
    }
    *INFO.lock() = info;

    if !info.has_battery && !info.has_ac && !info.has_lid {
        crate::serial_println!(
            "[acpi-pwr] no ACPI battery / AC adapter / lid in this firmware (expected on a desktop or VM)"
        );
        return;
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(b) = &info.battery {
        let pct = b.percent.map(|p| format!("{p}%")).unwrap_or_else(|| String::from("?"));
        let state = if b.charging {
            "charging"
        } else if b.discharging {
            "discharging"
        } else {
            "idle"
        };
        parts.push(format!("battery {pct} ({state}, {} mV)", b.voltage_mv));
    } else if info.has_battery {
        parts.push(String::from("battery present (EC-backed _BST — needs the EC driver, deferred)"));
    }
    if let Some(online) = info.ac_online {
        parts.push(format!("AC {}", if online { "online" } else { "offline" }));
    } else if info.has_ac {
        parts.push(String::from("AC adapter present"));
    }
    if info.has_lid {
        parts.push(String::from("lid switch present"));
    }
    crate::serial_println!("[acpi-pwr] {}", parts.join(", "));
}

/// The `battery` / `power` shell command: report the ACPI power state.
pub fn status_lines() -> Vec<String> {
    let info = *INFO.lock();
    let mut out = Vec::new();
    if !info.has_battery && !info.has_ac {
        out.push(String::from("No ACPI battery or AC adapter in this firmware."));
        out.push(String::from("(This is normal on a desktop or virtual machine.)"));
        return out;
    }
    if let Some(b) = &info.battery {
        let pct = b.percent.map(|p| format!("{p}%")).unwrap_or_else(|| String::from("unknown"));
        let state = if b.charging {
            "charging"
        } else if b.discharging {
            "discharging"
        } else {
            "not charging"
        };
        out.push(format!("Battery:  {pct}  ({state})"));
        out.push(format!("  rate {} · remaining {} · {} mV", b.rate, b.remaining, b.voltage_mv));
    } else if info.has_battery {
        out.push(String::from("Battery:  present, reading unavailable"));
        out.push(String::from("  (its _BST reads Embedded-Controller fields; the EC driver is deferred)"));
    }
    match info.ac_online {
        Some(true) => out.push(String::from("AC:       online")),
        Some(false) => out.push(String::from("AC:       offline (on battery)")),
        None if info.has_ac => out.push(String::from("AC:       adapter present")),
        None => {}
    }
    if info.has_lid {
        out.push(String::from("Lid:      switch present"));
    }
    out
}
