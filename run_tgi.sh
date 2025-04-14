#!/bin/bash
set -e

# Kill les anciens process
pkill -f text-generation-launcher || true

# Rebuild le binaire
cargo build --release

# Lance le nouveau binaire avec les bons arguments
./target/release/text-generation-launcher \
  --model-id HuggingFaceH4/zephyr-7b-beta \
  --disable-custom-kernels
