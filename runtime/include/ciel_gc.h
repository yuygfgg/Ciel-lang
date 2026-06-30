#ifndef CIEL_GC_H
#define CIEL_GC_H

#include "ciel_base.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct CielRoot CielRoot;

CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE1 CIEL_RETURNS_NONNULL void*
ciel_alloc(size_t size);
CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE1 CIEL_RETURNS_NONNULL void*
ciel_alloc_atomic(size_t size);
CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE2 CIEL_RETURNS_NONNULL void*
ciel_alloc_array(size_t elem_size, size_t len);
CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE2 CIEL_RETURNS_NONNULL void*
ciel_alloc_atomic_array(size_t elem_size, size_t len);
CIEL_ALLOC_SIZE_ARG2 CIEL_RETURNS_NONNULL void* ciel_realloc(void* old,
                                                             size_t size);
CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE1 CIEL_RETURNS_NONNULL void*
ciel_alloc_uncollectable(size_t size);
void ciel_free(void* ptr);

typedef void (*CielFinalizerFn)(void* obj, void* client_data);
void ciel_register_finalizer(void* obj, CielFinalizerFn finalizer,
                             void* client_data);

void* ciel_raw_alloc_zeroed(size_t elem_size, size_t align, size_t capacity);
void* ciel_raw_realloc_zeroed(void* old, size_t elem_size, size_t align,
                              size_t initialized, size_t next_capacity);

CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE_ARG2 CIEL_RETURNS_NONNULL void*
ciel_box_value(const void* value, size_t size);
CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE1 void* ciel_box_copy(size_t size, size_t align,
                                                      const void* source);
CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE1 void* ciel_actor_message_alloc(size_t size);
CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE1 void*
ciel_actor_box_copy(size_t size, size_t align, const void* source);
int ciel_u8_copy(uint8_t* dst, const uint8_t* src, size_t len);

int32_t ciel_thread_attach(void);
void ciel_thread_detach(void);
int32_t ciel_runtime_enter_callback(void);
void ciel_runtime_leave_callback(void);
CielRoot* ciel_root_pin(void* ptr);
void* ciel_root_get(CielRoot* root);
void ciel_root_unpin(CielRoot* root);

#ifdef __cplusplus
}
#endif

#endif
