#include "internal.h"

CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE1 CIEL_RETURNS_NONNULL void *
ciel_alloc(size_t size) {
    ciel_runtime_init();
    void *ptr = GC_MALLOC(size);
    if (ptr == NULL) {
        fputs("out of memory\n", stderr);
        exit(0);
    }
    return ptr;
}

CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE1 CIEL_RETURNS_NONNULL void *
ciel_alloc_atomic(size_t size) {
    ciel_runtime_init();
    void *ptr = GC_MALLOC_ATOMIC(size == 0 ? 1 : size);
    if (ptr == NULL) {
        fputs("out of memory\n", stderr);
        exit(0);
    }
    return ptr;
}

CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE2 CIEL_RETURNS_NONNULL void *
ciel_alloc_array(size_t elem_size, size_t len) {
    if (elem_size != 0 && len > SIZE_MAX / elem_size) {
        fputs("allocation size overflow\n", stderr);
        exit(0);
    }
    size_t bytes = elem_size * len;
    return ciel_alloc(bytes == 0 ? 1 : bytes);
}

CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE2 CIEL_RETURNS_NONNULL void *
ciel_alloc_atomic_array(size_t elem_size, size_t len) {
    if (elem_size != 0 && len > SIZE_MAX / elem_size) {
        fputs("allocation size overflow\n", stderr);
        exit(0);
    }
    size_t bytes = elem_size * len;
    return ciel_alloc_atomic(bytes == 0 ? 1 : bytes);
}

CIEL_ALLOC_SIZE_ARG2 CIEL_RETURNS_NONNULL void *ciel_realloc(void *old,
                                                             size_t size) {
    ciel_runtime_init();
    void *ptr = GC_REALLOC(old, size == 0 ? 1 : size);
    if (ptr == NULL) {
        fputs("out of memory\n", stderr);
        exit(0);
    }
    return ptr;
}

void *ciel_raw_alloc_zeroed(size_t elem_size, size_t align, size_t capacity) {
    (void)align;
    if (elem_size != 0 && capacity > SIZE_MAX / elem_size) {
        errno = EOVERFLOW;
        return NULL;
    }
    size_t bytes = elem_size * capacity;
    // GC_MALLOC clears pointerful objects, so we don't need manual memset here.
    return ciel_alloc(bytes == 0 ? 1 : bytes);
}

void *ciel_raw_realloc_zeroed(void *old, size_t elem_size, size_t align,
                              size_t initialized, size_t next_capacity) {
    (void)align;
    if (initialized > next_capacity) {
        errno = EINVAL;
        return NULL;
    }
    if (elem_size != 0 && (next_capacity > SIZE_MAX / elem_size ||
                           initialized > SIZE_MAX / elem_size)) {
        errno = EOVERFLOW;
        return NULL;
    }
    size_t bytes = elem_size * next_capacity;
    size_t init_bytes = elem_size * initialized;
    void *out = ciel_alloc(bytes == 0 ? 1 : bytes);
    if (init_bytes > 0 && old != NULL)
        memcpy(out, old, init_bytes);
    return out;
}

CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE1 CIEL_RETURNS_NONNULL void *
ciel_alloc_uncollectable(size_t size) {
    ciel_runtime_init();
    void *ptr = GC_MALLOC_UNCOLLECTABLE(size == 0 ? 1 : size);
    if (ptr == NULL) {
        fputs("out of memory\n", stderr);
        exit(0);
    }
    memset(ptr, 0, size == 0 ? 1 : size);
    return ptr;
}

void ciel_free(void *ptr) {
    if (ptr != NULL)
        GC_FREE(ptr);
}

void ciel_register_finalizer(void *obj, CielFinalizerFn finalizer,
                             void *client_data) {
    if (obj == NULL || finalizer == NULL)
        return;
    GC_register_finalizer(obj, finalizer, client_data, NULL, NULL);
}

CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE_ARG2 CIEL_RETURNS_NONNULL void *
ciel_box_value(const void *value, size_t size) {
    void *out = ciel_alloc(size == 0 ? 1 : size);
    if (size > 0)
        memcpy(out, value, size);
    return out;
}

CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE1 void *ciel_box_copy(size_t size, size_t align,
                                                      const void *source) {
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

CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE1 void *ciel_actor_message_alloc(size_t size) {
    return ciel_alloc_uncollectable(size == 0 ? 1 : size);
}

CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE1 void *
ciel_actor_box_copy(size_t size, size_t align, const void *source) {
    (void)align;
    if (source == NULL && size > 0) {
        errno = EINVAL;
        return NULL;
    }
    void *out = ciel_actor_message_alloc(size == 0 ? 1 : size);
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

static pthread_mutex_t ciel_root_pool_mutex = PTHREAD_MUTEX_INITIALIZER;
static CielRoot *ciel_root_pool = NULL;

static pthread_key_t ciel_gc_thread_key;
static pthread_once_t ciel_gc_thread_key_once = PTHREAD_ONCE_INIT;
static __thread int ciel_runtime_callback_depth = 0;

static void ciel_gc_thread_key_destructor(void *value) {
#if defined(GC_THREADS)
    if (value != NULL)
        GC_unregister_my_thread();
#else
    (void)value;
#endif
}

static void ciel_gc_thread_key_init(void) {
    (void)pthread_key_create(&ciel_gc_thread_key,
                             ciel_gc_thread_key_destructor);
}

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

int32_t ciel_thread_attach_persistent(void) {
    ciel_runtime_init();
#if defined(GC_THREADS)
    pthread_once(&ciel_gc_thread_key_once, ciel_gc_thread_key_init);
    if (pthread_getspecific(ciel_gc_thread_key) != NULL)
        return 0;
    struct GC_stack_base stack_base;
    if (GC_get_stack_base(&stack_base) != GC_SUCCESS)
        return 1;
    int result = GC_register_my_thread(&stack_base);
    if (result == GC_SUCCESS) {
        (void)pthread_setspecific(ciel_gc_thread_key, (void *)1);
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
        int32_t rc = ciel_thread_attach_persistent();
        if (rc != 0)
            return rc;
    }
    ciel_runtime_callback_depth++;
    return 0;
}

void ciel_runtime_leave_callback(void) {
    if (ciel_runtime_callback_depth <= 0)
        return;
    ciel_runtime_callback_depth--;
}

CielRoot *ciel_root_pin(void *ptr) {
    ciel_runtime_init();
    CielRoot *root = NULL;
    pthread_mutex_lock(&ciel_root_pool_mutex);
    if (ciel_root_pool != NULL) {
        root = ciel_root_pool;
        ciel_root_pool = root->next;
    }
    pthread_mutex_unlock(&ciel_root_pool_mutex);
    if (root == NULL) {
        root = (CielRoot *)GC_MALLOC_UNCOLLECTABLE(sizeof(CielRoot));
        if (root == NULL) {
            fputs("out of memory\n", stderr);
            exit(0);
        }
    }
    root->next = NULL;
    root->ptr = ptr;
    return root;
}

void *ciel_root_get(CielRoot *root) { return root == NULL ? NULL : root->ptr; }

void ciel_root_unpin(CielRoot *root) {
    if (root == NULL)
        return;
    root->ptr = NULL;
    pthread_mutex_lock(&ciel_root_pool_mutex);
    root->next = ciel_root_pool;
    ciel_root_pool = root;
    pthread_mutex_unlock(&ciel_root_pool_mutex);
}
