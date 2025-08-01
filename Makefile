#   Copyright The containerd Authors.

#   Licensed under the Apache License, Version 2.0 (the "License");
#   you may not use this file except in compliance with the License.
#   You may obtain a copy of the License at

#       http://www.apache.org/licenses/LICENSE-2.0

#   Unless required by applicable law or agreed to in writing, software
#   distributed under the License is distributed on an "AS IS" BASIS,
#   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
#   See the License for the specific language governing permissions and
#   limitations under the License.


# Go command to use for build
GO ?= go
INSTALL ?= install

# Root directory of the project (absolute path).
ROOTDIR=$(dir $(abspath $(lastword $(MAKEFILE_LIST))))

# Base path used to install.
# The files will be installed under `$(DESTDIR)/$(PREFIX)`.
# The convention of `DESTDIR` was changed in containerd v1.6.
PREFIX        ?= /usr/local
BINDIR        ?= $(PREFIX)/bin
DATADIR       ?= $(PREFIX)/share
DOCDIR        ?= $(DATADIR)/doc
MANDIR        ?= $(DATADIR)/man

TEST_IMAGE_LIST ?=

# Used to populate variables in version package.
VERSION ?= $(shell git describe --match 'v[0-9]*' --dirty='.m' --always)
REVISION ?= $(shell git rev-parse HEAD)$(shell if ! git diff --no-ext-diff --quiet --exit-code; then echo .m; fi)
PACKAGE=github.com/containerd/containerd/v2
SHIM_CGO_ENABLED ?= 0

ifneq "$(strip $(shell command -v $(GO) 2>/dev/null))" ""
	GOOS ?= linux
	GOARCH ?= amd64
else
	ifeq ($(GOOS),)
		# approximate GOOS for the platform if we don't have Go and GOOS isn't
		# set. We leave GOARCH unset, so that may need to be fixed.
		ifeq ($(OS),Windows_NT)
			GOOS = windows
		else
			UNAME_S := $(shell uname -s)
			ifeq ($(UNAME_S),Linux)
				GOOS = linux
			endif
			ifeq ($(UNAME_S),Darwin)
				GOOS = darwin
			endif
			ifeq ($(UNAME_S),FreeBSD)
				GOOS = freebsd
			endif
		endif
	else
		GOOS ?= $$GOOS
		GOARCH ?= $$GOARCH
	endif
endif

ifndef GODEBUG
	EXTRA_LDFLAGS += -s -w
	DEBUG_GO_GCFLAGS :=
	DEBUG_TAGS :=
else
	DEBUG_GO_GCFLAGS := -gcflags=all="-N -l"
	DEBUG_TAGS := static_build
endif

WHALE = "🇩"
ONI = "👹"

RELEASE=containerd-$(VERSION:v%=%)-${GOOS}-${GOARCH}
STATICRELEASE=containerd-static-$(VERSION:v%=%)-${GOOS}-${GOARCH}
CRIRELEASE=cri-containerd-$(VERSION:v%=%)-${GOOS}-${GOARCH}
CRICNIRELEASE=cri-containerd-cni-$(VERSION:v%=%)-${GOOS}-${GOARCH}

PKG=github.com/containerd/containerd/v2

# Project binaries.
COMMANDS=ctr containerd containerd-stress
MANPAGES=ctr.8 containerd.8 containerd-config.8 containerd-config.toml.5

ifdef BUILDTAGS
    GO_BUILDTAGS = ${BUILDTAGS}
endif
GO_BUILDTAGS ?=
GO_BUILDTAGS += urfave_cli_no_docs
GO_BUILDTAGS += ${DEBUG_TAGS}
ifneq ($(STATIC),)
	GO_BUILDTAGS += osusergo netgo static_build
endif

SHIM_GO_BUILDTAGS := $(GO_BUILDTAGS) no_grpc

GO_TAGS=$(if $(GO_BUILDTAGS),-tags "$(strip $(GO_BUILDTAGS))",)
SHIM_GO_TAGS=$(if $(SHIM_GO_BUILDTAGS),-tags "$(strip $(SHIM_GO_BUILDTAGS))",)

GO_LDFLAGS=-ldflags '-X $(PKG)/version.Version=$(VERSION) -X $(PKG)/version.Revision=$(REVISION) -X $(PKG)/version.Package=$(PACKAGE) $(EXTRA_LDFLAGS)
ifneq ($(STATIC),)
	GO_LDFLAGS += -extldflags "-static"
