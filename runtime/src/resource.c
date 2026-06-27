#include "internal.h"

typedef enum CielResourceState {
    CIEL_RESOURCE_STATE_OPEN = 1,
    CIEL_RESOURCE_STATE_CLOSED = 2,
    CIEL_RESOURCE_STATE_MOVED = 3,
    CIEL_RESOURCE_STATE_RETIRED = 4,
} CielResourceState;

typedef struct CielResourceEntry {
    uint64_t id;
    uint64_t generation;
    CielResourceKind kind;
    CielResourceState state;
    int fd;
    int borrowed;
    void *ptr;
    CielResourceCloseFn close_fn;
    const void *native_type;
    struct CielResourceEntry *next;
} CielResourceEntry;

struct CielResourceOwner {
    uint64_t id;
    uint64_t generation;
    CielResourceLimits limits;
    size_t live_resources;
    size_t live_child_owners;
    size_t live_pending_ops;
    size_t live_descriptors;
    uint64_t next_resource_id;
    uint8_t closed;
    struct CielResourceOwner *parent;
    struct CielResourceOwner *first_child;
    struct CielResourceOwner *next_sibling;
    CielResourceEntry *entries;
};

typedef struct CielResourceCloseAction {
    CielResourceKind kind;
    int fd;
    int borrowed;
    void *ptr;
    CielResourceCloseFn close_fn;
} CielResourceCloseAction;

static pthread_mutex_t ciel_resource_mutex = PTHREAD_MUTEX_INITIALIZER;
static atomic_uint_fast64_t ciel_resource_next_owner_id = 1;
static CielResourceOwner *ciel_resource_root_owner = NULL;
static __thread CielResourceOwner *ciel_resource_current = NULL;
static __thread CielResourceOwner *ciel_resource_owner_stack[128];
static __thread size_t ciel_resource_owner_stack_len = 0;

static int ciel_resource_kind_is_descriptor(CielResourceKind kind) {
    return kind == CIEL_RESOURCE_KIND_FILE ||
           kind == CIEL_RESOURCE_KIND_TCP_LISTENER ||
           kind == CIEL_RESOURCE_KIND_TCP_STREAM ||
           kind == CIEL_RESOURCE_KIND_ASYNC_FD ||
           kind == CIEL_RESOURCE_KIND_ASYNC_TCP_LISTENER;
}

static int ciel_resource_kind_is_pending_op(CielResourceKind kind) {
    return kind == CIEL_RESOURCE_KIND_ASYNC_OP;
}

CielResourceLimits ciel_resource_default_limits(void) {
    CielResourceLimits limits;
    limits.max_resources = 4096;
    limits.max_child_owners = 1024;
    limits.max_pending_ops = 8192;
    limits.max_descriptors = 4096;
    return limits;
}

static uint64_t ciel_resource_alloc_owner_id(void) {
    uint64_t id = atomic_fetch_add_explicit(&ciel_resource_next_owner_id, 1,
                                            memory_order_relaxed);
    if (id == 0)
        id = atomic_fetch_add_explicit(&ciel_resource_next_owner_id, 1,
                                       memory_order_relaxed);
    return id;
}

static CielResourceOwner *
ciel_resource_owner_alloc_locked(CielResourceOwner *parent,
                                 CielResourceLimits limits, int32_t *out_rc) {
    if (out_rc != NULL)
        *out_rc = 0;
    uint64_t id = ciel_resource_alloc_owner_id();
    if (id == UINT64_MAX) {
        if (out_rc != NULL)
            *out_rc = EOVERFLOW;
        return NULL;
    }
    if (parent != NULL) {
        if (parent->closed) {
            if (out_rc != NULL)
                *out_rc = EBADF;
            return NULL;
        }
        if (parent->limits.max_child_owners != 0 &&
            parent->live_child_owners >= parent->limits.max_child_owners) {
            if (out_rc != NULL)
                *out_rc = ENOSPC;
            return NULL;
        }
    }
    CielResourceOwner *owner = (CielResourceOwner *)ciel_alloc_uncollectable(
        sizeof(CielResourceOwner));
    memset(owner, 0, sizeof(*owner));
    owner->id = id;
    owner->generation = 1;
    owner->limits = limits;
    owner->next_resource_id = 1;
    owner->parent = parent;
    if (parent != NULL) {
        owner->next_sibling = parent->first_child;
        parent->first_child = owner;
        parent->live_child_owners++;
    }
    return owner;
}

static void ciel_resource_init_locked(void) {
    if (ciel_resource_root_owner != NULL)
        return;
    int32_t rc = 0;
    ciel_resource_root_owner = ciel_resource_owner_alloc_locked(
        NULL, ciel_resource_default_limits(), &rc);
    (void)rc;
}

void ciel_resource_runtime_init(void) {
    pthread_mutex_lock(&ciel_resource_mutex);
    ciel_resource_init_locked();
    pthread_mutex_unlock(&ciel_resource_mutex);
}

