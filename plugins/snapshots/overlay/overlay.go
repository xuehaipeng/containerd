//go:build linux

/*
   Copyright The containerd Authors.

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.
*/

package overlay

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"

	"github.com/containerd/containerd/v2/core/mount"
	"github.com/containerd/containerd/v2/core/snapshots"
	"github.com/containerd/containerd/v2/core/snapshots/storage"
	"github.com/containerd/containerd/v2/internal/userns"
	"github.com/containerd/containerd/v2/plugins/snapshots/overlay/overlayutils"
	"github.com/containerd/continuity/fs"
	"github.com/containerd/errdefs"
	"github.com/containerd/log"
)

// upperdirKey is a key of an optional label to each snapshot.
// This optional label of a snapshot contains the location of "upperdir" where
// the change set between this snapshot and its parent is stored.
const upperdirKey = "containerd.io/snapshot/overlay.upperdir"

// CUSTOM SNAPSHOTTER LABELS
const (
	// LabelK8sNamespace informs the snapshotter about the K8s namespace.
	LabelK8sNamespace = "containerd.io/snapshot/k8s-namespace"
	// LabelK8sPodName informs the snapshotter about the K8s pod name.
	LabelK8sPodName = "containerd.io/snapshot/k8s-pod-name"
	// LabelK8sContainerName informs the snapshotter about the K8s container name.
	LabelK8sContainerName = "containerd.io/snapshot/k8s-container-name"
	// LabelSharedDiskPath specifies the base path on shared storage.
	LabelSharedDiskPath = "containerd.io/snapshot/shared-disk-path"
	// LabelUseSharedStorage is a marker to activate this custom logic.
	LabelUseSharedStorage = "containerd.io/snapshot/use-shared-storage" // Value "true"
)

// SnapshotterConfig is used to configure the overlay snapshotter instance
type SnapshotterConfig struct {
	asyncRemove   bool
	upperdirLabel bool
	ms            MetaStore
	mountOptions  []string
	remapIDs      bool
	slowChown     bool
}

// Opt is an option to configure the overlay snapshotter
type Opt func(config *SnapshotterConfig) error

// AsynchronousRemove defers removal of filesystem content until
// the Cleanup method is called. Removals will make the snapshot
// referred to by the key unavailable and make the key immediately
// available for re-use.
func AsynchronousRemove(config *SnapshotterConfig) error {
	config.asyncRemove = true
	return nil
}

// WithUpperdirLabel adds as an optional label
// "containerd.io/snapshot/overlay.upperdir". This stores the location
// of the upperdir that contains the changeset between the labelled
// snapshot and its parent.
func WithUpperdirLabel(config *SnapshotterConfig) error {
	config.upperdirLabel = true
	return nil
}

// WithMountOptions defines the default mount options used for the overlay mount.
// NOTE: Options are not applied to bind mounts.
func WithMountOptions(options []string) Opt {
	return func(config *SnapshotterConfig) error {
		config.mountOptions = append(config.mountOptions, options...)
		return nil
	}
}

type MetaStore interface {
	TransactionContext(ctx context.Context, writable bool) (context.Context, storage.Transactor, error)
	WithTransaction(ctx context.Context, writable bool, fn storage.TransactionCallback) error
	Close() error
}

// WithMetaStore allows the MetaStore to be created outside the snapshotter
// and passed in.
func WithMetaStore(ms MetaStore) Opt {
	return func(config *SnapshotterConfig) error {
		config.ms = ms
		return nil
	}
}

func WithRemapIDs(config *SnapshotterConfig) error {
	config.remapIDs = true
	return nil
}

func WithSlowChown(config *SnapshotterConfig) error {
	config.slowChown = true
	return nil
}

// isSharedSnapshot checks labels to see if this snapshot should use shared storage.
func isSharedSnapshot(info snapshots.Info) bool {
	if val, ok := info.Labels[LabelUseSharedStorage]; ok && val == "true" {
		log.L.Debugf("isSharedSnapshot: returning true")
		return true
	}
	return false
}

