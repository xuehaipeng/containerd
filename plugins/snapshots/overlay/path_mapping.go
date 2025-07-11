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
	
	data, err := json.MarshalIndent(globalMappings, "", "  ")
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
			return nil
		}
		return fmt.Errorf("failed to read path mappings: %w", err)
	}
	
	globalMappings.mu.Lock()
	defer globalMappings.mu.Unlock()
	
	if err := json.Unmarshal(data, globalMappings); err != nil {
		return fmt.Errorf("failed to unmarshal path mappings: %w", err)
	}
	
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