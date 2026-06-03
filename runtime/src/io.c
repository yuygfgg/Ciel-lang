#include "internal.h"

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

int ciel_file_open_mode_flags(int32_t mode) {
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
