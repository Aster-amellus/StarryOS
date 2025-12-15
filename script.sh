#!/bin/bash

# 编译 qperf
make clean
make -j$(nproc)

# 挂载并复制
sudo mount ../StarryOS/rootfs-riscv64.img ../StarryOS/mnt
sudo cp src/qperf ../StarryOS/mnt/bin/
sudo umount ../StarryOS/mnt

# 运行 StarryOS
cd ../StarryOS
make img
make rv
