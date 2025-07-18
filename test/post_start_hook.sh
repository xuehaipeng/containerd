#!/bin/sh
set -x # Enable command tracing for debugging

# Post-Start Hook Script for Hash-Based Shared Snapshot Session Management
# This script uses the path mappings file to discover current session info

LOG_FILE="/tmp/poststart.log"
LOCK_FILE="/tmp/poststart.lock"
PATH_MAPPINGS_FILE="/etc/path-mappings.json"

# Function to log messages
log() {
    echo "$1" >> "$LOG_FILE" 2>&1
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
echo "POST-START HOOK EXECUTED at $(date)" > "$LOG_FILE" 2>&1
log "=== Post-Start Hook: Hash-Based Shared Storage Session Management ==="
log "Using shell: sh (POSIX-compliant)"

# Also try to write to stdout/stderr
echo "POST-START HOOK RUNNING" >&1
echo "POST-START HOOK RUNNING" >&2

# Get current pod information from environment or defaults
CURRENT_NAMESPACE="${CURRENT_NAMESPACE:-default}"
CURRENT_POD_NAME="${HOSTNAME:-nb-test-0}"
CURRENT_CONTAINER_NAME="${CURRENT_CONTAINER_NAME:-inference}"

log "Current pod info: namespace=$CURRENT_NAMESPACE, pod=$CURRENT_POD_NAME, container=$CURRENT_CONTAINER_NAME"

# Read path mappings file to find our current session
if [ ! -f "$PATH_MAPPINGS_FILE" ]; then
    log "ERROR: Path mappings file not found: $PATH_MAPPINGS_FILE"
    log "Starting with fresh session."
    exit 0
fi

log "Reading path mappings from: $PATH_MAPPINGS_FILE"

# Extract our current session info using a more direct approach
CURRENT_MAPPING=""
if [ -f "$PATH_MAPPINGS_FILE" ]; then
    # Use a simple script to find the most recent matching entry
    TEMP_SCRIPT="/tmp/parse_mappings_$$.sh"
    
    cat > "$TEMP_SCRIPT" << 'SCRIPT_EOF'
#!/bin/bash
NAMESPACE="$1"
POD_NAME="$2"
CONTAINER_NAME="$3"
MAPPINGS_FILE="$4"

LATEST_TIME=""
LATEST_PATH=""

# Read the file and look for matching entries
while IFS= read -r line; do
    if echo "$line" | grep -q "\"[^\"]*\/[^\"]*\": {"; then
        # Extract the path key
        PATH_KEY=$(echo "$line" | sed 's/.*"\([^"]*\/[^"]*\)": {.*/\1/')
        
        # Read the next several lines to get the entry data
        ENTRY_DATA=""
        for i in $(seq 1 10); do
            read -r next_line || break
            ENTRY_DATA="$ENTRY_DATA$next_line"
            if echo "$next_line" | grep -q "^    }"; then
                break
            fi
        done
        
        # Check if this entry matches our criteria
        if echo "$ENTRY_DATA" | grep -q "\"namespace\": \"$NAMESPACE\"" && \
           echo "$ENTRY_DATA" | grep -q "\"pod_name\": \"$POD_NAME\"" && \
           echo "$ENTRY_DATA" | grep -q "\"container_name\": \"$CONTAINER_NAME\""; then
            
            # Extract the created_at time
            CREATED_AT=$(echo "$ENTRY_DATA" | grep "\"created_at\":" | sed 's/.*"created_at": "\([^"]*\)".*/\1/')
            
            # Check if this is the most recent
            if [ "$CREATED_AT" \> "$LATEST_TIME" ] || [ -z "$LATEST_TIME" ]; then
                LATEST_TIME="$CREATED_AT"
                LATEST_PATH="$PATH_KEY"
            fi
        fi
    fi
done < "$MAPPINGS_FILE"

if [ -n "$LATEST_PATH" ]; then
    POD_HASH=$(echo "$LATEST_PATH" | cut -d'/' -f1)
    SNAPSHOT_HASH=$(echo "$LATEST_PATH" | cut -d'/' -f2)
    echo "$POD_HASH:$SNAPSHOT_HASH:$LATEST_PATH"
fi
SCRIPT_EOF

    chmod +x "$TEMP_SCRIPT"
    CURRENT_MAPPING=$("$TEMP_SCRIPT" "$CURRENT_NAMESPACE" "$CURRENT_POD_NAME" "$CURRENT_CONTAINER_NAME" "$PATH_MAPPINGS_FILE")
    rm -f "$TEMP_SCRIPT"
    
    if [ -n "$CURRENT_MAPPING" ]; then
        log "Successfully parsed current mapping: $CURRENT_MAPPING"
    else
        log "ERROR: Could not find matching entry in mappings file"
        log "Looking for: namespace=$CURRENT_NAMESPACE, pod=$CURRENT_POD_NAME, container=$CURRENT_CONTAINER_NAME"
    fi
fi

if [ -z "$CURRENT_MAPPING" ]; then
    log "ERROR: Could not find current session in path mappings file"
    log "Starting with fresh session."
    exit 0
fi

# Parse the result
MY_POD_HASH=$(echo "$CURRENT_MAPPING" | cut -d: -f1)
MY_OWN_SNAPSHOT_HASH=$(echo "$CURRENT_MAPPING" | cut -d: -f2)
CURRENT_PATH=$(echo "$CURRENT_MAPPING" | cut -d: -f3)

log "Found current session: pod_hash=$MY_POD_HASH, snapshot_hash=$MY_OWN_SNAPSHOT_HASH, path=$CURRENT_PATH"

# The base path for all sessions, as seen INSIDE the container
CONTAINER_SESSIONS_PATH="/etc/sessions"
log "Base sessions path (inside container): $CONTAINER_SESSIONS_PATH"

# Look for previous session directories in the same pod hash directory
PREVIOUS_SNAPSHOT_HASH=""
PREVIOUS_SNAPSHOT_PATH=""

if [ -d "$CONTAINER_SESSIONS_PATH/$MY_POD_HASH" ]; then
    log "Scanning for previous sessions in pod hash directory: $MY_POD_HASH"
    log "Current session hash from path mappings: $MY_OWN_SNAPSHOT_HASH"
    
    # List all available sessions for debugging
    log "Available session directories:"
    for DEBUG_DIR in "$CONTAINER_SESSIONS_PATH/$MY_POD_HASH"/* ; do
        if [ -d "$DEBUG_DIR" ]; then
            DEBUG_HASH=$(basename "$DEBUG_DIR")
            DEBUG_TIME=$(stat -c %Y "$DEBUG_DIR" 2>/dev/null || echo 0)
            log "  $DEBUG_HASH (mod time: $DEBUG_TIME)"
        fi
    done
    
    # Look for the most recent non-current session in our pod hash directory
    LATEST_TIME=0
    for SNAPSHOT_DIR in "$CONTAINER_SESSIONS_PATH/$MY_POD_HASH"/* ; do
        if [ -d "${SNAPSHOT_DIR}/fs" ]; then
            SNAPSHOT_HASH=$(basename "$SNAPSHOT_DIR")
            if [ "$SNAPSHOT_HASH" != "$MY_OWN_SNAPSHOT_HASH" ]; then
                # Check if directory has content
                if [ -n "$(find "${SNAPSHOT_DIR}/fs" -mindepth 1 -print -quit 2>/dev/null)" ]; then
                    # Get the modification time of the directory to find the most recent
                    DIR_TIME=$(stat -c %Y "$SNAPSHOT_DIR" 2>/dev/null || echo 0)
                    log "Evaluating candidate session: $SNAPSHOT_HASH (mod time: $DIR_TIME) vs current latest: $LATEST_TIME"
                    if [ "$DIR_TIME" -gt "$LATEST_TIME" ]; then
                        log "Found candidate session: $SNAPSHOT_HASH (mod time: $DIR_TIME)"
                        PREVIOUS_SNAPSHOT_HASH=$SNAPSHOT_HASH
                        PREVIOUS_SNAPSHOT_PATH="${SNAPSHOT_DIR}/fs"
                        LATEST_TIME=$DIR_TIME
                    fi
                fi
            else
                log "Skipping current session: $SNAPSHOT_HASH"
            fi
        fi
    done
    
    if [ -n "$PREVIOUS_SNAPSHOT_HASH" ]; then
        log "Selected most recent previous session: $PREVIOUS_SNAPSHOT_HASH"
        log "Previous session path: $PREVIOUS_SNAPSHOT_PATH"
    else
        log "No previous session found"
    fi
else
    log "No pod hash directory found: $CONTAINER_SESSIONS_PATH/$MY_POD_HASH"
fi

# Restore from previous session if found
if [ -n "$PREVIOUS_SNAPSHOT_HASH" ] && [ -n "$PREVIOUS_SNAPSHOT_PATH" ]; then
    log "=== Restoring from previous session: $PREVIOUS_SNAPSHOT_HASH ==="
    log "Source (container path): $PREVIOUS_SNAPSHOT_PATH"
    log "Target: / (container root)"
    
    # Count successful and failed operations
    COPY_SUCCESS_COUNT=0
    COPY_FAIL_COUNT=0
    COPY_SKIP_COUNT=0
    
    # Copy data with error-tolerant approach
    if command -v rsync >/dev/null 2>&1; then
        log "Using rsync for data migration with error tolerance... (timeout 5m)"
        # Use rsync with options to continue on errors and preserve what can be preserved
        # --no-times: Don't try to preserve modification times (avoids "preserving times" errors on read-only filesystems)
        # --ignore-errors: Continue on errors instead of stopping
        # --partial: Keep partially transferred files
        # --no-perms: Don't try to preserve permissions that might cause issues
        timeout 300 rsync -av --delete --ignore-errors --partial --no-times --no-perms "${PREVIOUS_SNAPSHOT_PATH}/" "/" 2>&1 | \
        while IFS= read -r line; do
            echo "$line" >> "$LOG_FILE"
            # Count different types of results
            if echo "$line" | grep -q "sent\|received\|total size"; then
                # Summary line, parse for success
                true
            elif echo "$line" | grep -qE "(failed|error|cannot|permission denied)" >/dev/null 2>&1; then
                COPY_FAIL_COUNT=$((COPY_FAIL_COUNT + 1))
            elif echo "$line" | grep -qE "(skipping|ignoring)" >/dev/null 2>&1; then
                COPY_SKIP_COUNT=$((COPY_SKIP_COUNT + 1))
            elif echo "$line" | grep -qE "^[^/]*/" >/dev/null 2>&1; then
                # Looks like a successful copy (filename pattern)
                COPY_SUCCESS_COUNT=$((COPY_SUCCESS_COUNT + 1))
            fi
        done
        COPY_EXIT_CODE=$?
    else
        log "rsync not available, using error-tolerant cp approach... (timeout 5m)"
        
        # Create a custom copy function that continues on errors
        TEMP_COPY_SCRIPT="/tmp/copy_with_tolerance_$$.sh"
        cat > "$TEMP_COPY_SCRIPT" << 'COPY_SCRIPT_EOF'
#!/bin/bash
SOURCE="$1"
TARGET="$2"
LOG_FILE="$3"

copy_item() {
    local src="$1"
    local dst="$2"
    local rel_path="$3"
    
    if [ -d "$src" ]; then
        # Create directory if it doesn't exist
        if [ ! -d "$dst" ]; then
            if mkdir -p "$dst" 2>/dev/null; then
                echo "Created directory: $rel_path" >> "$LOG_FILE"
                return 0
            else
                echo "ERROR: Cannot create directory: $rel_path" >> "$LOG_FILE"
                return 1
            fi
        fi
        return 0
    elif [ -f "$src" ]; then
        # Copy file, skip if target is read-only or cannot be written
        if cp "$src" "$dst" 2>/dev/null; then
            echo "Copied file: $rel_path" >> "$LOG_FILE"
            return 0
        else
            echo "SKIP: Cannot copy file (read-only or permission denied): $rel_path" >> "$LOG_FILE"
            return 2
        fi
    elif [ -L "$src" ]; then
        # Copy symlink
        if cp -P "$src" "$dst" 2>/dev/null; then
            echo "Copied symlink: $rel_path" >> "$LOG_FILE"
            return 0
        else
            echo "SKIP: Cannot copy symlink: $rel_path" >> "$LOG_FILE"
            return 2
        fi
    else
        echo "SKIP: Unknown file type: $rel_path" >> "$LOG_FILE"
        return 2
    fi
}

# Walk through source directory
find "$SOURCE" -type f -o -type d -o -type l | while IFS= read -r item; do
    # Calculate relative path
    REL_PATH="${item#$SOURCE}"
    REL_PATH="${REL_PATH#/}"
    
    # Skip empty relative path (source directory itself)
    [ -z "$REL_PATH" ] && continue
    
    # Calculate target path
    TARGET_ITEM="$TARGET/$REL_PATH"
    
    # Copy the item
    copy_item "$item" "$TARGET_ITEM" "$REL_PATH"
    case $? in
        0) echo "SUCCESS" ;;
        1) echo "FAIL" ;;
        2) echo "SKIP" ;;
    esac