endif
GO_LDFLAGS+='

SHIM_GO_LDFLAGS=-ldflags '-X $(PKG)/version.Version=$(VERSION) -X $(PKG)/version.Revision=$(REVISION) -X $(PKG)/version.Package=$(PACKAGE) -extldflags "-static" $(EXTRA_LDFLAGS)'

# Project packages.
PACKAGES=$(shell $(GO) list ${GO_TAGS} ./... | grep -v /vendor/ | grep -v /integration)
API_PACKAGES=$(shell (cd api && $(GO) list ${GO_TAGS} ./... | grep -v /vendor/ | grep -v /integration))
TEST_REQUIRES_ROOT_PACKAGES=$(filter \
    ${PACKAGES}, \
    $(shell \
	for f in $$(git grep -l testutil.RequiresRoot | grep -v Makefile); do \
		d="$$(dirname $$f)"; \
		[ "$$d" = "." ] && echo "${PKG}" && continue; \
		echo "${PKG}/$$d"; \
	done | sort -u) \
    )

ifdef SKIPTESTS
    PACKAGES:=$(filter-out ${SKIPTESTS},${PACKAGES})
    TEST_REQUIRES_ROOT_PACKAGES:=$(filter-out ${SKIPTESTS},${TEST_REQUIRES_ROOT_PACKAGES})
endif

#Replaces ":" (*nix), ";" (windows) with newline for easy parsing
GOPATHS=$(shell $(GO) env GOPATH | tr ":" "\n" | tr ";" "\n")

TESTFLAGS_RACE=
GO_BUILD_FLAGS ?=
# See Golang issue re: '-trimpath': https://github.com/golang/go/issues/13809
GO_GCFLAGS=$(shell				\
	set -- ${GOPATHS};			\
	echo "-gcflags=-trimpath=$${1}/src";	\
	)

BINARIES=$(addprefix bin/,$(COMMANDS))

#include platform specific makefile
-include Makefile.$(GOOS)

# Flags passed to `go test`
TESTFLAGS ?= $(TESTFLAGS_RACE) $(EXTRA_TESTFLAGS)
TESTFLAGS_PARALLEL ?= 8

# Use this to replace `go test` with, for instance, `gotestsum`
GOTEST ?= $(GO) test

OUTPUTDIR = $(join $(ROOTDIR), _output)
CRIDIR=$(OUTPUTDIR)/cri


.PHONY: clean all AUTHORS build binaries test integration generate protos check-protos coverage ci check help install uninstall vendor release static-release mandir install-man install-doc genman install-cri-deps cri-release cri-cni-release cri-integration install-deps bin/cri-integration.test remove-replace clean-vendor
.DEFAULT: default

# Forcibly set the default goal to all, in case an include above brought in a rule definition.
.DEFAULT_GOAL := all

all: binaries

check: proto-fmt ## run all linters
	@echo "$(WHALE) $@"
	GOGC=75 golangci-lint run

ci: check binaries check-protos coverage coverage-integration ## to be used by the CI

AUTHORS: .mailmap .git/HEAD
	git log --format='%aN <%aE>' | sort -fu > $@

generate: protos
	@echo "$(WHALE) $@"
	@PATH="${ROOTDIR}/bin:${PATH}" $(GO) generate -x ${PACKAGES}

protos: bin/protoc-gen-go-fieldpath bin/go-buildtag
	@echo "$(WHALE) $@"
	@find . -path ./vendor -prune -false -o -name '*.pb.go' | xargs rm
	$(eval TMPDIR := $(shell mktemp -d))
	@mv ${ROOTDIR}/vendor ${TMPDIR}
	@(cd ${ROOTDIR}/api && PATH="${ROOTDIR}/bin:${PATH}" protobuild --quiet ${API_PACKAGES})
	@mv ${TMPDIR}/vendor ${ROOTDIR}
	@rm -rf ${TMPDIR} v2
	go-fix-acronym -w -a '^Os' $(shell find api/ -name '*.pb.go')
	go-fix-acronym -w -a '(Id|Io|Uuid|Os)$$' $(shell find api/ -name '*.pb.go')
	bin/go-buildtag -w --tags '!no_grpc' $(shell find api/ -name '*_grpc.pb.go')
	@test -z "$$(git status --short | grep "api/next.pb.txt" | tee /dev/stderr)" || \
		$(GO) mod edit -replace=github.com/containerd/containerd/api=./api

