//! EuroDrv: the host-tested core of EuroOS userspace drivers.
//!
//! A userspace driver on EuroOS holds exactly one thing the kernel gave it: a
//! [`grant::DeviceGrant`] for one PCI function (one BAR range, one DMA window,
//! optionally one interrupt vector). Everything it does goes through EuroIPC
//! messages in the [`proto`] format and the few driver syscalls of [`abi`].
//! When it dies, the kernel's supervisor decides with [`restart`] whether and
//! when a fresh instance starts.
//!
//! This crate holds the rules; the kernel (`kernel/src/udrv.rs`) and the
//! driver processes (`userland/e1000d`) are thin users of it. It is `no_std`
//! without `alloc`, so a driver process without a heap can use it too.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod abi;
pub mod grant;
pub mod proto;
pub mod restart;
