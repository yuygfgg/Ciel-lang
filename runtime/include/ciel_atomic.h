#ifndef CIEL_ATOMIC_H
#define CIEL_ATOMIC_H

#include "ciel_base.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct CielAtomic CielAtomic;

CielAtomic* ciel_atomic_make(size_t value_size, size_t value_align,
                             const void* initial);
void* ciel_atomic_load(CielAtomic* atomic, int32_t order);
int32_t ciel_atomic_store(CielAtomic* atomic, const void* value, int32_t order);
void* ciel_atomic_exchange(CielAtomic* atomic, const void* value,
                           int32_t order);
void* ciel_atomic_compare_exchange(CielAtomic* atomic, const void* expected,
                                   const void* desired, int32_t* exchanged,
                                   int32_t success, int32_t failure);
void* ciel_atomic_fetch_add(CielAtomic* atomic, const void* value,
                            int32_t order);
void* ciel_atomic_fetch_sub(CielAtomic* atomic, const void* value,
                            int32_t order);

#ifdef __cplusplus
}
#endif

#endif
