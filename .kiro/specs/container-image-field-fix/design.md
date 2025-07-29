# Design Document

## Overview

This design addresses a critical issue in a customized containerd implementation where some containers show "-" for the image field when using `ctr containers list`, while others correctly display their image reference. The root cause has been identified as an inconsistency in container metadata population during container creation, specifically affecting containers created via Kubernetes/CRI with shared snapshots enabled.

## Architecture

### Current System Architecture

The containerd system has two primary container creation paths:

1. **Direct `ctr` tool path**: Uses containerd client API directly
2. **Kubernetes/CRI path**: Uses CRI plugin which creates containers through the containerd client

Both paths should populate container metadata consistently, but the customized shared storage snapshotter implementation has introduced a divergence in metadata handling.

### Container Metadata Flow

```mermaid
graph TD
    A[Container Creation Request] --> B{Creation Path}
    B -->|ctr tool| C[Direct containerd client]
    B -->|Kubernetes/CRI| D[CRI Plugin]
    
    C --> E[Container.Info.Image populated]
    D --> F[Container.ImageRef populated]
    
    E --> G[ctr containers list]
    F --> H[CRI ListContainers]
    
    G --> I[Shows image correctly]
    H --> J{ImageRef populated?}
    J -->|Yes| K[Shows image correctly]
    J -->|No| L[Shows "-"]
```

### Root Cause Analysis

The real issue is that the CRI implementation is NOT setting the core containerd container's `Image` field during container creation. There are two separate image fields:

1. **Core containerd container `Image` field**: Used by `ctr` command (from `core/containers/containers.go`)
2. **CRI metadata `ImageRef` field**: Used by CRI ListContainers (from `internal/cri/store/container/metadata.go`)

The CRI implementation populates its own `ImageRef` field at line 258:
```go
r.meta.ImageRef = r.imageID
```

But it fails to set the core containerd container's `Image` field using the available options:
- `containerd.WithImage(i Image)` 
- `containerd.WithImageName(n string)`

This causes the discrepancy where:
- `ctr containers list` shows "-" because `info.Image` is empty
- CRI-created containers have proper `ImageRef` in their CRI metadata
- Containers created via other methods (like `ctr create`) properly set both fields

## Components and Interfaces

### 1. Container Metadata Structure

**Location**: `internal/cri/store/container/metadata.go`

```go
type Metadata struct {
    ID string
    Name string
    SandboxID string
    Config *runtime.ContainerConfig
    ImageRef string  // ← This field is inconsistently populated
    LogPath string
    StopSignal string
    ProcessLabel string
}
```

### 2. Container Creation Flow

**Location**: `internal/cri/server/container_create.go`

The `createContainer` function contains the critical metadata population logic:

```go
func (c *criService) createContainer(r *createContainerRequest) (_ string, retErr error) {
    // ... snapshot preparation logic ...
    
    // CRITICAL: This assignment must happen for all creation paths
    r.meta.ImageRef = r.imageID  // Line 444
    r.meta.StopSignal = r.imageConfig.StopSignal
    
    // ... rest of container creation ...
}
```

### 3. Container Listing Implementation

**Location**: `internal/cri/server/container_list.go`

The `toCRIContainer` function relies on the populated `ImageRef` field:

```go
func toCRIContainer(container containerstore.Container) *runtime.Container {
    return &runtime.Container{
        // ... other fields ...
        ImageRef: container.ImageRef,  // ← Depends on consistent population
        // ... other fields ...
    }
}
```

### 4. Shared Snapshot Integration Points

**Location**: `internal/cri/server/container_create.go` (lines 350-420)

The shared snapshot logic includes:
- Regex-based filtering for namespaces and pod names
- Custom label application for shared storage
- Snapshot preparation with custom options

## Data Models

### Container Creation Request Structure

```go
type createContainerRequest struct {
    ctx                   context.Context
    containerID           string
    sandbox               *sandbox.Sandbox
    sandboxID             string
    imageID               string           // ← Source for ImageRef
    containerConfig       *runtime.ContainerConfig
    imageConfig           *v1.ImageConfig
    podSandboxConfig      *runtime.PodSandboxConfig
    sandboxRuntimeHandler string
    sandboxPid            uint32
    NetNSPath             string
    containerName         string
    containerdImage       *containerd.Image
    meta                  *containerstore.Metadata  // ← Target for ImageRef
    restore               bool
    start                 time.Time
}
```

### Metadata Population Flow

1. **Image Resolution**: `image.ID` is obtained from `c.LocalResolve(config.GetImage().GetImage())`
2. **Request Creation**: `imageID` is set in `createContainerRequest`
3. **Metadata Assignment**: `r.meta.ImageRef = r.imageID` should occur consistently
4. **Container Storage**: Metadata is stored in container store
5. **Listing Retrieval**: `ImageRef` is used in `toCRIContainer`

## Error Handling

### Current Error Scenarios

1. **Snapshot Preparation Failure**: If `snapshotter.Prepare()` fails, the function returns early, potentially bypassing metadata population
2. **Shared Storage Path Errors**: Invalid shared storage configuration may cause early returns
3. **Regex Matching Errors**: Invalid regex patterns may affect container creation flow
4. **Path Mapping Failures**: Path mapping registration errors are logged but don't stop creation

