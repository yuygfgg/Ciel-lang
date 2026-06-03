#include "internal.h"

typedef struct CielQueueNode {
    void *value;
    struct CielQueueNode *next;
} CielQueueNode;

struct CielChannel {
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    CielQueueNode *head;
    CielQueueNode *tail;
    size_t value_size;
    size_t value_align;
    int closed;
};

CielChannel *ciel_channel_make(size_t value_size, size_t value_align) {
    ciel_runtime_init();
    CielChannel *channel = (CielChannel *)ciel_alloc(sizeof(CielChannel));
    channel->head = NULL;
    channel->tail = NULL;
    channel->value_size = value_size;
    channel->value_align = value_align;
    channel->closed = 0;
    int rc = pthread_mutex_init(&channel->mutex, NULL);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    rc = pthread_cond_init(&channel->cond, NULL);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    return channel;
}

int32_t ciel_channel_send(CielChannel *channel, const void *value) {
    if (channel == NULL || value == NULL)
        return EINVAL;
    pthread_mutex_lock(&channel->mutex);
    if (channel->closed) {
        pthread_mutex_unlock(&channel->mutex);
        return EPIPE;
    }
    CielQueueNode *node = (CielQueueNode *)ciel_alloc(sizeof(CielQueueNode));
    void *copy = ciel_alloc(channel->value_size == 0 ? 1 : channel->value_size);
    if (channel->value_size > 0)
        memcpy(copy, value, channel->value_size);
    node->value = copy;
    node->next = NULL;
    if (channel->tail != NULL)
        channel->tail->next = node;
    else
        channel->head = node;
    channel->tail = node;
    pthread_cond_signal(&channel->cond);
    pthread_mutex_unlock(&channel->mutex);
    return 0;
}

void *ciel_channel_recv(CielChannel *channel) {
    if (channel == NULL) {
        errno = EINVAL;
        return NULL;
    }
    pthread_mutex_lock(&channel->mutex);
    while (channel->head == NULL && !channel->closed)
        pthread_cond_wait(&channel->cond, &channel->mutex);
    if (channel->head == NULL && channel->closed) {
        pthread_mutex_unlock(&channel->mutex);
        errno = EPIPE;
        return NULL;
    }
    CielQueueNode *node = channel->head;
    channel->head = node->next;
    if (channel->head == NULL)
        channel->tail = NULL;
    pthread_mutex_unlock(&channel->mutex);
    return node->value;
}

int32_t ciel_channel_close(CielChannel *channel) {
    if (channel == NULL)
        return EINVAL;
    pthread_mutex_lock(&channel->mutex);
    channel->closed = 1;
    pthread_cond_broadcast(&channel->cond);
    pthread_mutex_unlock(&channel->mutex);
    return 0;
}

struct CielMutex {
    pthread_mutex_t mutex;
    void *slot;
    size_t value_size;
    size_t value_align;
};

CielMutex *ciel_mutex_make(size_t value_size, size_t value_align,
                           const void *initial) {
    if (initial == NULL) {
        errno = EINVAL;
        return NULL;
    }
    ciel_runtime_init();
    CielMutex *mutex = (CielMutex *)ciel_alloc(sizeof(CielMutex));
    mutex->value_size = value_size;
    mutex->value_align = value_align;
    mutex->slot = ciel_alloc(value_size == 0 ? 1 : value_size);
    if (value_size > 0)
        memcpy(mutex->slot, initial, value_size);
    int rc = pthread_mutex_init(&mutex->mutex, NULL);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    return mutex;
}

void *ciel_mutex_lock(CielMutex *mutex) {
    if (mutex == NULL) {
        errno = EINVAL;
        return NULL;
    }
    int rc = pthread_mutex_lock(&mutex->mutex);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    return mutex->slot;
}

int32_t ciel_mutex_unlock(CielMutex *mutex) {
    if (mutex == NULL)
        return EINVAL;
    int rc = pthread_mutex_unlock(&mutex->mutex);
    if (rc != 0)
        errno = rc;
    return rc;
}
