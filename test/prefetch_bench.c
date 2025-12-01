/**
 * StarryOS Memory Prefetch Benchmark (Improved)
 * * Compile: musl-gcc -static -O2 -o prefetch_bench prefetch_bench.c
 * Run: ./prefetch_bench
 */

#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <time.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <unistd.h>

#define PAGE_SIZE 4096
#define KB 1024ULL
#define MB (1024ULL * 1024ULL)
#define GB (1024ULL * 1024ULL * 1024ULL)

#define ITERATIONS 3  /* Run each test 3 times and take the average */

/*==========================================================================
 * Utilities
 *==========================================================================*/

/* Nanosecond precision timing */
static inline uint64_t get_time_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ULL + ts.tv_nsec;
}

static long get_total_page_faults(void) {
    struct rusage usage;
    if (getrusage(RUSAGE_SELF, &usage) != 0) return 0;
    return usage.ru_minflt + usage.ru_majflt;
}

/* Prevent compiler optimization */
static inline void compiler_barrier(void) {
    asm volatile("" ::: "memory");
}

typedef struct {
    const char *name;
    size_t size_bytes;
    uint64_t duration_ns;
    long page_faults;
    double throughput_mb_s;
} BenchResult;

static void print_header(void) {
    printf("%-25s %10s %12s %10s %12s %10s\n", 
           "Test", "Size", "Time(us)", "Faults", "us/fault", "Speed");
    printf("--------------------------------------------------------------------------------------\n");
}

static void print_result(BenchResult *r) {
    double us_per_fault = r->page_faults > 0 ? 
        (double)(r->duration_ns / 1000.0) / r->page_faults : 0.0;
    
    char size_buf[16];
    if (r->size_bytes >= GB)
        snprintf(size_buf, sizeof(size_buf), "%zu GB", r->size_bytes / GB);
    else
        snprintf(size_buf, sizeof(size_buf), "%zu MB", r->size_bytes / MB);

    printf("%-25s %10s %12lu %10ld %12.3f %7.0f MB/s\n",
           r->name, 
           size_buf,
           (unsigned long)(r->duration_ns / 1000), 
           r->page_faults,
           us_per_fault,
           r->throughput_mb_s);
}

/*==========================================================================
 * Core Benchmark Framework
 *==========================================================================*/

typedef void (*test_func_t)(volatile char *mem, size_t size, void *arg);