CielResourceOwner *ciel_resource_current_owner(void) {
    if (ciel_resource_current != NULL)
        return ciel_resource_current;
    return ciel_resource_current_owner_or_root();
}

CielResourceOwner *ciel_resource_current_owner_or_root(void) {
    ciel_resource_runtime_init();
    if (ciel_resource_current == NULL)
        ciel_resource_current = ciel_resource_root_owner;
    return ciel_resource_current;
}

CielResourceOwner *ciel_resource_set_current_owner(CielResourceOwner *owner) {
    CielResourceOwner *previous = ciel_resource_current_owner_or_root();
    ciel_resource_current = owner == NULL ? ciel_resource_root_owner : owner;
    return previous;
}

void ciel_resource_restore_current_owner(CielResourceOwner *previous) {
    if (previous == NULL)
        previous = ciel_resource_root_owner;
    ciel_resource_current = previous;
}

CielResourceOwner *ciel_resource_owner_new_child(CielResourceOwner *parent,
                                                 CielResourceLimits limits,
                                                 int32_t *out_rc) {
    ciel_resource_runtime_init();
    if (parent == NULL)
        parent = ciel_resource_current_owner_or_root();
    pthread_mutex_lock(&ciel_resource_mutex);
    CielResourceOwner *owner =
        ciel_resource_owner_alloc_locked(parent, limits, out_rc);
    pthread_mutex_unlock(&ciel_resource_mutex);
    return owner;
}

static int ciel_resource_owner_unlink_child_locked(CielResourceOwner *parent,
                                                   CielResourceOwner *child) {
    if (parent == NULL || child == NULL)
        return 0;
    CielResourceOwner **cursor = &parent->first_child;
    while (*cursor != NULL) {
        if (*cursor == child) {
            *cursor = child->next_sibling;
            child->next_sibling = NULL;
            return 1;
        }
        cursor = &(*cursor)->next_sibling;
    }
    return 0;
}

int32_t ciel_resource_owner_detach(CielResourceOwner *owner) {
    if (owner == NULL)
        return EINVAL;
    ciel_resource_runtime_init();
    pthread_mutex_lock(&ciel_resource_mutex);
    if (owner->closed || ciel_resource_root_owner == NULL ||
        ciel_resource_root_owner->closed) {
        pthread_mutex_unlock(&ciel_resource_mutex);
        return EBADF;
    }
    CielResourceOwner *old_parent = owner->parent;
    if (old_parent == NULL || old_parent == ciel_resource_root_owner) {
        pthread_mutex_unlock(&ciel_resource_mutex);
        return 0;
    }
    CielResourceOwner *root = ciel_resource_root_owner;
    if (root->limits.max_child_owners != 0 &&
        root->live_child_owners >= root->limits.max_child_owners) {
        pthread_mutex_unlock(&ciel_resource_mutex);
        return ENOSPC;
    }
    if (!ciel_resource_owner_unlink_child_locked(old_parent, owner)) {
        pthread_mutex_unlock(&ciel_resource_mutex);
        return EBADF;
    }
    if (old_parent->live_child_owners > 0)
        old_parent->live_child_owners--;
    owner->parent = root;
    owner->next_sibling = root->first_child;
    root->first_child = owner;
    root->live_child_owners++;
    pthread_mutex_unlock(&ciel_resource_mutex);
    return 0;
}

static int32_t ciel_resource_handle_parts(CielResourceHandle handle,
                                          uint64_t *out_owner_id,
                                          uint64_t *out_resource_id,
                                          uint64_t *out_generation) {
    if (out_owner_id == NULL || out_resource_id == NULL ||
        out_generation == NULL)
        return EINVAL;
    *out_owner_id = handle.owner_id;
    *out_resource_id = handle.resource_id;
    *out_generation = handle.generation;
    return 0;
}

static CielResourceHandle ciel_resource_handle_from_parts(uint64_t owner_id,
                                                          uint64_t resource_id,
                                                          uint64_t generation) {
    CielResourceHandle handle;
    handle.owner_id = owner_id;
    handle.resource_id = resource_id;
    handle.generation = generation;
    return handle;
}

int32_t ciel_resource_scope_push_default(void) {
    return ciel_resource_scope_push_limits(ciel_resource_default_limits());
}

int32_t ciel_resource_scope_push_limits(CielResourceLimits limits) {
    if (ciel_resource_owner_stack_len >=
        sizeof(ciel_resource_owner_stack) /
            sizeof(ciel_resource_owner_stack[0]))
        return ENOSPC;
    int32_t rc = 0;
    CielResourceOwner *parent = ciel_resource_current_owner_or_root();
    CielResourceOwner *child =
        ciel_resource_owner_new_child(parent, limits, &rc);
    if (child == NULL)
        return rc == 0 ? ENOMEM : rc;
    ciel_resource_owner_stack[ciel_resource_owner_stack_len++] = parent;
    ciel_resource_current = child;
    return 0;
}

