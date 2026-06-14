//! EuroMM — physical memory management (Track 3.1).
//!
//! Contains the `FrameAllocator`: a bitmap allocator for 4 KiB physical frames.
//! One bit per frame (0 = free, 1 = in use). For 4 GiB RAM that is a 128 KiB
//! bitmap. Initialized from the UEFI memory map (the usable
//! regions). `no_std` + `alloc`; fully host-tested.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod frame;
pub mod swap;

pub use frame::{FrameAllocator, MemoryRegion, PAGE_SIZE};
pub use swap::{Clock, SwapArea};
