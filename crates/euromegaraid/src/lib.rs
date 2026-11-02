//! EuroMegaRAID: the LSI/Broadcom MegaRAID driver logic, free of hardware access.
//!
//! MegaRAID controllers speak **MFI** (MegaRAID Firmware Interface): the host
//! writes the physical address of a 64-byte command frame into a queue-port
//! register, the firmware executes it and reports completion through a reply
//! queue in host memory (plus an interrupt). On top of that sit DCMDs
//! ("direct commands": controller info, the logical-drive list, cache flush)
//! and logical-drive I/O frames.
//!
//! This crate implements the **classic MFI frame path** of the gen2 family
//! (SAS2108, PCI `1000:0079`/`1000:0078`), which is what QEMU's
//! `-device megasas-gen2` emulates and what this code is proven against. The
//! newer **fusion** family (SAS2208 "Thunderbolt" `1000:005b`, SAS3108
//! "Invader" `1000:005d` = Dell PERC H730/H730P, SAS3008 "Fury" `1000:005f`)
//! uses MPT-style request descriptors and an MPI2 reply-descriptor queue
//! instead; [`regs::family`] recognises those and the driver refuses them
//! rather than guessing (see `docs/SPRINT-PLAN-MEGARAID.md`).
//!
//! Sources (hardware facts only, no code taken over): the MFI register and
//! frame definitions as documented in QEMU `hw/scsi/mfi.h` (BSD-licensed,
//! itself from FreeBSD `sys/dev/mfi/mfireg.h`) and the behaviour of QEMU
//! `hw/scsi/megasas.c` v10.2.0; the init and interrupt sequence as FreeBSD
//! `mfi(4)`/`mrsas(4)` perform it. Linux `megaraid_sas` (GPL) was read for
//! understanding only.
//!
//! Layering, the EuroOS way:
//! - [`regs`], [`fw`], [`frame`], [`info`], [`layout`]: pure encoders and
//!   decoders; every value from the device is untrusted and bounds-checked.
//! - [`mfi`]: the controller logic (handshake, init frame, DCMD, LD I/O, reply
//!   queue) against the [`mfi::Hal`] trait.
//! - [`blkif`]: the message format between the driver process and the kernel's
//!   block layer. Deliberately separate and small, so it can be replaced by a
//!   shared block-over-IPC interface without touching the MFI logic.
//! - [`grant`]: which part of the register BAR a driver process may map
//!   (the MSI-X table stays with the kernel).
//! - `sim` (tests only): a simulated MFI controller with fault injection.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod blkif;
pub mod frame;
pub mod fw;
pub mod grant;
pub mod info;
pub mod layout;
pub mod mfi;
pub mod regs;

#[cfg(test)]
mod sim;
