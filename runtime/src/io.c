#include "internal.h"

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

static CielResourceHandle ciel_resource_handle_from_parts(uint64_t owner_id,
                                                          uint64_t resource_id,
                                                          uint64_t generation) {
    CielResourceHandle handle;
    handle.owner_id = owner_id;
    handle.resource_id = resource_id;
    handle.generation = generation;
    return handle;
}

static int32_t ciel_file_handle_out(CielResourceHandle handle,
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

int32_t ciel_file_open(int32_t mode, const char *path, uint64_t *out_owner_id,
                       uint64_t *out_resource_id, uint64_t *out_generation) {
    if (path == NULL)
        return EINVAL;
    int flags = ciel_file_open_mode_flags(mode);
    if (flags < 0)
        return errno == 0 ? EINVAL : errno;
    int fd = open(path, flags, 0666);
    if (fd < 0)
        return errno == 0 ? EIO : errno;
    CielResourceHandle handle;
    int32_t rc =
        ciel_resource_register_fd(CIEL_RESOURCE_KIND_FILE, fd, 0, &handle);
    if (rc != 0) {
        close(fd);
        return rc;
    }
    return ciel_file_handle_out(handle, out_owner_id, out_resource_id,
                                out_generation);
}

static int32_t ciel_file_borrowed_fd(int fd, uint64_t *out_owner_id,
                                     uint64_t *out_resource_id,
                                     uint64_t *out_generation) {
    CielResourceHandle handle;
    int32_t rc =
        ciel_resource_register_fd(CIEL_RESOURCE_KIND_FILE, fd, 1, &handle);
    if (rc != 0)
        return rc;
    return ciel_file_handle_out(handle, out_owner_id, out_resource_id,
                                out_generation);
}

int32_t ciel_file_stdout(uint64_t *out_owner_id, uint64_t *out_resource_id,
                         uint64_t *out_generation) {
    return ciel_file_borrowed_fd(STDOUT_FILENO, out_owner_id, out_resource_id,
                                 out_generation);
}

int32_t ciel_file_stderr(uint64_t *out_owner_id, uint64_t *out_resource_id,
                         uint64_t *out_generation) {
    return ciel_file_borrowed_fd(STDERR_FILENO, out_owner_id, out_resource_id,
                                 out_generation);
}

int32_t ciel_file_close(uint64_t owner_id, uint64_t resource_id,
                        uint64_t generation) {
    return ciel_resource_close_handle(owner_id, resource_id, generation);
}

ssize_t ciel_file_read(uint64_t owner_id, uint64_t resource_id,
                       uint64_t generation, void *buf, size_t count) {
    if (buf == NULL && count > 0) {
        errno = EINVAL;
        return -1;
    }
    int fd = -1;
    int32_t rc = ciel_resource_fd_snapshot(
        ciel_resource_handle_from_parts(owner_id, resource_id, generation),
        CIEL_RESOURCE_KIND_FILE, &fd);
    if (rc != 0) {
        errno = rc;
        return -1;
    }
    return read(fd, buf, count);
}

ssize_t ciel_file_write(uint64_t owner_id, uint64_t resource_id,
                        uint64_t generation, const void *buf, size_t count) {
    if (buf == NULL && count > 0) {
        errno = EINVAL;
        return -1;
    }
    int fd = -1;
    int32_t rc = ciel_resource_fd_snapshot(
        ciel_resource_handle_from_parts(owner_id, resource_id, generation),
        CIEL_RESOURCE_KIND_FILE, &fd);
    if (rc != 0) {
        errno = rc;
        return -1;
    }
    return write(fd, buf, count);
}
