#!/bin/bash
# Session restore script for postStart hook
# Restores container session data from shared backup storage to local session storage

set -euo pipefail

# Default values
PATH_MAPPINGS_FILE="/etc/path-mappings.json"
LOCAL_SESSIONS_PATH="/etc/sessions"  # Mounted from /shared/nb
BACKUP_STORAGE_PATH="/etc/backup"    # Mounted from /tecofs/nb-sessions/<namespace>/<pod_name>/<container_name>
NAMESPACE="${CURRENT_NAMESPACE:-${NAMESPACE:-default}}"
POD_NAME="${HOSTNAME:-${POD_NAME:-nb-test-0}}"
CONTAINER_NAME="${CURRENT_CONTAINER_NAME:-${CONTAINER_NAME:-inference}}"
TIMEOUT="${TIMEOUT:-300}"
LOG_FILE="/tmp/session-restore.log"
DRY_RUN=false

# Logging function
log() {
    echo "$(date '+%Y-%m-%d %H:%M:%S') $1" | tee -a "$LOG_FILE"
}

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --mappings-file)
            PATH_MAPPINGS_FILE="$2"
            shift 2
            ;;
        --local-sessions-path)
            LOCAL_SESSIONS_PATH="$2"
            shift 2
            ;;
        --backup-storage-path)
            BACKUP_STORAGE_PATH="$2"
            shift 2
            ;;
        --namespace)
            NAMESPACE="$2"
            shift 2
            ;;
        --pod-name)
            POD_NAME="$2"
            shift 2
            ;;
        --container-name)
            CONTAINER_NAME="$2"
            shift 2
            ;;
        --timeout)
            TIMEOUT="$2"
            shift 2
            ;;
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        *)
            log "Unknown option: $1"
            exit 1
            ;;
    esac
done

log "=== Session Restore Started ==="
log "Path mappings file: $PATH_MAPPINGS_FILE"
log "Local sessions path: $LOCAL_SESSIONS_PATH"
log "Backup storage path: $BACKUP_STORAGE_PATH"
log "Namespace: $NAMESPACE"
log "Pod name: $POD_NAME"
log "Container name: $CONTAINER_NAME"
log "Timeout: $TIMEOUT seconds"
log "Dry run: $DRY_RUN"

# Validate required parameters
if [[ -z "$NAMESPACE" ]] || [[ -z "$POD_NAME" ]] || [[ -z "$CONTAINER_NAME" ]]; then
    log "ERROR: Missing required parameters (namespace, pod-name, container-name)"
    exit 1
fi

# Check if jq is available for JSON parsing
if ! command -v jq >/dev/null 2>&1; then
    log "ERROR: jq is required for JSON parsing but not found"
    exit 1
fi

# Check if path mappings file exists
if [[ ! -f "$PATH_MAPPINGS_FILE" ]]; then
    log "WARNING: Path mappings file not found: $PATH_MAPPINGS_FILE"
    log "=== Session Restore Completed (No Path Mappings) ==="
    exit 0
fi