int32_t ciel_resource_scope_push_limits_raw(size_t max_resources,
                                            size_t max_child_owners,
                                            size_t max_pending_ops,
                                            size_t max_descriptors) {
    CielResourceLimits limits;
    limits.max_resources = max_resources;
    limits.max_child_owners = max_child_owners;
    limits.max_pending_ops = max_pending_ops;
    limits.max_descriptors = max_descriptors;
    return ciel_resource_scope_push_limits(limits);
}

static int32_t
ciel_resource_close_action_push(CielResourceCloseAction **actions, size_t *len,
                                size_t *cap, CielResourceEntry *entry) {
    if (*len == *cap) {
        size_t next_cap = *cap == 0 ? 16 : *cap * 2;
        CielResourceCloseAction *next = (CielResourceCloseAction *)realloc(
            *actions, next_cap * sizeof(**actions));
        if (next == NULL)
            return ENOMEM;
        *actions = next;
        *cap = next_cap;
    }
    (*actions)[*len].kind = entry->kind;
    (*actions)[*len].fd = entry->fd;
    (*actions)[*len].borrowed = entry->borrowed;
    (*actions)[*len].ptr = entry->ptr;
    (*actions)[*len].close_fn = entry->close_fn;
    (*len)++;
    return 0;
}

static int32_t
ciel_resource_apply_close_action(CielResourceCloseAction action) {
    switch (action.kind) {
    case CIEL_RESOURCE_KIND_FILE:
        if (!action.borrowed && action.fd >= 0 && close(action.fd) != 0)
            return errno == 0 ? EIO : errno;
        return 0;
    case CIEL_RESOURCE_KIND_TCP_LISTENER:
    case CIEL_RESOURCE_KIND_TCP_STREAM:
        if (action.fd >= 0 && close(action.fd) != 0)
            return errno == 0 ? EIO : errno;
        return 0;
    case CIEL_RESOURCE_KIND_ASYNC_FD:
        return ciel_async_close((CielAsyncFd *)action.ptr);
    case CIEL_RESOURCE_KIND_ASYNC_TCP_LISTENER:
        return ciel_async_tcp_close_listener(
            (CielAsyncTcpListener *)action.ptr);
    case CIEL_RESOURCE_KIND_ASYNC_OP: {
        int32_t rc = ciel_async_cancel((CielAsyncOp *)action.ptr);
        return rc == EALREADY ? 0 : rc;
    }
    case CIEL_RESOURCE_KIND_NATIVE:
        if (action.close_fn == NULL)
            return EINVAL;
        return action.close_fn(action.ptr);
    default:
        return EINVAL;
    }
}

static int32_t ciel_resource_collect_owner_close_locked(
    CielResourceOwner *owner, CielResourceCloseAction **actions, size_t *len,
    size_t *cap, CielResourceOwner ***children, size_t *child_len,
    size_t *child_cap) {
    if (owner == NULL)
        return EINVAL;
    if (owner->closed)
        return 0;
    owner->closed = 1;
    if (owner->parent != NULL && owner->parent->live_child_owners > 0)
        owner->parent->live_child_owners--;

    for (CielResourceEntry *entry = owner->entries; entry != NULL;
         entry = entry->next) {
        if (entry->state != CIEL_RESOURCE_STATE_OPEN)
            continue;
        int32_t rc = ciel_resource_close_action_push(actions, len, cap, entry);
        if (rc != 0)
            return rc;
        entry->state = CIEL_RESOURCE_STATE_CLOSED;
        if (owner->live_resources > 0)
            owner->live_resources--;
        if (ciel_resource_kind_is_pending_op(entry->kind) &&
            owner->live_pending_ops > 0)
            owner->live_pending_ops--;
        if (ciel_resource_kind_is_descriptor(entry->kind) &&
            owner->live_descriptors > 0)
            owner->live_descriptors--;
    }

    for (CielResourceOwner *child = owner->first_child; child != NULL;
         child = child->next_sibling) {
        if (child->closed)
            continue;
        if (*child_len == *child_cap) {
            size_t next_cap = *child_cap == 0 ? 8 : *child_cap * 2;
            CielResourceOwner **next = (CielResourceOwner **)realloc(
                *children, next_cap * sizeof(**children));
            if (next == NULL)
                return ENOMEM;
            *children = next;
            *child_cap = next_cap;
        }
        (*children)[(*child_len)++] = child;
    }
    return 0;
}

int32_t ciel_resource_owner_close(CielResourceOwner *owner) {
    if (owner == NULL)
        return EINVAL;
    CielResourceCloseAction *actions = NULL;
    size_t action_len = 0;
    size_t action_cap = 0;
    CielResourceOwner **children = NULL;
    size_t child_len = 0;
    size_t child_cap = 0;

    pthread_mutex_lock(&ciel_resource_mutex);
    int32_t rc = ciel_resource_collect_owner_close_locked(
        owner, &actions, &action_len, &action_cap, &children, &child_len,
        &child_cap);
    pthread_mutex_unlock(&ciel_resource_mutex);

    int32_t first_error = rc;
    for (size_t i = 0; i < action_len; i++) {
        int32_t close_rc = ciel_resource_apply_close_action(actions[i]);
        if (first_error == 0 && close_rc != 0)
            first_error = close_rc;
    }
    for (size_t i = 0; i < child_len; i++) {
        int32_t child_rc = ciel_resource_owner_close(children[i]);
        if (first_error == 0 && child_rc != 0)
            first_error = child_rc;
    }
    free(actions);
    free(children);
    return first_error;
}

