/**
 * StarryOS 文件预读 (File Readahead) 性能测试
 *
 * 测试文件系统的预读功能对顺序读取的优化效果
 *
 * 编译 (RISC-V):
 *   riscv64-linux-musl-gcc -static -O2 -o file_readahead_bench file_readahead_bench.c
 *
 * 运行:
 *   ./file_readahead_bench [test_file_path]
 *
 * 默认使用 /tmp/readahead_test_file 作为测试文件
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/time.h>
#include <sys/stat.h>
#include <errno.h>

#define KB 1024
#define MB (1024 * 1024)
#define PAGE_SIZE 4096

/* 默认测试文件路径 */
#define DEFAULT_TEST_FILE "/tmp/readahead_test_file"

/*==========================================================================
 * 工具函数
 *==========================================================================*/

static inline long long get_time_us(void) {
    struct timeval tv;
    gettimeofday(&tv, NULL);
    return tv.tv_sec * 1000000LL + tv.tv_usec;
}

typedef struct {
    const char *name;
    size_t total_bytes;
    size_t block_size;
    long long time_us;
    double throughput_mb_s;
} BenchResult;

static void print_header(void) {
    printf("%-40s %10s %10s %12s %12s\n",
           "Test", "Size", "Block", "Time(us)", "MB/s");
    printf("--------------------------------------------------------------------------------\n");
}

static void print_result(BenchResult *r) {
    printf("%-40s %7zuKB %7zuKB %12lld %12.2f\n",
           r->name,
           r->total_bytes / KB,
           r->block_size / KB,
           r->time_us,
           r->throughput_mb_s);
}

/* 创建测试文件 */
static int create_test_file(const char *path, size_t size) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        perror("create_test_file: open");
        return -1;
    }

    char *buf = malloc(PAGE_SIZE);
    if (!buf) {
        close(fd);
        return -1;
    }

    /* 填充随机数据 */
    for (int i = 0; i < PAGE_SIZE; i++) {
        buf[i] = (char)(i & 0xFF);
    }

    size_t written = 0;
    while (written < size) {
        size_t to_write = (size - written) < PAGE_SIZE ? (size - written) : PAGE_SIZE;
        ssize_t ret = write(fd, buf, to_write);
        if (ret < 0) {
            perror("create_test_file: write");
            free(buf);
            close(fd);
            return -1;
        }
        written += ret;
    }

    fsync(fd);
    free(buf);
    close(fd);
    printf("Created test file: %s (%zu KB)\n", path, size / KB);
    return 0;
}

/* 清除文件缓存 (尝试) - 在 StarryOS 上可能不支持 */
static void drop_caches(void) {
    /* Linux: echo 3 > /proc/sys/vm/drop_caches */
    int fd = open("/proc/sys/vm/drop_caches", O_WRONLY);
    if (fd >= 0) {
        write(fd, "3", 1);
        close(fd);
        printf("Dropped page cache\n");
    }
    /* 如果不支持，静默失败 */
}

/*==========================================================================
 * 测试用例
 *==========================================================================*/

/**
 * 测试1: 顺序读取 - 最能体现预读效果
 *
 * 预读应该显著提升顺序读取性能，因为后续的页面已经被预取到缓存中
 */
BenchResult test_sequential_read(const char *path, size_t block_size) {
    BenchResult r = {
        .name = "sequential_read",
        .block_size = block_size,
    };

    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        perror("open");
        return r;
    }

    struct stat st;
    fstat(fd, &st);
    r.total_bytes = st.st_size;

    char *buf = malloc(block_size);
    if (!buf) {
        close(fd);
        return r;
    }

    long long t1 = get_time_us();

    size_t total_read = 0;
    while (1) {
        ssize_t ret = read(fd, buf, block_size);
        if (ret <= 0) break;
        total_read += ret;
    }

    long long t2 = get_time_us();

    r.time_us = t2 - t1;
    r.throughput_mb_s = (double)total_read / MB / ((double)r.time_us / 1000000.0);

    free(buf);
    close(fd);
    return r;
}