// getSharedPathBase constructs the base directory on shared storage for a given snapshot.
// It requires the snapshot ID for uniqueness.
func getSharedPathBase(info snapshots.Info, id string) (string, error) {
	log.L.Debugf("getSharedPathBase: id=%s, info.Labels: %+v", id, info.Labels)
	if info.Labels == nil {
		return "", fmt.Errorf("missing labels for shared storage path construction")
	}

	sharedDiskPath, okS := info.Labels[LabelSharedDiskPath]
	kubeNamespace, okN := info.Labels[LabelK8sNamespace]
	podName, okP := info.Labels[LabelK8sPodName]
	containerName, okC := info.Labels[LabelK8sContainerName]

	if !okS || !okN || !okP || !okC {
		return "", fmt.Errorf("missing one or more required labels for shared storage path (sharedPath, namespace, podName, containerName)")
	}
	if id == "" {
		return "", fmt.Errorf("snapshot ID is required for shared storage path")
	}

	basePath := filepath.Join(sharedDiskPath, kubeNamespace, podName, containerName, id)
	log.L.Debugf("getSharedPathBase: constructed basePath: %s", basePath)
	return basePath, nil
}

type snapshotter struct {
	root          string
	ms            MetaStore
	asyncRemove   bool
	upperdirLabel bool
	options       []string
	remapIDs      bool
	slowChown     bool
}

// NewSnapshotter returns a Snapshotter which uses overlayfs. The overlayfs
// diffs are stored under the provided root. A metadata file is stored under
// the root.
func NewSnapshotter(root string, opts ...Opt) (snapshots.Snapshotter, error) {
	var config SnapshotterConfig
	for _, opt := range opts {
		if err := opt(&config); err != nil {
			return nil, err
		}
	}

	if err := os.MkdirAll(root, 0700); err != nil {
		return nil, err
	}
	supportsDType, err := fs.SupportsDType(root)
	if err != nil {
		return nil, err
	}
	if !supportsDType {
		return nil, fmt.Errorf("%s does not support d_type. If the backing filesystem is xfs, please reformat with ftype=1 to enable d_type support", root)
	}
	if config.ms == nil {
		config.ms, err = storage.NewMetaStore(filepath.Join(root, "metadata.db"))
		if err != nil {
			return nil, err
		}
	}

	if err := os.Mkdir(filepath.Join(root, "snapshots"), 0700); err != nil && !os.IsExist(err) {
		return nil, err
	}

	if !hasOption(config.mountOptions, "userxattr", false) {
		// figure out whether "userxattr" option is recognized by the kernel && needed
		userxattr, err := overlayutils.NeedsUserXAttr(root)
		if err != nil {
			log.L.WithError(err).Warnf("cannot detect whether \"userxattr\" option needs to be used, assuming to be %v", userxattr)
		}
		if userxattr {
			config.mountOptions = append(config.mountOptions, "userxattr")
		}
	}

	if !hasOption(config.mountOptions, "index", false) && supportsIndex() {
		config.mountOptions = append(config.mountOptions, "index=off")
	}

	return &snapshotter{
		root:          root,
		ms:            config.ms,
		asyncRemove:   config.asyncRemove,
		upperdirLabel: config.upperdirLabel,
		options:       config.mountOptions,
		remapIDs:      config.remapIDs,
		slowChown:     config.slowChown,
	}, nil
}

func hasOption(options []string, key string, hasValue bool) bool {
	for _, option := range options {
		if hasValue {
			if strings.HasPrefix(option, key) && len(option) > len(key) && option[len(key)] == '=' {
				return true
			}
		} else if option == key {
			return true
		}
	}
	return false
}

// Stat returns the info for an active or committed snapshot by name or
// key.
//
// Should be used for parent resolution, existence checks and to discern
// the kind of snapshot.
func (o *snapshotter) Stat(ctx context.Context, key string) (info snapshots.Info, err error) {
	var id string
	if err := o.ms.WithTransaction(ctx, false, func(ctx context.Context) error {
		id, info, _, err = storage.GetInfo(ctx, key)
		return err
	}); err != nil {
		return snapshots.Info{}, err
	}

	if o.upperdirLabel {
		if info.Labels == nil {
			info.Labels = make(map[string]string)
		}
		upperPathValue, pathErr := o.determineUpperPath(id, info)
		if pathErr != nil {
			log.G(ctx).WithError(pathErr).Warnf("Failed to determine upper path for stat label on %s, using default", id)
			upperPathValue = filepath.Join(o.root, "snapshots", id, "fs")
		}
		info.Labels[upperdirKey] = upperPathValue
	}
	return info, nil
}

