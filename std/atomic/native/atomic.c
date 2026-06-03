#include "ciel_atomic.h"
#include "ciel_core.h"
#include "ciel_gc.h"

#include <errno.h>
#include <stdatomic.h>
#include <string.h>

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