/**
 * 测试2: 随机读取 - 预读应该不生效或负优化
 *
 * 随机访问模式应该禁用预读，避免浪费 I/O
 */
BenchResult test_random_read(const char *path, size_t block_size, int num_reads) {
    BenchResult r = {
        .name = "random_read",
        .block_size = block_size,
    };

    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        perror("open");
        return r;
    }

    struct stat st;
    fstat(fd, &st);
    size_t file_size = st.st_size;
    r.total_bytes = num_reads * block_size;

    char *buf = malloc(block_size);
    if (!buf) {
        close(fd);
        return r;
    }

    /* 生成随机偏移 */
    size_t max_offset = file_size - block_size;
    srand(12345);

    long long t1 = get_time_us();

    size_t total_read = 0;
    for (int i = 0; i < num_reads; i++) {
        off_t offset = (rand() % (max_offset / PAGE_SIZE)) * PAGE_SIZE;
        lseek(fd, offset, SEEK_SET);
        ssize_t ret = read(fd, buf, block_size);
        if (ret > 0) total_read += ret;
    }

    long long t2 = get_time_us();

    r.time_us = t2 - t1;
    r.throughput_mb_s = (double)total_read / MB / ((double)r.time_us / 1000000.0);

    free(buf);
    close(fd);
    return r;
}

/**
 * 测试3: 步进读取 - 检测预读的自适应能力
 *
 * 以固定步长跳跃读取，测试预读算法是否能适应
 */
BenchResult test_stride_read(const char *path, size_t block_size, size_t stride) {
    static char name_buf[64];
    snprintf(name_buf, sizeof(name_buf), "stride_read (stride=%zuKB)", stride / KB);

    BenchResult r = {
        .name = name_buf,
        .block_size = block_size,
    };

    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        perror("open");
        return r;
    }

    struct stat st;
    fstat(fd, &st);
    size_t file_size = st.st_size;

    char *buf = malloc(block_size);
    if (!buf) {
        close(fd);
        return r;
    }

    long long t1 = get_time_us();

    size_t total_read = 0;
    off_t offset = 0;
    while (offset + block_size <= file_size) {
        lseek(fd, offset, SEEK_SET);
        ssize_t ret = read(fd, buf, block_size);
        if (ret <= 0) break;
        total_read += ret;
        offset += stride;
    }

    long long t2 = get_time_us();

    r.total_bytes = total_read;
    r.time_us = t2 - t1;
    r.throughput_mb_s = (double)total_read / MB / ((double)r.time_us / 1000000.0);

    free(buf);
    close(fd);
    return r;
}

/**
 * 测试4: 反向顺序读取
 *
 * 从文件末尾向前读取，测试预读是否支持反向模式
 */
BenchResult test_reverse_read(const char *path, size_t block_size) {
    BenchResult r = {
        .name = "reverse_sequential_read",
        .block_size = block_size,
    };

    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        perror("open");
        return r;
    }

    struct stat st;
    fstat(fd, &st);
    size_t file_size = st.st_size;
    r.total_bytes = file_size;

    char *buf = malloc(block_size);
    if (!buf) {
        close(fd);
        return r;
    }

    long long t1 = get_time_us();

    size_t total_read = 0;
    off_t offset = file_size - block_size;
    while (offset >= 0) {
        lseek(fd, offset, SEEK_SET);
        ssize_t ret = read(fd, buf, block_size);
        if (ret > 0) total_read += ret;
        offset -= block_size;
    }

    long long t2 = get_time_us();

    r.time_us = t2 - t1;
    r.throughput_mb_s = (double)total_read / MB / ((double)r.time_us / 1000000.0);

    free(buf);
    close(fd);
    return r;
}

/**
 * 测试5: 热缓存读取 - 第二遍顺序读取
 *
 * 测试缓存命中时的性能基准
 */
