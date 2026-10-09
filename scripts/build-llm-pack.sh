#!/usr/bin/env bash
# build-llm-pack.sh: the on-device inference EuroPack (llama.cpp engine + a GGUF
# model), signed with the EuroOS developer key. The engine is built in an
# Ubuntu 24.04 container so it links against the glibc 2.39 EuroOS bundles,
# WITHOUT AVX/FMA/F16C/SSE4.2 (it runs under `-cpu qemu64` and on any x86-64)
# and without OpenMP (EuroOS bundles no libgomp).
#
#   scripts/build-llm-pack.sh OUT.img [MODEL.gguf]
#
# Default model: SmolLM2-135M-Instruct Q8_0 (145 MB, the boot self-test model).
# Boot with the pack as a virtio disk:
#   PACK=OUT.img python3 scripts/app-euro-desktop.py ...   (or -drive id=pk,if=none,file=OUT.img -device virtio-blk-pci,drive=pk)
# Then in the Terminal:  llm <prompt>
set -euo pipefail
OUT="${1:?usage: build-llm-pack.sh OUT.img [MODEL.gguf]}"
MODEL="${2:-}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
WORK="${WORK:-/tmp/euroos-llm-pack}"
mkdir -p "$WORK"
if [ ! -d "$WORK/llama.cpp" ]; then
  git clone -q --depth 1 https://github.com/ggml-org/llama.cpp "$WORK/llama.cpp"
fi
if [ -z "$MODEL" ]; then
  MODEL="$WORK/SmolLM2-135M-Instruct-Q8_0.gguf"
  [ -s "$MODEL" ] || curl -sL -o "$MODEL" "https://huggingface.co/bartowski/SmolLM2-135M-Instruct-GGUF/resolve/main/SmolLM2-135M-Instruct-Q8_0.gguf"
  head -c 4 "$MODEL" | grep -q GGUF || { echo "model download failed"; exit 1; }
fi
docker run --rm -v "$WORK/llama.cpp:/src" -v "$WORK:/out" ubuntu:24.04 bash -c '
  set -e; export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq >/dev/null; apt-get install -y -qq cmake g++ make git >/dev/null 2>&1
  cd /src
  cmake -S . -B build-euroos -DGGML_NATIVE=OFF -DGGML_AVX=OFF -DGGML_AVX2=OFF -DGGML_AVX512=OFF \
    -DGGML_FMA=OFF -DGGML_F16C=OFF -DGGML_SSE42=OFF -DGGML_BMI2=OFF -DGGML_OPENMP=OFF \
    -DBUILD_SHARED_LIBS=OFF -DLLAMA_CURL=OFF -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_SERVER=OFF \
    -DCMAKE_BUILD_TYPE=Release >/dev/null
  cmake --build build-euroos --target llama-simple -j"$(nproc)" >/dev/null 2>&1
  strip -o /out/llama-simple build-euroos/bin/llama-simple'
objdump -T "$WORK/llama-simple" | grep -oE 'GLIBC_2\.[0-9]+' | sort -t. -k2 -n | uniq | tail -1 | grep -qE 'GLIBC_2\.(3[0-9]|[0-2][0-9])$' || { echo "engine needs a glibc newer than 2.39"; exit 1; }
python3 "$HERE/scripts/mkeuropack.py" "$OUT" "$WORK/llama-simple:/pack/llama-simple" "$MODEL:/pack/model.gguf"
python3 "$HERE/scripts/mkeuropack.py" --check "$OUT"