int32_t ciel_resource_scope_close_current(void) {
    CielResourceOwner *owner = ciel_resource_current_owner_or_root();
    if (owner == NULL || owner == ciel_resource_root_owner)
        return EINVAL;
    CielResourceOwner *parent = owner->parent;
    if (ciel_resource_owner_stack_len > 0)
        parent = ciel_resource_owner_stack[--ciel_resource_owner_stack_len];
    ciel_resource_current = parent == NULL ? ciel_resource_root_owner : parent;
    return ciel_resource_owner_close(owner);
}

static CielResourceOwner *
ciel_resource_find_owner_locked(uint64_t owner_id, CielResourceOwner *root) {
    if (root == NULL)
        return NULL;
    if (root->id == owner_id)
        return root;
    for (CielResourceOwner *child = root->first_child; child != NULL;
         child = child->next_sibling) {
        CielResourceOwner *found =
            ciel_resource_find_owner_locked(owner_id, child);
        if (found != NULL)
            return found;
    }
    return NULL;
}

static CielResourceEntry *
ciel_resource_find_entry_locked(CielResourceOwner *owner,
                                uint64_t resource_id) {
    for (CielResourceEntry *entry = owner == NULL ? NULL : owner->entries;
         entry != NULL; entry = entry->next) {
        if (entry->id == resource_id)
            return entry;
    }
    return NULL;
}

int32_t ciel_resource_owner_enter_child_limits_raw(
    size_t max_resources, size_t max_child_owners, size_t max_pending_ops,
    size_t max_descriptors, uint64_t *out_owner_id,
    uint64_t *out_previous_owner_id) {
    if (out_owner_id == NULL || out_previous_owner_id == NULL)
        return EINVAL;
    CielResourceLimits limits;
    limits.max_resources = max_resources;
    limits.max_child_owners = max_child_owners;
    limits.max_pending_ops = max_pending_ops;
    limits.max_descriptors = max_descriptors;
    CielResourceOwner *parent = ciel_resource_current_owner_or_root();
    int32_t rc = 0;
    CielResourceOwner *child =
        ciel_resource_owner_new_child(parent, limits, &rc);
    if (child == NULL)
        return rc == 0 ? ENOMEM : rc;
    *out_previous_owner_id = parent == NULL ? 0 : parent->id;
    *out_owner_id = child->id;
    ciel_resource_current = child;
    return 0;
}

int32_t ciel_resource_restore_owner(uint64_t owner_id) {
    ciel_resource_runtime_init();
    pthread_mutex_lock(&ciel_resource_mutex);
    CielResourceOwner *owner = owner_id == 0
                                   ? ciel_resource_root_owner
                                   : ciel_resource_find_owner_locked(
                                         owner_id, ciel_resource_root_owner);
    if (owner == NULL || owner->closed) {
        pthread_mutex_unlock(&ciel_resource_mutex);
        return EBADF;
    }
    ciel_resource_current = owner;
    pthread_mutex_unlock(&ciel_resource_mutex);
    return 0;
}

int32_t ciel_resource_owner_close_id(uint64_t owner_id) {
    if (owner_id == 0)
        return EINVAL;
    ciel_resource_runtime_init();
    pthread_mutex_lock(&ciel_resource_mutex);
    CielResourceOwner *owner =
        ciel_resource_find_owner_locked(owner_id, ciel_resource_root_owner);
    pthread_mutex_unlock(&ciel_resource_mutex);
    if (owner == NULL)
        return EBADF;
    return ciel_resource_owner_close(owner);
}

static int32_t ciel_resource_resolve_locked(CielResourceHandle handle,
                                            CielResourceKind expected,
                                            CielResourceOwner **out_owner,
                                            CielResourceEntry **out_entry) {
    if (handle.owner_id == 0 || handle.resource_id == 0 ||
        handle.generation == 0)
        return EBADF;
    CielResourceOwner *owner = ciel_resource_find_owner_locked(
        handle.owner_id, ciel_resource_root_owner);
    if (owner == NULL || owner->closed)
        return EBADF;
    CielResourceEntry *entry =
        ciel_resource_find_entry_locked(owner, handle.resource_id);
    if (entry == NULL)
        return EBADF;
    if (entry->generation != handle.generation)
        return EBADF;
    if (expected != 0 && entry->kind != expected)
        return EINVAL;
    if (out_owner != NULL)
        *out_owner = owner;
    if (out_entry != NULL)
        *out_entry = entry;
    if (entry->state == CIEL_RESOURCE_STATE_CLOSED)
        return 0;
    if (entry->state != CIEL_RESOURCE_STATE_OPEN)
        return EBADF;
    return 0;
}

