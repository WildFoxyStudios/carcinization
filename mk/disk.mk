# Configuration file with the commands configuration of the Carcinization image

$(BUILD)/harddrive.img: $(FSTOOLS) $(REPO_TAG)
ifeq ($(FSTOOLS_IN_PODMAN),1)
	$(PODMAN_RUN) make $@
else
	mkdir -p $(BUILD)
	-$(FUMOUNT) $(MOUNT_DIR) || true
	-$(FUMOUNT) /tmp/redox_installer || true
	rm -rf $@  $@.partial $(MOUNT_DIR)
	FILESYSTEM_SIZE=$(FILESYSTEM_SIZE) && \
	if [ -z "$$FILESYSTEM_SIZE" ] ; then \
	FILESYSTEM_SIZE=$(shell $(INSTALLER) --filesystem-size -c $(FILESYSTEM_CONFIG)); \
	fi && \
	truncate -s "$$FILESYSTEM_SIZE"m $@.partial
	umask 002 && $(INSTALLER) $(INSTALLER_OPTS) -c $(FILESYSTEM_CONFIG) $@.partial
	mv $@.partial $@
endif

$(BUILD)/carcinization-live.iso: $(FSTOOLS) $(REPO_TAG) carcinization.ipxe
ifeq ($(FSTOOLS_IN_PODMAN),1)
	$(PODMAN_RUN) make $@
else
	mkdir -p $(BUILD)
	rm -rf $@  $@.partial
	-$(FUMOUNT) /tmp/redox_installer || true
	FILESYSTEM_SIZE=$(FILESYSTEM_SIZE) && \
	if [ -z "$$FILESYSTEM_SIZE" ] ; then \
	FILESYSTEM_SIZE=$(shell $(INSTALLER) --filesystem-size -c $(FILESYSTEM_CONFIG)); \
	fi && \
	truncate -s "$$FILESYSTEM_SIZE"m $@.partial
	umask 002 && $(INSTALLER) $(INSTALLER_OPTS) -c $(FILESYSTEM_CONFIG) --write-bootloader="$(BUILD)/bootloader-live.efi" --live $@.partial
	mv $@.partial $@
	cp carcinization.ipxe $(BUILD)/carcinization.ipxe
endif

$(BUILD)/filesystem.img: $(FSTOOLS) $(REPO_TAG)
ifeq ($(FSTOOLS_IN_PODMAN),1)
	$(PODMAN_RUN) make $@
else
	mkdir -p $(BUILD)
	-$(FUMOUNT) $(MOUNT_DIR) || true
	rm -rf $@  $@.partial $(MOUNT_DIR)
	-$(FUMOUNT) /tmp/redox_installer || true
	FILESYSTEM_SIZE=$(FILESYSTEM_SIZE) && \
	if [ -z "$$FILESYSTEM_SIZE" ] ; then \
	FILESYSTEM_SIZE=$(shell $(INSTALLER) --filesystem-size -c $(FILESYSTEM_CONFIG)); \
	fi && \
	truncate -s "$$FILESYSTEM_SIZE"m $@.partial
	$(CARCINIZATIONFS_MKFS) $(CARCINIZATIONFS_MKFS_FLAGS) $@.partial
	mkdir -p $(MOUNT_DIR)
	$(CARCINIZATIONFS) $@.partial $(MOUNT_DIR)
	sleep 1
	pgrep carcinizationfs
	umask 002 && $(INSTALLER) $(INSTALLER_OPTS) -c $(FILESYSTEM_CONFIG) $(MOUNT_DIR)
	sync
	-$(FUMOUNT) $(MOUNT_DIR) || true
	rm -rf $(MOUNT_DIR)
	mv $@.partial $@
endif

mount: $(FSTOOLS) FORCE
ifeq ($(FSTOOLS_IN_PODMAN),1)
	$(PODMAN_RUN) make $@
else
	@mkdir -p $(MOUNT_DIR)
	$(CARCINIZATIONFS) $(BUILD)/harddrive.img $(MOUNT_DIR)
	@sleep 2
	@echo "\033[1;36;49mharddrive.img mounted ($$(pgrep carcinizationfs))\033[0m"
endif

mount_extra: $(FSTOOLS) FORCE
ifeq ($(FSTOOLS_IN_PODMAN),1)
	$(PODMAN_RUN) make $@
else
	@mkdir -p $(MOUNT_DIR)
	$(CARCINIZATIONFS) $(BUILD)/extra.img $(MOUNT_DIR)
	@sleep 2
	@echo "\033[1;36;49mextra.img mounted ($$(pgrep carcinizationfs))\033[0m"
endif

mount_live: $(FSTOOLS) FORCE
ifeq ($(FSTOOLS_IN_PODMAN),1)
	$(PODMAN_RUN) make $@
else
	@mkdir -p $(MOUNT_DIR)
	$(CARCINIZATIONFS) $(BUILD)/carcinization-live.iso $(MOUNT_DIR)
	@sleep 2
	@echo "\033[1;36;49mcarcinization-live.iso mounted ($$(pgrep carcinizationfs))\033[0m"
endif

unmount: FORCE
ifeq ($(FSTOOLS_IN_PODMAN),1)
	$(PODMAN_RUN) make $@
else
	@sync
ifneq ($(wildcard $(MOUNT_DIR)),)
	$(FUMOUNT) $(MOUNT_DIR)
	@rm -rf $(MOUNT_DIR)
	@echo "\033[1;36;49mFilesystem unmounted\033[0m"
else
	@echo "\033[1;36;49mDisk is not mounted\033[0m"
endif
	@-$(FUMOUNT) /tmp/redox_installer 2>/dev/null || true
endif
