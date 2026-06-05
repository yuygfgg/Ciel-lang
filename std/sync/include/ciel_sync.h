#ifndef CIEL_SYNC_H
#define CIEL_SYNC_H

#include "ciel_base.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct CielChannel CielChannel;
typedef struct CielMutex CielMutex;

CielChannel* ciel_channel_make(size_t value_size, size_t value_align);
int32_t ciel_channel_send(CielChannel* channel, const void* value);
void* ciel_channel_recv(CielChannel* channel);
int32_t ciel_channel_close(CielChannel* channel);
CielMutex* ciel_mutex_make(size_t value_size, size_t value_align,
                           const void* initial);
void* ciel_mutex_lock(CielMutex* mutex);
int32_t ciel_mutex_unlock(CielMutex* mutex);

#ifdef __cplusplus
}
#endif

#endif