func (o *snapshotter) Update(ctx context.Context, info snapshots.Info, fieldpaths ...string) (newInfo snapshots.Info, err error) {
	err = o.ms.WithTransaction(ctx, true, func(ctx context.Context) error {
		newInfo, err = storage.UpdateInfo(ctx, info, fieldpaths...)
		if err != nil {
			return err
		}

		if o.upperdirLabel {
			id, _, _, errGet := storage.GetInfo(ctx, newInfo.Name)
			if errGet != nil {
				log.G(ctx).WithError(errGet).Warnf("Failed to get ID for updated snapshot info %s during label update", newInfo.Name)
			} else {
				if newInfo.Labels == nil {
					newInfo.Labels = make(map[string]string)
				}
				upperPathValue, pathErr := o.determineUpperPath(id, newInfo)
				if pathErr != nil {
					log.G(ctx).WithError(pathErr).Warnf("Failed to determine upper path for update label on %s, using default", id)
					upperPathValue = filepath.Join(o.root, "snapshots", id, "fs")
				}
				newInfo.Labels[upperdirKey] = upperPathValue
			}
		}
		return nil
	})
	return newInfo, err
}

// Usage returns the resources taken by the snapshot identified by key.
//
// For active snapshots, this will scan the usage of the overlay "diff" (aka
// "upper") directory and may take some time.
//
// For committed snapshots, the value is returned from the metadata database.
func (o *snapshotter) Usage(ctx context.Context, key string) (_ snapshots.Usage, err error) {
	var (
		usage snapshots.Usage
		info  snapshots.Info
		id    string
	)
	if err := o.ms.WithTransaction(ctx, false, func(ctx context.Context) error {
		id, info, usage, err = storage.GetInfo(ctx, key)
		return err
	}); err != nil {
		return snapshots.Usage{}, err
	}

	if info.Kind == snapshots.KindActive {
		activeUpperPath, pathErr := o.determineUpperPath(id, info)
		if pathErr != nil {
			return snapshots.Usage{}, fmt.Errorf("failed to determine upper path for usage calculation on %s: %w", id, pathErr)
		}

		du, err := fs.DiskUsage(ctx, activeUpperPath)
		if err != nil {
			// TODO(stevvooe): Consider not reporting an error in this case.
			return snapshots.Usage{}, err
		}
		usage = snapshots.Usage(du)
	}
	return usage, nil
}

func (o *snapshotter) Prepare(ctx context.Context, key, parent string, opts ...snapshots.Opt) ([]mount.Mount, error) {
	return o.createSnapshot(ctx, snapshots.KindActive, key, parent, opts)
}

func (o *snapshotter) View(ctx context.Context, key, parent string, opts ...snapshots.Opt) ([]mount.Mount, error) {
	return o.createSnapshot(ctx, snapshots.KindView, key, parent, opts)
}

// Mounts returns the mounts for the transaction identified by key. Can be
// called on an read-write or readonly transaction.
//
// This can be used to recover mounts after calling View or Prepare.
func (o *snapshotter) Mounts(ctx context.Context, key string) (_ []mount.Mount, err error) {
	var s storage.Snapshot
	var info snapshots.Info
	if err := o.ms.WithTransaction(ctx, false, func(ctx context.Context) error {
		s, err = storage.GetSnapshot(ctx, key)
		if err != nil {
			return fmt.Errorf("failed to get active mount: %w", err)
		}

		_, info, _, err = storage.GetInfo(ctx, key)
		if err != nil {
			return fmt.Errorf("failed to get snapshot info: %w", err)
		}
		return nil
	}); err != nil {
		return nil, err
	}
	return o.mounts(s, info), nil
}

func (o *snapshotter) Commit(ctx context.Context, name, key string, opts ...snapshots.Opt) error {
	return o.ms.WithTransaction(ctx, true, func(ctx context.Context) error {
		// grab the existing id and info
		id, currentInfo, _, err := storage.GetInfo(ctx, key)
		if err != nil {
			return err
		}

		activeUpperPath, pathErr := o.determineUpperPath(id, currentInfo)
		if pathErr != nil {
			return fmt.Errorf("failed to determine upper path for commit on %s: %w", id, pathErr)
		}
		usage, err := fs.DiskUsage(ctx, activeUpperPath)
		if err != nil {
			return err
		}

		if _, err = storage.CommitActive(ctx, key, name, snapshots.Usage(usage), opts...); err != nil {
			return fmt.Errorf("failed to commit snapshot %s: %w", key, err)
		}
		return nil
	})
}