static int32_t ciel_resource_owner_has_capacity_locked(CielResourceOwner *owner,
                                                       CielResourceKind kind) {
    if (owner == NULL || owner->closed)
        return EBADF;
    if (owner->next_resource_id == UINT64_MAX)
        return EOVERFLOW;
    if (owner->limits.max_resources != 0 &&
        owner->live_resources >= owner->limits.max_resources)
        return ENOSPC;
    if (ciel_resource_kind_is_pending_op(kind) &&
        owner->limits.max_pending_ops != 0 &&
        owner->live_pending_ops >= owner->limits.max_pending_ops)
        return ENOSPC;
    if (ciel_resource_kind_is_descriptor(kind) &&
        owner->limits.max_descriptors != 0 &&
        owner->live_descriptors >= owner->limits.max_descriptors)
        return ENOSPC;
    return 0;
}

static int32_t ciel_resource_register_locked(CielResourceOwner *owner,
                                             CielResourceKind kind, int fd,
                                             int borrowed, void *ptr,
                                             CielResourceCloseFn close_fn,
                                             const void *native_type,
                                             CielResourceHandle *out) {
    if (out == NULL)
        return EINVAL;
    int32_t rc = ciel_resource_owner_has_capacity_locked(owner, kind);
    if (rc != 0)
        return rc;
    CielResourceEntry *entry = (CielResourceEntry *)ciel_alloc_uncollectable(
        sizeof(CielResourceEntry));
    memset(entry, 0, sizeof(*entry));
    entry->id = owner->next_resource_id++;
    entry->generation = 1;
    entry->kind = kind;
    entry->state = CIEL_RESOURCE_STATE_OPEN;
    entry->fd = fd;
    entry->borrowed = borrowed != 0;
    entry->ptr = ptr;
    entry->close_fn = close_fn;
    entry->native_type = native_type;
    entry->next = owner->entries;
    owner->entries = entry;
    owner->live_resources++;
    if (ciel_resource_kind_is_pending_op(kind))
        owner->live_pending_ops++;
    if (ciel_resource_kind_is_descriptor(kind))
        owner->live_descriptors++;
    out->owner_id = owner->id;
    out->resource_id = entry->id;
    out->generation = entry->generation;
    return 0;
}

int32_t ciel_resource_register_fd(CielResourceKind kind, int fd, int borrowed,
                                  CielResourceHandle *out) {
    if (fd < 0 || out == NULL)
        return EINVAL;
    if (!ciel_resource_kind_is_descriptor(kind))
        return EINVAL;
    CielResourceOwner *owner = ciel_resource_current_owner_or_root();
    pthread_mutex_lock(&ciel_resource_mutex);
    int32_t rc = ciel_resource_register_locked(owner, kind, fd, borrowed, NULL,
                                               NULL, NULL, out);
    pthread_mutex_unlock(&ciel_resource_mutex);
    return rc;
}

int32_t ciel_resource_register_async_fd(CielAsyncFd *fd,
                                        CielResourceHandle *out) {
    if (fd == NULL || out == NULL)
        return EINVAL;
    CielResourceOwner *owner = ciel_resource_current_owner_or_root();
    pthread_mutex_lock(&ciel_resource_mutex);
    int32_t rc = ciel_resource_register_locked(
        owner, CIEL_RESOURCE_KIND_ASYNC_FD, -1, 0, fd, NULL, NULL, out);
    pthread_mutex_unlock(&ciel_resource_mutex);
    return rc;
}

int32_t ciel_resource_register_async_fd_handle(CielAsyncFd *fd,
                                               uint64_t *out_owner_id,
                                               uint64_t *out_resource_id,
                                               uint64_t *out_generation) {
    CielResourceHandle handle;
    int32_t rc = ciel_resource_register_async_fd(fd, &handle);
    if (rc != 0)
        return rc;
    return ciel_resource_handle_parts(handle, out_owner_id, out_resource_id,
                                      out_generation);
}

int32_t ciel_resource_register_async_listener(CielAsyncTcpListener *listener,
                                              CielResourceHandle *out) {
    if (listener == NULL || out == NULL)
        return EINVAL;
    CielResourceOwner *owner = ciel_resource_current_owner_or_root();
    pthread_mutex_lock(&ciel_resource_mutex);
    int32_t rc = ciel_resource_register_locked(
        owner, CIEL_RESOURCE_KIND_ASYNC_TCP_LISTENER, -1, 0, listener, NULL,
        NULL, out);
    pthread_mutex_unlock(&ciel_resource_mutex);
    return rc;
}

int32_t ciel_resource_register_async_listener_handle(
    CielAsyncTcpListener *listener, uint64_t *out_owner_id,
    uint64_t *out_resource_id, uint64_t *out_generation) {
    CielResourceHandle handle;
    int32_t rc = ciel_resource_register_async_listener(listener, &handle);
    if (rc != 0)
        return rc;
    return ciel_resource_handle_parts(handle, out_owner_id, out_resource_id,
                                      out_generation);
}