BenchResult test_hot_cache_read(const char *path, size_t block_size) {
    BenchResult r = {
        .name = "hot_cache_read (2nd pass)",
        .block_size = block_size,
    };

    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        perror("open");
        return r;
    }

    struct stat st;
    fstat(fd, &st);
    r.total_bytes = st.st_size;

    char *buf = malloc(block_size);
    if (!buf) {
        close(fd);
        return r;
    }

    /* 第一遍: 预热缓存 */
    while (read(fd, buf, block_size) > 0) {}
    lseek(fd, 0, SEEK_SET);

    /* 第二遍: 测量 */
    long long t1 = get_time_us();

    size_t total_read = 0;
    while (1) {
        ssize_t ret = read(fd, buf, block_size);
        if (ret <= 0) break;
        total_read += ret;
    }

    long long t2 = get_time_us();

    r.time_us = t2 - t1;
    r.throughput_mb_s = (double)total_read / MB / ((double)r.time_us / 1000000.0);

    free(buf);
    close(fd);
    return r;
}

/**
 * 测试6: 不同块大小的顺序读取
 *
 * 测试块大小对预读效果的影响
 */
void test_block_sizes(const char *path) {
    printf("\n[Block Size Impact on Sequential Read]\n");
    print_header();

    size_t block_sizes[] = {512, 1*KB, 4*KB, 16*KB, 64*KB, 256*KB};
    int num_sizes = sizeof(block_sizes) / sizeof(block_sizes[0]);

    for (int i = 0; i < num_sizes; i++) {
        static char name_buf[64];
        snprintf(name_buf, sizeof(name_buf), "sequential (block=%zuB)", block_sizes[i]);

        drop_caches();
        usleep(100000); /* 100ms delay */

        BenchResult r = test_sequential_read(path, block_sizes[i]);
        r.name = name_buf;
        print_result(&r);
    }
}

/*==========================================================================
 * 主函数
 *==========================================================================*/

int main(int argc, char *argv[]) {
    const char *test_file = (argc > 1) ? argv[1] : DEFAULT_TEST_FILE;
    size_t file_size = 16 * MB;  /* 16MB 测试文件 */

    printf("\n");
    printf("==================================================\n");
    printf("    StarryOS File Readahead Benchmark\n");
    printf("==================================================\n");
    printf("Test file: %s\n", test_file);
    printf("File size: %zu KB\n", file_size / KB);
    printf("Page size: %d bytes\n\n", PAGE_SIZE);

    /* 创建测试文件 */
    if (create_test_file(test_file, file_size) < 0) {
        fprintf(stderr, "Failed to create test file\n");
        return 1;
    }

    BenchResult r;

    /* === 冷缓存顺序读取 vs 热缓存 === */
    printf("\n[Cold vs Hot Cache Sequential Read] (4KB block)\n");
    print_header();

    drop_caches();
    usleep(100000);
    r = test_sequential_read(test_file, 4*KB);
    r.name = "cold_cache_sequential";
    print_result(&r);

    r = test_hot_cache_read(test_file, 4*KB);
    print_result(&r);

    /* === 顺序 vs 随机 vs 反向 === */
    printf("\n[Access Pattern Comparison] (4KB block)\n");
    print_header();

    drop_caches();
    usleep(100000);
    r = test_sequential_read(test_file, 4*KB);
    r.name = "sequential";
    print_result(&r);

    drop_caches();
    usleep(100000);
    r = test_random_read(test_file, 4*KB, 1024);
    print_result(&r);

    drop_caches();
    usleep(100000);
    r = test_reverse_read(test_file, 4*KB);
    print_result(&r);

    /* === 步进访问测试 === */
    printf("\n[Stride Access Tests] (4KB block)\n");
    print_header();

    size_t strides[] = {4*KB, 8*KB, 16*KB, 64*KB, 256*KB};
    for (int i = 0; i < 5; i++) {
        drop_caches();
        usleep(100000);
        r = test_stride_read(test_file, 4*KB, strides[i]);
        print_result(&r);
    }

    /* === 块大小影响 === */
    test_block_sizes(test_file);

    printf("\n==================================================\n");
    printf("    Benchmark Complete\n");
    printf("==================================================\n\n");

    /* 清理测试文件 */
    unlink(test_file);

    return 0;
}
