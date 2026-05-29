#include <errno.h>
#if !defined(__linux__) && !defined(__APPLE__)
#error "Ciel runtime prelude currently supports only Linux and macOS targets"
#else

#if !defined(GC_THREADS)
#define GC_THREADS 1
#endif
#if !defined(GC_NO_THREAD_REDIRECTS)
#define GC_NO_THREAD_REDIRECTS 1
#endif
#include <botan/ffi.h>
#include <gc/gc.h>
#include <limits.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <arpa/inet.h>
#include <dispatch/dispatch.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <pthread.h>
#include <stdatomic.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#if defined(__GNUC__) || defined(__clang__)
#define CIEL_MAYBE_UNUSED __attribute__((unused))
#else
#define CIEL_MAYBE_UNUSED
#endif

#define CIEL_PANIC_EXIT_CODE 101

#if defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L
#define CIEL_ALIGNOF(T) _Alignof(T)
#elif defined(__GNUC__) || defined(__clang__)
#define CIEL_ALIGNOF(T) __alignof__(T)
#else
#define CIEL_ALIGNOF(T) sizeof(void *)
#endif

typedef struct {
    uint8_t *ptr;
    size_t len;
} CielSlice_u8;

typedef struct {
    const char *ptr;
    size_t len;
} CielConstSlice_char;

#define CIEL_CONST_STR(S)                                                      \
    ((CielConstSlice_char){.ptr = (S), .len = sizeof(S) - 1})

static int ciel_runtime_initialized = 0;
static int ciel_runtime_argc = 0;
static char **ciel_runtime_argv = NULL;
static __thread int ciel_runtime_callback_depth = 0;
static __thread int ciel_runtime_callback_registered = 0;

void ciel_runtime_init(void) {
    if (ciel_runtime_initialized)
        return;
    ciel_runtime_initialized = 1;
    GC_INIT();
#if defined(GC_THREADS)
    GC_allow_register_threads();
#endif
}

void ciel_runtime_set_args(int argc, char **argv) {
    ciel_runtime_argc = argc < 0 ? 0 : argc;
    ciel_runtime_argv = argv;
}

int ciel_env_args_len(size_t *out) {
    if (out == NULL)
        return EINVAL;
    if (ciel_runtime_argc < 0)
        return EIO;
    *out = (size_t)ciel_runtime_argc;
    return 0;
}

CielConstSlice_char ciel_env_arg_unchecked(size_t index) {
    static const char empty[] = "";
    CielConstSlice_char out;
    if (ciel_runtime_argc < 0 || ciel_runtime_argv == NULL ||
        index >= (size_t)ciel_runtime_argc ||
        ciel_runtime_argv[index] == NULL) {
        out.ptr = empty;
        out.len = 0;
        return out;
    }
    out.ptr = ciel_runtime_argv[index];
    out.len = strlen(ciel_runtime_argv[index]);
    return out;
}

static CIEL_MAYBE_UNUSED void *ciel_alloc(size_t size) {
    ciel_runtime_init();
    void *ptr = GC_MALLOC(size);
    if (ptr == NULL) {
        fputs("out of memory\n", stderr);
        exit(0);
    }
    return ptr;
}

static CIEL_MAYBE_UNUSED void *ciel_alloc_array(size_t elem_size, size_t len) {
    if (elem_size != 0 && len > SIZE_MAX / elem_size) {
        fputs("allocation size overflow\n", stderr);
        exit(0);
    }
    size_t bytes = elem_size * len;
    return ciel_alloc(bytes == 0 ? 1 : bytes);
}

CielSlice_u8 ciel_runtime_u8_alloc_slice(size_t len) {
    CielSlice_u8 out;
    out.ptr = (uint8_t *)ciel_alloc_array(sizeof(uint8_t), len);
    out.len = len;
    return out;
}

static CIEL_MAYBE_UNUSED void *ciel_alloc_uncollectable(size_t size) {
    ciel_runtime_init();
    void *ptr = GC_MALLOC_UNCOLLECTABLE(size == 0 ? 1 : size);
    if (ptr == NULL) {
        fputs("out of memory\n", stderr);
        exit(0);
    }
    memset(ptr, 0, size == 0 ? 1 : size);
    return ptr;
}

static CIEL_MAYBE_UNUSED void *ciel_box_value(const void *value, size_t size) {
    void *out = ciel_alloc(size == 0 ? 1 : size);
    if (size > 0)
        memcpy(out, value, size);
    return out;
}

void *ciel_box_copy(size_t size, size_t align, const void *source) {
    (void)align;
    if (source == NULL && size > 0) {
        errno = EINVAL;
        return NULL;
    }
    void *out = ciel_alloc(size == 0 ? 1 : size);
    if (size > 0)
        memcpy(out, source, size);
    return out;
}

int ciel_u8_copy(uint8_t *dst, const uint8_t *src, size_t len) {
    if ((dst == NULL || src == NULL) && len > 0) {
        errno = EINVAL;
        return EINVAL;
    }
    if (len > 0)
        memmove(dst, src, len);
    return 0;
}

void ciel_panic_at(const char *message, size_t len, const char *file,
                   size_t line) {
    fputs("panic", stderr);
    if (file != NULL && file[0] != '\0')
        fprintf(stderr, " at %s:%zu", file, line);
    fputs(": ", stderr);
    if (message != NULL && len > 0)
        fwrite(message, 1, len, stderr);
    if (message == NULL || len == 0 || message[len - 1] != '\n')
        fputc('\n', stderr);
    exit(CIEL_PANIC_EXIT_CODE);
}

void ciel_panic(const char *message, size_t len) {
    ciel_panic_at(message, len, "<runtime>", 0);
}

int ciel_errno(void) { return errno; }

char *ciel_cstr_from_slice(const char *ptr, size_t len) {
    char *out = (char *)ciel_alloc_array(sizeof(char), len + 1);
    for (size_t i = 0; i < len; i++)
        out[i] = ptr[i];
    out[len] = '\0';
    return out;
}

static CIEL_MAYBE_UNUSED size_t ciel_format_float(char *out, size_t cap,
                                                  const char *fmt,
                                                  double value) {
    int written = snprintf(out, cap, fmt, value);
    if (written < 0) {
        if (cap > 0)
            out[0] = '\0';
        return 0;
    }
    if ((size_t)written >= cap) {
        return cap > 0 ? cap - 1 : 0;
    }
    return (size_t)written;
}

size_t ciel_f32_to_string(float value, char *out, size_t cap) {
    return ciel_format_float(out, cap, "%.9g", (double)value);
}

size_t ciel_f64_to_string(double value, char *out, size_t cap) {
    return ciel_format_float(out, cap, "%.17g", value);
}

int ciel_io_open_read(const char *path) { return open(path, O_RDONLY); }

int ciel_io_open_write(const char *path) {
    return open(path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
}

int ciel_io_open_append(const char *path) {
    return open(path, O_WRONLY | O_CREAT | O_APPEND, 0666);
}

#ifndef NDEBUG
#define CIEL_DEFINE_BINOP(NAME, BUILTIN, OP, SUFFIX, C_TY, U_TY)               \
    static CIEL_MAYBE_UNUSED C_TY ciel_##NAME##_##SUFFIX(                      \
        C_TY lhs, C_TY rhs, char *file, size_t line) {                         \
        C_TY out;                                                              \
        if (BUILTIN(lhs, rhs, &out))                                           \
            ciel_panic_at("integer overflow", 16, file, line);                 \
        return out;                                                            \
    }
#else
#define CIEL_DEFINE_BINOP(NAME, BUILTIN, OP, SUFFIX, C_TY, U_TY)               \
    static CIEL_MAYBE_UNUSED C_TY ciel_##NAME##_##SUFFIX(                      \
        C_TY lhs, C_TY rhs, char *file, size_t line) {                         \
        (void)file;                                                            \
        (void)line;                                                            \
        return (C_TY)((U_TY)lhs OP(U_TY) rhs);                                 \
    }
#endif

#ifndef NDEBUG
#define CIEL_DEFINE_SIGNED_NEG(SUFFIX, C_TY, U_TY, MIN_VALUE)                  \
    static CIEL_MAYBE_UNUSED C_TY ciel_neg_##SUFFIX(C_TY value, char *file,    \
                                                    size_t line) {             \
        if (value == (C_TY)MIN_VALUE)                                          \
            ciel_panic_at("integer overflow", 16, file, line);                 \
        return (C_TY)(((U_TY)0) - (U_TY)value);                                \
    }
#else
#define CIEL_DEFINE_SIGNED_NEG(SUFFIX, C_TY, U_TY, MIN_VALUE)                  \
    static CIEL_MAYBE_UNUSED C_TY ciel_neg_##SUFFIX(C_TY value, char *file,    \
                                                    size_t line) {             \
        (void)file;                                                            \
        (void)line;                                                            \
        return (C_TY)(((U_TY)0) - (U_TY)value);                                \
    }
#endif

#ifndef NDEBUG
#define CIEL_SIGNED_DIV_OVERFLOW_CHECK(C_TY, MIN_VALUE, lhs, rhs, file, line)  \
    do {                                                                       \
        if ((lhs) == (C_TY)MIN_VALUE && (rhs) == (C_TY) - 1)                   \
            ciel_panic_at("integer overflow", 16, file, line);                 \
    } while (0)
#else
#define CIEL_SIGNED_DIV_OVERFLOW_CHECK(C_TY, MIN_VALUE, lhs, rhs, file, line)  \
    do {                                                                       \
        (void)file;                                                            \
        (void)line;                                                            \
    } while (0)
#endif

#define CIEL_DEFINE_SIGNED_DIV_REM(SUFFIX, C_TY, MIN_VALUE)                    \
    static CIEL_MAYBE_UNUSED C_TY ciel_div_##SUFFIX(C_TY lhs, C_TY rhs,        \
                                                    char *file, size_t line) { \
        if (rhs == 0)                                                          \
            ciel_panic_at("division by zero", 16, file, line);                 \
        CIEL_SIGNED_DIV_OVERFLOW_CHECK(C_TY, MIN_VALUE, lhs, rhs, file, line); \
        if (lhs == (C_TY)MIN_VALUE && rhs == (C_TY) - 1)                       \
            return lhs;                                                        \
        return lhs / rhs;                                                      \
    }                                                                          \
    static CIEL_MAYBE_UNUSED C_TY ciel_rem_##SUFFIX(C_TY lhs, C_TY rhs,        \
                                                    char *file, size_t line) { \
        if (rhs == 0)                                                          \
            ciel_panic_at("division by zero", 16, file, line);                 \
        CIEL_SIGNED_DIV_OVERFLOW_CHECK(C_TY, MIN_VALUE, lhs, rhs, file, line); \
        if (lhs == (C_TY)MIN_VALUE && rhs == (C_TY) - 1)                       \
            return 0;                                                          \
        return lhs % rhs;                                                      \
    }

#define CIEL_DEFINE_UNSIGNED_DIV_REM(SUFFIX, C_TY)                             \
    static CIEL_MAYBE_UNUSED C_TY ciel_div_##SUFFIX(C_TY lhs, C_TY rhs,        \
                                                    char *file, size_t line) { \
        if (rhs == 0)                                                          \
            ciel_panic_at("division by zero", 16, file, line);                 \
        (void)file;                                                            \
        (void)line;                                                            \
        return lhs / rhs;                                                      \
    }                                                                          \
    static CIEL_MAYBE_UNUSED C_TY ciel_rem_##SUFFIX(C_TY lhs, C_TY rhs,        \
                                                    char *file, size_t line) { \
        if (rhs == 0)                                                          \
            ciel_panic_at("division by zero", 16, file, line);                 \
        (void)file;                                                            \
        (void)line;                                                            \
        return lhs % rhs;                                                      \
    }