done | sort | uniq -c | while read count status; do
    case "$status" in
        "SUCCESS") echo "Successful operations: $count" >> "$LOG_FILE" ;;
        "FAIL") echo "Failed operations: $count" >> "$LOG_FILE" ;;
        "SKIP") echo "Skipped operations: $count" >> "$LOG_FILE" ;;
    esac
done
COPY_SCRIPT_EOF
        
        chmod +x "$TEMP_COPY_SCRIPT"
        timeout 300 "$TEMP_COPY_SCRIPT" "${PREVIOUS_SNAPSHOT_PATH}" "/" "$LOG_FILE"
        COPY_EXIT_CODE=$?
        rm -f "$TEMP_COPY_SCRIPT"
    fi
    
    # Evaluate the migration result
    if [ $COPY_EXIT_CODE -eq 0 ]; then
        log "=== Data migration completed successfully ==="
    elif [ $COPY_EXIT_CODE -eq 124 ]; then
        log "WARNING: Data migration timed out after 5 minutes"
        log "Some files may not have been restored. Continuing with available data."
    else
        log "WARNING: Data migration completed with some errors (exit code: $COPY_EXIT_CODE)"
        log "Some files could not be copied (likely read-only or permission issues)."
        log "This is normal for certain system directories. Continuing with available data."
    fi
    
    log "=== Migration Summary ==="
    log "Data restoration attempted from previous session: $PREVIOUS_SNAPSHOT_HASH"
    log "Files that could not be copied will remain unchanged from the base image."
    log "Administrators can manually handle any critical files if needed."
    
