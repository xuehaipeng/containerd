#!/bin/sh
# Post-Start Hook Script for Containerd Session Restoration
# This script calls the Rust session-restore binary

# Set up logging
LOG_FILE="/tmp/session-restore.log"
LOCK_FILE="/tmp/session-restore.lock"

# Function to log messages
log() {
    echo "$(date '+%Y-%m-%d %H:%M:%S') $1" >> "$LOG_FILE" 2>&1
}

# Check if another instance is running
if [ -f "$LOCK_FILE" ]; then
    log "Another instance is running. Exiting."
    exit 0
fi

# Create lock file
echo "$$" > "$LOCK_FILE"

# Cleanup function
cleanup() {
    rm -f "$LOCK_FILE"
}

# Set up trap to ensure lock file is removed
trap cleanup EXIT

# Initialize log
log "=== Session Restore Hook Started ==="

# Set the log level to show info messages
export RUST_LOG=info

# Call the Rust binary for session restoration
if [ -x "/usr/local/bin/session-restore" ]; then
    log "Executing session restoration binary..."
    /usr/local/bin/session-restore >> "$LOG_FILE" 2>&1
    EXIT_CODE=$?
    
    if [ $EXIT_CODE -eq 0 ]; then
        log "Session restoration completed successfully"
    else
        log "Session restoration failed with exit code: $EXIT_CODE"
    fi
else
    log "ERROR: session-restore binary not found at /usr/local/bin/session-restore"
    exit 1
fi

log "=== Session Restore Hook Completed ==="