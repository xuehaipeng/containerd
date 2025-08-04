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
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"sync"
	"time"

	"github.com/containerd/log"
)

const pathMappingFile = ".path-mappings.json"

// PathMapping represents a mapping between hash-based paths and original identifiers
type PathMapping struct {
	PodHash       string    `json:"pod_hash"`
	SnapshotHash  string    `json:"snapshot_hash"`
	Namespace     string    `json:"namespace"`
	PodName       string    `json:"pod_name"`
	ContainerName string    `json:"container_name"`
	SnapshotID    string    `json:"snapshot_id"`
	CreatedAt     time.Time `json:"created_at"`
	LastAccessed  time.Time `json:"last_accessed"`
}

// PathMappings holds all path mappings
type PathMappings struct {
	mu       sync.RWMutex
	Mappings map[string]*PathMapping `json:"mappings"` // key is "podHash/snapshotHash"
}

var (
	globalMappings *PathMappings
	mappingOnce    sync.Once
)

// initPathMappings initializes the global path mappings
func initPathMappings() {
	globalMappings = &PathMappings{
		Mappings: make(map[string]*PathMapping),
	}
}

// RegisterPathMapping saves a mapping between hash-based paths and original identifiers
func RegisterPathMapping(basePath, podHash, snapshotHash, namespace, podName, containerName, snapshotID string) error {
	mappingOnce.Do(initPathMappings)

	globalMappings.mu.Lock()
	defer globalMappings.mu.Unlock()

	key := fmt.Sprintf("%s/%s", podHash, snapshotHash)
	
	// Check if mapping already exists to preserve original created_at
	if existing, exists := globalMappings.Mappings[key]; exists {
		// Update existing mapping but preserve created_at
		existing.Namespace = namespace
		existing.PodName = podName
		existing.ContainerName = containerName
		existing.SnapshotID = snapshotID
		existing.LastAccessed = time.Now()
	} else {
		// Create new mapping
		globalMappings.Mappings[key] = &PathMapping{
			PodHash:       podHash,
			SnapshotHash:  snapshotHash,
			Namespace:     namespace,
			PodName:       podName,
			ContainerName: containerName,
			SnapshotID:    snapshotID,
			CreatedAt:     time.Now(),
			LastAccessed:  time.Now(),
		}
	}

	// Save to file
	return savePathMappings(basePath)
}

// savePathMappings persists the mappings to disk
func savePathMappings(basePath string) error {
	mappingFilePath := filepath.Join(basePath, pathMappingFile)

	// Ensure directory exists
	dir := filepath.Dir(mappingFilePath)
	if err := os.MkdirAll(dir, 0755); err != nil {
		return fmt.Errorf("failed to create directory for path mappings: %w", err)
	}

	// Clean up non-existent directories before saving
	if err := cleanupNonExistentMappings(basePath); err != nil {
		log.L.Warnf("Failed to cleanup non-existent mappings: %v", err)
	}

	// Sort mappings by snapshot_id in descending order for consistent ordering
	sortedMappings := createSortedMappings()

	data, err := json.MarshalIndent(sortedMappings, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal path mappings: %w", err)
	}

	// Write atomically
	tmpFile := mappingFilePath + ".tmp"
	if err := os.WriteFile(tmpFile, data, 0644); err != nil {
		return fmt.Errorf("failed to write path mappings: %w", err)
	}

	if err := os.Rename(tmpFile, mappingFilePath); err != nil {
		os.Remove(tmpFile) // Clean up on error
		return fmt.Errorf("failed to rename path mappings file: %w", err)
	}

	log.L.Debugf("Saved path mapping to %s", mappingFilePath)
	return nil
}

// LoadPathMappings loads mappings from disk
func LoadPathMappings(basePath string) error {
	mappingOnce.Do(initPathMappings)

	mappingFilePath := filepath.Join(basePath, pathMappingFile)

	data, err := os.ReadFile(mappingFilePath)
	if err != nil {
		if os.IsNotExist(err) {
			// File doesn't exist yet, that's OK
			log.L.Debugf("Path mappings file does not exist yet: %s", mappingFilePath)
			return nil
		}
		return fmt.Errorf("failed to read path mappings: %w", err)
	}

	globalMappings.mu.Lock()
	defer globalMappings.mu.Unlock()

	// Keep track of existing mappings count
	existingCount := len(globalMappings.Mappings)

	if err := json.Unmarshal(data, globalMappings); err != nil {
		return fmt.Errorf("failed to unmarshal path mappings: %w", err)
	}

	newCount := len(globalMappings.Mappings)
	log.L.Infof("Loaded path mappings from %s: %d mappings loaded (existing: %d, total: %d)", 
		mappingFilePath, newCount-existingCount, existingCount, newCount)

	return nil
}

// LookupPathMapping finds a mapping by hash-based path
func LookupPathMapping(podHash, snapshotHash string) (*PathMapping, bool) {
	mappingOnce.Do(initPathMappings)

	globalMappings.mu.RLock()
	defer globalMappings.mu.RUnlock()

	key := fmt.Sprintf("%s/%s", podHash, snapshotHash)
	mapping, ok := globalMappings.Mappings[key]
	if ok {
		// Update last accessed time
		mapping.LastAccessed = time.Now()
	}
	return mapping, ok
}