#define CIEL_DEFINE_SHIFTS(SUFFIX, C_TY, U_TY, BITS)                           \
    static CIEL_MAYBE_UNUSED C_TY ciel_shl_##SUFFIX(C_TY lhs, uintmax_t rhs,   \
                                                    char *file, size_t line) { \
        if (rhs >= (uintmax_t)(BITS))                                          \
            ciel_panic_at("shift count out of range", 24, file, line);         \
        return (C_TY)((U_TY)lhs << rhs);                                       \
    }                                                                          \
    static CIEL_MAYBE_UNUSED C_TY ciel_shr_##SUFFIX(C_TY lhs, uintmax_t rhs,   \
                                                    char *file, size_t line) { \
        if (rhs >= (uintmax_t)(BITS))                                          \
            ciel_panic_at("shift count out of range", 24, file, line);         \
        return (C_TY)(lhs >> rhs);                                             \
    }

#define CIEL_DEFINE_UNSIGNED_SHIFTS(SUFFIX, C_TY, BITS)                        \
    static CIEL_MAYBE_UNUSED C_TY ciel_shl_##SUFFIX(C_TY lhs, uintmax_t rhs,   \
                                                    char *file, size_t line) { \
        if (rhs >= (uintmax_t)(BITS))                                          \
            ciel_panic_at("shift count out of range", 24, file, line);         \
        return (C_TY)(lhs << rhs);                                             \
    }                                                                          \
    static CIEL_MAYBE_UNUSED C_TY ciel_shr_##SUFFIX(C_TY lhs, uintmax_t rhs,   \
                                                    char *file, size_t line) { \
        if (rhs >= (uintmax_t)(BITS))                                          \
            ciel_panic_at("shift count out of range", 24, file, line);         \
        return (C_TY)(lhs >> rhs);                                             \
    }

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, i8, int8_t, uint8_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, i8, int8_t, uint8_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, i8, int8_t, uint8_t)
CIEL_DEFINE_SIGNED_NEG(i8, int8_t, uint8_t, INT8_MIN)
CIEL_DEFINE_SIGNED_DIV_REM(i8, int8_t, INT8_MIN)
CIEL_DEFINE_SHIFTS(i8, int8_t, uint8_t, 8)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, i16, int16_t, uint16_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, i16, int16_t, uint16_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, i16, int16_t, uint16_t)
CIEL_DEFINE_SIGNED_NEG(i16, int16_t, uint16_t, INT16_MIN)
CIEL_DEFINE_SIGNED_DIV_REM(i16, int16_t, INT16_MIN)
CIEL_DEFINE_SHIFTS(i16, int16_t, uint16_t, 16)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, i32, int32_t, uint32_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, i32, int32_t, uint32_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, i32, int32_t, uint32_t)
CIEL_DEFINE_SIGNED_NEG(i32, int32_t, uint32_t, INT32_MIN)
CIEL_DEFINE_SIGNED_DIV_REM(i32, int32_t, INT32_MIN)
CIEL_DEFINE_SHIFTS(i32, int32_t, uint32_t, 32)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, i64, int64_t, uint64_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, i64, int64_t, uint64_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, i64, int64_t, uint64_t)
CIEL_DEFINE_SIGNED_NEG(i64, int64_t, uint64_t, INT64_MIN)
CIEL_DEFINE_SIGNED_DIV_REM(i64, int64_t, INT64_MIN)
CIEL_DEFINE_SHIFTS(i64, int64_t, uint64_t, 64)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, u8, uint8_t, uint8_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, u8, uint8_t, uint8_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, u8, uint8_t, uint8_t)
CIEL_DEFINE_UNSIGNED_DIV_REM(u8, uint8_t)
CIEL_DEFINE_UNSIGNED_SHIFTS(u8, uint8_t, 8)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, u16, uint16_t, uint16_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, u16, uint16_t, uint16_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, u16, uint16_t, uint16_t)
CIEL_DEFINE_UNSIGNED_DIV_REM(u16, uint16_t)
CIEL_DEFINE_UNSIGNED_SHIFTS(u16, uint16_t, 16)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, u32, uint32_t, uint32_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, u32, uint32_t, uint32_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, u32, uint32_t, uint32_t)
CIEL_DEFINE_UNSIGNED_DIV_REM(u32, uint32_t)
CIEL_DEFINE_UNSIGNED_SHIFTS(u32, uint32_t, 32)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, u64, uint64_t, uint64_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, u64, uint64_t, uint64_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, u64, uint64_t, uint64_t)
CIEL_DEFINE_UNSIGNED_DIV_REM(u64, uint64_t)
CIEL_DEFINE_UNSIGNED_SHIFTS(u64, uint64_t, 64)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, usize, size_t, size_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, usize, size_t, size_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, usize, size_t, size_t)
CIEL_DEFINE_UNSIGNED_DIV_REM(usize, size_t)
CIEL_DEFINE_UNSIGNED_SHIFTS(usize, size_t, sizeof(size_t) * CHAR_BIT)

#undef CIEL_DEFINE_BINOP
#undef CIEL_DEFINE_SIGNED_NEG
#undef CIEL_SIGNED_DIV_OVERFLOW_CHECK
#undef CIEL_DEFINE_SIGNED_DIV_REM
#undef CIEL_DEFINE_UNSIGNED_DIV_REM
#undef CIEL_DEFINE_SHIFTS
#undef CIEL_DEFINE_UNSIGNED_SHIFTS

static CIEL_MAYBE_UNUSED size_t ciel_bounds_check(size_t index, size_t len,
                                                  char *file, size_t line) {
    if (index >= len)
        ciel_panic_at("index out of bounds", 19, file, line);
    return index;
}

static CIEL_MAYBE_UNUSED size_t ciel_slice_range_check(size_t start, size_t end,
                                                       size_t len, char *file,
                                                       size_t line) {
    if (start > end || end > len)
        ciel_panic_at("slice range out of bounds", 25, file, line);
    return start;
}

typedef struct CielRoot {
    void *ptr;
} CielRoot;

static int32_t ciel_thread_attach_status(int *registered) {
    if (registered != NULL)
        *registered = 0;
    ciel_runtime_init();
#if defined(GC_THREADS)
    struct GC_stack_base stack_base;
    if (GC_get_stack_base(&stack_base) != GC_SUCCESS)
        return 1;
    int result = GC_register_my_thread(&stack_base);
    if (result == GC_SUCCESS) {
        if (registered != NULL)
            *registered = 1;
        return 0;
    }
    if (result == GC_DUPLICATE)
        return 0;
    return result;
#else
    return 0;
#endif
}

int32_t ciel_thread_attach(void) { return ciel_thread_attach_status(NULL); }

void ciel_thread_detach(void) {
#if defined(GC_THREADS)
    GC_unregister_my_thread();
#endif
}

int32_t ciel_runtime_enter_callback(void) {
    if (ciel_runtime_callback_depth == 0) {
        int registered = 0;
        int32_t rc = ciel_thread_attach_status(&registered);
        if (rc != 0)
            return rc;
        ciel_runtime_callback_registered = registered;
    }
    ciel_runtime_callback_depth++;
    return 0;
}

void ciel_runtime_leave_callback(void) {
    if (ciel_runtime_callback_depth <= 0)
        return;
    ciel_runtime_callback_depth--;
    if (ciel_runtime_callback_depth == 0 && ciel_runtime_callback_registered) {
        ciel_runtime_callback_registered = 0;
        ciel_thread_detach();
    }
}

CielRoot *ciel_root_pin(void *ptr) {
    ciel_runtime_init();
    CielRoot *root = (CielRoot *)GC_MALLOC_UNCOLLECTABLE(sizeof(CielRoot));
    if (root == NULL) {
        fputs("out of memory\n", stderr);
        exit(0);
    }
    root->ptr = ptr;
    return root;
}

void *ciel_root_get(CielRoot *root) { return root == NULL ? NULL : root->ptr; }

void ciel_root_unpin(CielRoot *root) {
    if (root != NULL)
        GC_FREE(root);
}

int32_t ciel_time_monotonic_ms(uint64_t *out) {
    if (out == NULL)
        return EINVAL;
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0)
        return errno == 0 ? EIO : errno;
    if (now.tv_sec < 0)
        return EIO;
    uint64_t sec = (uint64_t)now.tv_sec;
    uint64_t msec = (uint64_t)(now.tv_nsec / 1000000L);
    if (sec > (UINT64_MAX - msec) / 1000ULL)
        return EOVERFLOW;
    *out = sec * 1000ULL + msec;
    return 0;
}

int32_t ciel_time_sleep_ms(uint64_t ms) {
    uint64_t seconds = ms / 1000ULL;
    if (seconds > (uint64_t)LONG_MAX)
        return EOVERFLOW;
    struct timespec remaining;
    remaining.tv_sec = (time_t)seconds;
    remaining.tv_nsec = (long)((ms % 1000ULL) * 1000000ULL);

    while (nanosleep(&remaining, &remaining) != 0) {
        if (errno != EINTR)
            return errno == 0 ? EIO : errno;
    }
    return 0;
}

typedef struct CielSocketAddr CielSocketAddr;

struct CielSocketAddr {
    struct sockaddr_storage storage;
    socklen_t len;
};

typedef enum {
    CIEL_NET_SLOT_FREE = 0,
    CIEL_NET_SLOT_LISTENER = 1,
    CIEL_NET_SLOT_STREAM = 2,
} CielNetSlotKind;

typedef struct CielNetSlot {
    int fd;
    uint32_t generation;
    CielNetSlotKind kind;
    uint32_t next_free;
} CielNetSlot;

static pthread_mutex_t ciel_net_table_mutex = PTHREAD_MUTEX_INITIALIZER;
static CielNetSlot *ciel_net_slots = NULL;
static uint32_t ciel_net_slot_count = 0;
static uint32_t ciel_net_slot_cap = 0;
static uint32_t ciel_net_free_head = UINT32_MAX;

static int32_t ciel_net_table_grow(void) {
    uint32_t old_cap = ciel_net_slot_cap;
    uint32_t new_cap = old_cap == 0 ? 16 : old_cap * 2;
    CielNetSlot *next = (CielNetSlot *)GC_MALLOC_UNCOLLECTABLE(
        sizeof(CielNetSlot) * (size_t)new_cap);
    if (next == NULL)
        return ENOMEM;
    memset(next, 0, sizeof(CielNetSlot) * (size_t)new_cap);
    if (ciel_net_slots != NULL) {
        memcpy(next, ciel_net_slots, sizeof(CielNetSlot) * (size_t)old_cap);
        GC_FREE(ciel_net_slots);
    }
    ciel_net_slots = next;
    ciel_net_slot_cap = new_cap;
    return 0;
}

static int32_t ciel_net_slot_insert_locked(int fd, CielNetSlotKind kind,
                                           uint32_t *out_slot,
                                           uint32_t *out_generation) {
    uint32_t slot;
    if (ciel_net_free_head != UINT32_MAX) {
        slot = ciel_net_free_head;
        ciel_net_free_head = ciel_net_slots[slot].next_free;
    } else {
        if (ciel_net_slot_count == ciel_net_slot_cap) {
            int32_t rc = ciel_net_table_grow();
            if (rc != 0)
                return rc;
        }
        slot = ciel_net_slot_count++;
        if (ciel_net_slots[slot].generation == 0)
            ciel_net_slots[slot].generation = 1;
    }
    ciel_net_slots[slot].fd = fd;
    ciel_net_slots[slot].kind = kind;
    ciel_net_slots[slot].next_free = UINT32_MAX;
    *out_slot = slot;
    *out_generation = ciel_net_slots[slot].generation;
    return 0;
}

static int32_t ciel_net_resolve_locked(uint32_t slot, uint32_t generation,
                                       CielNetSlotKind kind,
                                       CielNetSlot **out) {
    if (out == NULL)
        return EINVAL;
    if (slot >= ciel_net_slot_count)
        return EBADF;
    CielNetSlot *entry = &ciel_net_slots[slot];
    if (entry->generation != generation || entry->kind != kind)
        return EBADF;
    *out = entry;
    return 0;
}

