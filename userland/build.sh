#!/usr/bin/env bash
# EuroToolchain (Track 6, phase 6.1): compile a freestanding C program into
# a flat, position-independent binary for EuroOS userspace (ring 3).
set -euo pipefail
cd "$(dirname "$0")"

CC=${CC:-gcc}
echo "==> compiler: $($CC --version | head -1)"

# Build each userspace program: C -> freestanding, stripped, position-independent ELF.
for prog in hello cat linuxprog muslprog argvprog daemon forktest execee forkpipe ticker; do
    # -fpie: rip-relative (no absolute relocations); -ffreestanding -nostdlib: no libc.
    $CC -ffreestanding -nostdlib -fpie -fno-stack-protector -fno-asynchronous-unwind-tables \
        -Os -c "$prog.c" -o "$prog.o"
    # -n (nmagic): no 0x1000 page alignment → compact ELF; then strip.
    ld -n -nostdlib -T link.ld -o "$prog.elf" "$prog.o"
    strip --strip-all "$prog.elf"
    if objdump -dr "$prog.elf" | grep -qE "R_X86_64_(64|32|32S)\b"; then
        echo "ERROR: $prog has absolute relocations — not position-independent!"; exit 1
    fi
    echo "==> $prog.elf: $(stat -c%s "$prog.elf") bytes (position-independent)"
done

# pieprog: a REAL PIE (ET_DYN) with R_X86_64_RELATIVE relocations — linked with
# `ld -pie` (not the flat link.ld), so .rela.dyn + PT_DYNAMIC are preserved.
# The kernel loader applies those relocations (as a musl static-PIE requires).
$CC -ffreestanding -nostdlib -fpie -fno-stack-protector -fno-asynchronous-unwind-tables \
    -Os -c pieprog.c -o pieprog.o
ld -pie -nostdlib -e _start -o pieprog.elf pieprog.o
strip --strip-all pieprog.elf
relcount=$(objdump -R pieprog.elf | grep -c R_X86_64_RELATIVE || true)
if [ "$relcount" -lt 1 ]; then
    echo "ERROR: pieprog has no R_X86_64_RELATIVE relocations — test pointless!"; exit 1
fi
echo "==> pieprog.elf: $(stat -c%s pieprog.elf) bytes (PIE, $relcount RELATIVE relocations)"

# muslreal: a REAL binary linked against musl libc (static-PIE) — no own
# syscall stubs, uses printf/malloc/strlen from musl. Proves that EuroKernel
# runs unmodified musl userspace via the Linux ABI + ELF relocations.
if command -v musl-gcc >/dev/null 2>&1; then
    for m in muslreal muslfile mcat mwrite mecho mupper msum menv msock mdns mtrack tlscount isotest worker mthread mpthread mmutex ipcrecv ipcsend; do
        musl-gcc -static-pie -Os -o "$m.elf" "$m.c"
        mrel=$(objdump -R "$m.elf" 2>/dev/null | grep -c R_X86_64_RELATIVE || true)
        mbad=$(readelf -r "$m.elf" 2>/dev/null | awk '{print $3}' | grep -cE "R_X86_64_(IRELATIVE|TPOFF|DTPMOD|DTPOFF)" || true)
        if [ "$mbad" -gt 0 ]; then
            echo "WARNING: $m has $mbad unsupported reloc types (IRELATIVE/TLS)"
        fi
        echo "==> $m.elf: $(stat -c%s "$m.elf") bytes (musl static-PIE, $mrel RELATIVE relocations)"
    done
else
    echo "WARNING: musl-gcc not found — muslreal/muslfile skipped"
fi

# EuroToolchain security: sign every userland binary with the EuroOS Ed25519
# developer key. The kernel verifies these signatures before running (verify-
# before-execute). Tampered binaries are cryptographically rejected.
echo "==> Ed25519 signing of all userland binaries..."
python3 sign.py *.elf

# H3: DYNAMIC LINKING — build a freestanding shared library (libeuro.so) + a
# dynamically-linked executable (dyntest.elf) that references it via PLT/GOT
# (R_X86_64_JUMP_SLOT). The kernel dynlinker loads both + resolves the symbol. After
# signing: these get embedded + loaded directly via the H3 self-test (no sig path).
echo "==> H3: dynamic-linking test artifacts (libeuro.so + dyntest.elf)..."
gcc -ffreestanding -nostdlib -fPIC -shared -Os -o libeuro.so dynlib.c
gcc -ffreestanding -nostdlib -fPIC -Os -c dyntest.c -o dyntest.o
ld -pie -nostdlib -e _start -o dyntest.elf dyntest.o libeuro.so
rm -f dyntest.o
jmpslot=$(readelf -rW dyntest.elf 2>/dev/null | grep -c JUMP_SLO || true)
if [ "$jmpslot" -lt 1 ]; then
    echo "ERROR: dyntest has no R_X86_64_JUMP_SLOT — dynamic-link test pointless!"; exit 1
fi
echo "==> dyntest.elf: $(stat -c%s dyntest.elf) bytes (PIE, $jmpslot JUMP_SLOT) · libeuro.so: $(stat -c%s libeuro.so) bytes"

# Sprint 1 / H3: TLS — a freestanding PIE with a __thread variable. The kernel
# ld.so sets up the static TLS block + FS_BASE (the binary does not set up TLS itself).
gcc -ffreestanding -nostdlib -fPIC -Os -c tlsprog.c -o tlsprog.o
ld -pie -nostdlib -e _start -o tlsprog.elf tlsprog.o
rm -f tlsprog.o
tlsseg=$(readelf -lW tlsprog.elf 2>/dev/null | grep -c TLS || true)
if [ "$tlsseg" -lt 1 ]; then
    echo "ERROR: tlsprog has no PT_TLS — TLS test pointless!"; exit 1
fi
echo "==> tlsprog.elf: $(stat -c%s tlsprog.elf) bytes (PIE, PT_TLS present)"

# Sprint 1 / H3 stage 1b: CROSS-MODULE TLS — libtls.so has a __thread `ctr`
# (initial-exec → R_X86_64_TPOFF64); dyntls.elf calls bump(). The kernel ld.so
# sets up the multi-module TLS block + patches the TPOFF64 relocation.
gcc -ffreestanding -nostdlib -fPIC -shared -ftls-model=initial-exec -Os -o libtls.so libtls.c
gcc -ffreestanding -nostdlib -fPIC -Os -c dyntls.c -o dyntls.o
ld -pie -nostdlib -e _start -o dyntls.elf dyntls.o libtls.so
rm -f dyntls.o
tpoff=$(readelf -rW libtls.so 2>/dev/null | grep -c TPOFF64 || true)
if [ "$tpoff" -lt 1 ]; then
    echo "ERROR: libtls.so has no R_X86_64_TPOFF64 — cross-module-TLS test pointless!"; exit 1
fi
echo "==> dyntls.elf + libtls.so: $tpoff TPOFF64 relocation(s)"
