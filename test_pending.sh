#!/bin/bash
# 文件名：force_pending_test.sh
# 在StarryOS shell中执行

echo "=== 开始强制pending测试 ==="

# 1. 创建测试文件（512MB）
echo "创建测试文件..."
dd if=/dev/urandom of=/tmp/test_pending bs=1M count=512 2>/dev/null

# 2. 设置环境变量，调整预读参数（如果支持）
export RA_DELAY=10000  # 10ms每页，单位微秒

# 3. 启动读取线程（后台）
echo "启动快速读取线程..."
(
    # 快速顺序读取，每页后不yield
    for i in $(seq 0 1000); do
        # 读取一页数据，使用dd确保每次都是新读取
        dd if=/tmp/test_pending of=/dev/null bs=4k count=1 skip=$i 2>/dev/null
        
        # 记录进度
        if [ $((i % 10)) -eq 0 ]; then
            echo "已读取 $i 页"
        fi
        
        # 主动让出CPU，让预读IO有机会执行
        # 注意：这里我们使用usleep控制读取速度
        # 调整usleep值可以控制读取速度
        usleep 1000  # 1ms延迟，比预读的10ms快10倍
    done
) &

# 4. 监控日志
echo "监控系统日志..."
# 这里假设有方法查看内核日志
# 在实际StarryOS中，可能需要添加日志输出

# 5. 等待测试完成
wait
echo "=== 测试完成 ==="