static int32_t ciel_net_fd_snapshot(uint32_t slot, uint32_t generation,
                                    CielNetSlotKind kind, int *fd) {
    if (fd == NULL)
        return EINVAL;
    pthread_mutex_lock(&ciel_net_table_mutex);
    CielNetSlot *entry = NULL;
    int32_t rc = ciel_net_resolve_locked(slot, generation, kind, &entry);
    if (rc == 0)
        *fd = entry->fd;
    pthread_mutex_unlock(&ciel_net_table_mutex);
    return rc;
}

static int32_t ciel_net_slot_close(uint32_t slot, uint32_t generation,
                                   CielNetSlotKind kind) {
    pthread_mutex_lock(&ciel_net_table_mutex);
    CielNetSlot *entry = NULL;
    int32_t rc = ciel_net_resolve_locked(slot, generation, kind, &entry);
    if (rc != 0) {
        pthread_mutex_unlock(&ciel_net_table_mutex);
        return rc;
    }
    int fd = entry->fd;
    entry->fd = -1;
    entry->kind = CIEL_NET_SLOT_FREE;
    entry->generation =
        entry->generation == UINT32_MAX ? 1 : entry->generation + 1;
    entry->next_free = ciel_net_free_head;
    ciel_net_free_head = slot;
    pthread_mutex_unlock(&ciel_net_table_mutex);
    if (close(fd) != 0)
        return errno == 0 ? EIO : errno;
    return 0;
}

static int32_t ciel_net_gai_error(int rc) {
    switch (rc) {
    case 0:
        return 0;
    case EAI_MEMORY:
        return ENOMEM;
    case EAI_FAMILY:
        return EAFNOSUPPORT;
    case EAI_NONAME:
        return ENOENT;
    case EAI_SERVICE:
        return EINVAL;
    case EAI_AGAIN:
        return EAGAIN;
    default:
        return EIO;
    }
}

static CielSocketAddr *ciel_net_addr_copy(const struct sockaddr *addr,
                                          socklen_t len) {
    if (addr == NULL || len == 0 || len > sizeof(struct sockaddr_storage)) {
        errno = EINVAL;
        return NULL;
    }
    CielSocketAddr *out =
        (CielSocketAddr *)ciel_alloc_uncollectable(sizeof(CielSocketAddr));
    memcpy(&out->storage, addr, len);
    out->len = len;
    return out;
}

static CielSocketAddr *ciel_net_addr_from_fd(int fd, int peer,
                                             int32_t *out_rc) {
    struct sockaddr_storage storage;
    socklen_t len = (socklen_t)sizeof(storage);
    int rc = peer ? getpeername(fd, (struct sockaddr *)&storage, &len)
                  : getsockname(fd, (struct sockaddr *)&storage, &len);
    if (rc != 0) {
        if (out_rc != NULL)
            *out_rc = errno == 0 ? EIO : errno;
        return NULL;
    }
    CielSocketAddr *addr = ciel_net_addr_copy((struct sockaddr *)&storage, len);
    if (addr == NULL && out_rc != NULL)
        *out_rc = errno == 0 ? ENOMEM : errno;
    return addr;
}

static int ciel_net_make_socket(const struct sockaddr *addr) {
    int fd = socket(addr->sa_family, SOCK_STREAM, 0);
    if (fd < 0)
        return -1;
#if defined(SO_NOSIGPIPE)
    int one = 1;
    (void)setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof(one));
#endif
    return fd;
}

static int32_t ciel_net_parse_port(const char *text, size_t len,
                                   char *out_service, size_t out_service_cap) {
    if (text == NULL || len == 0 || out_service == NULL ||
        out_service_cap == 0 || len >= out_service_cap)
        return EINVAL;
    uint32_t value = 0;
    for (size_t i = 0; i < len; i++) {
        unsigned char ch = (unsigned char)text[i];
        if (ch < '0' || ch > '9')
            return EINVAL;
        value = value * 10U + (uint32_t)(ch - '0');
        if (value > 65535U)
            return ERANGE;
    }
    memcpy(out_service, text, len);
    out_service[len] = '\0';
    return 0;
}

static int32_t ciel_net_parse_endpoint(const char *text, size_t text_len,
                                       char **out_host, char *out_service,
                                       size_t out_service_cap) {
    if (text == NULL || text_len == 0 || out_host == NULL ||
        out_service == NULL)
        return EINVAL;

    size_t host_start = 0;
    size_t host_len = 0;
    size_t port_start = 0;
    size_t port_len = 0;

    if (text[0] == '[') {
        size_t close = 1;
        while (close < text_len && text[close] != ']')
            close++;
        if (close >= text_len || close + 1 >= text_len ||
            text[close + 1] != ':')
            return EINVAL;
        host_start = 1;
        host_len = close - 1;
        port_start = close + 2;
        port_len = text_len - port_start;
    } else {
        size_t colon = text_len;
        for (size_t i = 0; i < text_len; i++) {
            if (text[i] == ':') {
                if (colon != text_len)
                    return EINVAL;
                colon = i;
            }
        }
        if (colon == text_len)
            return EINVAL;
        host_start = 0;
        host_len = colon;
        port_start = colon + 1;
        port_len = text_len - port_start;
    }

    if (host_len == 0 || port_len == 0)
        return EINVAL;
    int32_t rc = ciel_net_parse_port(text + port_start, port_len, out_service,
                                     out_service_cap);
    if (rc != 0)
        return rc;

    char *host = (char *)ciel_alloc_array(sizeof(char), host_len + 1);
    memcpy(host, text + host_start, host_len);
    host[host_len] = '\0';
    *out_host = host;
    return 0;
}

static int32_t ciel_net_addr_from_name(const char *host, const char *service,
                                       int flags, CielSocketAddr **out) {
    if (host == NULL || service == NULL || out == NULL)
        return EINVAL;
    struct addrinfo hints;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    hints.ai_flags = flags;

    struct addrinfo *results = NULL;
    int rc = getaddrinfo(host, service, &hints, &results);
    if (rc != 0)
        return ciel_net_gai_error(rc);

    int32_t out_rc = ENOENT;
    for (struct addrinfo *it = results; it != NULL; it = it->ai_next) {
        if (it->ai_family != AF_INET && it->ai_family != AF_INET6)
            continue;
        CielSocketAddr *addr = ciel_net_addr_copy(it->ai_addr, it->ai_addrlen);
        if (addr == NULL) {
            out_rc = errno == 0 ? ENOMEM : errno;
            break;
        }
        *out = addr;
        out_rc = 0;
        break;
    }
    freeaddrinfo(results);
    return out_rc;
}

int32_t ciel_net_parse_addr(const char *text, size_t text_len,
                            CielSocketAddr **out) {
    char service[6];
    char *host = NULL;
    int32_t rc = ciel_net_parse_endpoint(text, text_len, &host, service,
                                         sizeof(service));
    if (rc != 0)
        return rc;
#if defined(AI_NUMERICSERV)
    int flags = AI_NUMERICHOST | AI_NUMERICSERV;
#else
    int flags = AI_NUMERICHOST;
#endif
    return ciel_net_addr_from_name(host, service, flags, out);
}

int32_t ciel_net_resolve_tcp(const char *host, size_t host_len, uint16_t port,
                             CielSocketAddr **out) {
    if (host == NULL || out == NULL)
        return EINVAL;
    char *host_c = ciel_cstr_from_slice(host, host_len);
    char service[6];
    int n = snprintf(service, sizeof(service), "%u", (unsigned)port);
    if (n < 0 || (size_t)n >= sizeof(service))
        return EOVERFLOW;
    return ciel_net_addr_from_name(host_c, service, 0, out);
}

int32_t ciel_net_addr_family(CielSocketAddr *addr, int32_t *out) {
    if (addr == NULL || out == NULL)
        return EINVAL;
    switch (((struct sockaddr *)&addr->storage)->sa_family) {
    case AF_INET:
        *out = 4;
        return 0;
    case AF_INET6:
        *out = 6;
        return 0;
    default:
        return EAFNOSUPPORT;
    }
}

int32_t ciel_net_addr_port(CielSocketAddr *addr, uint16_t *out) {
    if (addr == NULL || out == NULL)
        return EINVAL;
    struct sockaddr *sa = (struct sockaddr *)&addr->storage;
    if (sa->sa_family == AF_INET) {
        *out = ntohs(((struct sockaddr_in *)sa)->sin_port);
        return 0;
    }
    if (sa->sa_family == AF_INET6) {
        *out = ntohs(((struct sockaddr_in6 *)sa)->sin6_port);
        return 0;
    }
    return EAFNOSUPPORT;
}

int32_t ciel_net_addr_write(CielSocketAddr *addr, char *out, size_t cap,
                            size_t *written) {
    if (addr == NULL || written == NULL || (out == NULL && cap > 0))
        return EINVAL;
    char host[INET6_ADDRSTRLEN];
    uint16_t port = 0;
    struct sockaddr *sa = (struct sockaddr *)&addr->storage;
    const void *source = NULL;
    int family = sa->sa_family;
    if (family == AF_INET) {
        struct sockaddr_in *in = (struct sockaddr_in *)sa;
        source = &in->sin_addr;
        port = ntohs(in->sin_port);
    } else if (family == AF_INET6) {
        struct sockaddr_in6 *in6 = (struct sockaddr_in6 *)sa;
        source = &in6->sin6_addr;
        port = ntohs(in6->sin6_port);
    } else {
        return EAFNOSUPPORT;
    }
    if (inet_ntop(family, source, host, sizeof(host)) == NULL)
        return errno == 0 ? EIO : errno;

    char tmp[INET6_ADDRSTRLEN + 10];
    int n = 0;
    if (family == AF_INET6)
        n = snprintf(tmp, sizeof(tmp), "[%s]:%u", host, (unsigned)port);
    else
        n = snprintf(tmp, sizeof(tmp), "%s:%u", host, (unsigned)port);
    if (n < 0)
        return EIO;
    size_t need = (size_t)n;
    if (need >= sizeof(tmp))
        return EOVERFLOW;
    *written = need;
    if (cap <= need)
        return ENOSPC;
    memcpy(out, tmp, need);
    out[need] = '\0';
    return 0;
}

int32_t ciel_net_tcp_listen(CielSocketAddr *addr, uint32_t *out_slot,
                            uint32_t *out_generation) {
    if (addr == NULL || out_slot == NULL || out_generation == NULL)
        return EINVAL;
    struct sockaddr *sa = (struct sockaddr *)&addr->storage;
    int fd = ciel_net_make_socket(sa);
    if (fd < 0)
        return errno == 0 ? EIO : errno;
    int one = 1;
    (void)setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
    if (bind(fd, sa, addr->len) != 0) {
        int err = errno == 0 ? EIO : errno;
        close(fd);
        return err;
    }
    if (listen(fd, 128) != 0) {
        int err = errno == 0 ? EIO : errno;
        close(fd);
        return err;
    }
    pthread_mutex_lock(&ciel_net_table_mutex);
    int32_t rc = ciel_net_slot_insert_locked(fd, CIEL_NET_SLOT_LISTENER,
                                             out_slot, out_generation);
    pthread_mutex_unlock(&ciel_net_table_mutex);
    if (rc != 0)
        close(fd);
    return rc;
}

int32_t ciel_net_tcp_accept(uint32_t listener_slot,
                            uint32_t listener_generation, uint32_t *out_slot,
                            uint32_t *out_generation) {
    if (out_slot == NULL || out_generation == NULL)
        return EINVAL;
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(listener_slot, listener_generation,
                                      CIEL_NET_SLOT_LISTENER, &fd);
    if (rc != 0)
        return rc;
    int accepted;
    do {
        accepted = accept(fd, NULL, NULL);
    } while (accepted < 0 && errno == EINTR);
    if (accepted < 0)
        return errno == 0 ? EIO : errno;
#if defined(SO_NOSIGPIPE)
    int one = 1;
    (void)setsockopt(accepted, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof(one));
#endif
    pthread_mutex_lock(&ciel_net_table_mutex);
    rc = ciel_net_slot_insert_locked(accepted, CIEL_NET_SLOT_STREAM, out_slot,
                                     out_generation);
    pthread_mutex_unlock(&ciel_net_table_mutex);
    if (rc != 0)
        close(accepted);
    return rc;
}