# Parse path mappings to find current session (newest by created_at)
log "Parsing path mappings file to find current session..."
SESSION_INFO=$(jq -r --arg ns "$NAMESPACE" --arg pod "$POD_NAME" --arg container "$CONTAINER_NAME" '
    .mappings | to_entries[] | 
    select(.value.namespace == $ns and .value.pod_name == $pod and .value.container_name == $container) | 
    .key + " " + .value.created_at' "$PATH_MAPPINGS_FILE" 2>/dev/null | sort -k2 -r | head -n 1)

if [[ -z "$SESSION_INFO" ]]; then
    log "WARNING: No session found in path mappings for namespace=$NAMESPACE, pod=$POD_NAME, container=$CONTAINER_NAME"
    log "=== Session Restore Completed (No Session Found) ==="
    exit 0
fi

# Extract session key (pod_hash/snapshot_hash)
SESSION_KEY=$(echo "$SESSION_INFO" | cut -d' ' -f1)
POD_HASH=$(echo "$SESSION_KEY" | cut -d'/' -f1)
SNAPSHOT_HASH=$(echo "$SESSION_KEY" | cut -d'/' -f2)

log "Found current session: pod_hash=$POD_HASH, snapshot_hash=$SNAPSHOT_HASH"

# Construct CURRENT SESSION directory (this is what we restore TO)
CURRENT_SESSION_DIR="$LOCAL_SESSIONS_PATH/$POD_HASH/$SNAPSHOT_HASH/fs"

log "Current session directory: $CURRENT_SESSION_DIR"
log "Backup storage directory: $BACKUP_STORAGE_PATH"

# Validate backup storage directory exists and has content
if [[ ! -d "$BACKUP_STORAGE_PATH" ]]; then
    log "WARNING: Backup storage directory does not exist: $BACKUP_STORAGE_PATH"
    log "=== Session Restore Completed (No Backup Data) ==="
    exit 0
fi

if [[ -z "$(ls -A "$BACKUP_STORAGE_PATH" 2>/dev/null)" ]]; then
    log "WARNING: Backup storage directory is empty: $BACKUP_STORAGE_PATH"
    log "=== Session Restore Completed (Empty Backup Data) ==="
    exit 0
fi

# Ensure current session directory exists
if [[ "$DRY_RUN" == false ]]; then
    mkdir -p "$CURRENT_SESSION_DIR"
    if [[ $? -ne 0 ]]; then
        log "ERROR: Failed to create current session directory: $CURRENT_SESSION_DIR"
        exit 1
    fi
else
    log "DRY RUN: Would create current session directory: $CURRENT_SESSION_DIR"
fi

# Debug: Show current session directory status before restore
if [[ -d "$CURRENT_SESSION_DIR" ]]; then
    log "Debug: Current session directory exists"
    log "Debug: Current session directory contents before restore:"
    ls -la "$CURRENT_SESSION_DIR" 2>&1 | tee -a "$LOG_FILE" || true
else
    log "Debug: Current session directory does not exist yet"
fi

# Debug: Show backup storage directory contents before restore
if [[ -d "$BACKUP_STORAGE_PATH" ]]; then
    log "Debug: Backup storage directory contents before restore:"
    ls -la "$BACKUP_STORAGE_PATH" 2>&1 | tee -a "$LOG_FILE" || true
else
    log "Debug: Backup storage directory does not exist"
fi

# Copy session data from backup storage with timeout
log "Starting restore of session data from $BACKUP_STORAGE_PATH to $CURRENT_SESSION_DIR..."

if [[ "$DRY_RUN" == false ]]; then
    if command -v timeout >/dev/null 2>&1 && command -v rsync >/dev/null 2>&1; then
        # Use rsync with options to handle all file types including hidden files, large files, and empty directories
        # --recursive: recurse into directories
        # --links: copy symlinks as symlinks
        # --perms: preserve permissions
        # --times: preserve times
        # --group: preserve group
        # --owner: preserve owner (super-user only)
        # --devices: preserve device files (super-user only)
        # --specials: preserve special files
        # --hard-links: preserve hard links
        # --delete: delete extraneous files from dest dirs
        # --ignore-errors: continue even if I/O errors occur (skip problematic files)
        # --force: force deletion of non-writable files
        timeout "$TIMEOUT" rsync -a --delete --ignore-errors --force "$BACKUP_STORAGE_PATH/" "$CURRENT_SESSION_DIR/" 2>&1 | tee -a "$LOG_FILE"
        RESULT=${PIPESTATUS[0]}
    elif command -v rsync >/dev/null 2>&1; then
        rsync -a --delete --ignore-errors --force "$BACKUP_STORAGE_PATH/" "$CURRENT_SESSION_DIR/" 2>&1 | tee -a "$LOG_FILE"
        RESULT=${PIPESTATUS[0]}
    else
        # Fallback to tar if rsync is not available
        log "Rsync not available, using tar for restore"
        # Use tar with options to properly handle all files including hidden ones
        # --ignore-failed-read: ignore failed reads (skip problematic files)
        # --warning=no-file-changed: suppress warnings for changed files
        # --warning=no-ignore-failed-read: suppress warnings for ignored failed reads
        # -p: preserve permissions
        # --xattrs: preserve extended attributes
        # --overwrite: overwrite existing files
        # --exclude: exclude any temporary tar files
        # -c: create archive
        # -f -: write to stdout
        # .: current directory (all files)
        (cd "$BACKUP_STORAGE_PATH" && tar -cpf - --exclude=".*.tar" --ignore-failed-read --warning=no-file-changed --warning=no-ignore-failed-read -p --xattrs .) | (cd "$CURRENT_SESSION_DIR" && tar -xpf - --overwrite -p --xattrs) 2>&1 | tee -a "$LOG_FILE"
        RESULT=${PIPESTATUS[0]}
    fi
else
    log "DRY RUN: Would copy data from $BACKUP_STORAGE_PATH to $CURRENT_SESSION_DIR"
    RESULT=0
fi

if [[ $RESULT -eq 0 ]]; then
    log "Session restore completed successfully"
elif [[ $RESULT -eq 124 ]]; then
    log "ERROR: Session restore timed out after $TIMEOUT seconds"
    exit 1
else
    log "WARNING: Session restore completed with some errors (exit code: $RESULT)"
    # Don't exit with error for partial success - some files might be read-only or busy
fi

# Debug: Show current session directory contents after restore
if [[ -d "$CURRENT_SESSION_DIR" ]]; then
    log "Debug: Current session directory contents after restore:"
    ls -la "$CURRENT_SESSION_DIR" 2>&1 | tee -a "$LOG_FILE" || true
    log "Debug: Session root directory contents after restore:"
    ls -la "$CURRENT_SESSION_DIR/root/" 2>&1 | tee -a "$LOG_FILE" || true
else
    log "Debug: Current session directory still does not exist after restore"
fi

log "=== Session Restore Completed ==="
exit 0