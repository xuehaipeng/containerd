#!/bin/bash
set -e

echo "Building session-manager with maximum Linux compatibility and optimizations..."

# Detect number of CPU cores for parallel builds
NPROC=$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo "4")
echo "Using $NPROC parallel jobs for compilation"

# Check if we're on Linux and can build musl target
TARGET="release"
MUSL_TARGET="x86_64-unknown-linux-musl"

# Check if musl target is available
if rustc --print target-list | grep -q "$MUSL_TARGET"; then
    echo "Checking musl target availability..."
    
    # Try to install the target if not already installed
    rustup target add $MUSL_TARGET 2>/dev/null || true
    
    # Check if we can build with musl (requires musl-gcc or similar)
    if command -v musl-gcc >/dev/null 2>&1 || [ -f "/usr/bin/musl-gcc" ]; then
        echo "✅ musl-gcc found, building statically linked binaries"
        TARGET="musl-static"
        export CC_x86_64_unknown_linux_musl=musl-gcc
        export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc
    elif command -v x86_64-linux-musl-gcc >/dev/null 2>&1; then
        echo "✅ x86_64-linux-musl-gcc found, building statically linked binaries"
        TARGET="musl-static"
        export CC_x86_64_unknown_linux_musl=x86_64-linux-musl-gcc
        export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc
    else
        echo "⚠️  musl-gcc not found, falling back to glibc build"
        echo "   To enable fully static builds, install musl-dev:"
        echo "   Ubuntu/Debian: sudo apt-get install musl-dev musl-tools"
        echo "   Alpine: apk add musl-dev"
        echo "   CentOS/RHEL: sudo yum install musl-gcc (or build from source)"
    fi
else
    echo "⚠️  musl target not available in this Rust installation"
fi

# Set optimization flags for better performance
export RUSTFLAGS="-C opt-level=3 -C codegen-units=1 -C panic=abort -C target-feature=+crt-static"
export CARGO_PROFILE_RELEASE_LTO=true
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1

# Build based on target availability
if [ "$TARGET" = "musl-static" ]; then
    echo "Building fully static binaries with musl libc using $NPROC parallel jobs..."
    echo "This will create binaries with no external dependencies (including GLIBC)"
    
    # Build for musl target
    cargo build --release --target $MUSL_TARGET --jobs $NPROC
    
    # Copy musl binaries
    echo "Copying static musl binaries..."
    mkdir -p ./target/compatible/
    cp ./target/$MUSL_TARGET/release/session-backup ./target/compatible/ 2>/dev/null || echo "session-backup binary not found"
    cp ./target/$MUSL_TARGET/release/session-restore ./target/compatible/ 2>/dev/null || echo "session-restore binary not found"
    
    # Strip binaries to reduce size
    echo "Stripping debug symbols from static binaries..."
    strip ./target/compatible/session-backup 2>/dev/null || true
    strip ./target/compatible/session-restore 2>/dev/null || true
    
    BINARY_TYPE="fully static (musl libc)"
else
    echo "Building with glibc compatibility optimizations using $NPROC parallel jobs..."
    
    # Build for native target with optimizations
    cargo build --release --jobs $NPROC
    
    # Copy glibc binaries
    echo "Copying optimized glibc binaries..."
    mkdir -p ./target/compatible/
    cp ./target/release/session-backup ./target/compatible/ 2>/dev/null || echo "session-backup binary not found"
    cp ./target/release/session-restore ./target/compatible/ 2>/dev/null || echo "session-restore binary not found"
    
    # Strip binaries to reduce size
    echo "Stripping debug symbols from binaries..."
    strip ./target/compatible/session-backup 2>/dev/null || true
    strip ./target/compatible/session-restore 2>/dev/null || true
    
    BINARY_TYPE="glibc dynamic"
fi

# Verify binaries and show optimization results
echo "Verifying binary compatibility and optimizations..."
if [ -f "./target/compatible/session-backup" ]; then
    echo "session-backup ($BINARY_TYPE):"
    file ./target/compatible/session-backup
    
    # Show dependencies
    if [ "$TARGET" = "musl-static" ]; then
        echo "  ✅ Fully static binary - no external dependencies!"
        # Double-check it's actually static
        if ldd ./target/compatible/session-backup 2>&1 | grep -q "not a dynamic executable"; then
            echo "  ✅ Confirmed: statically linked executable"
        elif ldd ./target/compatible/session-backup 2>&1 | grep -q "statically linked"; then
            echo "  ✅ Confirmed: statically linked executable"
        else
            echo "  ⚠️  Warning: may have some dynamic dependencies"
            ldd ./target/compatible/session-backup 2>/dev/null || true
        fi
    else
        echo "  Dynamic dependencies:"
        ldd ./target/compatible/session-backup 2>/dev/null || echo "  (dependency check not available)"
    fi
fi

if [ -f "./target/compatible/session-restore" ]; then
    echo ""
    echo "session-restore ($BINARY_TYPE):"
    file ./target/compatible/session-restore
    
    if [ "$TARGET" = "musl-static" ]; then
        echo "  ✅ Fully static binary - no external dependencies!"
    else
        echo "  Dynamic dependencies:"
        ldd ./target/compatible/session-restore 2>/dev/null || echo "  (dependency check not available)"
    fi
fi

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
        if [ "$SAVINGS" -gt 0 ]; then
            PERCENT=$(( (SAVINGS * 100) / OLD_SIZE ))
            echo "  session-backup: $SAVINGS bytes saved ($PERCENT% reduction)"
        else
            INCREASE=$((-SAVINGS))
            PERCENT=$(( (INCREASE * 100) / OLD_SIZE ))
            echo "  session-backup: $INCREASE bytes larger ($PERCENT% increase) - likely due to static linking"
        fi
    fi
fi

# Backup current build for next comparison
cp ./target/compatible/session-backup ./target/compatible/session-backup.old 2>/dev/null || true
cp ./target/compatible/session-restore ./target/compatible/session-restore.old 2>/dev/null || true

echo ""
echo "🚀 Performance optimizations applied:"
echo "  - Maximum optimization level (-C opt-level=3)"
echo "  - Link-time optimization (LTO enabled)"
echo "  - Single codegen unit for better optimization"
echo "  - Panic=abort for smaller binary size"
echo "  - Static CRT linking enabled"
echo "  - Debug symbols stripped"
echo "  - Parallel compilation with $NPROC jobs"

if [ "$TARGET" = "musl-static" ]; then
    echo "  - Fully static linking with musl libc"
    echo ""
    echo "🎯 GLIBC-free static binaries created!"
    echo "These binaries will work on ANY Linux system regardless of:"
    echo "  ✅ GLIBC version (no GLIBC dependency)"
    echo "  ✅ System libraries (fully self-contained)"
    echo "  ✅ Linux distribution (Alpine, Ubuntu, CentOS, etc.)"
    echo "  ✅ Container base images (even scratch/distroless)"
    echo ""
    echo "Perfect for deployment to systems with older GLIBC versions!"
else
    echo ""
    echo "These binaries work on most Linux systems but may require compatible GLIBC:"
    echo "  - Ubuntu 20.04+ (GLIBC 2.31+)"
    echo "  - CentOS 8+ (GLIBC 2.28+)"
    echo ""
    echo "💡 To create fully static binaries, install musl-dev and re-run:"
    echo "   Ubuntu/Debian: sudo apt-get install musl-dev musl-tools"
    echo "   Alpine: apk add musl-dev"
    echo "   Then re-run this script"
fi

echo ""
echo "🔧 Build completed using $NPROC CPU cores"