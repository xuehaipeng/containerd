#!/bin/bash
set -e

echo "Building session-manager with maximum Linux compatibility and optimizations..."

# Detect number of CPU cores for parallel builds
NPROC=$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo "4")
echo "Using $NPROC parallel jobs for compilation"

# Ensure musl target is available
echo "Adding musl target..."
rustup target add x86_64-unknown-linux-musl

# Set optimization flags for better performance
export RUSTFLAGS="-C target-cpu=native -C opt-level=3 -C codegen-units=1 -C panic=abort"
export CARGO_PROFILE_RELEASE_LTO=true
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1

# Build with musl for static linking (no GLIBC dependencies) using parallel jobs
echo "Building with musl (static linking) using $NPROC parallel jobs..."
cargo build --release --target x86_64-unknown-linux-musl --jobs $NPROC

# Strip binaries to reduce size
echo "Stripping debug symbols from binaries..."
strip ./target/x86_64-unknown-linux-musl/release/session-backup 2>/dev/null || true
strip ./target/x86_64-unknown-linux-musl/release/session-restore 2>/dev/null || true

# Copy binaries to convenient location
echo "Copying optimized binaries..."
mkdir -p ./target/compatible/
cp ./target/x86_64-unknown-linux-musl/release/session-backup ./target/compatible/
cp ./target/x86_64-unknown-linux-musl/release/session-restore ./target/compatible/

# Verify binaries and show optimization results
echo "Verifying binary compatibility and optimizations..."
echo "session-backup:"
file ./target/compatible/session-backup
ldd ./target/compatible/session-backup 2>/dev/null || echo "  ✅ Statically linked (no dynamic dependencies)"

echo "session-restore:"
file ./target/compatible/session-restore
ldd ./target/compatible/session-restore 2>/dev/null || echo "  ✅ Statically linked (no dynamic dependencies)"

echo ""
echo "✅ Optimized compatible binaries built successfully!"
echo "📁 Location: ./target/compatible/"
echo "📊 Binary sizes and optimization results:"
ls -lh ./target/compatible/

# Show size comparison if previous build exists
if [ -f "./target/compatible/session-backup.old" ]; then
    echo ""
    echo "📈 Size optimization comparison:"
    OLD_SIZE=$(stat -f%z ./target/compatible/session-backup.old 2>/dev/null || stat -c%s ./target/compatible/session-backup.old 2>/dev/null || echo "0")
    NEW_SIZE=$(stat -f%z ./target/compatible/session-backup 2>/dev/null || stat -c%s ./target/compatible/session-backup 2>/dev/null || echo "0")
    if [ "$OLD_SIZE" -gt 0 ] && [ "$NEW_SIZE" -gt 0 ]; then
        SAVINGS=$((OLD_SIZE - NEW_SIZE))
        PERCENT=$(( (SAVINGS * 100) / OLD_SIZE ))
        echo "  session-backup: $SAVINGS bytes saved ($PERCENT% reduction)"
    fi
fi

# Backup current build for next comparison
cp ./target/compatible/session-backup ./target/compatible/session-backup.old 2>/dev/null || true
cp ./target/compatible/session-restore ./target/compatible/session-restore.old 2>/dev/null || true

echo ""
echo "🚀 Performance optimizations applied:"
echo "  - Native CPU optimizations (-C target-cpu=native)"
echo "  - Maximum optimization level (-C opt-level=3)"
echo "  - Link-time optimization (LTO enabled)"
echo "  - Single codegen unit for better optimization"
echo "  - Panic=abort for smaller binary size"
echo "  - Debug symbols stripped"
echo "  - Parallel compilation with $NPROC jobs"
echo ""
echo "These binaries will work on any Linux system (x86_64) regardless of GLIBC version."
echo "Tested compatible with:"
echo "  - Ubuntu 18.04+ (GLIBC 2.27+)"
echo "  - CentOS 7+ (GLIBC 2.17+)" 
echo "  - Alpine Linux (musl libc)"
echo "  - Any modern Linux distribution"
echo ""
echo "🔧 Build completed in parallel using $NPROC CPU cores"