int32_t ciel_resource_register_async_op(CielAsyncOp *op,
                                        CielResourceHandle *out) {
    if (op == NULL || out == NULL)
        return EINVAL;
    CielResourceOwner *owner = ciel_resource_current_owner_or_root();
    pthread_mutex_lock(&ciel_resource_mutex);
    int32_t rc = ciel_resource_register_locked(
        owner, CIEL_RESOURCE_KIND_ASYNC_OP, -1, 0, op, NULL, NULL, out);
    pthread_mutex_unlock(&ciel_resource_mutex);
    return rc;
}

int32_t ciel_resource_register_async_op_handle(CielAsyncOp *op,
                                               uint64_t *out_owner_id,
                                               uint64_t *out_resource_id,
                                               uint64_t *out_generation) {
    CielResourceHandle handle;
    int32_t rc = ciel_resource_register_async_op(op, &handle);
    if (rc != 0)
        return rc;
    return ciel_resource_handle_parts(handle, out_owner_id, out_resource_id,
                                      out_generation);
}

int32_t ciel_resource_fd_snapshot(CielResourceHandle handle,
                                  CielResourceKind expected, int *out_fd) {
    if (out_fd == NULL)
        return EINVAL;
    pthread_mutex_lock(&ciel_resource_mutex);
    CielResourceEntry *entry = NULL;
    int32_t rc = ciel_resource_resolve_locked(handle, expected, NULL, &entry);
    if (rc == 0) {
        if (entry == NULL || entry->state != CIEL_RESOURCE_STATE_OPEN)
            rc = EBADF;
        else
            *out_fd = entry->fd;
    }
    pthread_mutex_unlock(&ciel_resource_mutex);
    return rc;
}

static int32_t ciel_resource_ptr_snapshot(CielResourceHandle handle,
                                          CielResourceKind expected,
                                          void **out) {
    if (out == NULL)
        return EINVAL;
    pthread_mutex_lock(&ciel_resource_mutex);
    CielResourceEntry *entry = NULL;
    int32_t rc = ciel_resource_resolve_locked(handle, expected, NULL, &entry);
    if (rc == 0) {
        if (entry == NULL || entry->state != CIEL_RESOURCE_STATE_OPEN)
            rc = EBADF;
        else
            *out = entry->ptr;
    }
    pthread_mutex_unlock(&ciel_resource_mutex);
    return rc;
}

int32_t ciel_resource_async_fd_snapshot(CielResourceHandle handle,
                                        CielAsyncFd **out) {
    return ciel_resource_ptr_snapshot(handle, CIEL_RESOURCE_KIND_ASYNC_FD,
                                      (void **)out);
}

int32_t ciel_resource_async_fd_snapshot_handle(uint64_t owner_id,
                                               uint64_t resource_id,
                                               uint64_t generation,
                                               CielAsyncFd **out) {
    return ciel_resource_async_fd_snapshot(
        ciel_resource_handle_from_parts(owner_id, resource_id, generation),
        out);
}

int32_t ciel_resource_async_listener_snapshot(CielResourceHandle handle,
                                              CielAsyncTcpListener **out) {
    return ciel_resource_ptr_snapshot(
        handle, CIEL_RESOURCE_KIND_ASYNC_TCP_LISTENER, (void **)out);
}

int32_t ciel_resource_async_listener_snapshot_handle(
    uint64_t owner_id, uint64_t resource_id, uint64_t generation,
    CielAsyncTcpListener **out) {
    return ciel_resource_async_listener_snapshot(
        ciel_resource_handle_from_parts(owner_id, resource_id, generation),
        out);
}

int32_t ciel_resource_async_op_snapshot(CielResourceHandle handle,
                                        CielAsyncOp **out) {
    return ciel_resource_ptr_snapshot(handle, CIEL_RESOURCE_KIND_ASYNC_OP,
                                      (void **)out);
}

int32_t ciel_resource_async_op_snapshot_handle(uint64_t owner_id,
                                               uint64_t resource_id,
                                               uint64_t generation,
                                               CielAsyncOp **out) {
    return ciel_resource_async_op_snapshot(
        ciel_resource_handle_from_parts(owner_id, resource_id, generation),
        out);
}

int32_t ciel_resource_register_native(void *ptr, CielResourceCloseFn close_fn,
                                      const void *native_type,
                                      CielResourceHandle *out) {
    if (ptr == NULL || close_fn == NULL || native_type == NULL || out == NULL)
        return EINVAL;
    CielResourceOwner *owner = ciel_resource_current_owner_or_root();
    pthread_mutex_lock(&ciel_resource_mutex);
    int32_t rc =
        ciel_resource_register_locked(owner, CIEL_RESOURCE_KIND_NATIVE, -1, 0,
                                      ptr, close_fn, native_type, out);
    pthread_mutex_unlock(&ciel_resource_mutex);
    return rc;
}

