#!/usr/bin/env bash
# Build and package the two example apps with the example developer key.
# Needs: gcc, libx11-dev, python3 + cryptography.
set -euo pipefail
cd "$(dirname "$0")"
KEY=keys/example-dev.key
[ -f "$KEY" ] || python3 ../euroapp.py keygen keys/example-dev
gcc -O2 -o hello-cli/hello hello-cli/hello.c
python3 ../euroapp.py build --name hello-cli --version 1.0.0 --binary hello-cli/hello --key "$KEY" \
  --title "Hello (CLI)" --caps console,proc --profile utility --display none \
  --description "Smallest EuroOS app" --out hello-cli/hello-cli-1.0.0.eupkg
gcc -O2 -o hello-x11/hello hello-x11/hello.c -lX11
python3 ../euroapp.py build --name hello-x11 --version 1.0.0 --binary hello-x11/hello --key "$KEY" \
  --title "Hello (window)" --caps console,file,proc --profile utility --display x11 \
  --description "Windowed EuroOS app" --out hello-x11/hello-x11-1.0.0.eupkg
python3 ../euroapp.py verify hello-cli/hello-cli-1.0.0.eupkg --pub keys/example-dev.pub
python3 ../euroapp.py verify hello-x11/hello-x11-1.0.0.eupkg --pub keys/example-dev.pub
