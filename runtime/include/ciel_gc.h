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

CielSlice_u8 ciel_runtime_u8_alloc_slice(size_t len);
CielSlice_char ciel_runtime_char_alloc_slice(size_t len);
CielSlice_u8 ciel_runtime_u8_realloc_slice(CielSlice_u8 old, size_t len);

void* ciel_map_alloc_buckets(size_t capacity);
void* ciel_map_bucket_get(void* buckets, size_t index);
void ciel_map_bucket_set(void* buckets, size_t index, void* value);

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
