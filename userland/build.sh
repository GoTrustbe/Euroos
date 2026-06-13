#!/usr/bin/env bash
# EuroToolchain (Track 6, fase 6.1): compileer een vrijstaand C-programma naar
# een platte, positie-onafhankelijke binary voor EuroOS userspace (ring 3).
set -euo pipefail
cd "$(dirname "$0")"

CC=${CC:-gcc}
echo "==> compiler: $($CC --version | head -1)"

# Bouw elk userspace-programma: C -> vrijstaande, gestripte, positie-onafhankelijke ELF.
for prog in hello cat linuxprog muslprog argvprog daemon forktest execee forkpipe ticker; do
    # -fpie: rip-relatief (geen absolute relocaties); -ffreestanding -nostdlib: geen libc.
    $CC -ffreestanding -nostdlib -fpie -fno-stack-protector -fno-asynchronous-unwind-tables \
        -Os -c "$prog.c" -o "$prog.o"
    # -n (nmagic): geen 0x1000-paginauitlijning → compacte ELF; daarna strippen.
    ld -n -nostdlib -T link.ld -o "$prog.elf" "$prog.o"
    strip --strip-all "$prog.elf"
    if objdump -dr "$prog.elf" | grep -qE "R_X86_64_(64|32|32S)\b"; then
        echo "FOUT: $prog heeft absolute relocaties — niet positie-onafhankelijk!"; exit 1
    fi
    echo "==> $prog.elf: $(stat -c%s "$prog.elf") bytes (positie-onafhankelijk)"
done

# pieprog: een ECHTE PIE (ET_DYN) met R_X86_64_RELATIVE-relocaties — gelinkt met
# `ld -pie` (niet de platte link.ld), zodat .rela.dyn + PT_DYNAMIC bewaard blijven.
# De kernel-loader past die relocaties toe (zoals een musl static-PIE vereist).
$CC -ffreestanding -nostdlib -fpie -fno-stack-protector -fno-asynchronous-unwind-tables \
    -Os -c pieprog.c -o pieprog.o
ld -pie -nostdlib -e _start -o pieprog.elf pieprog.o
strip --strip-all pieprog.elf
relcount=$(objdump -R pieprog.elf | grep -c R_X86_64_RELATIVE || true)
if [ "$relcount" -lt 1 ]; then
    echo "FOUT: pieprog heeft geen R_X86_64_RELATIVE relocaties — test zinloos!"; exit 1
fi
echo "==> pieprog.elf: $(stat -c%s pieprog.elf) bytes (PIE, $relcount RELATIVE-relocaties)"

# muslreal: een ECHTE binary gelinkt tegen musl libc (static-PIE) — geen eigen
# syscall-stubs, gebruikt printf/malloc/strlen uit musl. Bewijst dat EuroKernel
# ongewijzigde musl-userspace draait via de Linux-ABI + ELF-relocaties.
if command -v musl-gcc >/dev/null 2>&1; then
    for m in muslreal muslfile mcat mwrite mecho mupper msum menv msock mdns mtrack tlscount isotest worker mthread mpthread mmutex ipcrecv ipcsend; do
        musl-gcc -static-pie -Os -o "$m.elf" "$m.c"
        mrel=$(objdump -R "$m.elf" 2>/dev/null | grep -c R_X86_64_RELATIVE || true)
        mbad=$(readelf -r "$m.elf" 2>/dev/null | awk '{print $3}' | grep -cE "R_X86_64_(IRELATIVE|TPOFF|DTPMOD|DTPOFF)" || true)
        if [ "$mbad" -gt 0 ]; then
            echo "WAARSCHUWING: $m heeft $mbad niet-ondersteunde reloc-types (IRELATIVE/TLS)"
        fi
        echo "==> $m.elf: $(stat -c%s "$m.elf") bytes (musl static-PIE, $mrel RELATIVE-relocaties)"
    done
else
    echo "WAARSCHUWING: musl-gcc niet gevonden — muslreal/muslfile overgeslagen"
fi

# EuroToolchain security: sign every userland binary with the EuroOS Ed25519
# developer key. The kernel verifies these signatures before running (verify-
# before-execute). Tampered binaries are cryptographically rejected.
echo "==> Ed25519-ondertekening van alle userland-binaries..."
python3 sign.py *.elf

# H3: DYNAMISCHE LINKING — bouw een vrijstaande shared library (libeuro.so) + een
# dynamisch-gelinkte executable (dyntest.elf) die ernaar verwijst via PLT/GOT
# (R_X86_64_JUMP_SLOT). De kernel-dynlinker laadt beide + resolved het symbool. Ná
# het tekenen: deze worden ingebed + via de H3-zelftest direct geladen (geen sig-pad).
echo "==> H3: dynamische-linking-testartefacten (libeuro.so + dyntest.elf)..."
gcc -ffreestanding -nostdlib -fPIC -shared -Os -o libeuro.so dynlib.c
gcc -ffreestanding -nostdlib -fPIC -Os -c dyntest.c -o dyntest.o
ld -pie -nostdlib -e _start -o dyntest.elf dyntest.o libeuro.so
rm -f dyntest.o
jmpslot=$(readelf -rW dyntest.elf 2>/dev/null | grep -c JUMP_SLO || true)
if [ "$jmpslot" -lt 1 ]; then
    echo "FOUT: dyntest heeft geen R_X86_64_JUMP_SLOT — dynamische-link-test zinloos!"; exit 1
fi
echo "==> dyntest.elf: $(stat -c%s dyntest.elf) bytes (PIE, $jmpslot JUMP_SLOT) · libeuro.so: $(stat -c%s libeuro.so) bytes"

# Sprint 1 / H3: TLS — een vrijstaande PIE met een __thread-variabele. De kernel-
# ld.so zet het statische TLS-blok + FS_BASE op (de binary zet zelf geen TLS op).
gcc -ffreestanding -nostdlib -fPIC -Os -c tlsprog.c -o tlsprog.o
ld -pie -nostdlib -e _start -o tlsprog.elf tlsprog.o
rm -f tlsprog.o
tlsseg=$(readelf -lW tlsprog.elf 2>/dev/null | grep -c TLS || true)
if [ "$tlsseg" -lt 1 ]; then
    echo "FOUT: tlsprog heeft geen PT_TLS — TLS-test zinloos!"; exit 1
fi
echo "==> tlsprog.elf: $(stat -c%s tlsprog.elf) bytes (PIE, PT_TLS aanwezig)"

# Sprint 1 / H3 stage 1b: CROSS-MODULE TLS — libtls.so heeft een __thread `ctr`
# (initial-exec → R_X86_64_TPOFF64); dyntls.elf roept bump() aan. De kernel-ld.so
# zet het multi-module TLS-blok op + patcht de TPOFF64-relocatie.
gcc -ffreestanding -nostdlib -fPIC -shared -ftls-model=initial-exec -Os -o libtls.so libtls.c
gcc -ffreestanding -nostdlib -fPIC -Os -c dyntls.c -o dyntls.o
ld -pie -nostdlib -e _start -o dyntls.elf dyntls.o libtls.so
rm -f dyntls.o
tpoff=$(readelf -rW libtls.so 2>/dev/null | grep -c TPOFF64 || true)
if [ "$tpoff" -lt 1 ]; then
    echo "FOUT: libtls.so heeft geen R_X86_64_TPOFF64 — cross-module-TLS-test zinloos!"; exit 1
fi
echo "==> dyntls.elf + libtls.so: $tpoff TPOFF64-relocatie(s)"