static int32_t ciel_net_connect_fd(const struct sockaddr *addr, socklen_t len,
                                   int *out_fd) {
    int fd = ciel_net_make_socket(addr);
    if (fd < 0)
        return errno == 0 ? EIO : errno;
    while (connect(fd, addr, len) != 0) {
        if (errno == EINTR)
            continue;
        int err = errno == 0 ? EIO : errno;
        close(fd);
        return err;
    }
    *out_fd = fd;
    return 0;
}

static int32_t ciel_net_insert_connected_fd(int fd, uint32_t *out_slot,
                                            uint32_t *out_generation) {
    pthread_mutex_lock(&ciel_net_table_mutex);
    int32_t rc = ciel_net_slot_insert_locked(fd, CIEL_NET_SLOT_STREAM, out_slot,
                                             out_generation);
    pthread_mutex_unlock(&ciel_net_table_mutex);
    if (rc != 0)
        close(fd);
    return rc;
}

int32_t ciel_net_tcp_connect(CielSocketAddr *addr, uint32_t *out_slot,
                             uint32_t *out_generation) {
    if (addr == NULL || out_slot == NULL || out_generation == NULL)
        return EINVAL;
    int fd = -1;
    int32_t rc =
        ciel_net_connect_fd((struct sockaddr *)&addr->storage, addr->len, &fd);
    if (rc != 0)
        return rc;
    return ciel_net_insert_connected_fd(fd, out_slot, out_generation);
}

int32_t ciel_net_tcp_connect_host(const char *host, size_t host_len,
                                  uint16_t port, uint32_t *out_slot,
                                  uint32_t *out_generation) {
    if (host == NULL || out_slot == NULL || out_generation == NULL)
        return EINVAL;
    char *host_c = ciel_cstr_from_slice(host, host_len);
    char service[6];
    int n = snprintf(service, sizeof(service), "%u", (unsigned)port);
    if (n < 0 || (size_t)n >= sizeof(service))
        return EOVERFLOW;

    struct addrinfo hints;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    struct addrinfo *results = NULL;
    int rc = getaddrinfo(host_c, service, &hints, &results);
    if (rc != 0)
        return ciel_net_gai_error(rc);

    int32_t last = ENOENT;
    for (struct addrinfo *it = results; it != NULL; it = it->ai_next) {
        if (it->ai_family != AF_INET && it->ai_family != AF_INET6)
            continue;
        int fd = -1;
        last = ciel_net_connect_fd(it->ai_addr, it->ai_addrlen, &fd);
        if (last == 0) {
            last = ciel_net_insert_connected_fd(fd, out_slot, out_generation);
            freeaddrinfo(results);
            return last;
        }
    }
    freeaddrinfo(results);
    return last;
}

intptr_t ciel_net_tcp_read(uint32_t stream_slot, uint32_t stream_generation,
                           void *buf, size_t count) {
    if (buf == NULL && count > 0) {
        errno = EINVAL;
        return -1;
    }
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(stream_slot, stream_generation,
                                      CIEL_NET_SLOT_STREAM, &fd);
    if (rc != 0) {
        errno = rc;
        return -1;
    }
    ssize_t n;
    do {
        n = recv(fd, buf, count, 0);
    } while (n < 0 && errno == EINTR);
    return (intptr_t)n;
}

intptr_t ciel_net_tcp_write(uint32_t stream_slot, uint32_t stream_generation,
                            const void *buf, size_t count) {
    if (buf == NULL && count > 0) {
        errno = EINVAL;
        return -1;
    }
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(stream_slot, stream_generation,
                                      CIEL_NET_SLOT_STREAM, &fd);
    if (rc != 0) {
        errno = rc;
        return -1;
    }
#if defined(MSG_NOSIGNAL)
    int flags = MSG_NOSIGNAL;
#else
    int flags = 0;
#endif
    ssize_t n;
    do {
        n = send(fd, buf, count, flags);
    } while (n < 0 && errno == EINTR);
    return (intptr_t)n;
}

int32_t ciel_net_tcp_shutdown_read(uint32_t stream_slot,
                                   uint32_t stream_generation) {
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(stream_slot, stream_generation,
                                      CIEL_NET_SLOT_STREAM, &fd);
    if (rc != 0)
        return rc;
    if (shutdown(fd, SHUT_RD) != 0)
        return errno == ENOTCONN ? 0 : (errno == 0 ? EIO : errno);
    return 0;
}

int32_t ciel_net_tcp_shutdown_write(uint32_t stream_slot,
                                    uint32_t stream_generation) {
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(stream_slot, stream_generation,
                                      CIEL_NET_SLOT_STREAM, &fd);
    if (rc != 0)
        return rc;
    if (shutdown(fd, SHUT_WR) != 0)
        return errno == ENOTCONN ? 0 : (errno == 0 ? EIO : errno);
    return 0;
}

int32_t ciel_net_tcp_shutdown(uint32_t stream_slot,
                              uint32_t stream_generation) {
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(stream_slot, stream_generation,
                                      CIEL_NET_SLOT_STREAM, &fd);
    if (rc != 0)
        return rc;
    if (shutdown(fd, SHUT_RDWR) != 0)
        return errno == ENOTCONN ? 0 : (errno == 0 ? EIO : errno);
    return 0;
}

int32_t ciel_net_tcp_close(uint32_t stream_slot, uint32_t stream_generation) {
    return ciel_net_slot_close(stream_slot, stream_generation,
                               CIEL_NET_SLOT_STREAM);
}

int32_t ciel_net_listener_close(uint32_t listener_slot,
                                uint32_t listener_generation) {
    return ciel_net_slot_close(listener_slot, listener_generation,
                               CIEL_NET_SLOT_LISTENER);
}

int32_t ciel_net_listener_addr(uint32_t listener_slot,
                               uint32_t listener_generation,
                               CielSocketAddr **out) {
    if (out == NULL)
        return EINVAL;
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(listener_slot, listener_generation,
                                      CIEL_NET_SLOT_LISTENER, &fd);
    if (rc != 0)
        return rc;
    CielSocketAddr *addr = ciel_net_addr_from_fd(fd, 0, &rc);
    if (addr == NULL)
        return rc == 0 ? EIO : rc;
    *out = addr;
    return 0;
}

int32_t ciel_net_stream_local_addr(uint32_t stream_slot,
                                   uint32_t stream_generation,
                                   CielSocketAddr **out) {
    if (out == NULL)
        return EINVAL;
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(stream_slot, stream_generation,
                                      CIEL_NET_SLOT_STREAM, &fd);
    if (rc != 0)
        return rc;
    CielSocketAddr *addr = ciel_net_addr_from_fd(fd, 0, &rc);
    if (addr == NULL)
        return rc == 0 ? EIO : rc;
    *out = addr;
    return 0;
}

int32_t ciel_net_stream_peer_addr(uint32_t stream_slot,
                                  uint32_t stream_generation,
                                  CielSocketAddr **out) {
    if (out == NULL)
        return EINVAL;
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(stream_slot, stream_generation,
                                      CIEL_NET_SLOT_STREAM, &fd);
    if (rc != 0)
        return rc;
    CielSocketAddr *addr = ciel_net_addr_from_fd(fd, 1, &rc);
    if (addr == NULL)
        return rc == 0 ? EIO : rc;
    *out = addr;
    return 0;
}

typedef struct CielCryptoRng CielCryptoRng;
typedef struct CielCryptoHash CielCryptoHash;
typedef struct CielCryptoMac CielCryptoMac;

#define CIEL_CRYPTO_MAX_ALGORITHM_LEN 128
#define CIEL_CRYPTO_MIN_MAC_KEY_LEN 16

CielConstSlice_char ciel_crypto_error_message(int32_t code) {
    const char *message = NULL;
    if (code == BOTAN_FFI_ERROR_EXCEPTION_THROWN)
        message = botan_error_last_exception_message();
    if (message == NULL || message[0] == '\0')
        message = botan_error_description(code);
    if (message == NULL || message[0] == '\0')
        message = "Unknown Botan error";
    size_t len = strlen(message);
    char *copy = ciel_cstr_from_slice(message, len);
    return (CielConstSlice_char){.ptr = copy, .len = len};
}

struct CielCryptoRng {
    botan_rng_t rng;
};

struct CielCryptoHash {
    botan_hash_t hash;
};

struct CielCryptoMac {
    botan_mac_t mac;
};

static int32_t ciel_crypto_check_input(const void *ptr, size_t len) {
    return (ptr == NULL && len > 0) ? EINVAL : 0;
}

static int32_t ciel_crypto_check_output(const void *ptr, size_t len) {
    return (ptr == NULL && len > 0) ? EINVAL : 0;
}

static const uint8_t *ciel_crypto_input_ptr(const uint8_t *ptr, size_t len) {
    static const uint8_t empty = 0;
    return len == 0 && ptr == NULL ? &empty : ptr;
}

static int32_t ciel_crypto_algorithm_cstr(const char *algorithm,
                                          size_t algorithm_len, char **out) {
    if (out == NULL)
        return EINVAL;
    *out = NULL;
    if (algorithm == NULL || algorithm_len == 0 ||
        algorithm_len > CIEL_CRYPTO_MAX_ALGORITHM_LEN)
        return EINVAL;
    for (size_t i = 0; i < algorithm_len; i++) {
        if (algorithm[i] == '\0')
            return EINVAL;
    }
    *out = ciel_cstr_from_slice(algorithm, algorithm_len);
    return 0;
}

static void ciel_crypto_rng_finalizer(void *obj, void *client_data) {
    (void)client_data;
    CielCryptoRng *ctx = (CielCryptoRng *)obj;
    if (ctx == NULL)
        return;
    if (ctx->rng != NULL) {
        botan_rng_destroy(ctx->rng);
        ctx->rng = NULL;
    }
}

static void ciel_crypto_hash_finalizer(void *obj, void *client_data) {
    (void)client_data;
    CielCryptoHash *ctx = (CielCryptoHash *)obj;
    if (ctx != NULL && ctx->hash != NULL) {
        botan_hash_destroy(ctx->hash);
        ctx->hash = NULL;
    }
}

static void ciel_crypto_mac_finalizer(void *obj, void *client_data) {
    (void)client_data;
    CielCryptoMac *ctx = (CielCryptoMac *)obj;
    if (ctx != NULL && ctx->mac != NULL) {
        botan_mac_destroy(ctx->mac);
        ctx->mac = NULL;
    }
}

int32_t ciel_crypto_random_bytes(uint8_t *out, size_t out_len) {
    int32_t check = ciel_crypto_check_output(out, out_len);
    if (check != 0)
        return check;
    if (out_len == 0)
        return 0;
    return botan_system_rng_get(out, out_len);
}

int32_t ciel_crypto_system_rng(CielCryptoRng **out) {
    if (out == NULL)
        return EINVAL;
    *out = NULL;
    botan_rng_t rng = NULL;
    int rc = botan_rng_init(&rng, "system");
    if (rc != 0)
        return rc;

    CielCryptoRng *ctx = (CielCryptoRng *)GC_MALLOC(sizeof(CielCryptoRng));
    if (ctx == NULL) {
        botan_rng_destroy(rng);
        return ENOMEM;
    }
    ctx->rng = rng;
    GC_register_finalizer(ctx, ciel_crypto_rng_finalizer, NULL, NULL, NULL);
    *out = ctx;
    return 0;
}

int32_t ciel_crypto_rng_random_bytes(CielCryptoRng *rng, uint8_t *out,
                                     size_t out_len) {
    if (rng == NULL || rng->rng == NULL)
        return EINVAL;
    int32_t check = ciel_crypto_check_output(out, out_len);
    if (check != 0)
        return check;
    if (out_len == 0)
        return 0;

    return botan_rng_get(rng->rng, out, out_len);
}