int32_t ciel_resource_native_snapshot(CielResourceHandle handle,
                                      const void *native_type, void **out) {
    if (native_type == NULL || out == NULL)
        return EINVAL;
    pthread_mutex_lock(&ciel_resource_mutex);
    CielResourceEntry *entry = NULL;
    int32_t rc = ciel_resource_resolve_locked(handle, CIEL_RESOURCE_KIND_NATIVE,
                                              NULL, &entry);
    if (rc == 0) {
        if (entry == NULL || entry->state != CIEL_RESOURCE_STATE_OPEN) {
            rc = EBADF;
        } else if (entry->native_type != native_type) {
            rc = EINVAL;
        } else {
            *out = entry->ptr;
        }
    }
    pthread_mutex_unlock(&ciel_resource_mutex);
    return rc;
}

int32_t ciel_resource_close(CielResourceHandle handle) {
    CielResourceCloseAction action;
    memset(&action, 0, sizeof(action));
    pthread_mutex_lock(&ciel_resource_mutex);
    CielResourceOwner *owner = NULL;
    CielResourceEntry *entry = NULL;
    int32_t rc = ciel_resource_resolve_locked(handle, 0, &owner, &entry);
    if (rc == 0) {
        if (entry == NULL) {
            rc = EBADF;
        } else if (entry->state == CIEL_RESOURCE_STATE_CLOSED) {
            rc = 0;
        } else if (entry->state != CIEL_RESOURCE_STATE_OPEN) {
            rc = EBADF;
        } else {
            action.kind = entry->kind;
            action.fd = entry->fd;
            action.borrowed = entry->borrowed;
            action.ptr = entry->ptr;
            action.close_fn = entry->close_fn;
            entry->state = CIEL_RESOURCE_STATE_CLOSED;
            if (owner->live_resources > 0)
                owner->live_resources--;
            if (ciel_resource_kind_is_pending_op(entry->kind) &&
                owner->live_pending_ops > 0)
                owner->live_pending_ops--;
            if (ciel_resource_kind_is_descriptor(entry->kind) &&
                owner->live_descriptors > 0)
                owner->live_descriptors--;
        }
    }
    pthread_mutex_unlock(&ciel_resource_mutex);
    if (rc != 0)
        return rc;
    if (action.kind == 0)
        return 0;
    return ciel_resource_apply_close_action(action);
}

int32_t ciel_resource_close_handle(uint64_t owner_id, uint64_t resource_id,
                                   uint64_t generation) {
    CielResourceHandle handle;
    handle.owner_id = owner_id;
    handle.resource_id = resource_id;
    handle.generation = generation;
    return ciel_resource_close(handle);
}

static int ciel_resource_owner_is_ancestor_locked(CielResourceOwner *ancestor,
                                                  CielResourceOwner *owner) {
    for (CielResourceOwner *cursor = owner; cursor != NULL;
         cursor = cursor->parent) {
        if (cursor == ancestor)
            return 1;
    }
    return 0;
}

static int32_t ciel_resource_transfer_to_owner_locked(
    CielResourceHandle handle, CielResourceOwner *destination,
    CielResourceHandle *out, CielResourceOwner **out_source) {
    if (destination == NULL || out == NULL)
        return EINVAL;
    CielResourceOwner *owner = NULL;
    CielResourceEntry *entry = NULL;
    int32_t rc = ciel_resource_resolve_locked(handle, 0, &owner, &entry);
    if (rc != 0)
        return rc;
    if (entry == NULL || entry->state != CIEL_RESOURCE_STATE_OPEN ||
        owner == NULL) {
        return EBADF;
    }
    if (out_source != NULL)
        *out_source = owner;
    if (owner == destination) {
        *out = handle;
        return 0;
    }
    rc = ciel_resource_owner_has_capacity_locked(destination, entry->kind);
    if (rc != 0)
        return rc;
    CielResourceHandle fresh;
    rc = ciel_resource_register_locked(
        destination, entry->kind, entry->fd, entry->borrowed, entry->ptr,
        entry->close_fn, entry->native_type, &fresh);
    if (rc == 0) {
        entry->state = CIEL_RESOURCE_STATE_MOVED;
        if (owner->live_resources > 0)
            owner->live_resources--;
        if (ciel_resource_kind_is_pending_op(entry->kind) &&
            owner->live_pending_ops > 0)
            owner->live_pending_ops--;
        if (ciel_resource_kind_is_descriptor(entry->kind) &&
            owner->live_descriptors > 0)
            owner->live_descriptors--;
        *out = fresh;
    }
    return rc;
}

int32_t ciel_resource_transfer_to_parent(CielResourceHandle handle,
                                         CielResourceHandle *out) {
    if (out == NULL)
        return EINVAL;
    pthread_mutex_lock(&ciel_resource_mutex);
    CielResourceOwner *owner = NULL;
    CielResourceEntry *entry = NULL;
    int32_t rc = ciel_resource_resolve_locked(handle, 0, &owner, &entry);
    if (rc == 0 && (entry == NULL || entry->state != CIEL_RESOURCE_STATE_OPEN ||
                    owner == NULL || owner->parent == NULL)) {
        rc = EBADF;
    }
    if (rc == 0) {
        CielResourceHandle fresh;
        rc = ciel_resource_transfer_to_owner_locked(handle, owner->parent,
                                                    &fresh, NULL);
        if (rc == 0)
            *out = fresh;
    }
    pthread_mutex_unlock(&ciel_resource_mutex);
    return rc;
}