check-protos: protos ## check if protobufs needs to be generated again
	@echo "$(WHALE) $@"
	@test -z "$$(git status --short | grep ".pb.go" | tee /dev/stderr)" || \
		((git diff | cat) && \
		(echo "$(ONI) please run 'make protos' when making changes to proto files" && false))

check-api-descriptors: protos ## check that protobuf changes aren't present.
	@echo "$(WHALE) $@"
	@test -z "$$(git status --short | grep ".pb.txt" | tee /dev/stderr)" || \
		((git diff $$(find . -name '*.pb.txt') | cat) && \
		(echo "$(ONI) please run 'make protos' when making changes to proto files and check-in the generated descriptor file changes" && false))

proto-fmt: ## check format of proto files
	@echo "$(WHALE) $@"
	@test -z "$$(find . -path ./vendor -prune -o -path ./protobuf/google/rpc -prune -o -name '*.proto' -type f -exec grep -Hn -e "^ " {} \; | tee /dev/stderr)" || \
		(echo "$(ONI) please indent proto files with tabs only" && false)

build: ## build the go packages
	@echo "$(WHALE) $@"
	@$(GO) build ${DEBUG_GO_GCFLAGS} ${GO_GCFLAGS} ${GO_BUILD_FLAGS} ${EXTRA_FLAGS} ${GO_LDFLAGS} ${PACKAGES}

test: ## run tests, except integration tests and tests that require root
	@echo "$(WHALE) $@"
	@$(GOTEST) ${TESTFLAGS} ${PACKAGES}

root-test: ## run tests, except integration tests
	@echo "$(WHALE) $@"
	@$(GOTEST) ${TESTFLAGS} ${TEST_REQUIRES_ROOT_PACKAGES} -test.root

integration: ## run integration tests
	@echo "$(WHALE) $@"
	@cd "${ROOTDIR}/integration/client" && $(GO) mod download && $(GOTEST) -v ${TESTFLAGS} -test.root -parallel ${TESTFLAGS_PARALLEL} .

# TODO integrate cri integration bucket with coverage
bin/cri-integration.test:
	@echo "$(WHALE) $@"
	@$(GO) test -c ./integration -o bin/cri-integration.test

cri-integration: binaries bin/cri-integration.test ## run cri integration tests (example: FOCUS=TestContainerListStats make cri-integration)
	@echo "$(WHALE) $@"
	@bash ./script/test/cri-integration.sh
	@rm -rf bin/cri-integration.test

# build runc shimv2 with failpoint control, only used by integration test
bin/containerd-shim-runc-fp-v1: integration/failpoint/cmd/containerd-shim-runc-fp-v1 FORCE
	@echo "$(WHALE) $@"
	@CGO_ENABLED=${SHIM_CGO_ENABLED} $(GO) build ${GO_BUILD_FLAGS} -o $@ ${SHIM_GO_LDFLAGS} ${GO_TAGS} ${SHIM_GO_TAGS} ./integration/failpoint/cmd/containerd-shim-runc-fp-v1

# build CNI bridge plugin wrapper with failpoint support, only used by integration test
bin/cni-bridge-fp: integration/failpoint/cmd/cni-bridge-fp FORCE
	@echo "$(WHALE) $@"
	@$(GO) build ${GO_BUILD_FLAGS} -o $@ ./integration/failpoint/cmd/cni-bridge-fp

# build runc-fp as runc wrapper to support failpoint, only used by integration test
bin/runc-fp: integration/failpoint/cmd/runc-fp FORCE
	@echo "$(WHALE) $@"
	@$(GO) build ${GO_BUILD_FLAGS} -o $@ ./integration/failpoint/cmd/runc-fp

# build loopback-v2 with failpoint support, only used by integration test
bin/loopback-v2: integration/failpoint/cmd/loopback-v2 FORCE
	@echo "$(WHALE) $@"
	@CGO_ENABLED=${SHIM_CGO_ENABLED} $(GO) build ${GO_BUILD_FLAGS} -o $@ ./integration/failpoint/cmd/loopback-v2

benchmark: ## run benchmarks tests
	@echo "$(WHALE) $@"
	@$(GO) test ${TESTFLAGS} -bench . -run Benchmark -test.root

