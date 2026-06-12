#!/usr/bin/env bash
# EuroOS panic-backtrace symbolisator (Sprint S1 observability).
#
# De UEFI-.efi is gestript, dus de kernel print bij een paniek RUWE runtime-adressen
# plus een ANCHOR-regel (runtime-adres van dump_registers_and_backtrace). Deze tool
# gebruikt de lld-link-map (target/kernel.map) om die adressen om te zetten naar
# functienamen. Map-regelformaat:  SEG:OFFSET   <symbool>   <absolute-VA>   <objfile>
#
# Gebruik:  scripts/symbolize.sh <kernel.map> <anchor_addr> <addr> [addr...]
set -u

IMGBASE=$((0x140000000)) # PE preferred load address (uit de map-header)
MAP="${1:?gebruik: symbolize.sh <kernel.map> <anchor_addr> <addr...>}"
ANCHOR="${2:?anchor-adres (uit '[panic] anchor ... @ 0x..') ontbreekt}"
shift 2
[ -f "$MAP" ] || { echo "map niet gevonden: $MAP"; exit 1; }

DEMANGLE="cat"
command -v rustfilt >/dev/null 2>&1 && DEMANGLE="rustfilt"

# (rva, mangled-naam) uit alle symboolregels; rva = absoluteVA - imagebase; gesorteerd.
SYMS=$(awk -v base="$IMGBASE" '
    $1 ~ /^[0-9a-fA-F]{4}:[0-9a-fA-F]{8}$/ && $3 ~ /^[0-9a-fA-F]{16}$/ {
        printf "%d %s\n", strtonum("0x" $3) - base, $2
    }' "$MAP" | sort -n)

anchor_rva=$(printf '%s\n' "$SYMS" | awk '/dump_registers_and_backtrace$/{print $1; exit}')
[ -n "$anchor_rva" ] || { echo "anchor-symbool niet in map"; exit 1; }
runtime_base=$(( $((ANCHOR)) - anchor_rva ))
printf "anchor_rva=0x%x  runtime_base=0x%x\n" "$anchor_rva" "$runtime_base"

for a in "$@"; do
    addr=$((a))
    rva=$((addr - runtime_base))
    res=$(printf '%s\n' "$SYMS" | awk -v t="$rva" '
        $1 <= t { n=$2; d=$1; next }
        { print n, t-d; done=1; exit }
        END { if (!done && n!="") print n, t-d }')
    sym=$(printf '%s' "$res" | awk '{print $1}' | $DEMANGLE)
    off=$(printf '%s' "$res" | awk '{printf "0x%x", $2}')
    printf "  %#018x  rva=%-#10x  ->  %s +%s\n" "$addr" "$rva" "${sym:-??}" "${off:-?}"
done