int32_t ciel_resource_transfer_to_current(CielResourceHandle handle,
                                          CielResourceHandle *out) {
    if (out == NULL)
        return EINVAL;
    CielResourceOwner *destination = ciel_resource_current_owner_or_root();
    pthread_mutex_lock(&ciel_resource_mutex);
    CielResourceOwner *owner = NULL;
    CielResourceEntry *entry = NULL;
    int32_t rc = ciel_resource_resolve_locked(handle, 0, &owner, &entry);
    if (rc == 0 && (entry == NULL || entry->state != CIEL_RESOURCE_STATE_OPEN ||
                    owner == NULL || destination == NULL)) {
        rc = EBADF;
    }
    if (rc == 0 &&
        !ciel_resource_owner_is_ancestor_locked(owner, destination)) {
        rc = EPERM;
    }
    if (rc == 0) {
        CielResourceHandle fresh;
        rc = ciel_resource_transfer_to_owner_locked(handle, destination, &fresh,
                                                    NULL);
        if (rc == 0)
            *out = fresh;
    }
    pthread_mutex_unlock(&ciel_resource_mutex);
    return rc;
}

int32_t ciel_resource_transfer_to_parent_handle(uint64_t owner_id,
                                                uint64_t resource_id,
                                                uint64_t generation,
                                                uint64_t *out_owner_id,
                                                uint64_t *out_resource_id,
                                                uint64_t *out_generation) {
    if (out_owner_id == NULL || out_resource_id == NULL ||
        out_generation == NULL)
        return EINVAL;
    CielResourceHandle handle;
    CielResourceHandle out;
    handle.owner_id = owner_id;
    handle.resource_id = resource_id;
    handle.generation = generation;
    int32_t rc = ciel_resource_transfer_to_parent(handle, &out);
    if (rc != 0)
        return rc;
    *out_owner_id = out.owner_id;
    *out_resource_id = out.resource_id;
    *out_generation = out.generation;
    return 0;
}

int32_t ciel_resource_reattach_to_parent_handle(uint64_t owner_id,
                                                uint64_t resource_id,
                                                uint64_t generation,
                                                uint64_t *out_owner_id,
                                                uint64_t *out_resource_id,
                                                uint64_t *out_generation) {
    if (out_owner_id == NULL || out_resource_id == NULL ||
        out_generation == NULL)
        return EINVAL;
    CielResourceOwner *current = ciel_resource_current_owner_or_root();
    if (current == NULL || current == ciel_resource_root_owner ||
        current->parent == NULL)
        return EINVAL;

    CielResourceHandle handle;
    handle.owner_id = owner_id;
    handle.resource_id = resource_id;
    handle.generation = generation;

    pthread_mutex_lock(&ciel_resource_mutex);
    CielResourceOwner *owner = NULL;
    CielResourceEntry *entry = NULL;
    int32_t rc = ciel_resource_resolve_locked(handle, 0, &owner, &entry);
    if (rc == 0 && (entry == NULL || entry->state != CIEL_RESOURCE_STATE_OPEN ||
                    owner == NULL)) {
        rc = EBADF;
    }
    if (rc == 0 && owner == current) {
        CielResourceHandle fresh;
        rc = ciel_resource_transfer_to_owner_locked(handle, current->parent,
                                                    &fresh, NULL);
        if (rc == 0) {
            *out_owner_id = fresh.owner_id;
            *out_resource_id = fresh.resource_id;
            *out_generation = fresh.generation;
        }
    } else if (rc == 0 &&
               ciel_resource_owner_is_ancestor_locked(owner, current->parent)) {
        *out_owner_id = handle.owner_id;
        *out_resource_id = handle.resource_id;
        *out_generation = handle.generation;
    } else if (rc == 0) {
        rc = EPERM;
    }
    pthread_mutex_unlock(&ciel_resource_mutex);
    return rc;
}

int32_t ciel_resource_transfer_to_current_handle(uint64_t owner_id,
                                                 uint64_t resource_id,
                                                 uint64_t generation,
                                                 uint64_t *out_owner_id,
                                                 uint64_t *out_resource_id,
                                                 uint64_t *out_generation) {
    if (out_owner_id == NULL || out_resource_id == NULL ||
        out_generation == NULL)
        return EINVAL;
    CielResourceHandle handle;
    CielResourceHandle out;
    handle.owner_id = owner_id;
    handle.resource_id = resource_id;
    handle.generation = generation;
    int32_t rc = ciel_resource_transfer_to_current(handle, &out);
    if (rc != 0)
        return rc;
    *out_owner_id = out.owner_id;
    *out_resource_id = out.resource_id;
    *out_generation = out.generation;
    return 0;
}

void ciel_resource_close_root_at_shutdown(void) {
    if (ciel_resource_root_owner != NULL)
        (void)ciel_resource_owner_close(ciel_resource_root_owner);
}