int32_t ciel_crypto_hash_once(const char *algorithm, size_t algorithm_len,
                              const uint8_t *data, size_t data_len,
                              uint8_t *out, size_t out_len, size_t *written) {
    if (written == NULL)
        return EINVAL;
    *written = 0;
    char *algorithm_name = NULL;
    int32_t algorithm_check =
        ciel_crypto_algorithm_cstr(algorithm, algorithm_len, &algorithm_name);
    if (algorithm_check != 0)
        return algorithm_check;
    int32_t check = ciel_crypto_check_input(data, data_len);
    if (check != 0)
        return check;
    check = ciel_crypto_check_output(out, out_len);
    if (check != 0)
        return check;

    botan_hash_t hash = NULL;
    int rc = botan_hash_init(&hash, algorithm_name, 0);
    if (rc != 0)
        return rc;

    size_t needed = 0;
    rc = botan_hash_output_length(hash, &needed);
    if (rc != 0) {
        botan_hash_destroy(hash);
        return rc;
    }
    if (out_len < needed) {
        *written = needed;
        botan_hash_destroy(hash);
        return ENOBUFS;
    }
    if (data_len > 0) {
        rc = botan_hash_update(hash, ciel_crypto_input_ptr(data, data_len),
                               data_len);
        if (rc != 0) {
            botan_hash_destroy(hash);
            return rc;
        }
    }
    rc = botan_hash_final(hash, out);
    botan_hash_destroy(hash);
    if (rc != 0)
        return rc;
    *written = needed;
    return 0;
}

int32_t ciel_crypto_hash_new(const char *algorithm, size_t algorithm_len,
                             CielCryptoHash **out) {
    if (out == NULL)
        return EINVAL;
    *out = NULL;
    char *algorithm_name = NULL;
    int32_t algorithm_check =
        ciel_crypto_algorithm_cstr(algorithm, algorithm_len, &algorithm_name);
    if (algorithm_check != 0)
        return algorithm_check;
    botan_hash_t hash = NULL;
    int rc = botan_hash_init(&hash, algorithm_name, 0);
    if (rc != 0)
        return rc;

    CielCryptoHash *ctx = (CielCryptoHash *)GC_MALLOC(sizeof(CielCryptoHash));
    if (ctx == NULL) {
        botan_hash_destroy(hash);
        return ENOMEM;
    }
    ctx->hash = hash;
    GC_register_finalizer(ctx, ciel_crypto_hash_finalizer, NULL, NULL, NULL);
    *out = ctx;
    return 0;
}

int32_t ciel_crypto_hash_update(CielCryptoHash *hash, const uint8_t *data,
                                size_t data_len) {
    if (hash == NULL || hash->hash == NULL)
        return EINVAL;
    int32_t check = ciel_crypto_check_input(data, data_len);
    if (check != 0)
        return check;
    if (data_len == 0)
        return 0;
    return botan_hash_update(hash->hash, ciel_crypto_input_ptr(data, data_len),
                             data_len);
}

int32_t ciel_crypto_hash_finish(CielCryptoHash *hash, uint8_t *out,
                                size_t out_len, size_t *written) {
    if (written == NULL)
        return EINVAL;
    *written = 0;
    if (hash == NULL || hash->hash == NULL)
        return EINVAL;
    int32_t check = ciel_crypto_check_output(out, out_len);
    if (check != 0)
        return check;

    size_t needed = 0;
    int rc = botan_hash_output_length(hash->hash, &needed);
    if (rc != 0)
        return rc;
    if (out_len < needed) {
        *written = needed;
        return ENOBUFS;
    }
    rc = botan_hash_final(hash->hash, out);
    if (rc != 0)
        return rc;
    *written = needed;
    return 0;
}

int32_t ciel_crypto_hash_clear(CielCryptoHash *hash) {
    if (hash == NULL || hash->hash == NULL)
        return EINVAL;
    botan_hash_t raw = hash->hash;
    hash->hash = NULL;
    return botan_hash_destroy(raw);
}

int32_t ciel_crypto_mac_once(const char *algorithm, size_t algorithm_len,
                             const uint8_t *key, size_t key_len,
                             const uint8_t *data, size_t data_len, uint8_t *out,
                             size_t out_len, size_t *written) {
    if (written == NULL)
        return EINVAL;
    *written = 0;
    char *algorithm_name = NULL;
    int32_t algorithm_check =
        ciel_crypto_algorithm_cstr(algorithm, algorithm_len, &algorithm_name);
    if (algorithm_check != 0)
        return algorithm_check;
    if (key_len < CIEL_CRYPTO_MIN_MAC_KEY_LEN)
        return EINVAL;
    int32_t check = ciel_crypto_check_input(key, key_len);
    if (check != 0)
        return check;
    check = ciel_crypto_check_input(data, data_len);
    if (check != 0)
        return check;
    check = ciel_crypto_check_output(out, out_len);
    if (check != 0)
        return check;

    botan_mac_t mac = NULL;
    int rc = botan_mac_init(&mac, algorithm_name, 0);
    if (rc != 0)
        return rc;
    rc = botan_mac_set_key(mac, ciel_crypto_input_ptr(key, key_len), key_len);
    if (rc != 0) {
        botan_mac_destroy(mac);
        return rc;
    }

    size_t needed = 0;
    rc = botan_mac_output_length(mac, &needed);
    if (rc != 0) {
        botan_mac_destroy(mac);
        return rc;
    }
    if (out_len < needed) {
        *written = needed;
        botan_mac_destroy(mac);
        return ENOBUFS;
    }
    if (data_len > 0) {
        rc = botan_mac_update(mac, ciel_crypto_input_ptr(data, data_len),
                              data_len);
        if (rc != 0) {
            botan_mac_destroy(mac);
            return rc;
        }
    }
    rc = botan_mac_final(mac, out);
    botan_mac_destroy(mac);
    if (rc != 0)
        return rc;
    *written = needed;
    return 0;
}

int32_t ciel_crypto_mac_new(const char *algorithm, size_t algorithm_len,
                            const uint8_t *key, size_t key_len,
                            CielCryptoMac **out) {
    if (out == NULL)
        return EINVAL;
    *out = NULL;
    char *algorithm_name = NULL;
    int32_t algorithm_check =
        ciel_crypto_algorithm_cstr(algorithm, algorithm_len, &algorithm_name);
    if (algorithm_check != 0)
        return algorithm_check;
    if (key_len < CIEL_CRYPTO_MIN_MAC_KEY_LEN)
        return EINVAL;
    int32_t check = ciel_crypto_check_input(key, key_len);
    if (check != 0)
        return check;

    botan_mac_t mac = NULL;
    int rc = botan_mac_init(&mac, algorithm_name, 0);
    if (rc != 0)
        return rc;
    rc = botan_mac_set_key(mac, ciel_crypto_input_ptr(key, key_len), key_len);
    if (rc != 0) {
        botan_mac_destroy(mac);
        return rc;
    }

    CielCryptoMac *ctx = (CielCryptoMac *)GC_MALLOC(sizeof(CielCryptoMac));
    if (ctx == NULL) {
        botan_mac_destroy(mac);
        return ENOMEM;
    }
    ctx->mac = mac;
    GC_register_finalizer(ctx, ciel_crypto_mac_finalizer, NULL, NULL, NULL);
    *out = ctx;
    return 0;
}

int32_t ciel_crypto_mac_update(CielCryptoMac *mac, const uint8_t *data,
                               size_t data_len) {
    if (mac == NULL || mac->mac == NULL)
        return EINVAL;
    int32_t check = ciel_crypto_check_input(data, data_len);
    if (check != 0)
        return check;
    if (data_len == 0)
        return 0;
    return botan_mac_update(mac->mac, ciel_crypto_input_ptr(data, data_len),
                            data_len);
}

int32_t ciel_crypto_mac_finish(CielCryptoMac *mac, uint8_t *out, size_t out_len,
                               size_t *written) {
    if (written == NULL)
        return EINVAL;
    *written = 0;
    if (mac == NULL || mac->mac == NULL)
        return EINVAL;
    int32_t check = ciel_crypto_check_output(out, out_len);
    if (check != 0)
        return check;

    size_t needed = 0;
    int rc = botan_mac_output_length(mac->mac, &needed);
    if (rc != 0)
        return rc;
    if (out_len < needed) {
        *written = needed;
        return ENOBUFS;
    }
    rc = botan_mac_final(mac->mac, out);
    if (rc != 0)
        return rc;
    *written = needed;
    return 0;
}

int32_t ciel_crypto_mac_clear(CielCryptoMac *mac) {
    if (mac == NULL || mac->mac == NULL)
        return EINVAL;
    botan_mac_t raw = mac->mac;
    mac->mac = NULL;
    return botan_mac_destroy(raw);
}

bool ciel_crypto_constant_time_eq(const uint8_t *left, size_t left_len,
                                  const uint8_t *right, size_t right_len) {
    if (left_len != right_len)
        return false;
    if (left_len == 0)
        return true;
    if (left == NULL || right == NULL)
        return false;
    return botan_constant_time_compare(left, right, left_len) == 0;
}

typedef struct CielActor CielActor;
typedef void (*CielActorDispatchFn)(void *state, void *handler, void *message,
                                    int32_t *failed);
typedef struct CielChannel CielChannel;
typedef struct CielMutex CielMutex;
typedef struct CielAtomic CielAtomic;
typedef struct CielBytes CielBytes;
typedef struct CielAsyncFd CielAsyncFd;
typedef struct CielAsyncOp CielAsyncOp;

typedef struct CielActorJob {
    CielActor *actor;
    void *value;
} CielActorJob;

struct CielActor {
    dispatch_queue_t queue;
    dispatch_group_t jobs;
    dispatch_semaphore_t lifecycle_lock;
    void *state;
    void *handler;
    CielActorDispatchFn dispatch;
    int closing;
    int joined;
    int failed;
    int join_result;
};

static char ciel_actor_queue_key;

static void ciel_actor_lock(CielActor *actor) {
    dispatch_semaphore_wait(actor->lifecycle_lock, DISPATCH_TIME_FOREVER);
}

static void ciel_actor_unlock(CielActor *actor) {
    dispatch_semaphore_signal(actor->lifecycle_lock);
}

static void ciel_actor_job_run(void *raw) {
    CielActorJob *job = (CielActorJob *)raw;
    CielActor *actor = job->actor;
    int32_t attach_rc = ciel_runtime_enter_callback();
    int32_t failed = attach_rc != 0;
    if (attach_rc == 0) {
        actor->dispatch(actor->state, actor->handler, job->value, &failed);
        ciel_runtime_leave_callback();
    }

    if (failed) {
        ciel_actor_lock(actor);
        actor->failed = 1;
        actor->closing = 1;
        ciel_actor_unlock(actor);
    }
    dispatch_group_leave(actor->jobs);
}

int32_t ciel_actor_spawn(CielActor **out, void *state, void *handler,
                         CielActorDispatchFn dispatch) {
    if (out == NULL || state == NULL || handler == NULL || dispatch == NULL)
        return EINVAL;
    ciel_runtime_init();
    CielActor *actor = (CielActor *)ciel_alloc_uncollectable(sizeof(CielActor));
    actor->state = state;
    actor->handler = handler;
    actor->dispatch = dispatch;
    actor->closing = 0;
    actor->joined = 0;
    actor->failed = 0;
    actor->join_result = 0;
    actor->queue = dispatch_queue_create("ciel.actor", DISPATCH_QUEUE_SERIAL);
    if (actor->queue == NULL)
        return ENOMEM;
    dispatch_queue_set_specific(actor->queue, &ciel_actor_queue_key, actor,
                                NULL);
    actor->jobs = dispatch_group_create();
    if (actor->jobs == NULL)
        return ENOMEM;
    actor->lifecycle_lock = dispatch_semaphore_create(1);
    if (actor->lifecycle_lock == NULL)
        return ENOMEM;
    *out = actor;
    return 0;
}

