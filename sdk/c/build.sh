#!/usr/bin/env bash
# Build the C example as a native EuroOS app (freestanding static PIE, no libc)
# and package it with the example developer key.
set -euo pipefail
cd "$(dirname "$0")"
gcc -O2 -ffreestanding -nostdlib -fpie -static-pie -fno-stack-protector -fno-asynchronous-unwind-tables \
    -o hello-euro-c hello-euro-c.c
readelf -h hello-euro-c | grep -q 'DYN' || { echo "not a PIE"; exit 1; }
KEY=${KEY:-../examples/keys/example-dev.key}
[ -f "$KEY" ] || python3 ../euroapp.py keygen "${KEY%.key}"
python3 ../euroapp.py build --abi euro --name hello-euro-c --version 1.0.0 --binary hello-euro-c --key "$KEY" \
    --title "Hello (C, native)" --caps console,window --display x11 \
    --description "A native EuroOS window in freestanding C" --out hello-euro-c-1.0.0.eupkg