// Remove abandons the snapshot identified by key. The snapshot will
// immediately become unavailable and unrecoverable. Disk space will
// be freed up on the next call to `Cleanup`.
func (o *snapshotter) Remove(ctx context.Context, key string) (err error) {
	var (
		id                 string
		info               snapshots.Info
		isDirectoryShared  bool
		sharedPathToRemove string
	)

	// First, get info to determine if it's a shared snapshot
	// Use a non-transactional read for GetInfo first.
	if errPreInfo := o.ms.WithTransaction(ctx, false, func(ctxContext context.Context) error {
		var getErr error
		id, info, _, getErr = storage.GetInfo(ctxContext, key)
		return getErr
	}); errPreInfo != nil {
		if !errdefs.IsNotFound(errPreInfo) { // If not "not found", it's a real error
			return fmt.Errorf("failed to get snapshot info for removal of %s: %w", key, errPreInfo)
		}
		log.G(ctx).WithError(errPreInfo).Warnf("Snapshot %s not found during pre-removal info fetch, proceeding with metadata removal if any", key)
		id = "" // Ensure no shared path is derived if info fetch fails with NotFound
	}

	if id != "" && isSharedSnapshot(info) {
		base, pathErr := getSharedPathBase(info, id)
		if pathErr == nil {
			isDirectoryShared = true
			sharedPathToRemove = base // The whole base dir: /.../<snapshot_id>
		} else {
			log.G(ctx).WithError(pathErr).Warnf("Failed to determine shared path for removal of snapshot %s, shared data may be orphaned", id)
		}
	}

	// Now, the main transaction to remove from metastore
	var localDirectoriesToRemove []string
	if err = o.ms.WithTransaction(ctx, true, func(ctxContext context.Context) error {
		// Re-fetch ID and info inside transaction to ensure consistency if Remove is slow
		// and something else happens, though storage.Remove should be atomic on 'key'.
		// For simplicity, we use the 'id' and 'info' from outside if they were good.
		// If 'id' was empty (due to initial NotFound), storage.Remove will also likely fail NotFound, which is fine.
		currentIDForMetaRemove, _, metaErr := storage.Remove(ctxContext, key) // Remove from metadata
		if metaErr != nil {
			return fmt.Errorf("failed to remove snapshot %s from metastore: %w", key, metaErr)
		}
		if id == "" { // If initial GetInfo failed NotFound, use ID from Remove
			id = currentIDForMetaRemove
		}

		if !o.asyncRemove {
			localDirectoriesToRemove, err = o.getCleanupDirectories(ctxContext)
			if err != nil {
				return fmt.Errorf("unable to get local directories for removal: %w", err)
			}
		}
		return nil
	}); err != nil {
		return err // Metastore transaction failed
	}

	// Actual removal outside transaction
	// Remove local directories first
	for _, dir := range localDirectoriesToRemove {
		if errR := os.RemoveAll(dir); errR != nil {
			log.G(ctx).WithError(errR).WithField("path", dir).Warn("failed to remove local directory")
		}
	}
	// Then remove shared directory if applicable
	if isDirectoryShared && sharedPathToRemove != "" {
		log.G(ctx).Infof("Preserving shared snapshot data for potential resume. Path: %s", sharedPathToRemove)
		// NOTE: The os.RemoveAll call is intentionally commented out to preserve the state
		// on the shared storage for notebook resume scenarios. An external process will be
		// responsible for the final cleanup of this directory.
		// if errR := os.RemoveAll(sharedPathToRemove); errR != nil {
		// 	log.G(ctx).WithError(errR).WithField("path", sharedPathToRemove).Warn("failed to remove shared directory")
		// }
	}
	return nil
}