int32_t ciel_actor_send(CielActor *actor, void *message) {
    if (actor == NULL || message == NULL)
        return EINVAL;
    CielActorJob *job =
        (CielActorJob *)ciel_alloc_uncollectable(sizeof(CielActorJob));
    job->actor = actor;
    job->value = message;
    ciel_actor_lock(actor);
    if (actor->closing) {
        ciel_actor_unlock(actor);
        return EPIPE;
    }
    dispatch_group_enter(actor->jobs);
    ciel_actor_unlock(actor);
    dispatch_async_f(actor->queue, job, ciel_actor_job_run);
    return 0;
}

int32_t ciel_actor_stop(CielActor *actor) {
    if (actor == NULL)
        return EINVAL;
    ciel_actor_lock(actor);
    actor->closing = 1;
    ciel_actor_unlock(actor);
    return 0;
}

int32_t ciel_actor_join(CielActor *actor) {
    if (actor == NULL)
        return EINVAL;
    if (dispatch_get_specific(&ciel_actor_queue_key) == actor)
        return EDEADLK;
    ciel_actor_lock(actor);
    if (actor->joined) {
        int result = actor->join_result;
        ciel_actor_unlock(actor);
        return result;
    }
    actor->closing = 1;
    ciel_actor_unlock(actor);

    dispatch_group_wait(actor->jobs, DISPATCH_TIME_FOREVER);
    ciel_actor_lock(actor);
    actor->joined = 1;
    actor->join_result = actor->failed ? EIO : 0;
    int result = actor->join_result;
    ciel_actor_unlock(actor);
    return result;
}

struct CielAtomic {
    size_t value_size;
    size_t value_align;
    union {
        _Atomic uint8_t u8;
        _Atomic uint32_t u32;
        _Atomic uint64_t u64;
    } value;
};

static int32_t ciel_atomic_order_from_code(int32_t code, memory_order *out) {
    if (out == NULL)
        return EINVAL;
    switch (code) {
    case 0:
        *out = memory_order_relaxed;
        return 0;
    case 1:
        *out = memory_order_acquire;
        return 0;
    case 2:
        *out = memory_order_release;
        return 0;
    case 3:
        *out = memory_order_acq_rel;
        return 0;
    case 4:
        *out = memory_order_seq_cst;
        return 0;
    default:
        return EINVAL;
    }
}

static int32_t ciel_atomic_validate_load_order(int32_t order) {
    return order == 2 || order == 3 ? EINVAL : 0;
}

static int32_t ciel_atomic_validate_store_order(int32_t order) {
    return order == 1 || order == 3 ? EINVAL : 0;
}

static int32_t ciel_atomic_validate_compare_orders(int32_t success,
                                                   int32_t failure) {
    if (failure == 2 || failure == 3)
        return EINVAL;
    if (failure == 1)
        return success == 1 || success == 3 || success == 4 ? 0 : EINVAL;
    if (failure == 4)
        return success == 4 ? 0 : EINVAL;
    return failure == 0 ? 0 : EINVAL;
}

static int32_t ciel_atomic_bits_from_value(const void *value, size_t size,
                                           uint64_t *bits) {
    if (value == NULL || bits == NULL)
        return EINVAL;
    *bits = 0;
    switch (size) {
    case 1: {
        uint8_t tmp = 0;
        memcpy(&tmp, value, sizeof(tmp));
        *bits = tmp;
        return 0;
    }
    case 4: {
        uint32_t tmp = 0;
        memcpy(&tmp, value, sizeof(tmp));
        *bits = tmp;
        return 0;
    }
    case 8: {
        uint64_t tmp = 0;
        memcpy(&tmp, value, sizeof(tmp));
        *bits = tmp;
        return 0;
    }
    default:
        return EINVAL;
    }
}

static void ciel_atomic_write_bits(void *out, size_t size, uint64_t bits) {
    switch (size) {
    case 1: {
        uint8_t tmp = (uint8_t)bits;
        memcpy(out, &tmp, sizeof(tmp));
        break;
    }
    case 4: {
        uint32_t tmp = (uint32_t)bits;
        memcpy(out, &tmp, sizeof(tmp));
        break;
    }
    case 8: {
        uint64_t tmp = (uint64_t)bits;
        memcpy(out, &tmp, sizeof(tmp));
        break;
    }
    default:
        break;
    }
}

CielAtomic *ciel_atomic_make(size_t value_size, size_t value_align,
                             const void *initial) {
    uint64_t bits = 0;
    int32_t rc = ciel_atomic_bits_from_value(initial, value_size, &bits);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    ciel_runtime_init();
    CielAtomic *atomic = (CielAtomic *)ciel_alloc(sizeof(CielAtomic));
    atomic->value_size = value_size;
    atomic->value_align = value_align;
    switch (value_size) {
    case 1:
        atomic_init(&atomic->value.u8, (uint8_t)bits);
        return atomic;
    case 4:
        atomic_init(&atomic->value.u32, (uint32_t)bits);
        return atomic;
    case 8:
        atomic_init(&atomic->value.u64, (uint64_t)bits);
        return atomic;
    default:
        errno = EINVAL;
        return NULL;
    }
}

void *ciel_atomic_load(CielAtomic *atomic, int32_t order) {
    if (atomic == NULL) {
        errno = EINVAL;
        return NULL;
    }
    int32_t rc = ciel_atomic_validate_load_order(order);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    memory_order c_order;
    rc = ciel_atomic_order_from_code(order, &c_order);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    uint64_t bits = 0;
    switch (atomic->value_size) {
    case 1:
        bits = atomic_load_explicit(&atomic->value.u8, c_order);
        break;
    case 4:
        bits = atomic_load_explicit(&atomic->value.u32, c_order);
        break;
    case 8:
        bits = atomic_load_explicit(&atomic->value.u64, c_order);
        break;
    default:
        errno = EINVAL;
        return NULL;
    }
    void *out = ciel_alloc(atomic->value_size);
    ciel_atomic_write_bits(out, atomic->value_size, bits);
    return out;
}

int32_t ciel_atomic_store(CielAtomic *atomic, const void *value,
                          int32_t order) {
    if (atomic == NULL || value == NULL)
        return EINVAL;
    int32_t rc = ciel_atomic_validate_store_order(order);
    if (rc != 0)
        return rc;
    memory_order c_order;
    rc = ciel_atomic_order_from_code(order, &c_order);
    if (rc != 0)
        return rc;
    uint64_t bits = 0;
    rc = ciel_atomic_bits_from_value(value, atomic->value_size, &bits);
    if (rc != 0)
        return rc;
    switch (atomic->value_size) {
    case 1:
        atomic_store_explicit(&atomic->value.u8, (uint8_t)bits, c_order);
        return 0;
    case 4:
        atomic_store_explicit(&atomic->value.u32, (uint32_t)bits, c_order);
        return 0;
    case 8:
        atomic_store_explicit(&atomic->value.u64, (uint64_t)bits, c_order);
        return 0;
    default:
        return EINVAL;
    }
}

void *ciel_atomic_exchange(CielAtomic *atomic, const void *value,
                           int32_t order) {
    if (atomic == NULL || value == NULL) {
        errno = EINVAL;
        return NULL;
    }
    memory_order c_order;
    int32_t rc = ciel_atomic_order_from_code(order, &c_order);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    uint64_t bits = 0;
    rc = ciel_atomic_bits_from_value(value, atomic->value_size, &bits);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    uint64_t previous = 0;
    switch (atomic->value_size) {
    case 1:
        previous =
            atomic_exchange_explicit(&atomic->value.u8, (uint8_t)bits, c_order);
        break;
    case 4:
        previous = atomic_exchange_explicit(&atomic->value.u32, (uint32_t)bits,
                                            c_order);
        break;
    case 8:
        previous = atomic_exchange_explicit(&atomic->value.u64, (uint64_t)bits,
                                            c_order);
        break;
    default:
        errno = EINVAL;
        return NULL;
    }
    void *out = ciel_alloc(atomic->value_size);
    ciel_atomic_write_bits(out, atomic->value_size, previous);
    return out;
}

void *ciel_atomic_compare_exchange(CielAtomic *atomic, const void *expected,
                                   const void *desired, int32_t *exchanged,
                                   int32_t success, int32_t failure) {
    if (atomic == NULL || expected == NULL || desired == NULL ||
        exchanged == NULL) {
        errno = EINVAL;
        return NULL;
    }
    int32_t rc = ciel_atomic_validate_compare_orders(success, failure);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    memory_order c_success;
    memory_order c_failure;
    rc = ciel_atomic_order_from_code(success, &c_success);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    rc = ciel_atomic_order_from_code(failure, &c_failure);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    uint64_t expected_bits = 0;
    uint64_t desired_bits = 0;
    rc = ciel_atomic_bits_from_value(expected, atomic->value_size,
                                     &expected_bits);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    rc =
        ciel_atomic_bits_from_value(desired, atomic->value_size, &desired_bits);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    bool did_exchange = false;
    switch (atomic->value_size) {
    case 1: {
        uint8_t expected_u8 = (uint8_t)expected_bits;
        did_exchange = atomic_compare_exchange_strong_explicit(
            &atomic->value.u8, &expected_u8, (uint8_t)desired_bits, c_success,
            c_failure);
        expected_bits = expected_u8;
        break;
    }
    case 4: {
        uint32_t expected_u32 = (uint32_t)expected_bits;
        did_exchange = atomic_compare_exchange_strong_explicit(
            &atomic->value.u32, &expected_u32, (uint32_t)desired_bits,
            c_success, c_failure);
        expected_bits = expected_u32;
        break;
    }
    case 8: {
        uint64_t expected_u64 = (uint64_t)expected_bits;
        did_exchange = atomic_compare_exchange_strong_explicit(
            &atomic->value.u64, &expected_u64, (uint64_t)desired_bits,
            c_success, c_failure);
        expected_bits = expected_u64;
        break;
    }
    default:
        errno = EINVAL;
        return NULL;
    }
    *exchanged = did_exchange ? 1 : 0;
    void *out = ciel_alloc(atomic->value_size);
    ciel_atomic_write_bits(out, atomic->value_size, expected_bits);
    return out;
}

void *ciel_atomic_fetch_add(CielAtomic *atomic, const void *value,
                            int32_t order) {
    if (atomic == NULL || value == NULL) {
        errno = EINVAL;
        return NULL;
    }
    memory_order c_order;
    int32_t rc = ciel_atomic_order_from_code(order, &c_order);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    uint64_t bits = 0;
    rc = ciel_atomic_bits_from_value(value, atomic->value_size, &bits);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    uint64_t previous = 0;
    switch (atomic->value_size) {
    case 4:
        previous = atomic_fetch_add_explicit(&atomic->value.u32, (uint32_t)bits,
                                             c_order);
        break;
    case 8:
        previous = atomic_fetch_add_explicit(&atomic->value.u64, (uint64_t)bits,
                                             c_order);
        break;
    default:
        errno = EINVAL;
        return NULL;
    }
    void *out = ciel_alloc(atomic->value_size);
    ciel_atomic_write_bits(out, atomic->value_size, previous);
    return out;
}

void *ciel_atomic_fetch_sub(CielAtomic *atomic, const void *value,
                            int32_t order) {
    if (atomic == NULL || value == NULL) {
        errno = EINVAL;
        return NULL;
    }
    memory_order c_order;
    int32_t rc = ciel_atomic_order_from_code(order, &c_order);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    uint64_t bits = 0;
    rc = ciel_atomic_bits_from_value(value, atomic->value_size, &bits);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    uint64_t previous = 0;
    switch (atomic->value_size) {
    case 4:
        previous = atomic_fetch_sub_explicit(&atomic->value.u32, (uint32_t)bits,
                                             c_order);
        break;
    case 8:
        previous = atomic_fetch_sub_explicit(&atomic->value.u64, (uint64_t)bits,
                                             c_order);
        break;
    default:
        errno = EINVAL;
        return NULL;
    }
    void *out = ciel_alloc(atomic->value_size);
    ciel_atomic_write_bits(out, atomic->value_size, previous);
    return out;
}

