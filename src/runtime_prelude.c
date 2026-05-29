#include <errno.h>
#if !defined(_WIN32) && !defined(GC_THREADS)
#define GC_THREADS 1
#endif
#if !defined(_WIN32) && !defined(GC_NO_THREAD_REDIRECTS)
#define GC_NO_THREAD_REDIRECTS 1
#endif
#include <gc/gc.h>
#include <limits.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if !defined(_WIN32)
#include <dispatch/dispatch.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <time.h>
#include <unistd.h>
#endif

#if defined(__GNUC__) || defined(__clang__)
#define CIEL_MAYBE_UNUSED __attribute__((unused))
#else
#define CIEL_MAYBE_UNUSED
#endif

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

#define CIEL_CONST_STR(S) ((CielConstSlice_char){.ptr = (S), .len = sizeof(S) - 1})

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
        index >= (size_t)ciel_runtime_argc || ciel_runtime_argv[index] == NULL) {
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
    exit(0);
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

#if defined(_WIN32)
int ciel_io_open_read(const char *path) {
    (void)path;
    errno = ENOSYS;
    return -1;
}

int ciel_io_open_write(const char *path) {
    (void)path;
    errno = ENOSYS;
    return -1;
}

int ciel_io_open_append(const char *path) {
    (void)path;
    errno = ENOSYS;
    return -1;
}
#else
int ciel_io_open_read(const char *path) { return open(path, O_RDONLY); }

int ciel_io_open_write(const char *path) {
    return open(path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
}

int ciel_io_open_append(const char *path) {
    return open(path, O_WRONLY | O_CREAT | O_APPEND, 0666);
}
#endif

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

#if defined(_WIN32)
int32_t ciel_time_monotonic_ms(uint64_t *out) {
    (void)out;
    return ENOSYS;
}

int32_t ciel_time_sleep_ms(uint64_t ms) {
    (void)ms;
    return ENOSYS;
}
#else
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
#endif

typedef struct CielActor CielActor;
typedef void (*CielActorDispatchFn)(void *state, void *handler, void *message,
                                    int32_t *failed);
typedef struct CielChannel CielChannel;
typedef struct CielMutex CielMutex;
typedef struct CielAtomic CielAtomic;
typedef struct CielBytes CielBytes;
typedef struct CielAsyncFd CielAsyncFd;
typedef struct CielAsyncOp CielAsyncOp;

#if defined(_WIN32)
int32_t ciel_actor_spawn(CielActor **out, void *state, void *handler,
                         CielActorDispatchFn dispatch) {
    (void)out;
    (void)state;
    (void)handler;
    (void)dispatch;
    return ENOSYS;
}

int32_t ciel_actor_send(CielActor *actor, void *message) {
    (void)actor;
    (void)message;
    return ENOSYS;
}

int32_t ciel_actor_stop(CielActor *actor) {
    (void)actor;
    return ENOSYS;
}

int32_t ciel_actor_join(CielActor *actor) {
    (void)actor;
    return ENOSYS;
}

CielChannel *ciel_channel_make(size_t value_size, size_t value_align) {
    (void)value_size;
    (void)value_align;
    errno = ENOSYS;
    return NULL;
}

int32_t ciel_channel_send(CielChannel *channel, const void *value) {
    (void)channel;
    (void)value;
    return ENOSYS;
}

void *ciel_channel_recv(CielChannel *channel) {
    (void)channel;
    errno = ENOSYS;
    return NULL;
}

int32_t ciel_channel_close(CielChannel *channel) {
    (void)channel;
    return ENOSYS;
}

CielMutex *ciel_mutex_make(size_t value_size, size_t value_align,
                           const void *initial) {
    (void)value_size;
    (void)value_align;
    (void)initial;
    errno = ENOSYS;
    return NULL;
}

void *ciel_mutex_lock(CielMutex *mutex) {
    (void)mutex;
    errno = ENOSYS;
    return NULL;
}

int32_t ciel_mutex_unlock(CielMutex *mutex) {
    (void)mutex;
    return ENOSYS;
}

CielAtomic *ciel_atomic_make(size_t value_size, size_t value_align,
                             const void *initial) {
    (void)value_size;
    (void)value_align;
    (void)initial;
    errno = ENOSYS;
    return NULL;
}

void *ciel_atomic_load(CielAtomic *atomic, int32_t order) {
    (void)atomic;
    (void)order;
    errno = ENOSYS;
    return NULL;
}

int32_t ciel_atomic_store(CielAtomic *atomic, const void *value,
                          int32_t order) {
    (void)atomic;
    (void)value;
    (void)order;
    return ENOSYS;
}

void *ciel_atomic_exchange(CielAtomic *atomic, const void *value,
                           int32_t order) {
    (void)atomic;
    (void)value;
    (void)order;
    errno = ENOSYS;
    return NULL;
}

void *ciel_atomic_compare_exchange(CielAtomic *atomic, const void *expected,
                                   const void *desired, int32_t *exchanged,
                                   int32_t success, int32_t failure) {
    (void)atomic;
    (void)expected;
    (void)desired;
    (void)exchanged;
    (void)success;
    (void)failure;
    errno = ENOSYS;
    return NULL;
}

void *ciel_atomic_fetch_add(CielAtomic *atomic, const void *value,
                            int32_t order) {
    (void)atomic;
    (void)value;
    (void)order;
    errno = ENOSYS;
    return NULL;
}

void *ciel_atomic_fetch_sub(CielAtomic *atomic, const void *value,
                            int32_t order) {
    (void)atomic;
    (void)value;
    (void)order;
    errno = ENOSYS;
    return NULL;
}

int32_t ciel_file_open(int32_t mode, const char *path, uint32_t *out_slot,
                       uint32_t *out_generation) {
    (void)mode;
    (void)path;
    (void)out_slot;
    (void)out_generation;
    return ENOSYS;
}

int32_t ciel_file_close(uint32_t slot, uint32_t generation) {
    (void)slot;
    (void)generation;
    return ENOSYS;
}

intptr_t ciel_file_read(uint32_t slot, uint32_t generation, void *buf,
                        size_t count) {
    (void)slot;
    (void)generation;
    (void)buf;
    (void)count;
    errno = ENOSYS;
    return -1;
}

intptr_t ciel_file_write(uint32_t slot, uint32_t generation, const void *buf,
                         size_t count) {
    (void)slot;
    (void)generation;
    (void)buf;
    (void)count;
    errno = ENOSYS;
    return -1;
}

int32_t ciel_file_stdout(uint32_t *out_slot, uint32_t *out_generation) {
    (void)out_slot;
    (void)out_generation;
    return ENOSYS;
}

int32_t ciel_file_stderr(uint32_t *out_slot, uint32_t *out_generation) {
    (void)out_slot;
    (void)out_generation;
    return ENOSYS;
}

CielBytes *ciel_bytes_copy(const uint8_t *ptr, size_t len) {
    (void)ptr;
    (void)len;
    errno = ENOSYS;
    return NULL;
}

size_t ciel_bytes_len(CielBytes *bytes) {
    (void)bytes;
    return 0;
}

int32_t ciel_bytes_copy_to(CielBytes *bytes, uint8_t *out, size_t cap,
                           size_t *copied) {
    (void)bytes;
    (void)out;
    (void)cap;
    (void)copied;
    return ENOSYS;
}

CielAsyncFd *ciel_async_open(int32_t mode, const char *path) {
    (void)mode;
    (void)path;
    errno = ENOSYS;
    return NULL;
}

CielAsyncFd *ciel_async_from_raw_fd(int32_t raw) {
    (void)raw;
    errno = ENOSYS;
    return NULL;
}

int32_t ciel_async_close(CielAsyncFd *fd) {
    (void)fd;
    return ENOSYS;
}

CielAsyncOp *ciel_async_read_bytes(CielAsyncFd *fd, size_t max_len) {
    (void)fd;
    (void)max_len;
    errno = ENOSYS;
    return NULL;
}

CielAsyncOp *ciel_async_write_bytes(CielAsyncFd *fd, CielBytes *bytes) {
    (void)fd;
    (void)bytes;
    errno = ENOSYS;
    return NULL;
}

int32_t ciel_async_notify_read(CielAsyncOp *op, CielActor *actor,
                               void *message) {
    (void)op;
    (void)actor;
    (void)message;
    return ENOSYS;
}

int32_t ciel_async_notify_write(CielAsyncOp *op, CielActor *actor,
                                void *message) {
    (void)op;
    (void)actor;
    (void)message;
    return ENOSYS;
}

int32_t ciel_async_finish_read(CielAsyncOp *op, CielBytes **out) {
    (void)op;
    (void)out;
    return ENOSYS;
}

int32_t ciel_async_finish_write(CielAsyncOp *op, size_t *written) {
    (void)op;
    (void)written;
    return ENOSYS;
}

int32_t ciel_async_cancel(CielAsyncOp *op) {
    (void)op;
    return ENOSYS;
}
#else
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
#endif

#if defined(__GNUC__) || defined(__clang__)
__attribute__((constructor))
#endif
static CIEL_MAYBE_UNUSED void ciel_internal_constructor(void) {
    ciel_runtime_init();
}
