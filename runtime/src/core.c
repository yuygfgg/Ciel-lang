#include "internal.h"

static int ciel_runtime_initialized = 0;
static int ciel_runtime_argc = 0;
static char **ciel_runtime_argv = NULL;

#define CIEL_DEFAULT_GC_INITIAL_HEAP_SIZE ((size_t)128 * 1024 * 1024)
#define CIEL_DEFAULT_GC_FREE_SPACE_DIVISOR ((GC_word)1)

static int ciel_env_is_set(const char *name) {
    const char *value = getenv(name);
    return value != NULL && value[0] != '\0';
}

static int ciel_parse_size_env(const char *name, size_t *out) {
    const char *value = getenv(name);
    if (value == NULL || value[0] == '\0')
        return 0;

    errno = 0;
    char *end = NULL;
    unsigned long long parsed = strtoull(value, &end, 10);
    if (errno != 0 || end == value || end == NULL || *end != '\0' ||
        parsed > SIZE_MAX)
        return 0;

    *out = (size_t)parsed;
    return 1;
}

static int ciel_parse_gc_word_env(const char *name, GC_word *out) {
    size_t parsed = 0;
    if (!ciel_parse_size_env(name, &parsed) || parsed == 0)
        return 0;
    *out = (GC_word)parsed;
    return 1;
}

static void ciel_configure_gc_before_init(void) {
    GC_word divisor = 0;
    if (ciel_parse_gc_word_env("CIEL_GC_FREE_SPACE_DIVISOR", &divisor)) {
        GC_set_free_space_divisor(divisor);
    } else if (!ciel_env_is_set("GC_FREE_SPACE_DIVISOR")) {
        GC_set_free_space_divisor(CIEL_DEFAULT_GC_FREE_SPACE_DIVISOR);
    }
}

static void ciel_configure_gc_after_init(void) {
    GC_word divisor = 0;
    if (ciel_parse_gc_word_env("CIEL_GC_FREE_SPACE_DIVISOR", &divisor))
        GC_set_free_space_divisor(divisor);

    size_t initial_heap_size = 0;
    if (!ciel_parse_size_env("CIEL_GC_INITIAL_HEAP_SIZE", &initial_heap_size)) {
        if (ciel_env_is_set("GC_INITIAL_HEAP_SIZE"))
            return;
        initial_heap_size = CIEL_DEFAULT_GC_INITIAL_HEAP_SIZE;
    }

    size_t current_heap_size = GC_get_heap_size();
    if (current_heap_size < initial_heap_size)
        (void)GC_expand_hp(initial_heap_size - current_heap_size);
}

void ciel_runtime_init(void) {
    if (ciel_runtime_initialized)
        return;
    ciel_runtime_initialized = 1;
    ciel_configure_gc_before_init();
    GC_INIT();
    ciel_configure_gc_after_init();
#if defined(GC_THREADS)
    GC_allow_register_threads();
#endif
    ciel_resource_runtime_init();
    atexit(ciel_resource_close_root_at_shutdown);
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

CIEL_COLD CIEL_NORETURN void ciel_panic_at(const char *message, size_t len,
                                           const char *file, size_t line) {
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

CIEL_COLD CIEL_NORETURN void ciel_panic(const char *message, size_t len) {
    ciel_panic_at(message, len, "<runtime>", 0);
}

int ciel_errno(void) { return errno; }

int ciel_async_timeout_errno(void) { return ETIMEDOUT; }

int ciel_async_channel_closed_errno(void) { return EPIPE; }

int ciel_async_again_errno(void) { return EAGAIN; }

CIEL_MALLOC_LIKE CIEL_RETURNS_NONNULL char *
ciel_cstr_from_slice(const char *ptr, size_t len) {
    char *out = (char *)ciel_alloc_atomic_array(sizeof(char), len + 1);
    for (size_t i = 0; i < len; i++)
        out[i] = ptr[i];
    out[len] = '\0';
    return out;
}

CielConstSlice_char ciel_diagnostic_text_copy(const char *ptr, size_t len) {
    CielConstSlice_char out;
    out.ptr = ciel_cstr_from_slice(ptr, len);
    out.len = len;
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

static int32_t ciel_parse_range(double value) {
    if (value == 0.0)
        return -1;
    return 1;
}

int32_t ciel_parse_f64(const char *text, size_t len, double *out,
                       size_t *out_end, int32_t *out_range) {
    if (text == NULL || out == NULL || out_end == NULL || out_range == NULL)
        return EINVAL;
    char *copy = ciel_cstr_from_slice(text, len);
    errno = 0;
    char *end = NULL;
    double value = strtod(copy, &end);
    if (end == copy || end == NULL) {
        *out_end = 0;
        *out_range = 0;
        return EINVAL;
    }
    *out = value;
    *out_end = (size_t)(end - copy);
    *out_range = errno == ERANGE ? ciel_parse_range(value) : 0;
    return 0;
}

int32_t ciel_parse_f32(const char *text, size_t len, float *out,
                       size_t *out_end, int32_t *out_range) {
    if (text == NULL || out == NULL || out_end == NULL || out_range == NULL)
        return EINVAL;
    char *copy = ciel_cstr_from_slice(text, len);
    errno = 0;
    char *end = NULL;
    float value = strtof(copy, &end);
    if (end == copy || end == NULL) {
        *out_end = 0;
        *out_range = 0;
        return EINVAL;
    }
    *out = value;
    *out_end = (size_t)(end - copy);
    *out_range = errno == ERANGE ? ciel_parse_range((double)value) : 0;
    return 0;
}

int ciel_io_open_read(const char *path) { return open(path, O_RDONLY); }

int ciel_io_open_write(const char *path) {
    return open(path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
}

int ciel_io_open_append(const char *path) {
    return open(path, O_WRONLY | O_CREAT | O_APPEND, 0666);
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

#if defined(__GNUC__) || defined(__clang__)
__attribute__((constructor))
#endif
static CIEL_MAYBE_UNUSED void ciel_internal_constructor(void) {
    ciel_runtime_init();
}