FORCE:

define BUILD_BINARY
@echo "$(WHALE) $@"
$(GO) build ${DEBUG_GO_GCFLAGS} ${GO_GCFLAGS} ${GO_BUILD_FLAGS} -o $@ ${GO_LDFLAGS} ${GO_TAGS}  ./$<
endef

# Build a binary from a cmd.
bin/%: cmd/% FORCE
	$(call BUILD_BINARY)

# gen-manpages must not have the urfave_cli_no_docs build-tag set
bin/gen-manpages: cmd/gen-manpages FORCE
	@echo "$(WHALE) $@"
	$(GO) build ${DEBUG_GO_GCFLAGS} ${GO_GCFLAGS} ${GO_BUILD_FLAGS} -o $@ ${GO_LDFLAGS} $(subst urfave_cli_no_docs,,${GO_TAGS})  ./cmd/gen-manpages

bin/containerd-shim-runc-v2: cmd/containerd-shim-runc-v2 FORCE # set !cgo and omit pie for a static shim build: https://github.com/golang/go/issues/17789#issuecomment-258542220
	@echo "$(WHALE) $@"
	CGO_ENABLED=${SHIM_CGO_ENABLED} $(GO) build ${GO_BUILD_FLAGS} -o $@ ${SHIM_GO_LDFLAGS} ${SHIM_GO_TAGS} ./cmd/containerd-shim-runc-v2

binaries: $(BINARIES) session-restore ## build binaries
	@echo "$(WHALE) $@"

session-restore: ## build session-restore Rust binary
	@echo "$(WHALE) $@"
	@if command -v cargo >/dev/null 2>&1; then \
		cargo build --release --target x86_64-unknown-linux-musl; \
		cp target/x86_64-unknown-linux-musl/release/session-restore bin/session-restore; \
	else \
		echo "Cargo not found. Skipping session-restore binary build."; \
	fi

man: $(addprefix man/,$(MANPAGES))
	@echo "$(WHALE) $@"

mandir:
	@mkdir -p man

# Kept for backwards compatibility
genman: man/containerd.8 man/ctr.8

man/containerd.8: bin/gen-manpages FORCE | mandir
	@echo "$(WHALE) $@"
	$< $(@F) $(@D)

man/ctr.8: bin/gen-manpages FORCE | mandir
	@echo "$(WHALE) $@"
	$< $(@F) $(@D)

man/%: docs/man/%.md FORCE | mandir
	@echo "$(WHALE) $@"
	go-md2man -in "$<" -out "$@"

define installmanpage
$(INSTALL) -d $(DESTDIR)$(MANDIR)/man$(2);
gzip -c $(1) >$(DESTDIR)$(MANDIR)/man$(2)/$(3).gz;
endef

install-man: man
	@echo "$(WHALE) $@"
	$(foreach manpage,$(addprefix man/,$(MANPAGES)), $(call installmanpage,$(manpage),$(subst .,,$(suffix $(manpage))),$(notdir $(manpage))))