// GetAllMappings returns a copy of all mappings
func GetAllMappings() map[string]*PathMapping {
	mappingOnce.Do(initPathMappings)

	globalMappings.mu.RLock()
	defer globalMappings.mu.RUnlock()

	// Create a copy to avoid race conditions
	result := make(map[string]*PathMapping)
	for k, v := range globalMappings.Mappings {
		// Deep copy the mapping
		mappingCopy := *v
		result[k] = &mappingCopy
	}
	return result
}

// CleanupStaleMappings removes mappings older than the specified duration
func CleanupStaleMappings(basePath string, maxAge time.Duration) error {
	mappingOnce.Do(initPathMappings)

	globalMappings.mu.Lock()
	defer globalMappings.mu.Unlock()

	now := time.Now()
	removed := 0

	for key, mapping := range globalMappings.Mappings {
		if now.Sub(mapping.LastAccessed) > maxAge {
			delete(globalMappings.Mappings, key)
			removed++
		}
	}

	if removed > 0 {
		log.L.Infof("Cleaned up %d stale path mappings", removed)
		return savePathMappings(basePath)
	}

	return nil
}

// FindPreviousMappings finds all previous mappings for the same pod identity
// This can be used by containers to discover previous state directories
func FindPreviousMappings(namespace, podName, containerName string) ([]*PathMapping, error) {
	mappingOnce.Do(initPathMappings)

	globalMappings.mu.RLock()
	defer globalMappings.mu.RUnlock()

	var previousMappings []*PathMapping

	for _, mapping := range globalMappings.Mappings {
		if mapping.Namespace == namespace &&
			mapping.PodName == podName &&
			mapping.ContainerName == containerName {
			// Create a copy to avoid race conditions
			mappingCopy := *mapping
			previousMappings = append(previousMappings, &mappingCopy)
		}
	}

	// Sort by creation time (newest first)
	for i := 0; i < len(previousMappings)-1; i++ {
		for j := i + 1; j < len(previousMappings); j++ {
			if previousMappings[i].CreatedAt.Before(previousMappings[j].CreatedAt) {
				previousMappings[i], previousMappings[j] = previousMappings[j], previousMappings[i]
			}
		}
	}

	return previousMappings, nil
}

// GetPreviousStateDirectories returns paths to previous state directories for the same pod
func GetPreviousStateDirectories(basePath, namespace, podName, containerName string) ([]string, error) {
	previousMappings, err := FindPreviousMappings(namespace, podName, containerName)
	if err != nil {
		return nil, err
	}

	var directories []string
	for _, mapping := range previousMappings {
		dirPath := filepath.Join(basePath, mapping.PodHash, mapping.SnapshotHash, "fs")
		// Check if directory exists
		if _, err := os.Stat(dirPath); err == nil {
			directories = append(directories, dirPath)
		}
	}

	return directories, nil
}

// cleanupNonExistentMappings removes mappings for directories that no longer exist
func cleanupNonExistentMappings(basePath string) error {
	var keysToRemove []string

	log.L.Debugf("Starting cleanup check for %d mappings in basePath: %s", len(globalMappings.Mappings), basePath)

	for key, mapping := range globalMappings.Mappings {
		// Construct the directory path for this mapping
		dirPath := filepath.Join(basePath, mapping.PodHash, mapping.SnapshotHash)
		
		// Check if the directory exists
		if _, err := os.Stat(dirPath); os.IsNotExist(err) {
			log.L.Debugf("Directory does not exist, marking for removal: %s", dirPath)
			keysToRemove = append(keysToRemove, key)
		} else if err != nil {
			log.L.Debugf("Error checking directory %s: %v", dirPath, err)
		} else {
			log.L.Debugf("Directory exists: %s", dirPath)
		}
	}

	// Remove the mappings for non-existent directories
	removed := 0
	for _, key := range keysToRemove {
		mapping := globalMappings.Mappings[key]
		log.L.Debugf("Removing mapping for %s (snapshot_id: %s)", key, mapping.SnapshotID)
		delete(globalMappings.Mappings, key)
		removed++
	}

	if removed > 0 {
		log.L.Infof("Cleaned up %d mappings for non-existent directories (total mappings: %d -> %d)", 
			removed, removed+len(globalMappings.Mappings), len(globalMappings.Mappings))
	}

	return nil
}

// createSortedMappings creates a sorted version of the mappings for consistent JSON output
func createSortedMappings() *PathMappings {
	// Create a slice of mapping entries for sorting
	type mappingEntry struct {
		key     string
		mapping *PathMapping
	}

	var entries []mappingEntry
	for key, mapping := range globalMappings.Mappings {
		entries = append(entries, mappingEntry{key: key, mapping: mapping})
	}

	// Sort by snapshot_id in descending order (newest first)
	sort.Slice(entries, func(i, j int) bool {
		idI, errI := strconv.ParseInt(entries[i].mapping.SnapshotID, 10, 64)
		idJ, errJ := strconv.ParseInt(entries[j].mapping.SnapshotID, 10, 64)
		
		// If parsing fails, fallback to string comparison
		if errI != nil || errJ != nil {
			return entries[i].mapping.SnapshotID > entries[j].mapping.SnapshotID
		}
		
		return idI > idJ
	})

	// Create sorted mappings structure
	sortedMappings := &PathMappings{
		Mappings: make(map[string]*PathMapping),
	}

	for _, entry := range entries {
		sortedMappings.Mappings[entry.key] = entry.mapping
	}

	return sortedMappings
}
