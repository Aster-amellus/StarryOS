# Build Options
export ARCH := riscv64
export LOG := warn
export DWARF := y
export MEMTRACK := n

# QEMU Options
export BLK := y
export NET := y
export VSOCK := n
export MEM := 1G
export ICOUNT := n

# Host-side path to the ext4 rootfs image used for injection/extraction.
# Note: do NOT export this into arceos; arceos expects DISK_IMG relative to its own cwd.
DISK_IMG_PATH ?= arceos/disk.img

# Generated Options
export A := $(PWD)
export NO_AXSTD := y
export AX_LIB := axfeat
export APP_FEATURES := qemu

ifeq ($(MEMTRACK), y)
	APP_FEATURES += starry-api/memtrack
endif

default: build

ROOTFS_URL = https://github.com/Starry-OS/rootfs/releases/download/20250917
ROOTFS_IMG = rootfs-$(ARCH).img

rootfs:
	@if [ ! -f $(ROOTFS_IMG) ]; then \
		echo "Image not found, downloading..."; \
		curl -f -L $(ROOTFS_URL)/$(ROOTFS_IMG).xz -O; \
		xz -d $(ROOTFS_IMG).xz; \
	fi
	@cp $(ROOTFS_IMG) $(DISK_IMG_PATH)

img:
	@echo -e "\033[33mWARN: The 'img' target is deprecated. Please use 'rootfs' instead.\033[0m"
	@$(MAKE) --no-print-directory rootfs

defconfig justrun clean:
	@make -C arceos $@

build run debug disasm: defconfig
	@make -C arceos $@

# Capture QEMU console output to a host log file (useful for dd/md5 logs).
LOGFILE ?= run.log
runlog: defconfig
	@make -C arceos run 2>&1 | tee $(LOGFILE)

# Inject host-side benchmark scripts into the ext4 rootfs image (requires sudo).
# This keeps the guest's /bin/bench_fio.sh in sync with scripts/bench_fio.sh.
inject-bench:
	@sh scripts/inject_bench_fio_to_disk.sh $(DISK_IMG_PATH)

# Extract bench results (bench_*) from the ext4 rootfs image to ./bench_out/.
fetch-bench:
	@sh scripts/extract_bench_from_disk.sh $(DISK_IMG_PATH) bench_out

# Aliases
rv:
	$(MAKE) ARCH=riscv64 run

la:
	$(MAKE) ARCH=loongarch64 run

vf2:
	$(MAKE) ARCH=riscv64 APP_FEATURES=vf2 MYPLAT=axplat-riscv64-visionfive2 BUS=mmio build

.PHONY: build run justrun debug disasm clean
