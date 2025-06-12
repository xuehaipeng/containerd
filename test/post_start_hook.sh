#!/bin/sh

# Post-Start Hook Script for Shared Snapshot Session Management
# This script handles migration of data from previous sessions to the current session

LOG_FILE="/tmp/poststart.log"

# Function to log messages
log() {
    echo "$1" >> "$LOG_FILE" 2>&1
}

# Initialize log
echo "POST-START HOOK EXECUTED at $(date)" > "$LOG_FILE" 2>&1
log "=== Post-Start Hook: Checking for previous session data ==="
log "Using shell: sh (POSIX-compliant)"

# Also try to write to stdout/stderr
echo "POST-START HOOK RUNNING" >&1
echo "POST-START HOOK RUNNING" >&2

# Determine own upperdir from mount info
# Use a more portable approach that works with sh
MY_OWN_UPPERDIR_HOST_PATH=$(awk '/overlay/ && /upperdir=/ { 
  # Find the field containing upperdir=
  for (i=1; i<=NF; i++) { 
    if ($i ~ /upperdir=/) { 
      # Extract everything after upperdir= and before the next comma
      # Use gsub for better POSIX compatibility
      temp = $i
      gsub(/.*upperdir=/, "", temp)
      gsub(/,.*/, "", temp)
      if (temp != "") {
        print temp
        exit
      }
    } 
  } 
}' /proc/self/mountinfo)

log "Raw mount info:"
cat /proc/self/mountinfo | grep overlay >> "$LOG_FILE" 2>&1
log "Extracted upperdir: '$MY_OWN_UPPERDIR_HOST_PATH'"

if [ -z "$MY_OWN_UPPERDIR_HOST_PATH" ]; then
    log "ERROR: Could not determine own shared upperdir. Proceeding with empty session."
    exit 0
fi

log "Current upperdir: $MY_OWN_UPPERDIR_HOST_PATH"
MY_OWN_SNAPSHOT_ID=$(basename "$(dirname "$MY_OWN_UPPERDIR_HOST_PATH")")
log "Current snapshot ID: $MY_OWN_SNAPSHOT_ID"

# Construct base path for this notebook identity
NOTEBOOK_SESSIONS_BASE_HOST_PATH=$(dirname "$(dirname "$MY_OWN_UPPERDIR_HOST_PATH")")
log "Base sessions path: $NOTEBOOK_SESSIONS_BASE_HOST_PATH"

# Expected path based on the configuration: /nvme1/default/test-shared-snapshot-mysql/mysql/
EXPECTED_BASE_PATH="/nvme1/default/test-shared-snapshot-mysql-0/mysql"
if [ "$NOTEBOOK_SESSIONS_BASE_HOST_PATH" != "$EXPECTED_BASE_PATH" ]; then
    log "WARNING: Detected base path ($NOTEBOOK_SESSIONS_BASE_HOST_PATH) differs from expected ($EXPECTED_BASE_PATH)"
fi

# Look for previous session directories
PREVIOUS_SNAPSHOT_ID=""
PREVIOUS_SNAPSHOT_PATH=""

if [ -d "$NOTEBOOK_SESSIONS_BASE_HOST_PATH" ]; then
    log "Scanning for previous session directories..."
    for D_HOST_PATH in "$NOTEBOOK_SESSIONS_BASE_HOST_PATH"/* ; do
        if [ -d "${D_HOST_PATH}/fs" ]; then
            SNAP_ID=$(basename "$D_HOST_PATH")
            if [ "$SNAP_ID" != "$MY_OWN_SNAPSHOT_ID" ]; then
                # Check if directory has content (non-empty)
                if [ "$(ls -A "${D_HOST_PATH}/fs" 2>/dev/null)" ]; then
                    log "Found non-empty previous session: $SNAP_ID"
                    PREVIOUS_SNAPSHOT_ID=$SNAP_ID
                    PREVIOUS_SNAPSHOT_PATH="${D_HOST_PATH}/fs"
                    break
                else
                    log "Found empty previous session: $SNAP_ID (skipping)"
                fi
            fi
        fi
    done
fi

# Restore from previous session if found
if [ -n "$PREVIOUS_SNAPSHOT_ID" ] && [ -n "$PREVIOUS_SNAPSHOT_PATH" ]; then
    log "=== Restoring from previous session: $PREVIOUS_SNAPSHOT_ID ==="
    log "Source: $PREVIOUS_SNAPSHOT_PATH"
    log "Target: $MY_OWN_UPPERDIR_HOST_PATH"
    
    # Copy data with fallback methods
    if command -v rsync >/dev/null 2>&1; then
        log "Using rsync for data migration..."
        rsync -avp --delete "${PREVIOUS_SNAPSHOT_PATH}/" "${MY_OWN_UPPERDIR_HOST_PATH}/" >> "$LOG_FILE" 2>&1
        COPY_EXIT_CODE=$?
    else
        log "rsync not available, using cp..."
        # Use POSIX-compliant cp approach
        cp -rp "${PREVIOUS_SNAPSHOT_PATH}/." "${MY_OWN_UPPERDIR_HOST_PATH}/" >> "$LOG_FILE" 2>&1
        COPY_EXIT_CODE=$?
    fi
    
    if [ $COPY_EXIT_CODE -eq 0 ]; then
        log "=== Data migration successful ==="
        log "Cleaning up old session directory: ${NOTEBOOK_SESSIONS_BASE_HOST_PATH}/${PREVIOUS_SNAPSHOT_ID}"
        rm -rf "${NOTEBOOK_SESSIONS_BASE_HOST_PATH}/${PREVIOUS_SNAPSHOT_ID}" >> "$LOG_FILE" 2>&1
        if [ $? -eq 0 ]; then
            log "Old session directory cleaned up successfully"
        else
            log "WARNING: Failed to clean up old session directory"
        fi
    else
        log "ERROR: Data migration failed (exit code: $COPY_EXIT_CODE). Old session data NOT cleaned up."
        log "Manual intervention may be required."
        # Don't exit with error to allow container to start
    fi
else
    log "No previous session data found. Starting with fresh session."
fi

log "=== Post-Start Hook completed ==="