typedef struct CielQueueNode {
    void *value;
    struct CielQueueNode *next;
} CielQueueNode;

struct CielChannel {
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    CielQueueNode *head;
    CielQueueNode *tail;
    size_t value_size;
    size_t value_align;
    int closed;
};

CielChannel *ciel_channel_make(size_t value_size, size_t value_align) {
    ciel_runtime_init();
    CielChannel *channel = (CielChannel *)ciel_alloc(sizeof(CielChannel));
    channel->head = NULL;
    channel->tail = NULL;
    channel->value_size = value_size;
    channel->value_align = value_align;
    channel->closed = 0;
    int rc = pthread_mutex_init(&channel->mutex, NULL);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    rc = pthread_cond_init(&channel->cond, NULL);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    return channel;
}

int32_t ciel_channel_send(CielChannel *channel, const void *value) {
    if (channel == NULL || value == NULL)
        return EINVAL;
    pthread_mutex_lock(&channel->mutex);
    if (channel->closed) {
        pthread_mutex_unlock(&channel->mutex);
        return EPIPE;
    }
    CielQueueNode *node = (CielQueueNode *)ciel_alloc(sizeof(CielQueueNode));
    void *copy = ciel_alloc(channel->value_size == 0 ? 1 : channel->value_size);
    if (channel->value_size > 0)
        memcpy(copy, value, channel->value_size);
    node->value = copy;
    node->next = NULL;
    if (channel->tail != NULL)
        channel->tail->next = node;
    else
        channel->head = node;
    channel->tail = node;
    pthread_cond_signal(&channel->cond);
    pthread_mutex_unlock(&channel->mutex);
    return 0;
}

void *ciel_channel_recv(CielChannel *channel) {
    if (channel == NULL) {
        errno = EINVAL;
        return NULL;
    }
    pthread_mutex_lock(&channel->mutex);
    while (channel->head == NULL && !channel->closed)
        pthread_cond_wait(&channel->cond, &channel->mutex);
    if (channel->head == NULL && channel->closed) {
        pthread_mutex_unlock(&channel->mutex);
        errno = EPIPE;
        return NULL;
    }
    CielQueueNode *node = channel->head;
    channel->head = node->next;
    if (channel->head == NULL)
        channel->tail = NULL;
    pthread_mutex_unlock(&channel->mutex);
    return node->value;
}

int32_t ciel_channel_close(CielChannel *channel) {
    if (channel == NULL)
        return EINVAL;
    pthread_mutex_lock(&channel->mutex);
    channel->closed = 1;
    pthread_cond_broadcast(&channel->cond);
    pthread_mutex_unlock(&channel->mutex);
    return 0;
}

struct CielMutex {
    pthread_mutex_t mutex;
    void *slot;
    size_t value_size;
    size_t value_align;
};

CielMutex *ciel_mutex_make(size_t value_size, size_t value_align,
                           const void *initial) {
    if (initial == NULL) {
        errno = EINVAL;
        return NULL;
    }
    ciel_runtime_init();
    CielMutex *mutex = (CielMutex *)ciel_alloc(sizeof(CielMutex));
    mutex->value_size = value_size;
    mutex->value_align = value_align;
    mutex->slot = ciel_alloc(value_size == 0 ? 1 : value_size);
    if (value_size > 0)
        memcpy(mutex->slot, initial, value_size);
    int rc = pthread_mutex_init(&mutex->mutex, NULL);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    return mutex;
}

void *ciel_mutex_lock(CielMutex *mutex) {
    if (mutex == NULL) {
        errno = EINVAL;
        return NULL;
    }
    int rc = pthread_mutex_lock(&mutex->mutex);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    return mutex->slot;
}

int32_t ciel_mutex_unlock(CielMutex *mutex) {
    if (mutex == NULL)
        return EINVAL;
    int rc = pthread_mutex_unlock(&mutex->mutex);
    if (rc != 0)
        errno = rc;
    return rc;
}

typedef enum {
    CIEL_FILE_OPEN = 1,
    CIEL_FILE_CLOSED = 2,
    CIEL_FILE_TRANSFERRED = 3,
} CielFileState;

typedef struct CielFileSlot {
    int fd;
    uint32_t generation;
    CielFileState state;
    int borrowed;
    uint32_t next_free;
} CielFileSlot;

static pthread_mutex_t ciel_file_table_mutex = PTHREAD_MUTEX_INITIALIZER;
static CielFileSlot *ciel_file_slots = NULL;
static uint32_t ciel_file_slot_count = 0;
static uint32_t ciel_file_slot_cap = 0;
static uint32_t ciel_file_free_head = UINT32_MAX;

static int32_t ciel_file_table_grow(void) {
    uint32_t old_cap = ciel_file_slot_cap;
    uint32_t new_cap = old_cap == 0 ? 16 : old_cap * 2;
    CielFileSlot *next = (CielFileSlot *)GC_MALLOC_UNCOLLECTABLE(
        sizeof(CielFileSlot) * (size_t)new_cap);
    if (next == NULL)
        return ENOMEM;
    memset(next, 0, sizeof(CielFileSlot) * (size_t)new_cap);
    if (ciel_file_slots != NULL) {
        memcpy(next, ciel_file_slots, sizeof(CielFileSlot) * (size_t)old_cap);
        GC_FREE(ciel_file_slots);
    }
    ciel_file_slots = next;
    ciel_file_slot_cap = new_cap;
    return 0;
}

static int32_t ciel_file_slot_insert_locked(int fd, int borrowed,
                                            uint32_t *out_slot,
                                            uint32_t *out_generation) {
    uint32_t slot;
    if (ciel_file_free_head != UINT32_MAX) {
        slot = ciel_file_free_head;
        ciel_file_free_head = ciel_file_slots[slot].next_free;
    } else {
        if (ciel_file_slot_count == ciel_file_slot_cap) {
            int32_t rc = ciel_file_table_grow();
            if (rc != 0)
                return rc;
        }
        slot = ciel_file_slot_count++;
        if (ciel_file_slots[slot].generation == 0)
            ciel_file_slots[slot].generation = 1;
    }
    ciel_file_slots[slot].fd = fd;
    ciel_file_slots[slot].state = CIEL_FILE_OPEN;
    ciel_file_slots[slot].borrowed = borrowed;
    ciel_file_slots[slot].next_free = UINT32_MAX;
    *out_slot = slot;
    *out_generation = ciel_file_slots[slot].generation;
    return 0;
}

static int32_t ciel_file_resolve_locked(uint32_t slot, uint32_t generation,
                                        CielFileSlot **out) {
    if (out == NULL)
        return EINVAL;
    if (slot >= ciel_file_slot_count)
        return EBADF;
    CielFileSlot *file = &ciel_file_slots[slot];
    if (file->generation != generation || file->state != CIEL_FILE_OPEN)
        return EBADF;
    *out = file;
    return 0;
}

static int ciel_file_open_mode_flags(int32_t mode) {
    switch (mode) {
    case 0:
        return O_RDONLY;
    case 1:
        return O_WRONLY | O_CREAT | O_TRUNC;
    case 2:
        return O_WRONLY | O_CREAT | O_APPEND;
    default:
        errno = EINVAL;
        return -1;
    }
}

int32_t ciel_file_open(int32_t mode, const char *path, uint32_t *out_slot,
                       uint32_t *out_generation) {
    if (path == NULL || out_slot == NULL || out_generation == NULL)
        return EINVAL;
    int flags = ciel_file_open_mode_flags(mode);
    if (flags < 0)
        return errno;
    int fd = open(path, flags, 0666);
    if (fd < 0)
        return errno;
    pthread_mutex_lock(&ciel_file_table_mutex);
    int32_t rc = ciel_file_slot_insert_locked(fd, 0, out_slot, out_generation);
    pthread_mutex_unlock(&ciel_file_table_mutex);
    if (rc != 0)
        close(fd);
    return rc;
}

static int32_t ciel_file_borrowed_fd(int fd, uint32_t *out_slot,
                                     uint32_t *out_generation) {
    if (out_slot == NULL || out_generation == NULL)
        return EINVAL;
    pthread_mutex_lock(&ciel_file_table_mutex);
    int32_t rc = ciel_file_slot_insert_locked(fd, 1, out_slot, out_generation);
    pthread_mutex_unlock(&ciel_file_table_mutex);
    return rc;
}

int32_t ciel_file_stdout(uint32_t *out_slot, uint32_t *out_generation) {
    return ciel_file_borrowed_fd(STDOUT_FILENO, out_slot, out_generation);
}

int32_t ciel_file_stderr(uint32_t *out_slot, uint32_t *out_generation) {
    return ciel_file_borrowed_fd(STDERR_FILENO, out_slot, out_generation);
}

int32_t ciel_file_close(uint32_t slot, uint32_t generation) {
    pthread_mutex_lock(&ciel_file_table_mutex);
    CielFileSlot *file = NULL;
    int32_t rc = ciel_file_resolve_locked(slot, generation, &file);
    if (rc != 0) {
        pthread_mutex_unlock(&ciel_file_table_mutex);
        return rc;
    }
    int fd = file->fd;
    int borrowed = file->borrowed;
    file->state = CIEL_FILE_CLOSED;
    file->fd = -1;
    file->borrowed = 0;
    file->generation =
        file->generation == UINT32_MAX ? 1 : file->generation + 1;
    file->next_free = ciel_file_free_head;
    ciel_file_free_head = slot;
    pthread_mutex_unlock(&ciel_file_table_mutex);
    if (!borrowed && close(fd) != 0)
        return errno;
    return 0;
}

ssize_t ciel_file_read(uint32_t slot, uint32_t generation, void *buf,
                       size_t count) {
    if (buf == NULL && count > 0) {
        errno = EINVAL;
        return -1;
    }
    pthread_mutex_lock(&ciel_file_table_mutex);
    CielFileSlot *file = NULL;
    int32_t rc = ciel_file_resolve_locked(slot, generation, &file);
    int fd = rc == 0 ? file->fd : -1;
    pthread_mutex_unlock(&ciel_file_table_mutex);
    if (rc != 0) {
        errno = rc;
        return -1;
    }
    return read(fd, buf, count);
}

ssize_t ciel_file_write(uint32_t slot, uint32_t generation, const void *buf,
                        size_t count) {
    if (buf == NULL && count > 0) {
        errno = EINVAL;
        return -1;
    }
    pthread_mutex_lock(&ciel_file_table_mutex);
    CielFileSlot *file = NULL;
    int32_t rc = ciel_file_resolve_locked(slot, generation, &file);
    int fd = rc == 0 ? file->fd : -1;
    pthread_mutex_unlock(&ciel_file_table_mutex);
    if (rc != 0) {
        errno = rc;
        return -1;
    }
    return write(fd, buf, count);
}

typedef struct CielBytes {
    size_t len;
    uint8_t *data;
} CielBytes;

typedef struct CielAsyncFd {
    int fd;
    dispatch_io_t channel;
    pthread_mutex_t mutex;
    int closed;
} CielAsyncFd;

typedef enum {
    CIEL_ASYNC_READ,
    CIEL_ASYNC_WRITE,
} CielAsyncKind;

typedef struct CielAsyncOp {
    CielAsyncKind kind;
    CielAsyncFd *fd;
    pthread_mutex_t mutex;
    int complete;
    int canceled;
    int finished;
    int notify_set;
    int notify_sent;
    int error;
    size_t written;
    CielBytes *bytes;
    CielBytes *write_bytes;
    CielActor *notify_actor;
    void *notify_message;
} CielAsyncOp;

static dispatch_queue_t ciel_async_global_queue;

static void ciel_async_queue_init(void) {
    ciel_async_global_queue =
        dispatch_queue_create("ciel.async-io", DISPATCH_QUEUE_SERIAL);
}