// Walk the snapshots.
func (o *snapshotter) Walk(ctx context.Context, fn snapshots.WalkFunc, fs ...string) error {
	return o.ms.WithTransaction(ctx, false, func(ctx context.Context) error {
		if o.upperdirLabel {
			return storage.WalkInfo(ctx, func(ctx context.Context, info snapshots.Info) error {
				// We need the ID to determine the correct upperPath for the label.
				// storage.WalkInfo provides info.Name, which is the key. We need the internal ID.
				// This requires another GetInfo call per walked item if ID is not on info directly.
				// Or, storage.WalkInfo should provide the ID if it's readily available.
				// For now, let's assume info.Name can be used to get the full Info object including ID.
				idForLabel, walkedInfo, _, errGet := storage.GetInfo(ctx, info.Name)
				if errGet != nil {
					log.G(ctx).WithError(errGet).Warnf("Failed to get full info for %s during Walk for label, skipping label", info.Name)
					return fn(ctx, info) // Call with original info, label might be missing/stale
				}

				if walkedInfo.Labels == nil {
					walkedInfo.Labels = make(map[string]string)
				}
				upperPathValue, pathErr := o.determineUpperPath(idForLabel, walkedInfo)
				if pathErr != nil {
					log.G(ctx).WithError(pathErr).Warnf("Failed to determine upper path for walk label on %s, using default", idForLabel)
					upperPathValue = filepath.Join(o.root, "snapshots", idForLabel, "fs") // Fallback
				}
				walkedInfo.Labels[upperdirKey] = upperPathValue
				return fn(ctx, walkedInfo) // Call with potentially modified info
			}, fs...)
		}
		return storage.WalkInfo(ctx, fn, fs...)
	})
}

// Cleanup cleans up disk resources from removed or abandoned snapshots
func (o *snapshotter) Cleanup(ctx context.Context) error {
	cleanup, err := o.cleanupDirectories(ctx)
	if err != nil {
		return err
	}

	for _, dir := range cleanup {
		if err := os.RemoveAll(dir); err != nil {
			log.G(ctx).WithError(err).WithField("path", dir).Warn("failed to remove directory")
		}
	}

	return nil
}

func (o *snapshotter) cleanupDirectories(ctx context.Context) (_ []string, err error) {
	var cleanupDirs []string
	// Get a write transaction to ensure no other write transaction can be entered
	// while the cleanup is scanning.
	if err := o.ms.WithTransaction(ctx, true, func(ctx context.Context) error {
		cleanupDirs, err = o.getCleanupDirectories(ctx)
		return err
	}); err != nil {
		return nil, err
	}
	return cleanupDirs, nil
}

func (o *snapshotter) getCleanupDirectories(ctx context.Context) ([]string, error) {
	ids, err := storage.IDMap(ctx)
	if err != nil {
		return nil, err
	}

	snapshotDir := filepath.Join(o.root, "snapshots")
	fd, err := os.Open(snapshotDir)
	if err != nil {
		return nil, err
	}
	defer fd.Close()

	dirs, err := fd.Readdirnames(0)
	if err != nil {
		return nil, err
	}

	cleanup := []string{}
	for _, d := range dirs {
		if _, ok := ids[d]; ok {
			continue
		}
		cleanup = append(cleanup, filepath.Join(snapshotDir, d))
	}

	return cleanup, nil
}

