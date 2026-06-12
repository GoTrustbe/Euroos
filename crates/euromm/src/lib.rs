//! EuroMM — fysiek geheugenbeheer (Track 3.1).
//!
//! Bevat de `FrameAllocator`: een bitmap-allocator voor 4 KiB fysieke frames.
//! Eén bit per frame (0 = vrij, 1 = in gebruik). Voor 4 GiB RAM is dat 128 KiB
//! bitmap. Wordt geïnitialiseerd vanuit de UEFI-geheugenkaart (de bruikbare
//! regio's). `no_std` + `alloc`; volledig host-getest.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod frame;
pub mod swap;

pub use frame::{FrameAllocator, MemoryRegion, PAGE_SIZE};
pub use swap::{Clock, SwapArea};
