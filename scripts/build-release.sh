#!/bin/bash
set -euo pipefail

# Build Release Script for Nellie Production
# Builds for multiple targets: x86_64 and aarch64 Linux

VERSION="${1:-$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')}"
OUTPUT_DIR="dist"

echo "Building Nellie Production v$VERSION..."

# Create output directory
mkdir -p "$OUTPUT_DIR"

# Build for x86_64 Linux
echo "Building for x86_64-unknown-linux-gnu..."
cargo build --release --target x86_64-unknown-linux-gnu
cp target/x86_64-unknown-linux-gnu/release/nellie "$OUTPUT_DIR/nellie-linux-x86_64"

# Build for aarch64 Linux (requires cross-compilation toolchain)
if command -v aarch64-linux-gnu-gcc &> /dev/null; then
    echo "Building for aarch64-unknown-linux-gnu..."
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
        cargo build --release --target aarch64-unknown-linux-gnu
    cp target/aarch64-unknown-linux-gnu/release/nellie "$OUTPUT_DIR/nellie-linux-aarch64"
else
    echo "Warning: aarch64-linux-gnu-gcc not found, skipping ARM64 build"
    echo "Install with: sudo apt-get install gcc-aarch64-linux-gnu"
fi

# Create checksums
echo "Creating checksums..."
cd "$OUTPUT_DIR"
sha256sum nellie-* > SHA256SUMS
cd -

# Print results
echo ""
echo "Build complete! Artifacts in $OUTPUT_DIR:"
ls -la "$OUTPUT_DIR"
echo ""
cat "$OUTPUT_DIR/SHA256SUMS"