func (o *snapshotter) createSnapshot(ctx context.Context, kind snapshots.Kind, key, parent string, opts []snapshots.Opt) (_ []mount.Mount, err error) {
	var (
		s                      storage.Snapshot
		info                   snapshots.Info
		localSnapshotTempDir   string
		localSnapshotFinalPath string // For local case
	)

	defer func() {
		if err != nil {
			if localSnapshotTempDir != "" { // only if it was created and not renamed/handled
				if err1 := os.RemoveAll(localSnapshotTempDir); err1 != nil {
					log.G(ctx).WithError(err1).Warn("failed to cleanup local temp snapshot directory")
				}
			}
			// NOTE: If shared directory creation fails mid-way, an explicit cleanup
			// of partially created shared directories would be needed here or inside the transaction.
			// The current structure relies on MkdirAll and then removing the base on error later.
		}
	}()

	if err := o.ms.WithTransaction(ctx, true, func(ctx context.Context) (err error) {
		s, err = storage.CreateSnapshot(ctx, kind, key, parent, opts...)
		if err != nil {
			return fmt.Errorf("failed to create snapshot metadata: %w", err)
		}

		_, info, _, err = storage.GetInfo(ctx, key)
		if err != nil {
			return fmt.Errorf("failed to get snapshot info after creation: %w", err)
		}

		// WORKAROUND: Manually apply snapshot options to the info struct.
		// This is necessary because labels passed to CreateSnapshot via opts
		// are not reflected in the Info object returned by GetInfo within the
		// same database transaction.
		for _, opt := range opts {
			opt(&info)
		}
		log.G(ctx).Debugf("Manually applied opts to info. Final labels for snapshot %s: %+v", s.ID, info.Labels)

		// Determine mapped UID/GID for chown, common for both local and shared
		var (
			mappedUID, mappedGID     = -1, -1
			uidmapLabel, gidmapLabel string
			needsRemap               = false
		)
		if v, ok := info.Labels[snapshots.LabelSnapshotUIDMapping]; ok {
			uidmapLabel = v
			needsRemap = true
		}
		if v, ok := info.Labels[snapshots.LabelSnapshotGIDMapping]; ok {
			gidmapLabel = v
			needsRemap = true
		}
		if needsRemap {
			var idMap userns.IDMap
			if err = idMap.Unmarshal(uidmapLabel, gidmapLabel); err != nil {
				return fmt.Errorf("failed to unmarshal snapshot ID mapped labels: %w", err)
			}
			rootPair, err := idMap.RootPair()
			if err != nil {
				return fmt.Errorf("failed to find root pair: %w", err)
			}
			mappedUID, mappedGID = int(rootPair.Uid), int(rootPair.Gid)
		}
		// Fallback to parent's UID/GID if not explicitly mapped and has parents
		if (mappedUID == -1 || mappedGID == -1) && len(s.ParentIDs) > 0 {
			// Stat parent's upper to get its ownership. Assumes parent is local.
			// If parent could also be shared, this stat path needs to be determined correctly.
			// For now, assume parent is local via o.root.
			parentUpperForStat := filepath.Join(o.root, "snapshots", s.ParentIDs[0], "fs")
			st, statErr := os.Stat(parentUpperForStat)
			if statErr != nil {
				return fmt.Errorf("failed to stat parent %s for UID/GID: %w", s.ParentIDs[0], statErr)
			}
			if stat, ok := st.Sys().(*syscall.Stat_t); ok {
				mappedUID = int(stat.Uid)
				mappedGID = int(stat.Gid)
			} else {
				return fmt.Errorf("incompatible types after stat call on parent: *syscall.Stat_t expected")
			}
		}

		if isSharedSnapshot(info) && kind == snapshots.KindActive {
			sharedBase, pathErr := getSharedPathBase(info, s.ID)
			if pathErr != nil {
				return fmt.Errorf("cannot determine shared path for snapshot %s: %w", s.ID, pathErr)
			}

			targetUpperPath := filepath.Join(sharedBase, "fs")
			targetWorkPath := filepath.Join(sharedBase, "work")

			if err = os.MkdirAll(targetUpperPath, 0755); err != nil {
				return fmt.Errorf("failed to create shared upperdir %s: %w", targetUpperPath, err)
			}
			// Defer cleanup of shared upper if work creation fails
			defer func() {
				if err != nil { // if an error occurred later in the transaction or during work dir creation
					os.RemoveAll(targetUpperPath)
				}
			}()
			if err = os.MkdirAll(targetWorkPath, 0711); err != nil {
				return fmt.Errorf("failed to create shared workdir %s: %w", targetWorkPath, err)
			}
			// Defer cleanup of shared work if something else fails
			defer func() {
				if err != nil {
					os.RemoveAll(targetWorkPath)
				}
			}()

			log.G(ctx).Debugf("Created shared upperdir at %s and workdir at %s", targetUpperPath, targetWorkPath)

			if mappedUID != -1 && mappedGID != -1 {
				if err = os.Lchown(targetUpperPath, mappedUID, mappedGID); err != nil {
					return fmt.Errorf("failed to chown shared upperdir %s: %w", targetUpperPath, err)
				}
			}
			// Ensure local snapshot ID marker directory exists
			ensureLocalSnapshotIDDir := filepath.Join(o.root, "snapshots", s.ID)
			if _, errStat := os.Stat(ensureLocalSnapshotIDDir); os.IsNotExist(errStat) {
				if errMk := os.Mkdir(ensureLocalSnapshotIDDir, 0700); errMk != nil {
					log.G(ctx).WithError(errMk).Warnf("Failed to create local marker directory for shared snapshot %s", s.ID)
				}
			}
		} else { // Local snapshot logic (or KindView which is always local-like)
			localSnapshotsRootDir := filepath.Join(o.root, "snapshots")
			localSnapshotTempDir, err = o.prepareDirectory(ctx, localSnapshotsRootDir, kind)
			if err != nil {
				return fmt.Errorf("failed to create prepare local snapshot dir: %w", err)
			}
			// Chown the 'fs' subdir of the temporary local directory
			if mappedUID != -1 && mappedGID != -1 {
				if err = os.Lchown(filepath.Join(localSnapshotTempDir, "fs"), mappedUID, mappedGID); err != nil {
					// localSnapshotTempDir will be cleaned by the outer defer if this fails
					return fmt.Errorf("failed to chown local temp snapshot: %w", err)
				}
			}

			localSnapshotFinalPath = filepath.Join(localSnapshotsRootDir, s.ID)
			if err = os.Rename(localSnapshotTempDir, localSnapshotFinalPath); err != nil {
				// localSnapshotTempDir will be cleaned by the outer defer
				return fmt.Errorf("failed to rename local snapshot dir from %s to %s: %w", localSnapshotTempDir, localSnapshotFinalPath, err)
			}
			localSnapshotTempDir = "" // Mark as successfully renamed
		}
		return nil // Transaction successful
	}); err != nil {
		return nil, err
	}
	return o.mounts(s, info), nil
}

