# ASLR S6 audit: every place that turns an arena address into another one

S4 audited every kernel path that reaches the memory of the launched process; S5 drew the base. With S6 one base is
three (image, heap, stack), so every place that computed "base + offset" or walked "base .. base + span" was looked at
again. Method: grep of `ARENA_BASE`, `ARENA_SPAN_DYN`, `LAUNCH_ARENA_PHYS`, `PERSIST_ARENA`, `HEAP_END`, `BRK_END`,
`stack_top`, `in_user_arena`, `arena_is_lazy`, every `paging` function with a `virt_arena`/`nblocks` pair, `ARENA_ZONE`,
`in_arena_zone`, `>> 39` in `kernel/src`; then the self-tests and workloads of the proof README. A place that would silently
do less (walk only the first segment, skip a slot) counts as a fault, as the S4 reaper finding did.

| place | before S6 | verdict |
|---|---|---|
| `paging::ARENA_ZONE_END` (12 TiB) and everything that uses it: `in_arena_zone` (page-fault dispatch of copy-on-write and lazy faults), `arena_pdpt`, `arena_pml4_slots` (walks of `free_address_space`, `free_fork_tables`, `lazy_arena_frames`), zone checks of `try_build_address_space_arena` and `lazy_arena_rebase`, `[pf-diag]` range | slots 16..23 | **would have been silent**: a heap or stack segment at 12..20 TiB would not be freed (tables leaked by `free_fork_tables`/`free_address_space`), not counted by the census, and its faults not dispatched. Zone now 8..20 TiB (slots 16..39), compile-time tied to `euromm::layout::ARENA_ZONE`. `[arena-segs]` checks teardown to the frame and the census; `[robust-paging]` `arena_segs` checks a refused build keeps 0 frames |
| `try_build_address_space_arena`, `map_user_arena` | one (virt, phys, nblocks) | per segment: RAM `phys + seg.off` at `seg.virt` |
| `arena_split` | `virt .. + nblocks` | every block of every segment (`Segs::blocks`) |
| `fill_remap_tables_lazy` (fork) | one range | every segment |
| `share_arena_cow` (fork) | one range | every segment; the child has the same list |
| `clone_arena_lazy` | one range | every segment. No caller (dead code since BUG-156); converted so a later caller cannot walk one range |
| `lazy_arena_discard` (exec) | one range | every segment |
| `lazy_arena_rebase` (exec) | old -> new base | every old segment unbuilt, then every new one built (a new segment may lie where an old one was), all undone on failure |
| `arena_cow_fault`, `lazy_arena_fault`, `lazy_pte_ptr`, `arena_pte_ptr` | by address | by address through `arena_pdpt`: follow the wider zone, no change |
| `in_user_arena` (every copy helper, `MAP_FIXED` in the arena, mremap) | `base <= ptr`, `end <= base + span` | inside **one** segment (`Segs::contains`); a range across the gap between two segments is EFAULT. Native paths that swap `ARENA_BASE` to their own arena (bg dispatch, `user_ptr_selftest`, native launchers) keep the one-range check: the segment atomics are keyed to the image base they belong to |
| three launchers (`run_glibc`, `spawn_glibc_persistent`, `glibc_disk_launch`; `run_glibc_disk` and `spawn_glibc_disk_persistent` through it) | `kern(va) = va - base + phys` | `kern(va) = phys + arena offset of va` (`ArenaPlace::kern`); `load_elf64` and the stack builder write inside one segment (exe + ld.so below 11 MiB of a >= 16 MiB image segment, the initial stack in the stack segment: host tests) |
| `glibc_arena_layout` / `euromm::arena::glibc_layout` | base + offsets | `glibc_layout_segs`: the same offsets seen through their segment; the mmap window (`HEAP_END`) ends with the heap segment instead of 1 MiB below the stack top |
| `HEAP_END`, `BRK_END`, `HEAP_BREAK`, brk, mmap window, `/proc/self/maps` `[heap]` | in one arena | all inside the heap segment: unchanged code |
| `do_child_execve` | one base; the non-lazy wipe `arena_fill(arena, span)` | discard, rebase and layout per segment; the non-lazy wipe per segment (over the whole span it would have written into the gap: a ring-0 fault). Order of design 2.4 kept |
| `do_glibc_fork` | one range | the segment list of the parent (`arena_segs_now`) |
| `ChildMem` | `arena_base`, `arena_span` | + heap and stack segment (base, offset) and the key; snapshot and swap |
| `reap_orphan_fork_children` copy-on-write sweep | RAM `LAUNCH_ARENA_PHYS .. + span` | correct unchanged: the RAM of the launched process is still one piece; the counts are per frame |
| `glibc_launch_abort`, `glibc_disk_launch_abort`, `PERSIST_ARENA`, teardown of the launched arena | RAM | unchanged, correct |
| `fork_child_teardown` | child arena = 0 (lazy) | unchanged; tables through `free_fork_tables` (wider zone) |
| `forkvdsotest::cow_pages` (`[fork-cow-write]`) | walked `base .. base + span` by table lookup | **would have been silent**: it found copy-on-write pages in the image segment only. Now per segment |
| `demand_addr_origin` (`arena+off` in fault reports), `[mm-fail]` window size | `addr - ARENA_BASE` | the arena offset through the segment; an address in no segment says so |
| `aslr_exec_check`, `place`, `exec_arena`, `arena_place` | one base | segment list; `fixed`, `identity` and `aslr.split=off` are one segment |
| `phys_probe_note`, `read_glibc_u32`, census, `mem_census` | by `translate_in` / sizes | unchanged |
| vDSO (`VDSO_SLOT` 200), demand zone 32..96 TiB | | no overlap with 8..20 TiB |
| A10 (another address space), A11 (DMA) | S4 audit | no new path: S6 changes only where the arena is mapped, not who reaches it |
