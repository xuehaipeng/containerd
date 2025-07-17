#!/bin/sh
# Wrapper script for session-restore binary

# Enable debug output
set -x

# Log file for debugging
LOG_FILE="/tmp/session-restore-wrapper.log"

# Function to log messages
log() {
    echo "$(date '+%Y-%m-%d %H:%M:%S') $1" | tee -a "$LOG_FILE"
}

# Add session separator
echo "" >> "$LOG_FILE"
echo "================================================================================" >> "$LOG_FILE"
echo "NEW SESSION: $(date '+%Y-%m-%d %H:%M:%S')" >> "$LOG_FILE"
echo "================================================================================" >> "$LOG_FILE"

log "=== Session Restore Wrapper Started ==="
log "Arguments: $*"
log "Working directory: $(pwd)"
log "User: $(whoami)"

# Check if binary exists and is executable
BINARY_PATH="/scripts/session-restore"
if [ ! -f "$BINARY_PATH" ]; then
    log "ERROR: Binary not found at $BINARY_PATH"
    ls -la /scripts/ | tee -a "$LOG_FILE"
    exit 1
fi

if [ ! -x "$BINARY_PATH" ]; then
    log "ERROR: Binary not executable at $BINARY_PATH"
    ls -la "$BINARY_PATH" | tee -a "$LOG_FILE"
    exit 1
fi

log "Binary found and executable: $BINARY_PATH"

# Check for required files
if [ ! -f "/etc/path-mappings.json" ]; then
    log "WARNING: Path mappings file not found at /etc/path-mappings.json"
fi

if [ ! -d "/sessions" ]; then
    log "WARNING: Sessions directory not found at /sessions"
fi

# Set environment for logging
export RUST_LOG=debug

# Execute the binary with all arguments
log "Executing: $BINARY_PATH $*"
"$BINARY_PATH" "$@" 2>&1 | tee -a "$LOG_FILE"
EXIT_CODE=$?

log "Binary execution completed with exit code: $EXIT_CODE"

if [ $EXIT_CODE -ne 0 ]; then
    log "ERROR: Session restore failed"
    # Check for common issues
    ldd "$BINARY_PATH" 2>&1 | tee -a "$LOG_FILE"
fi

log "=== Session Restore Wrapper Completed ==="
exit $EXIT_CODE