func (o *snapshotter) prepareDirectory(ctx context.Context, snapshotDir string, kind snapshots.Kind) (string, error) {
	td, err := os.MkdirTemp(snapshotDir, "new-")
	if err != nil {
		return "", fmt.Errorf("failed to create temp dir: %w", err)
	}

	if err := os.Mkdir(filepath.Join(td, "fs"), 0755); err != nil {
		return td, err
	}

	if kind == snapshots.KindActive {
		if err := os.Mkdir(filepath.Join(td, "work"), 0711); err != nil {
			return td, err
		}
	}

	return td, nil
}

func (o *snapshotter) mounts(s storage.Snapshot, info snapshots.Info) []mount.Mount {
	var options []string
	log.L.WithField("snapshotID", s.ID).WithField("kind", s.Kind).Debugf("mounts: determining mount options for snapshot")

	if o.remapIDs {
		if v, ok := info.Labels[snapshots.LabelSnapshotUIDMapping]; ok {
			options = append(options, fmt.Sprintf("uidmap=%s", v))
		}
		if v, ok := info.Labels[snapshots.LabelSnapshotGIDMapping]; ok {
			options = append(options, fmt.Sprintf("gidmap=%s", v))
		}
	}

	actualUpperPath, upperErr := o.determineUpperPath(s.ID, info)
	if upperErr != nil {
		log.L.WithError(upperErr).Errorf("Failed to determine upper path for snapshot %s, attempting fallback to local", s.ID)
		actualUpperPath = filepath.Join(o.root, "snapshots", s.ID, "fs") // Fallback
	}
	log.L.WithField("snapshotID", s.ID).Debugf("mounts: determined upperdir to be %s", actualUpperPath)

	if len(s.ParentIDs) == 0 {
		roFlag := "rw"
		if s.Kind == snapshots.KindView {
			roFlag = "ro"
		}
		return []mount.Mount{
			{
				Source: actualUpperPath,
				Type:   "bind",
				Options: append(options,
					roFlag,
					"rbind",
				),
			},
		}
	}

	if s.Kind == snapshots.KindActive {
		actualWorkPath, workErr := o.determineWorkPath(s.ID, info)
		if workErr != nil {
			log.L.WithError(workErr).Errorf("Failed to determine work path for snapshot %s, attempting fallback to local", s.ID)
			actualWorkPath = filepath.Join(o.root, "snapshots", s.ID, "work") // Fallback
		}
		log.L.WithField("snapshotID", s.ID).Debugf("mounts: determined workdir to be %s", actualWorkPath)
		options = append(options,
			fmt.Sprintf("workdir=%s", actualWorkPath),
			fmt.Sprintf("upperdir=%s", actualUpperPath),
		)
	} else if len(s.ParentIDs) == 1 && s.Kind == snapshots.KindView {
		// View of a single committed layer. Assume parent is local.
		// determineUpperPath for parent (if it had no special labels) would return local path.
		// To be fully robust, we'd need parentInfo here. For now, assume parent is local.
		parentUpperPath := filepath.Join(o.root, "snapshots", s.ParentIDs[0], "fs")
		return []mount.Mount{
			{
				Source: parentUpperPath,
				Type:   "bind",
				Options: append(options,
					"ro",
					"rbind",
				),
			},
		}
	}

	parentPaths := make([]string, len(s.ParentIDs))
	for i := range s.ParentIDs {
		// Assuming parents are always local style snapshots.
		// Their "fs" directory is their upper data.
		parentPaths[i] = filepath.Join(o.root, "snapshots", s.ParentIDs[i], "fs")
	}
	options = append(options, fmt.Sprintf("lowerdir=%s", strings.Join(parentPaths, ":")))
	options = append(options, o.options...)

	return []mount.Mount{
		{
			Type:    "overlay",
			Source:  "overlay",
			Options: options,
		},
	}
}

