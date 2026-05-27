#include <errno.h>
#if !defined(_WIN32) && !defined(GC_THREADS)
#define GC_THREADS 1
#endif
#if !defined(_WIN32) && !defined(GC_NO_THREAD_REDIRECTS)
#define GC_NO_THREAD_REDIRECTS 1
#endif
#include <gc/gc.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if !defined(_WIN32)
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
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

static int ciel_runtime_initialized = 0;

void ciel_runtime_init(void) {
    if (ciel_runtime_initialized)
        return;
    ciel_runtime_initialized = 1;
    GC_INIT();
#if defined(GC_THREADS)
    GC_allow_register_threads();
#endif
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

void ciel_panic_at(const char *message, size_t len, const char *file, size_t line) {
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

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, i8, int8_t, uint8_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, i8, int8_t, uint8_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, i8, int8_t, uint8_t)
CIEL_DEFINE_SIGNED_NEG(i8, int8_t, uint8_t, INT8_MIN)
CIEL_DEFINE_SIGNED_DIV_REM(i8, int8_t, INT8_MIN)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, i16, int16_t, uint16_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, i16, int16_t, uint16_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, i16, int16_t, uint16_t)
CIEL_DEFINE_SIGNED_NEG(i16, int16_t, uint16_t, INT16_MIN)
CIEL_DEFINE_SIGNED_DIV_REM(i16, int16_t, INT16_MIN)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, i32, int32_t, uint32_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, i32, int32_t, uint32_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, i32, int32_t, uint32_t)
CIEL_DEFINE_SIGNED_NEG(i32, int32_t, uint32_t, INT32_MIN)
CIEL_DEFINE_SIGNED_DIV_REM(i32, int32_t, INT32_MIN)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, i64, int64_t, uint64_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, i64, int64_t, uint64_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, i64, int64_t, uint64_t)
CIEL_DEFINE_SIGNED_NEG(i64, int64_t, uint64_t, INT64_MIN)
CIEL_DEFINE_SIGNED_DIV_REM(i64, int64_t, INT64_MIN)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, u8, uint8_t, uint8_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, u8, uint8_t, uint8_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, u8, uint8_t, uint8_t)
CIEL_DEFINE_UNSIGNED_DIV_REM(u8, uint8_t)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, u16, uint16_t, uint16_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, u16, uint16_t, uint16_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, u16, uint16_t, uint16_t)
CIEL_DEFINE_UNSIGNED_DIV_REM(u16, uint16_t)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, u32, uint32_t, uint32_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, u32, uint32_t, uint32_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, u32, uint32_t, uint32_t)
CIEL_DEFINE_UNSIGNED_DIV_REM(u32, uint32_t)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, u64, uint64_t, uint64_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, u64, uint64_t, uint64_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, u64, uint64_t, uint64_t)
CIEL_DEFINE_UNSIGNED_DIV_REM(u64, uint64_t)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, usize, size_t, size_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, usize, size_t, size_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, usize, size_t, size_t)
CIEL_DEFINE_UNSIGNED_DIV_REM(usize, size_t)

#undef CIEL_DEFINE_BINOP
#undef CIEL_DEFINE_SIGNED_NEG
#undef CIEL_SIGNED_DIV_OVERFLOW_CHECK
#undef CIEL_DEFINE_SIGNED_DIV_REM
#undef CIEL_DEFINE_UNSIGNED_DIV_REM

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

int32_t ciel_thread_attach(void) {
    ciel_runtime_init();
#if defined(GC_THREADS)
    struct GC_stack_base stack_base;
    if (GC_get_stack_base(&stack_base) != GC_SUCCESS)
        return 1;
    int result = GC_register_my_thread(&stack_base);
    return (result == GC_SUCCESS || result == GC_DUPLICATE) ? 0 : result;
#else
    return 0;
#endif
}