install-doc:
	@echo "$(WHALE) $@"
	@mkdir -p $(DESTDIR)/$(DOCDIR)/containerd
	@cp -R docs/* $(DESTDIR)/$(DOCDIR)/containerd

define pack_release
	@rm -rf releases/$(1) releases/$(1).tar.gz
	@$(INSTALL) -d releases/$(1)/bin
	@$(INSTALL) $(BINARIES) releases/$(1)/bin
	@tar -czf releases/$(1).tar.gz -C releases/$(1) bin
	@rm -rf releases/$(1)
endef


releases/$(RELEASE).tar.gz: $(BINARIES)
	@echo "$(WHALE) $@"
	$(call pack_release,$(RELEASE))

release: releases/$(RELEASE).tar.gz
	@echo "$(WHALE) $@"
	@cd releases && sha256sum $(RELEASE).tar.gz >$(RELEASE).tar.gz.sha256sum

releases/$(STATICRELEASE).tar.gz:
ifeq ($(GOOS),linux)
	@make STATIC=1 $(BINARIES)
	@echo "$(WHALE) $@"
	$(call pack_release,$(STATICRELEASE))
else
	@echo "Skipping $(STATICRELEASE) for $(GOOS)"
endif

static-release: releases/$(STATICRELEASE).tar.gz
ifeq ($(GOOS),linux)
	@echo "$(WHALE) $@"
	@cd releases && sha256sum $(STATICRELEASE).tar.gz >$(STATICRELEASE).tar.gz.sha256sum
else
	@echo "Skipping releasing $(STATICRELEASE) for $(GOOS)"
endif

# install of cri deps into release output directory
ifeq ($(GOOS),windows)
install-cri-deps: $(BINARIES)
	$(INSTALL) -d $(CRIDIR)
	DESTDIR=$(CRIDIR) script/setup/install-cni-windows
	cp bin/* $(CRIDIR)
else
install-cri-deps: $(BINARIES)
	@rm -rf ${CRIDIR}
	@$(INSTALL) -d ${CRIDIR}/usr/local/bin
	@$(INSTALL) -D -m 755 bin/* ${CRIDIR}/usr/local/bin
	@$(INSTALL) -d ${CRIDIR}/opt/containerd/cluster
	@cp -r contrib/gce ${CRIDIR}/opt/containerd/cluster/
	@$(INSTALL) -d ${CRIDIR}/etc/systemd/system
	@$(INSTALL) -m 644 containerd.service ${CRIDIR}/etc/systemd/system
	echo "CONTAINERD_VERSION: '$(VERSION:v%=%)'" | tee ${CRIDIR}/opt/containerd/cluster/version

	DESTDIR=$(CRIDIR) script/setup/install-runc
	DESTDIR=$(CRIDIR) script/setup/install-cni
	DESTDIR=$(CRIDIR) script/setup/install-critools
	DESTDIR=$(CRIDIR) script/setup/install-imgcrypt

	@$(INSTALL) -d $(CRIDIR)/bin
	@$(INSTALL) $(BINARIES) $(CRIDIR)/bin
endif

$(CRIDIR)/cri-containerd.DEPRECATED.txt:
	@mkdir -p $(CRIDIR)
	@$(INSTALL) -m 644 releases/cri-containerd.DEPRECATED.txt $@

ifeq ($(GOOS),windows)
releases/$(CRIRELEASE).tar.gz: install-cri-deps $(CRIDIR)/cri-containerd.DEPRECATED.txt
	@echo "$(WHALE) $@"
	@cd $(CRIDIR) && tar -czf ../../releases/$(CRIRELEASE).tar.gz *

releases/$(CRICNIRELEASE).tar.gz: install-cri-deps $(CRIDIR)/cri-containerd.DEPRECATED.txt
	@echo "$(WHALE) $@"
	@cd $(CRIDIR) && tar -czf ../../releases/$(CRICNIRELEASE).tar.gz *
else
releases/$(CRIRELEASE).tar.gz: install-cri-deps $(CRIDIR)/cri-containerd.DEPRECATED.txt
	@echo "$(WHALE) $@"
	@tar -czf releases/$(CRIRELEASE).tar.gz -C $(CRIDIR) cri-containerd.DEPRECATED.txt etc/crictl.yaml etc/systemd usr opt/containerd

releases/$(CRICNIRELEASE).tar.gz: install-cri-deps $(CRIDIR)/cri-containerd.DEPRECATED.txt
	@echo "$(WHALE) $@"
	@tar -czf releases/$(CRICNIRELEASE).tar.gz -C $(CRIDIR) cri-containerd.DEPRECATED.txt etc usr opt
endif

cri-release: releases/$(CRIRELEASE).tar.gz ## Deprecated (only kept for external CI)
	@echo "$(WHALE) $@"
	@cd releases && sha256sum $(CRIRELEASE).tar.gz >$(CRIRELEASE).tar.gz.sha256sum && ln -sf $(CRIRELEASE).tar.gz cri-containerd.tar.gz

cri-cni-release: releases/$(CRICNIRELEASE).tar.gz ## Deprecated (only kept for external CI)
	@echo "$(WHALE) $@"
	@cd releases && sha256sum $(CRICNIRELEASE).tar.gz >$(CRICNIRELEASE).tar.gz.sha256sum && ln -sf $(CRICNIRELEASE).tar.gz cri-cni-containerd.tar.gz

clean: ## clean up binaries
	@echo "$(WHALE) $@"
	@rm -f $(BINARIES)
	@rm -f bin/session-restore
	@rm -f releases/*.tar.gz*
	@rm -rf $(OUTPUTDIR)
	@rm -rf bin/cri-integration.test
	@if command -v cargo >/dev/null 2>&1; then cargo clean; fi

clean-test: ## clean up debris from previously failed tests
	@echo "$(WHALE) $@"
	$(eval containers=$(shell find /run/containerd/runc -mindepth 2 -maxdepth 3  -type d -exec basename {} \;))
	$(shell pidof containerd runc | xargs -r -n 1 kill -9)
	@( for container in $(containers); do \
	    grep $$container /proc/self/mountinfo | while read -r mountpoint; do \
		umount $$(echo $$mountpoint | awk '{print $$5}'); \
	    done; \
	    find /sys/fs/cgroup -name $$container -print0 | xargs -r -0 rmdir; \
	done )
	@rm -rf /run/containerd/runc/*
	@rm -rf /run/containerd/fifo/*
	@rm -rf /run/containerd-test/*
	@rm -rf bin/cri-integration.test
	@rm -rf bin/cni-bridge-fp
	@rm -rf bin/containerd-shim-runc-fp-v1

install: ## install binaries
	@echo "$(WHALE) $@ $(BINARIES)"
	@$(INSTALL) -d $(DESTDIR)$(BINDIR)
	@$(INSTALL) $(BINARIES) $(DESTDIR)$(BINDIR)

uninstall:
	@echo "$(WHALE) $@"
	@rm -f $(addprefix $(DESTDIR)$(BINDIR)/,$(notdir $(BINARIES)))

ifeq ($(GOOS),windows)
install-deps:
	script/setup/install-critools
	script/setup/install-cni-windows
else
install-deps: ## install cri dependencies
	script/setup/install-seccomp
	script/setup/install-runc
	script/setup/install-critools
	script/setup/install-cni
endif

coverage: ## generate coverprofiles from the unit tests, except tests that require root
	@echo "$(WHALE) $@"
	@rm -f coverage.txt
	@$(GO) test -i ${TESTFLAGS} ${PACKAGES} 2> /dev/null
	@( for pkg in ${PACKAGES}; do \
		$(GO) test ${TESTFLAGS} \
			-cover \
			-coverprofile=profile.out \
			-covermode=atomic $$pkg || exit; \
		if [ -f profile.out ]; then \
			cat profile.out >> coverage.txt; \
			rm profile.out; \
		fi; \
	done )

root-coverage: ## generate coverage profiles for unit tests that require root
	@echo "$(WHALE) $@"
	@$(GO) test -i ${TESTFLAGS} ${TEST_REQUIRES_ROOT_PACKAGES} 2> /dev/null
	@( for pkg in ${TEST_REQUIRES_ROOT_PACKAGES}; do \
		$(GO) test ${TESTFLAGS} \
			-cover \
			-coverprofile=profile.out \
			-covermode=atomic $$pkg -test.root || exit; \
		if [ -f profile.out ]; then \
			cat profile.out >> coverage.txt; \
			rm profile.out; \
		fi; \
	done )

remove-replace:
	@echo "$(WHALE) $@"
	@$(GO) mod edit -dropreplace=github.com/containerd/containerd/api

vendor: ## ensure all the go.mod/go.sum files are up-to-date including vendor/ directory
	@echo "$(WHALE) $@"
	@$(GO) mod tidy
	@$(GO) mod vendor
	@$(GO) mod verify
	@(cd ${ROOTDIR}/api && ${GO} mod tidy)

verify-vendor: ## verify if all the go.mod/go.sum files are up-to-date
	@echo "$(WHALE) $@"
	$(eval TMPDIR := $(shell mktemp -d))
	@cp -R ${ROOTDIR} ${TMPDIR}
	@(cd ${TMPDIR}/containerd && ${GO} mod tidy)
	@(cd ${TMPDIR}/containerd && ${GO} mod vendor)
	@(cd ${TMPDIR}/containerd && ${GO} mod verify)
	@(cd ${TMPDIR}/containerd/api && ${GO} mod tidy)
	@diff -r -u -q ${ROOTDIR} ${TMPDIR}/containerd
	@rm -rf ${TMPDIR}

clean-vendor: remove-replace vendor


help: ## this help
	@awk 'BEGIN {FS = ":.*?## "} /^[a-zA-Z_-]+:.*?## / {printf "\033[36m%-30s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST) | sort