// determineUpperPath resolves the correct upper directory path.
func (o *snapshotter) determineUpperPath(id string, info snapshots.Info) (string, error) {
	log.L.WithField("snapshotID", id).Debug("determining upper path")
	if isSharedSnapshot(info) {
		log.L.WithField("snapshotID", id).Debug("isSharedSnapshot returned true, determining shared path")
		// For KindActive, this is the RW layer.
		// For KindCommitted or KindView, if it *was* a shared snapshot, its 'fs' is on shared storage.
		base, err := getSharedPathBase(info, id)
		if err != nil {
			return "", fmt.Errorf("failed to get shared path base for upperdir of snapshot %s: %w", id, err)
		}
		sharedPath := filepath.Join(base, "fs")
		log.L.WithField("snapshotID", id).Debugf("determined shared upper path to be %s", sharedPath)
		return sharedPath, nil
	}
	// Default local path for non-shared snapshots or if determination fails
	return filepath.Join(o.root, "snapshots", id, "fs"), nil
}

// determineWorkPath resolves the correct work directory path.
// Workdir is only relevant for KindActive.
func (o *snapshotter) determineWorkPath(id string, info snapshots.Info) (string, error) {
	log.L.WithField("snapshotID", id).Debug("determining work path")
	if isSharedSnapshot(info) { // and info.Kind == snapshots.KindActive implicitly by usage context
		log.L.WithField("snapshotID", id).Debug("isSharedSnapshot returned true, determining shared path")
		base, err := getSharedPathBase(info, id)
		if err != nil {
			return "", fmt.Errorf("failed to get shared path base for workdir of snapshot %s: %w", id, err)
		}
		sharedPath := filepath.Join(base, "work")
		log.L.WithField("snapshotID", id).Debugf("determined shared work path to be %s", sharedPath)
		return sharedPath, nil
	}
	// Default local path
	return filepath.Join(o.root, "snapshots", id, "work"), nil
}

// upperPath is the original simple version, might be used by older parts or for non-Info contexts.
// It should ideally be deprecated or made internal if all call sites can use determineUpperPath.
// For now, it represents the default local path.
func (o *snapshotter) upperPath(id string) string {
	// This function is now ambiguous if a snapshot *could* be shared.
	// It should ideally not be called directly if info is available.
	// Defaulting to local, assuming it's called for a parent or context where info isn't known.
	return filepath.Join(o.root, "snapshots", id, "fs")
}

// workPath is the original simple version. Similar ambiguity.
func (o *snapshotter) workPath(id string) string {
	return filepath.Join(o.root, "snapshots", id, "work")
}

// Close closes the snapshotter
func (o *snapshotter) Close() error {
	return o.ms.Close()
}

// supportsIndex checks whether the "index=off" option is supported by the kernel.
func supportsIndex() bool {
	if _, err := os.Stat("/sys/module/overlay/parameters/index"); err == nil {
		return true
	}
	return false
}