else
    log "No previous session found to restore from. Starting with fresh session."
fi

log "=== Cleanup: Removing old sessions (keeping current and most recent previous) ==="
CLEANUP_COUNT=0

# CRITICAL FIX: Find the actual current session directory
# The current session might have just been created and not yet in path mappings
ACTUAL_CURRENT_SNAPSHOT_HASH=""
if [ -d "$CONTAINER_SESSIONS_PATH/$MY_POD_HASH" ]; then
    log "Finding actual current session directory in $CONTAINER_SESSIONS_PATH/$MY_POD_HASH"
    LATEST_DIR_TIME=0
    for SNAPSHOT_DIR in "$CONTAINER_SESSIONS_PATH/$MY_POD_HASH"/* ; do
        if [ -d "$SNAPSHOT_DIR" ]; then
            SNAPSHOT_HASH=$(basename "$SNAPSHOT_DIR")
            # Find the most recently modified directory (likely the current session)
            DIR_TIME=$(stat -c %Y "$SNAPSHOT_DIR" 2>/dev/null || echo 0)
            log "Found session directory: $SNAPSHOT_HASH (mod time: $DIR_TIME)"
            if [ "$DIR_TIME" -gt "$LATEST_DIR_TIME" ]; then
                LATEST_DIR_TIME=$DIR_TIME
                ACTUAL_CURRENT_SNAPSHOT_HASH=$SNAPSHOT_HASH
            fi
        fi
    done
    log "Identified actual current session: $ACTUAL_CURRENT_SNAPSHOT_HASH"
fi

# Use the actual current session hash if found, otherwise fall back to path mappings
if [ -n "$ACTUAL_CURRENT_SNAPSHOT_HASH" ]; then
    MY_OWN_SNAPSHOT_HASH=$ACTUAL_CURRENT_SNAPSHOT_HASH
    log "Using actual current session hash: $MY_OWN_SNAPSHOT_HASH"
else
    log "Using path mappings session hash: $MY_OWN_SNAPSHOT_HASH"
fi

if [ -d "$CONTAINER_SESSIONS_PATH/$MY_POD_HASH" ]; then
    for SNAPSHOT_DIR in "$CONTAINER_SESSIONS_PATH/$MY_POD_HASH"/* ; do
        if [ -d "$SNAPSHOT_DIR" ]; then
            SNAPSHOT_HASH=$(basename "$SNAPSHOT_DIR")
            # Keep current session and the one we just restored from
            if [ "$SNAPSHOT_HASH" != "$MY_OWN_SNAPSHOT_HASH" ] && [ "$SNAPSHOT_HASH" != "$PREVIOUS_SNAPSHOT_HASH" ]; then
                # SAFETY CHECK: Don't delete directories created within the last 5 minutes (300 seconds)
                DIR_TIME=$(stat -c %Y "$SNAPSHOT_DIR" 2>/dev/null || echo 0)
                CURRENT_TIME=$(date +%s)
                TIME_DIFF=$((CURRENT_TIME - DIR_TIME))
                
                if [ "$TIME_DIFF" -lt 300 ]; then
                    log "SAFETY: Keeping recently created session directory: $SNAPSHOT_DIR (created $TIME_DIFF seconds ago)"
                else
                    log "Removing old session directory: $SNAPSHOT_DIR (created $TIME_DIFF seconds ago, timeout 5m)"
                    timeout 300 rm -rf "$SNAPSHOT_DIR" >> "$LOG_FILE" 2>&1
                    RM_EXIT_CODE=$?
                    if [ $RM_EXIT_CODE -ne 0 ]; then
                        log "WARNING: 'rm -rf $SNAPSHOT_DIR' finished with exit code $RM_EXIT_CODE. It may have timed out or failed."
                    fi
                    CLEANUP_COUNT=$((CLEANUP_COUNT + 1))
                fi
            else
                log "Keeping session directory: $SNAPSHOT_DIR (current or previous)"
            fi
        fi
    done
fi

# Also clean up any legacy structure that might exist
if [ -d "$CONTAINER_SESSIONS_PATH/default" ]; then
    log "Cleaning up legacy session structure..."
    for POD_DIR in "$CONTAINER_SESSIONS_PATH/default"/* ; do
        if [ -d "$POD_DIR" ]; then
            POD_NAME=$(basename "$POD_DIR")
            log "Removing legacy session directory: $POD_DIR (timeout 5m)"
            timeout 300 rm -rf "$POD_DIR" >> "$LOG_FILE" 2>&1
            RM_EXIT_CODE=$?
            if [ $RM_EXIT_CODE -ne 0 ]; then
                log "WARNING: 'rm -rf $POD_DIR' finished with exit code $RM_EXIT_CODE. It may have timed out or failed."
            fi
            CLEANUP_COUNT=$((CLEANUP_COUNT + 1))
        fi
    done
fi

log "Cleanup complete. Removed $CLEANUP_COUNT old session director(y/ies)."
log "=== Post-Start Hook completed ==="