static dispatch_queue_t ciel_async_queue(void) {
    static pthread_once_t once = PTHREAD_ONCE_INIT;
    pthread_once(&once, ciel_async_queue_init);
    return ciel_async_global_queue;
}

CielBytes *ciel_bytes_copy(const uint8_t *ptr, size_t len) {
    if (ptr == NULL && len > 0) {
        errno = EINVAL;
        return NULL;
    }
    CielBytes *bytes = (CielBytes *)ciel_alloc_uncollectable(sizeof(CielBytes));
    bytes->len = len;
    bytes->data = (uint8_t *)ciel_alloc_uncollectable(len == 0 ? 1 : len);
    if (len > 0)
        memcpy(bytes->data, ptr, len);
    return bytes;
}

size_t ciel_bytes_len(CielBytes *bytes) {
    return bytes == NULL ? 0 : bytes->len;
}

int32_t ciel_bytes_copy_to(CielBytes *bytes, uint8_t *out, size_t cap,
                           size_t *copied) {
    if (bytes == NULL || copied == NULL || (out == NULL && cap > 0))
        return EINVAL;
    size_t n = bytes->len < cap ? bytes->len : cap;
    if (n > 0)
        memcpy(out, bytes->data, n);
    *copied = n;
    return 0;
}

static CielAsyncFd *ciel_async_fd_new(int fd) {
    CielAsyncFd *async_fd =
        (CielAsyncFd *)ciel_alloc_uncollectable(sizeof(CielAsyncFd));
    async_fd->fd = fd;
    async_fd->closed = 0;
    int rc = pthread_mutex_init(&async_fd->mutex, NULL);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    async_fd->channel = dispatch_io_create(DISPATCH_IO_STREAM, fd,
                                           ciel_async_queue(), ^(int error) {
                                             (void)error;
                                           });
    if (async_fd->channel == NULL) {
        pthread_mutex_destroy(&async_fd->mutex);
        errno = ENOMEM;
        return NULL;
    }
    return async_fd;
}

CielAsyncFd *ciel_async_open(int32_t mode, const char *path) {
    if (path == NULL) {
        errno = EINVAL;
        return NULL;
    }
    int flags = ciel_file_open_mode_flags(mode);
    if (flags < 0)
        return NULL;
    int fd = open(path, flags, 0666);
    if (fd < 0)
        return NULL;
    CielAsyncFd *async_fd = ciel_async_fd_new(fd);
    if (async_fd == NULL) {
        close(fd);
        return NULL;
    }
    return async_fd;
}

CielAsyncFd *ciel_async_from_raw_fd(int32_t raw) {
    if (raw < 0) {
        errno = EBADF;
        return NULL;
    }
    return ciel_async_fd_new(raw);
}

int32_t ciel_async_close(CielAsyncFd *fd) {
    if (fd == NULL)
        return EINVAL;
    pthread_mutex_lock(&fd->mutex);
    if (fd->closed) {
        pthread_mutex_unlock(&fd->mutex);
        return 0;
    }
    fd->closed = 1;
    dispatch_io_t channel = fd->channel;
    fd->channel = NULL;
    pthread_mutex_unlock(&fd->mutex);
    dispatch_io_close(channel, DISPATCH_IO_STOP);
    return 0;
}

static int32_t ciel_async_fd_snapshot(CielAsyncFd *fd, dispatch_io_t *channel) {
    if (fd == NULL || channel == NULL)
        return EINVAL;
    pthread_mutex_lock(&fd->mutex);
    if (fd->closed) {
        pthread_mutex_unlock(&fd->mutex);
        return EBADF;
    }
    *channel = fd->channel;
    pthread_mutex_unlock(&fd->mutex);
    return 0;
}

static CielAsyncOp *ciel_async_op_new(CielAsyncKind kind, CielAsyncFd *fd) {
    CielAsyncOp *op =
        (CielAsyncOp *)ciel_alloc_uncollectable(sizeof(CielAsyncOp));
    op->kind = kind;
    op->fd = fd;
    int rc = pthread_mutex_init(&op->mutex, NULL);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    return op;
}

static void ciel_async_send_notification_locked(CielAsyncOp *op) {
    if (!op->complete || op->canceled || !op->notify_set || op->notify_sent ||
        op->notify_actor == NULL || op->notify_message == NULL)
        return;
    CielActor *actor = op->notify_actor;
    void *message = op->notify_message;
    op->notify_sent = 1;
    op->notify_message = NULL;
    pthread_mutex_unlock(&op->mutex);
    int32_t rc = ciel_actor_send(actor, message);
    pthread_mutex_lock(&op->mutex);
    if (rc != 0)
        op->error = rc;
}

static void ciel_async_complete(CielAsyncOp *op, int error, CielBytes *bytes,
                                size_t written) {
    pthread_mutex_lock(&op->mutex);
    if (!op->canceled) {
        op->error = error;
        op->bytes = bytes;
        op->written = written;
    }
    op->complete = 1;
    ciel_async_send_notification_locked(op);
    pthread_mutex_unlock(&op->mutex);
}

CielAsyncOp *ciel_async_read_bytes(CielAsyncFd *fd, size_t max_len) {
    dispatch_io_t channel = NULL;
    int32_t rc = ciel_async_fd_snapshot(fd, &channel);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    CielAsyncOp *op = ciel_async_op_new(CIEL_ASYNC_READ, fd);
    if (op == NULL)
        return NULL;
    op->written = max_len;
    CielBytes *bytes = (CielBytes *)ciel_alloc_uncollectable(sizeof(CielBytes));
    bytes->len = 0;
    bytes->data =
        (uint8_t *)ciel_alloc_uncollectable(max_len == 0 ? 1 : max_len);
    op->bytes = bytes;
    dispatch_io_read(
        channel, 0, max_len, ciel_async_queue(),
        ^(bool done, dispatch_data_t data, int error) {
          int32_t attach_rc = ciel_runtime_enter_callback();
          if (attach_rc != 0) {
              if (done)
                  ciel_async_complete(op, attach_rc, bytes, 0);
              return;
          }
          size_t data_size = data == NULL ? 0 : dispatch_data_get_size(data);
          if (data_size > 0) {
              dispatch_data_apply(data, ^bool(dispatch_data_t region,
                                              size_t offset, const void *buffer,
                                              size_t size) {
                (void)region;
                (void)offset;
                size_t remaining = max_len - bytes->len;
                size_t copy = size < remaining ? size : remaining;
                if (copy > 0)
                    memcpy(bytes->data + bytes->len, buffer, copy);
                bytes->len += copy;
                return bytes->len < max_len;
              });
          }
          if (done)
              ciel_async_complete(op, error, bytes, 0);
          ciel_runtime_leave_callback();
        });
    return op;
}

CielAsyncOp *ciel_async_write_bytes(CielAsyncFd *fd, CielBytes *bytes) {
    if (bytes == NULL) {
        errno = EINVAL;
        return NULL;
    }
    dispatch_io_t channel = NULL;
    int32_t rc = ciel_async_fd_snapshot(fd, &channel);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    CielAsyncOp *op = ciel_async_op_new(CIEL_ASYNC_WRITE, fd);
    if (op == NULL)
        return NULL;
    op->write_bytes = bytes;
    dispatch_data_t data =
        dispatch_data_create(bytes->data, bytes->len, ciel_async_queue(), NULL);
    if (data == NULL) {
        errno = ENOMEM;
        return NULL;
    }
    dispatch_io_write(channel, 0, data, ciel_async_queue(),
                      ^(bool done, dispatch_data_t remaining_data, int error) {
                        int32_t attach_rc = ciel_runtime_enter_callback();
                        if (attach_rc != 0) {
                            if (done)
                                ciel_async_complete(op, attach_rc, NULL, 0);
                            return;
                        }
                        if (done) {
                            size_t remaining =
                                remaining_data == NULL
                                    ? 0
                                    : dispatch_data_get_size(remaining_data);
                            size_t written = bytes->len >= remaining
                                                 ? bytes->len - remaining
                                                 : 0;
                            ciel_async_complete(op, error, NULL, written);
                        }
                        ciel_runtime_leave_callback();
                      });
    return op;
}

static int32_t ciel_async_notify(CielAsyncOp *op, CielAsyncKind kind,
                                 CielActor *actor, void *message) {
    if (op == NULL || actor == NULL || message == NULL)
        return EINVAL;
    pthread_mutex_lock(&op->mutex);
    if (op->kind != kind) {
        pthread_mutex_unlock(&op->mutex);
        return EINVAL;
    }
    if (op->notify_set) {
        pthread_mutex_unlock(&op->mutex);
        return EALREADY;
    }
    if (op->canceled) {
        pthread_mutex_unlock(&op->mutex);
        return ECANCELED;
    }
    op->notify_actor = actor;
    op->notify_message = message;
    op->notify_set = 1;
    ciel_async_send_notification_locked(op);
    pthread_mutex_unlock(&op->mutex);
    return 0;
}

int32_t ciel_async_notify_read(CielAsyncOp *op, CielActor *actor,
                               void *message) {
    return ciel_async_notify(op, CIEL_ASYNC_READ, actor, message);
}

int32_t ciel_async_notify_write(CielAsyncOp *op, CielActor *actor,
                                void *message) {
    return ciel_async_notify(op, CIEL_ASYNC_WRITE, actor, message);
}

int32_t ciel_async_finish_read(CielAsyncOp *op, CielBytes **out) {
    if (op == NULL || out == NULL)
        return EINVAL;
    pthread_mutex_lock(&op->mutex);
    if (op->kind != CIEL_ASYNC_READ) {
        pthread_mutex_unlock(&op->mutex);
        return EINVAL;
    }
    if (op->finished) {
        pthread_mutex_unlock(&op->mutex);
        return EALREADY;
    }
    if (op->canceled) {
        op->finished = 1;
        pthread_mutex_unlock(&op->mutex);
        return ECANCELED;
    }
    if (!op->complete) {
        pthread_mutex_unlock(&op->mutex);
        return EAGAIN;
    }
    if (op->error != 0) {
        int err = op->error;
        op->finished = 1;
        pthread_mutex_unlock(&op->mutex);
        return err;
    }
    op->finished = 1;
    *out = op->bytes;
    pthread_mutex_unlock(&op->mutex);
    return 0;
}

int32_t ciel_async_finish_write(CielAsyncOp *op, size_t *written) {
    if (op == NULL || written == NULL)
        return EINVAL;
    pthread_mutex_lock(&op->mutex);
    if (op->kind != CIEL_ASYNC_WRITE) {
        pthread_mutex_unlock(&op->mutex);
        return EINVAL;
    }
    if (op->finished) {
        pthread_mutex_unlock(&op->mutex);
        return EALREADY;
    }
    if (op->canceled) {
        op->finished = 1;
        pthread_mutex_unlock(&op->mutex);
        return ECANCELED;
    }
    if (!op->complete) {
        pthread_mutex_unlock(&op->mutex);
        return EAGAIN;
    }
    if (op->error != 0) {
        int err = op->error;
        op->finished = 1;
        pthread_mutex_unlock(&op->mutex);
        return err;
    }
    op->finished = 1;
    *written = op->written;
    pthread_mutex_unlock(&op->mutex);
    return 0;
}

int32_t ciel_async_cancel(CielAsyncOp *op) {
    if (op == NULL)
        return EINVAL;
    pthread_mutex_lock(&op->mutex);
    if (op->finished) {
        pthread_mutex_unlock(&op->mutex);
        return EALREADY;
    }
    op->canceled = 1;
    op->notify_actor = NULL;
    op->notify_message = NULL;
    pthread_mutex_unlock(&op->mutex);
    return 0;
}

#if defined(__GNUC__) || defined(__clang__)
__attribute__((constructor))
#endif
static CIEL_MAYBE_UNUSED void ciel_internal_constructor(void) {
    ciel_runtime_init();
}

#endif /* linux || macOS */