### Proposed Error Handling Strategy

1. **Fail-Safe Metadata Population**: Ensure `ImageRef` is set even if non-critical operations fail
2. **Early Metadata Assignment**: Move metadata population before error-prone operations
3. **Defensive Programming**: Add validation to ensure `ImageRef` is never empty
4. **Comprehensive Logging**: Add debug logging for metadata population tracking

## Testing Strategy

### Unit Tests

1. **Metadata Population Tests**:
   - Test `ImageRef` assignment in all container creation scenarios
   - Test error conditions that might bypass metadata population
   - Test shared snapshot enabled/disabled scenarios

2. **Container Listing Tests**:
   - Verify `toCRIContainer` handles empty `ImageRef` gracefully
   - Test container listing with mixed creation methods

### Integration Tests

1. **End-to-End Container Creation**:
   - Create containers via `ctr` tool and verify image field
   - Create containers via Kubernetes and verify image field
   - Test with shared snapshots enabled and disabled

2. **Regression Tests**:
   - Ensure existing shared storage functionality remains intact
   - Verify path optimization features continue working
   - Test regex-based filtering with metadata consistency

### Test Scenarios

```go
// Test cases to implement
func TestContainerImageRefPopulation(t *testing.T) {
    scenarios := []struct {
        name string
        setup func() *createContainerRequest
        expectImageRef bool
    }{
        {"Standard container creation", setupStandardContainer, true},
        {"Shared snapshot enabled", setupSharedSnapshotContainer, true},
        {"Regex filtering applied", setupRegexFilteredContainer, true},
        {"Snapshot preparation error", setupSnapshotError, true}, // Should still populate
        {"Path mapping error", setupPathMappingError, true},      // Should still populate
    }
    
    for _, scenario := range scenarios {
        t.Run(scenario.name, func(t *testing.T) {
            req := scenario.setup()
            result, err := createContainer(req)
            
            if scenario.expectImageRef {
                assert.NotEmpty(t, req.meta.ImageRef, "ImageRef should be populated")
                assert.Equal(t, req.imageID, req.meta.ImageRef, "ImageRef should match imageID")
            }
        })
    }
}
```

## Implementation Plan

### Phase 1: Root Cause Verification
1. Add comprehensive logging around metadata population
2. Create test scenarios to reproduce the issue
3. Verify the exact conditions that cause `ImageRef` to be empty

### Phase 2: Defensive Metadata Population
1. Move `r.meta.ImageRef = r.imageID` assignment earlier in the function
2. Add validation to ensure `imageID` is never empty before assignment
3. Add fallback logic if `imageID` is somehow empty

### Phase 3: Error Path Analysis
1. Review all error return paths in `createContainer` function
2. Ensure metadata population occurs before any early returns
3. Add defensive checks for critical metadata fields

### Phase 4: Testing and Validation
1. Implement comprehensive unit tests
2. Run integration tests with shared snapshots enabled/disabled
3. Verify no regression in existing functionality

### Proposed Code Changes

#### 1. Add containerd.WithImageName to container options

The primary fix is to add the missing `containerd.WithImageName()` option to the `opts` array in `createContainer`:

```go
func (c *criService) createContainer(r *createContainerRequest) (_ string, retErr error) {
    // ... existing logic ...
    
    opts = append(opts,
        containerd.WithSpec(spec, specOpts...),
        containerd.WithRuntime(runtimeName, runtimeOption),
        containerd.WithContainerLabels(containerLabels),
        containerd.WithContainerExtension(crilabels.ContainerMetadataExtension, r.meta),
        containerd.WithImageName(r.imageID), // ← ADD THIS LINE
    )
    
    // ... rest of container creation ...
}
```

#### 2. Alternative: Use WithImage if containerdImage is available

If the containerd image object is available, use `WithImage` instead:

```go
opts = append(opts,
    containerd.WithSpec(spec, specOpts...),
    containerd.WithRuntime(runtimeName, runtimeOption),
    containerd.WithContainerLabels(containerLabels),
    containerd.WithContainerExtension(crilabels.ContainerMetadataExtension, r.meta),
    containerd.WithImage(*r.containerdImage), // ← Alternative approach
)
```

#### 3. Validation to ensure both fields are set

Add validation to ensure both the core containerd `Image` field and CRI `ImageRef` field are populated:

```go
// After container creation, validate both fields are set
if cntr, err = c.client.NewContainer(r.ctx, r.containerID, opts...); err != nil {
    return "", fmt.Errorf("failed to create containerd container: %w", err)
}

// Validate that the core containerd Image field was set
info, err := cntr.Info(r.ctx)
if err != nil {
    return "", fmt.Errorf("failed to get container info for validation: %w", err)
}
if info.Image == "" {
    log.G(r.ctx).Warnf("Container %s: Core containerd Image field is empty", r.containerID)
}

// Validate that CRI ImageRef field is set
if r.meta.ImageRef == "" {
    log.G(r.ctx).Errorf("Container %s: CRI ImageRef field is empty", r.containerID)
}
```

This design ensures that the `ImageRef` field is consistently populated across all container creation paths while maintaining compatibility with the existing shared storage snapshotter customizations.