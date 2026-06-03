#include "internal.h"

static void ciel_bytes_finalizer(void *obj, void *client_data) {
    (void)client_data;
    CielBytes *bytes = (CielBytes *)obj;
    if (bytes == NULL)
        return;
    free(bytes->data);
    bytes->data = NULL;
    bytes->len = 0;
    bytes->cap = 0;
}

uint8_t *ciel_bytes_data_alloc(size_t len) {
    uint8_t *data = (uint8_t *)malloc(len == 0 ? 1 : len);
    if (data == NULL) {
        fputs("out of memory\n", stderr);
        exit(0);
    }
    return data;
}

CielBytes *ciel_bytes_new(size_t len) {
    ciel_runtime_init();
    CielBytes *bytes = (CielBytes *)GC_MALLOC_ATOMIC(sizeof(CielBytes));
    if (bytes == NULL) {
        fputs("out of memory\n", stderr);
        exit(0);
    }
    bytes->len = len;
    bytes->cap = len;
    bytes->data = ciel_bytes_data_alloc(len);
    GC_register_finalizer(bytes, ciel_bytes_finalizer, NULL, NULL, NULL);
    return bytes;
}

CielBytes *ciel_bytes_copy(const uint8_t *ptr, size_t len) {
    if (ptr == NULL && len > 0) {
        errno = EINVAL;
        return NULL;
    }
    CielBytes *bytes = ciel_bytes_new(len);
    if (len > 0)
        memcpy(bytes->data, ptr, len);
    return bytes;
}

CielBytes *ciel_bytes_copy_chars(const char *ptr, size_t len) {
    return ciel_bytes_copy((const uint8_t *)ptr, len);
}

CielBytes *ciel_bytes_concat(const uint8_t *left, size_t left_len,
                             const uint8_t *right, size_t right_len) {
    if ((left == NULL && left_len > 0) || (right == NULL && right_len > 0)) {
        errno = EINVAL;
        return NULL;
    }
    if (left_len > SIZE_MAX - right_len) {
        errno = EOVERFLOW;
        return NULL;
    }
    CielBytes *bytes = ciel_bytes_new(left_len + right_len);
    if (left_len > 0)
        memcpy(bytes->data, left, left_len);
    if (right_len > 0)
        memcpy(bytes->data + left_len, right, right_len);
    return bytes;
}

CielBytes *ciel_bytes_prepend(const uint8_t *prefix, size_t prefix_len,
                              CielBytes *bytes) {
    if (bytes == NULL || (prefix == NULL && prefix_len > 0)) {
        errno = EINVAL;
        return NULL;
    }
    if (prefix_len > SIZE_MAX - bytes->len) {
        errno = EOVERFLOW;
        return NULL;
    }
    CielBytes *out = ciel_bytes_new(prefix_len + bytes->len);
    if (prefix_len > 0)
        memcpy(out->data, prefix, prefix_len);
    if (bytes->len > 0)
        memcpy(out->data + prefix_len, bytes->data, bytes->len);
    return out;
}

CielBytes *ciel_bytes_slice(CielBytes *bytes, size_t offset, size_t len) {
    if (bytes == NULL) {
        errno = EINVAL;
        return NULL;
    }
    if (offset > bytes->len || len > bytes->len - offset) {
        errno = EINVAL;
        return NULL;
    }
    CielBytes *out = ciel_bytes_new(len);
    if (len > 0)
        memcpy(out->data, bytes->data + offset, len);
    return out;
}

size_t ciel_bytes_len(CielBytes *bytes) {
    return bytes == NULL ? 0 : bytes->len;
}

size_t ciel_bytes_capacity(CielBytes *bytes) {
    return bytes == NULL ? 0 : bytes->cap;
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

int32_t ciel_bytes_copy_to_chars(CielBytes *bytes, char *out, size_t cap,
                                 size_t *copied) {
    return ciel_bytes_copy_to(bytes, (uint8_t *)out, cap, copied);
}