void ciel_thread_detach(void) {
#if defined(GC_THREADS)
    GC_unregister_my_thread();
#endif
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

typedef struct CielActor CielActor;
typedef void (*CielActorDispatchFn)(void *state, void *handler, void *message,
                                    int32_t *failed);
typedef struct CielChannel CielChannel;
typedef struct CielMutex CielMutex;
typedef struct CielAtomic CielAtomic;

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
#else
typedef struct CielActorMessage {
    void *value;
    struct CielActorMessage *next;
} CielActorMessage;

struct CielActor {
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    pthread_t thread;
    CielActorMessage *head;
    CielActorMessage *tail;
    void *state;
    void *handler;
    CielActorDispatchFn dispatch;
    int stopping;
    int joined;
    int failed;
};

static void *ciel_actor_thread_main(void *raw) {
    CielActor *actor = (CielActor *)raw;
    (void)ciel_thread_attach();
    for (;;) {
        pthread_mutex_lock(&actor->mutex);
        while (actor->head == NULL && !actor->stopping)
            pthread_cond_wait(&actor->cond, &actor->mutex);
        if (actor->head == NULL && actor->stopping) {
            pthread_mutex_unlock(&actor->mutex);
            break;
        }
        CielActorMessage *message = actor->head;
        actor->head = message->next;
        if (actor->head == NULL)
            actor->tail = NULL;
        pthread_mutex_unlock(&actor->mutex);

        int32_t failed = 0;
        actor->dispatch(actor->state, actor->handler, message->value, &failed);
        if (failed) {
            pthread_mutex_lock(&actor->mutex);
            actor->failed = 1;
            actor->stopping = 1;
            pthread_cond_broadcast(&actor->cond);
            pthread_mutex_unlock(&actor->mutex);
        }
    }
    ciel_thread_detach();
    return NULL;
}

int32_t ciel_actor_spawn(CielActor **out, void *state, void *handler,
                         CielActorDispatchFn dispatch) {
    if (out == NULL || state == NULL || handler == NULL || dispatch == NULL)
        return EINVAL;
    ciel_runtime_init();
    CielActor *actor = (CielActor *)ciel_alloc(sizeof(CielActor));
    actor->head = NULL;
    actor->tail = NULL;
    actor->state = state;
    actor->handler = handler;
    actor->dispatch = dispatch;
    actor->stopping = 0;
    actor->joined = 0;
    actor->failed = 0;
    int rc = pthread_mutex_init(&actor->mutex, NULL);
    if (rc != 0)
        return rc;
    rc = pthread_cond_init(&actor->cond, NULL);
    if (rc != 0)
        return rc;
    rc = pthread_create(&actor->thread, NULL, ciel_actor_thread_main, actor);
    if (rc != 0)
        return rc;
    *out = actor;
    return 0;
}

int32_t ciel_actor_send(CielActor *actor, void *message) {
    if (actor == NULL || message == NULL)
        return EINVAL;
    CielActorMessage *node =
        (CielActorMessage *)ciel_alloc(sizeof(CielActorMessage));
    node->value = message;
    node->next = NULL;
    pthread_mutex_lock(&actor->mutex);
    if (actor->stopping) {
        pthread_mutex_unlock(&actor->mutex);
        return EPIPE;
    }
    if (actor->tail != NULL)
        actor->tail->next = node;
    else
        actor->head = node;
    actor->tail = node;
    pthread_cond_signal(&actor->cond);
    pthread_mutex_unlock(&actor->mutex);
    return 0;
}

int32_t ciel_actor_stop(CielActor *actor) {
    if (actor == NULL)
        return EINVAL;
    pthread_mutex_lock(&actor->mutex);
    actor->stopping = 1;
    pthread_cond_broadcast(&actor->cond);
    pthread_mutex_unlock(&actor->mutex);
    return 0;
}

int32_t ciel_actor_join(CielActor *actor) {
    if (actor == NULL)
        return EINVAL;
    pthread_mutex_lock(&actor->mutex);
    if (actor->joined) {
        int failed = actor->failed;
        pthread_mutex_unlock(&actor->mutex);
        return failed ? EIO : 0;
    }
    actor->stopping = 1;
    pthread_cond_broadcast(&actor->cond);
    pthread_mutex_unlock(&actor->mutex);

    int rc = pthread_join(actor->thread, NULL);
    if (rc != 0)
        return rc;
    pthread_mutex_lock(&actor->mutex);
    actor->joined = 1;
    int failed = actor->failed;
    pthread_mutex_unlock(&actor->mutex);
    return failed ? EIO : 0;
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
#endif

#if defined(__GNUC__) || defined(__clang__)
__attribute__((constructor))
#endif
static CIEL_MAYBE_UNUSED void ciel_internal_constructor(void) {
    ciel_runtime_init();
}
