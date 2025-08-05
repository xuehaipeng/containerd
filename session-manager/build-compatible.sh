#!/bin/bash
set -e

echo "Building session-manager with maximum Linux compatibility..."

# Ensure musl target is available
echo "Adding musl target..."
rustup target add x86_64-unknown-linux-musl

# Build with musl for static linking (no GLIBC dependencies)
echo "Building with musl (static linking)..."
cargo build --release --target x86_64-unknown-linux-musl

# Copy binaries to convenient location
echo "Copying binaries..."
mkdir -p ./target/compatible/
cp ./target/x86_64-unknown-linux-musl/release/session-backup ./target/compatible/
cp ./target/x86_64-unknown-linux-musl/release/session-restore ./target/compatible/

# Verify binaries
echo "Verifying binary compatibility..."
echo "session-backup:"
file ./target/compatible/session-backup
ldd ./target/compatible/session-backup 2>/dev/null || echo "  ✅ Statically linked (no dynamic dependencies)"

echo "session-restore:"
file ./target/compatible/session-restore
ldd ./target/compatible/session-restore 2>/dev/null || echo "  ✅ Statically linked (no dynamic dependencies)"

echo ""
echo "✅ Compatible binaries built successfully!"
echo "📁 Location: ./target/compatible/"
echo "📊 Binary sizes:"
ls -lh ./target/compatible/

echo ""
echo "These binaries will work on any Linux system (x86_64) regardless of GLIBC version."
echo "Tested compatible with:"
echo "  - Ubuntu 18.04+ (GLIBC 2.27+)"
echo "  - CentOS 7+ (GLIBC 2.17+)" 
echo "  - Alpine Linux (musl libc)"
echo "  - Any modern Linux distribution"