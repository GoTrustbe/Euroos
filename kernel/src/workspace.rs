//! Virtual desktops (workspaces): Alt+1..4 switches between four sets of
//! windows. Implemented by saving each window's visibility per workspace and
//! restoring it on switch, so every existing `visible` check keeps working with
//! no change to the window model. This module holds the pure switch logic.

use alloc::vec::Vec;

/// The number of workspaces.
pub const COUNT: usize = 4;

/// Compute the visibility vector after switching: restore the target's saved
/// state, or, on the first visit to an empty workspace, hide everything.
pub fn switch_visibility(current_len: usize, target: Option<&[bool]>) -> Vec<bool> {
    match target {
        Some(saved) if saved.len() == current_len => saved.to_vec(),
        _ => alloc::vec![false; current_len],
    }
}

/// `[ws]` boot self-test: switching to a saved workspace restores its windows;
/// switching to a fresh one hides all.
pub fn selftest() {
    let current = [true, false, true];
    let saved = [false, true, false];
    let restored = switch_visibility(current.len(), Some(&saved)) == alloc::vec![false, true, false];
    let fresh = switch_visibility(current.len(), None) == alloc::vec![false, false, false];
    let ok = restored && fresh;
    crate::serial_println!(
        "[ws] Workspaces: restore-saved={restored}, fresh-workspace-empty={fresh} → {}",
        if ok { "OK (Alt+1..4 virtual desktops) ✓" } else { "FAILED ✗" }
    );
}
