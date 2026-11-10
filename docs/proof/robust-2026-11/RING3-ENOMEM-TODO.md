# To do in `ring3.rs`: use the `try_` address-space builders

`paging.rs` now has a refusing variant next to every address-space builder:

| panics when out of frames | refuses with `Err(OutOfFrames)` |
|---|---|
| `build_address_space` | `try_build_address_space` |
| `build_address_space_big` | `try_build_address_space_big` |
| `build_address_space_rwx` | `try_build_address_space_rwx` |
| `build_address_space_rwx_big` | `try_build_address_space_rwx_big` |
| `build_address_space_remap` (no caller left) | `try_build_address_space_remap` |

A refusal leaves the frame allocator exactly as it was (boot self-test
`[robust-paging]`: every builder at 0..n free frames, 19 refusals with 0 frames
kept, 5 builds freed again). The `_big` variants are stricter than the old
ones: when a page directory for a later arena block cannot be allocated they
free what they built and refuse, where the old functions hand out an address
space whose arena tail is not mapped for the process.

The old functions are now `try_…().expect(...)`: same behaviour as before, one
panic message (`out of frames for process page tables (caller must use the try_
variant)`). All ten callers are in `ring3.rs`, which this branch does not touch
(the debug team works in it). Until they are converted, starting a process
with fewer than 3-5 free frames is still a kernel panic. In order of exposure:

| `ring3.rs` (at `a36ef4b`) | function | returns | what a refusal has to undo |
|---|---|---|---|
| 6363 | `spawn_bg_musl` | `bool` | arena (512 frames), kstack (4): exactly the block that already handles `idx == usize::MAX` below it, minus `free_address_space` |
| 6503 | `spawn_bg_app` | `Option<…>` | arena (`frames`), kstack (4): the same |
| 10742 | `spawn_glibc_persistent` | `Option<…>` (`?` is already used above it) | the arena and what was allocated before the call |
| 11362 | `glibc_disk_launch` | `Result<_, &str>` | as its other `Err("(…)")` exits; `Err("(out of memory: page tables)")` |
| 11824 | `run_glibc` | `(String, u64)` | as its `(String::from("(bad ld.so ELF)"), u64::MAX)` exit |
| 9262 | `run_dynamic` | see its early exits | arena |
| 9362 | `run_interp` | see its early exits | arena |
| 16667 | `run_args` | see its early exits | arena, kstack |
| 3992 | `spawn_daemon` | `()` (boot) | log and return; `daemon-kstack` two lines above is an `expect` of the same kind |
| 8082 | `spawn_counter_task` | `u64` (boot demo) | log and return 0 |

The two background spawners carry the BUG-015 pattern already; for them the
change is this shape (shown for `spawn_bg_musl`):

```rust
let pml4 = match crate::paging::try_build_address_space(falloc, arena, &info.exec_pages, &info.writ_pages) {
    Ok(p) => p,
    Err(crate::paging::OutOfFrames) => {
        for f in 0..512u64 {
            let _ = falloc.free(arena + f * 4096);
        }
        for f in 0..4u64 {
            let _ = falloc.free(kstack + f * 4096);
        }
        crate::serial_println!("[euro] bg-musl (pid {pid}): no frames for page tables - not started");
        return false;
    }
};
```

Other `expect`s on allocation in `ring3.rs` (5 at `a36ef4b`,
`grep -n 'allocate.*\.expect' kernel/src/ring3.rs`) are the same class and
were left for the same reason.