/* Generic test runner */
BenchResult run_test(const char *name, size_t size, test_func_t func, void *arg) {
    BenchResult r = { .name = name, .size_bytes = size };
    uint64_t total_ns = 0;
    long total_faults = 0;

    /* Multiple iterations for stability */
    for (int i = 0; i < ITERATIONS; i++) {
        /* 1. Allocate Memory (Anonymous Map) */
        char *mem = mmap(NULL, size, PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        
        if (mem == MAP_FAILED) {
            perror("mmap failed");
            return r;
        }

        /* 2. Sync state */
        compiler_barrier();
        long f_start = get_total_page_faults();
        uint64_t t_start = get_time_ns();

        /* 3. Run workload */
        func((volatile char *)mem, size, arg);

        /* 4. Measure */
        compiler_barrier();
        uint64_t t_end = get_time_ns();
        long f_end = get_total_page_faults();

        total_ns += (t_end - t_start);
        total_faults += (f_end - f_start);

        munmap(mem, size);
    }

    /* Average results */
    r.duration_ns = total_ns / ITERATIONS;
    r.page_faults = total_faults / ITERATIONS;
    
    double seconds = (double)r.duration_ns / 1e9;
    if (seconds > 0) {
        r.throughput_mb_s = (double)size / MB / seconds;
    }

    return r;
}

/*==========================================================================
 * Access Patterns
 *==========================================================================*/

void pattern_seq_write(volatile char *mem, size_t size, void *arg) {
    (void)arg;
    for (size_t i = 0; i < size; i += PAGE_SIZE) {
        mem[i] = 1;
    }
}

void pattern_seq_read(volatile char *mem, size_t size, void *arg) {
    (void)arg;
    volatile char sum = 0;
    for (size_t i = 0; i < size; i += PAGE_SIZE) {
        sum += mem[i];
    }
}

/* Stride access: Access every Nth page */
void pattern_stride(volatile char *mem, size_t size, void *arg) {
    size_t stride_pages = (size_t)arg;
    size_t stride_bytes = stride_pages * PAGE_SIZE;
    
    for (size_t i = 0; i < size; i += stride_bytes) {
        mem[i] = 1;
    }
}

/* Reverse sequential */
void pattern_reverse(volatile char *mem, size_t size, void *arg) {
    (void)arg;
    // Prevent underflow by stopping at 0 explicitly
    if (size == 0) return;
    
    for (size_t i = size - PAGE_SIZE; i > 0; i -= PAGE_SIZE) {
        mem[i] = 1;
    }
    mem[0] = 1; 
}

/* Random Access logic helper */
typedef struct {
    size_t *indices;
    size_t count;
} RandomContext;

void pattern_random(volatile char *mem, size_t size, void *arg) {
    RandomContext *ctx = (RandomContext *)arg;
    size_t *indices = ctx->indices;
    size_t count = ctx->count;
    
    for (size_t i = 0; i < count; i++) {
        mem[indices[i] * PAGE_SIZE] = 1;
    }
}

/*==========================================================================
 * Main
 *==========================================================================*/

int main(int argc, char *argv[]) {
    printf("\n");
    printf("==============================================================\n");
    printf("    StarryOS Memory Prefetch Benchmark (v2.0)\n");
    printf("    Page Size: %d bytes | Iterations: %d\n", PAGE_SIZE, ITERATIONS);
    printf("==============================================================\n\n");

    size_t sizes[] = { 
        4UL * MB, 
        64UL * MB, 
        256UL * MB, 
        1UL * GB 
    };
    int num_sizes = sizeof(sizes) / sizeof(sizes[0]);
    BenchResult r;

    /* --- Sequential Write --- */
    printf("[Sequential Write] (Tests basic fault handling)\n");
    print_header();
    for (int i = 0; i < num_sizes; i++) {
        r = run_test("seq_write", sizes[i], pattern_seq_write, NULL);
        print_result(&r);
    }
    printf("\n");

    /* --- Sequential Read --- */
    printf("[Sequential Read] (Tests read-fault latency)\n");
    print_header();
    for (int i = 0; i < num_sizes; i++) {
        r = run_test("seq_read", sizes[i], pattern_seq_read, NULL);
        print_result(&r);
    }
    printf("\n");

    /* --- Stride Tests (Fixed 256MB) --- */
    printf("[Stride Write] (Tests prefetch distance/aggressiveness)\n");
    print_header();
    size_t stride_test_size = 256 * MB;
    size_t strides[] = {1, 2, 4, 8, 16, 32};
    
    for (int i = 0; i < 6; i++) {
        char name[32];
        snprintf(name, 32, "stride_%zu_pg", strides[i]);
        r = run_test(name, stride_test_size, pattern_stride, (void*)strides[i]);
        print_result(&r);
    }
    printf("\n");

    /* --- Random Access (Fixed 256MB) --- */
    printf("[Random Access] (Tests worst-case fault latency)\n");
    print_header();
    
    /* Pre-calculate random order to avoid measuring RNG overhead */
    size_t num_pages = stride_test_size / PAGE_SIZE;
    size_t *indices = malloc(num_pages * sizeof(size_t));
    if (indices) {
        for (size_t i = 0; i < num_pages; i++) indices[i] = i;
        
        // Fisher-Yates
        srand(0xDEADBEEF);
        for (size_t i = num_pages - 1; i > 0; i--) {
            size_t j = rand() % (i + 1);
            size_t tmp = indices[i];
            indices[i] = indices[j];
            indices[j] = tmp;
        }

        RandomContext rnd_ctx = { .indices = indices, .count = num_pages };
        r = run_test("random_write", stride_test_size, pattern_random, &rnd_ctx);
        print_result(&r);
        
        free(indices);
    } else {
        printf("Error: Failed to allocate random index buffer\n");
    }

    printf("\n==============================================================\n");
    printf("    Benchmark Complete\n");
    printf("==============================================================\n");

    return 0;
}