#include "internal.h"

typedef struct CielTask CielTask;
typedef struct CielTaskWaitNode CielTaskWaitNode;
struct CielTaskWaitNode {
    CielTask *task;
    struct CielTaskWaitNode *next;
};

static void ciel_task_schedule_waiters(CielTaskWaitNode *waiters);

struct CielBufferedReader {
    CielAsyncFd *fd;
    pthread_mutex_t mutex;
    CielBytes *buffer;
    size_t offset;
    size_t capacity;
    CielAsyncOp *pending_read;
};

typedef struct CielAsyncQueueNode {
    void *value;
    struct CielAsyncQueueNode *next;
} CielAsyncQueueNode;

struct CielAsyncChannel {
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    CielAsyncQueueNode *head;
    CielAsyncQueueNode *tail;
    CielTaskWaitNode *send_waiters;
    CielTaskWaitNode *recv_waiters;
    size_t len;
    size_t reserved;
    size_t capacity;
    size_t value_size;
    size_t value_align;
    size_t live_senders;
    size_t live_receivers;
};

struct CielAsyncSender {
    CielAsyncChannel *channel;
    uint8_t closed;
};

struct CielAsyncReceiver {
    CielAsyncChannel *channel;
    uint8_t closed;
};

struct CielAsyncSendPermit {
    CielAsyncChannel *channel;
    uint8_t used;
};

static void ciel_async_channel_broadcast(CielAsyncChannel *channel) {
    if (channel == NULL)
        return;
    CielTaskWaitNode *send_waiters = NULL;
    CielTaskWaitNode *recv_waiters = NULL;
    pthread_mutex_lock(&channel->mutex);
    send_waiters = channel->send_waiters;
    recv_waiters = channel->recv_waiters;
    channel->send_waiters = NULL;
    channel->recv_waiters = NULL;
    pthread_cond_broadcast(&channel->cond);
    pthread_mutex_unlock(&channel->mutex);
    ciel_task_schedule_waiters(send_waiters);
    ciel_task_schedule_waiters(recv_waiters);
}

static CielTaskWaitNode *
ciel_async_channel_take_all_waiters_locked(CielTaskWaitNode **head) {
    CielTaskWaitNode *waiters = *head;
    *head = NULL;
    return waiters;
}

static CielTaskWaitNode *
ciel_async_channel_take_one_waiter_locked(CielTaskWaitNode **head) {
    CielTaskWaitNode *waiter = *head;
    if (waiter != NULL)
        *head = waiter->next;
    if (waiter != NULL)
        waiter->next = NULL;
    return waiter;
}

static CielTaskWaitNode *
ciel_async_channel_take_all_send_waiters_locked(CielAsyncChannel *channel) {
    CielTaskWaitNode *waiters =
        ciel_async_channel_take_all_waiters_locked(&channel->send_waiters);
    pthread_cond_broadcast(&channel->cond);
    return waiters;
}

static CielTaskWaitNode *
ciel_async_channel_take_all_recv_waiters_locked(CielAsyncChannel *channel) {
    CielTaskWaitNode *waiters =
        ciel_async_channel_take_all_waiters_locked(&channel->recv_waiters);
    pthread_cond_broadcast(&channel->cond);
    return waiters;
}

static CielTaskWaitNode *
ciel_async_channel_take_send_waiter_locked(CielAsyncChannel *channel) {
    CielTaskWaitNode *waiter =
        ciel_async_channel_take_one_waiter_locked(&channel->send_waiters);
    pthread_cond_signal(&channel->cond);
    return waiter;
}

static CielTaskWaitNode *
ciel_async_channel_take_recv_waiter_locked(CielAsyncChannel *channel) {
    CielTaskWaitNode *waiter =
        ciel_async_channel_take_one_waiter_locked(&channel->recv_waiters);
    pthread_cond_signal(&channel->cond);
    return waiter;
}

static int32_t ciel_async_channel_enqueue_locked(CielAsyncChannel *channel,
                                                 const void *value) {
    if (channel == NULL || (value == NULL && channel->value_size > 0))
        return EINVAL;
    CielAsyncQueueNode *node =
        (CielAsyncQueueNode *)ciel_alloc(sizeof(CielAsyncQueueNode));
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
    channel->len++;
    return 0;
}

static void *ciel_async_channel_pop_locked(CielAsyncChannel *channel) {
    CielAsyncQueueNode *node = channel->head;
    if (node == NULL)
        return NULL;
    channel->head = node->next;
    if (channel->head == NULL)
        channel->tail = NULL;
    channel->len--;
    void *value = node->value;
    return value;
}

int32_t ciel_async_channel_make(size_t value_size, size_t value_align,
                                size_t capacity, CielAsyncSender **sender_out,
                                CielAsyncReceiver **receiver_out) {
    if (sender_out == NULL || receiver_out == NULL || value_align == 0 ||
        capacity == 0)
        return EINVAL;
    ciel_runtime_init();
    CielAsyncChannel *channel =
        (CielAsyncChannel *)ciel_alloc(sizeof(CielAsyncChannel));
    memset(channel, 0, sizeof(*channel));
    channel->capacity = capacity;
    channel->value_size = value_size;
    channel->value_align = value_align;
    channel->live_senders = 1;
    channel->live_receivers = 1;
    int rc = pthread_mutex_init(&channel->mutex, NULL);
    if (rc != 0)
        return rc;
    rc = pthread_cond_init(&channel->cond, NULL);
    if (rc != 0)
        return rc;
    CielAsyncSender *sender =
        (CielAsyncSender *)ciel_alloc(sizeof(CielAsyncSender));
    sender->channel = channel;
    sender->closed = 0;
    CielAsyncReceiver *receiver =
        (CielAsyncReceiver *)ciel_alloc(sizeof(CielAsyncReceiver));
    receiver->channel = channel;
    receiver->closed = 0;
    *sender_out = sender;
    *receiver_out = receiver;
    return 0;
}

CielAsyncSender *ciel_async_sender_clone(CielAsyncSender *sender) {
    if (sender == NULL || sender->channel == NULL || sender->closed) {
        errno = EPIPE;
        return NULL;
    }
    CielAsyncChannel *channel = sender->channel;
    pthread_mutex_lock(&channel->mutex);
    if (channel->live_receivers == 0) {
        pthread_mutex_unlock(&channel->mutex);
        errno = EPIPE;
        return NULL;
    }
    channel->live_senders++;
    pthread_mutex_unlock(&channel->mutex);
    CielAsyncSender *clone =
        (CielAsyncSender *)ciel_alloc(sizeof(CielAsyncSender));
    clone->channel = channel;
    clone->closed = 0;
    return clone;
}

CielAsyncReceiver *ciel_async_receiver_clone(CielAsyncReceiver *receiver) {
    if (receiver == NULL || receiver->channel == NULL || receiver->closed) {
        errno = EPIPE;
        return NULL;
    }
    CielAsyncChannel *channel = receiver->channel;
    pthread_mutex_lock(&channel->mutex);
    channel->live_receivers++;
    pthread_mutex_unlock(&channel->mutex);
    CielAsyncReceiver *clone =
        (CielAsyncReceiver *)ciel_alloc(sizeof(CielAsyncReceiver));
    clone->channel = channel;
    clone->closed = 0;
    return clone;
}

int32_t ciel_async_sender_close(CielAsyncSender *sender) {
    if (sender == NULL || sender->channel == NULL)
        return EINVAL;
    CielAsyncChannel *channel = sender->channel;
    CielTaskWaitNode *waiters = NULL;
    pthread_mutex_lock(&channel->mutex);
    if (!sender->closed) {
        sender->closed = 1;
        if (channel->live_senders > 0)
            channel->live_senders--;
        if (channel->live_senders == 0)
            waiters = ciel_async_channel_take_all_recv_waiters_locked(channel);
    }
    pthread_mutex_unlock(&channel->mutex);
    ciel_task_schedule_waiters(waiters);
    return 0;
}

int32_t ciel_async_receiver_close(CielAsyncReceiver *receiver) {
    if (receiver == NULL || receiver->channel == NULL)
        return EINVAL;
    CielAsyncChannel *channel = receiver->channel;
    CielTaskWaitNode *waiters = NULL;
    pthread_mutex_lock(&channel->mutex);
    if (!receiver->closed) {
        receiver->closed = 1;
        if (channel->live_receivers > 0)
            channel->live_receivers--;
        if (channel->live_receivers == 0)
            waiters = ciel_async_channel_take_all_send_waiters_locked(channel);
    }
    pthread_mutex_unlock(&channel->mutex);
    ciel_task_schedule_waiters(waiters);
    return 0;
}

int32_t ciel_async_channel_try_send(CielAsyncSender *sender,
                                    const void *value) {
    if (sender == NULL || sender->channel == NULL)
        return EINVAL;
    CielAsyncChannel *channel = sender->channel;
    pthread_mutex_lock(&channel->mutex);
    if (sender->closed || channel->live_receivers == 0) {
        pthread_mutex_unlock(&channel->mutex);
        return EPIPE;
    }
    if (channel->len + channel->reserved >= channel->capacity) {
        pthread_mutex_unlock(&channel->mutex);
        return EAGAIN;
    }
    int32_t rc = ciel_async_channel_enqueue_locked(channel, value);
    CielTaskWaitNode *waiters =
        rc == 0 ? ciel_async_channel_take_recv_waiter_locked(channel) : NULL;
    pthread_mutex_unlock(&channel->mutex);
    ciel_task_schedule_waiters(waiters);
    return rc;
}

int32_t ciel_async_send_permit_send(CielAsyncSendPermit *permit,
                                    const void *value) {
    if (permit == NULL || permit->channel == NULL)
        return EINVAL;
    CielAsyncChannel *channel = permit->channel;
    pthread_mutex_lock(&channel->mutex);
    if (permit->used) {
        pthread_mutex_unlock(&channel->mutex);
        return EALREADY;
    }
    permit->used = 1;
    if (channel->reserved > 0)
        channel->reserved--;
    if (channel->live_receivers == 0) {
        CielTaskWaitNode *waiters =
            ciel_async_channel_take_all_send_waiters_locked(channel);
        pthread_mutex_unlock(&channel->mutex);
        ciel_task_schedule_waiters(waiters);
        return EPIPE;
    }
    int32_t rc = ciel_async_channel_enqueue_locked(channel, value);
    CielTaskWaitNode *waiters =
        rc == 0 ? ciel_async_channel_take_recv_waiter_locked(channel) : NULL;
    pthread_mutex_unlock(&channel->mutex);
    ciel_task_schedule_waiters(waiters);
    return rc;
}

int32_t ciel_async_send_permit_release(CielAsyncSendPermit *permit) {
    if (permit == NULL || permit->channel == NULL)
        return EINVAL;
    CielAsyncChannel *channel = permit->channel;
    pthread_mutex_lock(&channel->mutex);
    if (!permit->used) {
        permit->used = 1;
        if (channel->reserved > 0)
            channel->reserved--;
        CielTaskWaitNode *waiters =
            ciel_async_channel_take_send_waiter_locked(channel);
        pthread_mutex_unlock(&channel->mutex);
        ciel_task_schedule_waiters(waiters);
        return 0;
    }
    pthread_mutex_unlock(&channel->mutex);
    return 0;
}

struct CielAsyncFd {
    int fd;
    dispatch_io_t channel;
    pthread_mutex_t mutex;
    int closed;
};

int32_t ciel_async_close(CielAsyncFd *fd);

typedef struct CielAcceptedStreamNode {
    CielAsyncFd *stream;
    struct CielAcceptedStreamNode *next;
} CielAcceptedStreamNode;

struct CielAsyncTcpListener {
    int fd;
    pthread_mutex_t mutex;
    int closed;
    CielAsyncOp *pending_accept;
    CielAcceptedStreamNode *accepted_head;
    CielAcceptedStreamNode *accepted_tail;
};

typedef enum {
    CIEL_ASYNC_READ,
    CIEL_ASYNC_WRITE,
    CIEL_ASYNC_ACCEPT,
    CIEL_ASYNC_CONNECT,
    CIEL_ASYNC_SLEEP,
} CielAsyncKind;

typedef int32_t (*CielFutureRunFn)(void *ctx, void *out);
typedef void (*CielFutureCleanupFn)(void *ctx, int32_t reason);
struct CielAsyncOp {
    CielAsyncKind kind;
    CielAsyncFd *fd;
    CielAsyncTcpListener *listener;
    dispatch_source_t source;
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    int complete;
    int canceled;
    int finished;
    int notify_set;
    int notify_sent;
    int error;
    int raw_fd;
    size_t written;
    CielBytes *bytes;
    CielBytes *write_bytes;
    CielAsyncFd *result_fd;
    CielBufferedReader *buffered_reader;
    CielActor *notify_actor;
    void *notify_message;
    CielTaskWaitNode *waiters;
    CielRoot *self_root;
    uint64_t route_task_id;
    uint64_t route_operation_id;
    uint64_t route_generation;
};

typedef enum {
    CIEL_PENDING_CHANNEL_NONE = 0,
    CIEL_PENDING_CHANNEL_SEND = 1,
    CIEL_PENDING_CHANNEL_RECV = 2,
} CielPendingChannelMode;

struct CielFuture {
    CielFutureRunFn run;
    CielFutureCleanupFn cleanup;
    CielResourceOwner *owner;
    void *ctx;
    void *result;
    size_t result_size;
    size_t result_align;
    pthread_mutex_t mutex;
    uint8_t state;
    uint8_t cleanup_started;
    int32_t failure;
    uint64_t task_id;
    uint64_t next_operation_id;
    uint64_t generation;
    CielAsyncOp *pending_op;
    CielTask *pending_task;
    CielSelectSet *pending_select;
    CielAsyncChannel *pending_channel;
    CielPendingChannelMode pending_channel_mode;
    CielTaskGroup *pending_group;
};

struct CielTask {
    CielFuture *future;
    CielFuture *wait_future;
    CielResourceOwner *owner;
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    CielTaskWaitNode *waiters;
    CielRoot *self_root;
    uint8_t scheduled;
    uint8_t finished;
    int32_t rc;
};

typedef struct CielTaskWait {
    CielTask *task;
    CielFuture *future;
} CielTaskWait;

typedef struct CielTaskGroupDoneNode {
    CielTask *task;
    struct CielTaskGroupDoneNode *next;
} CielTaskGroupDoneNode;

typedef struct CielTaskGroupTaskNode {
    CielTask *task;
    uint8_t completed;
    struct CielTaskGroupTaskNode *next;
} CielTaskGroupTaskNode;

struct CielTaskGroup {
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    CielTaskGroupTaskNode *tasks;
    CielTaskGroupDoneNode *done_head;
    CielTaskGroupDoneNode *done_tail;
    CielTaskWaitNode *waiters;
    size_t live_tasks;
    uint8_t closed;
    uint8_t cancel_all;
};

typedef struct CielTaskGroupWatch {
    CielTaskGroup *group;
    CielTaskGroupTaskNode *node;
} CielTaskGroupWatch;

typedef struct CielSelectArm {
    CielFuture *future;
    void *result;
    size_t result_size;
    size_t result_align;
    int32_t rc;
    uint8_t completed;
} CielSelectArm;

struct CielSelectSet {
    CielFuture *future;
    CielSelectArm *arms;
    size_t len;
    size_t cap;
    int biased;
    int started;
    ssize_t winner;
    int32_t winner_rc;
    CielTaskWaitNode *waiters;
    pthread_mutex_t mutex;
    pthread_cond_t cond;
};

typedef struct CielSelectWaiter {
    CielSelectSet *set;
    size_t index;
} CielSelectWaiter;

static pthread_mutex_t ciel_select_fairness_mutex = PTHREAD_MUTEX_INITIALIZER;
static size_t ciel_select_next_start = 0;

static void ciel_async_channel_broadcast(CielAsyncChannel *channel);
static void ciel_task_group_broadcast(CielTaskGroup *group);
static void ciel_task_run(void *ctx_raw);
static dispatch_queue_t ciel_task_queue(void);

static CielTaskWaitNode *ciel_task_wait_node_new(CielTask *task) {
    CielTaskWaitNode *node =
        (CielTaskWaitNode *)malloc(sizeof(CielTaskWaitNode));
    if (node == NULL)
        return NULL;
    node->task = task;
    node->next = NULL;
    return node;
}

static int32_t ciel_task_waiter_push(CielTaskWaitNode **head, CielTask *task) {
    if (head == NULL || task == NULL)
        return EINVAL;
    CielTaskWaitNode *node = ciel_task_wait_node_new(task);
    if (node == NULL)
        return ENOMEM;
    node->next = *head;
    *head = node;
    return 0;
}

static void ciel_task_schedule(CielTask *task) {
    if (task == NULL)
        return;
    int should_schedule = 0;
    pthread_mutex_lock(&task->mutex);
    if (!task->finished && !task->scheduled) {
        task->scheduled = 1;
        should_schedule = 1;
    }
    pthread_mutex_unlock(&task->mutex);
    if (should_schedule)
        dispatch_async_f(ciel_task_queue(), task, ciel_task_run);
}

static void ciel_task_schedule_waiters(CielTaskWaitNode *waiters) {
    while (waiters != NULL) {
        CielTaskWaitNode *next = waiters->next;
        ciel_task_schedule(waiters->task);
        free(waiters);
        waiters = next;
    }
}

enum {
    CIEL_FUTURE_PENDING = 0,
    CIEL_FUTURE_RUNNING = 1,
    CIEL_FUTURE_COMPLETE = 2,
    CIEL_FUTURE_FAILED = 3,
};

static dispatch_queue_t ciel_async_io_queue;
static dispatch_queue_t ciel_async_net_global_queue;
static dispatch_queue_t ciel_select_waiter_global_queue;
static dispatch_queue_t ciel_task_global_queue;

static void ciel_async_queue_init(void) {
    ciel_async_io_queue =
        dispatch_queue_create("ciel.async-io", DISPATCH_QUEUE_SERIAL);
    ciel_async_net_global_queue =
        dispatch_queue_create("ciel.async-net", DISPATCH_QUEUE_CONCURRENT);
    ciel_select_waiter_global_queue =
        dispatch_queue_create("ciel.select-waiter", DISPATCH_QUEUE_CONCURRENT);
    ciel_task_global_queue =
        dispatch_queue_create("ciel.task", DISPATCH_QUEUE_CONCURRENT);
}

static dispatch_queue_t ciel_async_queue(void) {
    static pthread_once_t once = PTHREAD_ONCE_INIT;
    pthread_once(&once, ciel_async_queue_init);
    return ciel_async_io_queue;
}

static dispatch_queue_t ciel_async_net_queue(void) {
    static pthread_once_t once = PTHREAD_ONCE_INIT;
    pthread_once(&once, ciel_async_queue_init);
    return ciel_async_net_global_queue;
}

static dispatch_queue_t ciel_select_waiter_queue(void) {
    static pthread_once_t once = PTHREAD_ONCE_INIT;
    pthread_once(&once, ciel_async_queue_init);
    return ciel_select_waiter_global_queue;
}

static dispatch_queue_t ciel_task_queue(void) {
    static pthread_once_t once = PTHREAD_ONCE_INIT;
    pthread_once(&once, ciel_async_queue_init);
    return ciel_task_global_queue;
}

static CielAsyncFd *ciel_async_fd_new(int fd) {
    CielAsyncFd *async_fd = (CielAsyncFd *)ciel_alloc(sizeof(CielAsyncFd));
    async_fd->fd = fd;
    async_fd->closed = 0;
    async_fd->channel = NULL;
    int rc = pthread_mutex_init(&async_fd->mutex, NULL);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    async_fd->channel = dispatch_io_create(DISPATCH_IO_STREAM, fd,
                                           ciel_async_queue(), ^(int error) {
                                             (void)error;
                                           });
    if (async_fd->channel == NULL) {
        pthread_mutex_destroy(&async_fd->mutex);
        errno = ENOMEM;
        return NULL;
    }
    return async_fd;
}

static void ciel_tcp_configure_stream_fd(int fd) {
    int one = 1;
#if defined(SO_NOSIGPIPE)
    (void)setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof(one));
#endif
#if defined(TCP_NODELAY)
    (void)setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
#endif
}

static CielAsyncFd *ciel_async_tcp_stream_new(int fd) {
    ciel_tcp_configure_stream_fd(fd);
    CielAsyncFd *async_fd = (CielAsyncFd *)ciel_alloc(sizeof(CielAsyncFd));
    async_fd->fd = fd;
    async_fd->closed = 0;
    async_fd->channel = NULL;
    int rc = pthread_mutex_init(&async_fd->mutex, NULL);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    return async_fd;
}

static CielAsyncFd *
ciel_async_listener_pop_accepted_locked(CielAsyncTcpListener *listener) {
    CielAcceptedStreamNode *node = listener->accepted_head;
    if (node == NULL)
        return NULL;
    listener->accepted_head = node->next;
    if (listener->accepted_head == NULL)
        listener->accepted_tail = NULL;
    CielAsyncFd *stream = node->stream;
    GC_FREE(node);
    return stream;
}

static void ciel_async_listener_enqueue_accepted(CielAsyncTcpListener *listener,
                                                 CielAsyncFd *stream) {
    if (stream == NULL)
        return;
    CielAcceptedStreamNode *node =
        (CielAcceptedStreamNode *)ciel_alloc_uncollectable(
            sizeof(CielAcceptedStreamNode));
    node->stream = stream;
    node->next = NULL;
    pthread_mutex_lock(&listener->mutex);
    if (listener->closed) {
        pthread_mutex_unlock(&listener->mutex);
        (void)ciel_async_close(stream);
        GC_FREE(node);
        return;
    }
    if (listener->accepted_tail != NULL)
        listener->accepted_tail->next = node;
    else
        listener->accepted_head = node;
    listener->accepted_tail = node;
    pthread_mutex_unlock(&listener->mutex);
}

static void ciel_async_close_accepted_queue(CielAcceptedStreamNode *node) {
    while (node != NULL) {
        CielAcceptedStreamNode *next = node->next;
        if (node->stream != NULL)
            (void)ciel_async_close(node->stream);
        GC_FREE(node);
        node = next;
    }
}

CielAsyncFd *ciel_async_open(int32_t mode, const char *path) {
    if (path == NULL) {
        errno = EINVAL;
        return NULL;
    }
    int flags = ciel_file_open_mode_flags(mode);
    if (flags < 0)
        return NULL;
    int fd = open(path, flags, 0666);
    if (fd < 0)
        return NULL;
    CielAsyncFd *async_fd = ciel_async_fd_new(fd);
    if (async_fd == NULL) {
        close(fd);
        return NULL;
    }
    return async_fd;
}

CielAsyncFd *ciel_async_from_raw_fd(int32_t raw) {
    if (raw < 0) {
        errno = EBADF;
        return NULL;
    }
    return ciel_async_fd_new(raw);
}

int32_t ciel_async_close(CielAsyncFd *fd) {
    if (fd == NULL)
        return EINVAL;
    pthread_mutex_lock(&fd->mutex);
    if (fd->closed) {
        pthread_mutex_unlock(&fd->mutex);
        return 0;
    }
    fd->closed = 1;
    int raw = fd->fd;
    fd->fd = -1;
    dispatch_io_t channel = fd->channel;
    fd->channel = NULL;
    pthread_mutex_unlock(&fd->mutex);
    if (channel != NULL) {
        dispatch_io_close(channel, DISPATCH_IO_STOP);
        return 0;
    }
    if (raw >= 0 && close(raw) != 0)
        return errno == 0 ? EIO : errno;
    return 0;
}

static int32_t ciel_async_fd_snapshot(CielAsyncFd *fd, dispatch_io_t *channel) {
    if (fd == NULL || channel == NULL)
        return EINVAL;
    pthread_mutex_lock(&fd->mutex);
    if (fd->closed) {
        pthread_mutex_unlock(&fd->mutex);
        return EBADF;
    }
    if (fd->channel == NULL) {
        pthread_mutex_unlock(&fd->mutex);
        return ENOTSUP;
    }
    *channel = fd->channel;
    pthread_mutex_unlock(&fd->mutex);
    return 0;
}

static int32_t ciel_async_fd_raw_snapshot(CielAsyncFd *fd, int *out_fd) {
    if (fd == NULL || out_fd == NULL)
        return EINVAL;
    pthread_mutex_lock(&fd->mutex);
    if (fd->closed) {
        pthread_mutex_unlock(&fd->mutex);
        return EBADF;
    }
    if (fd->fd < 0) {
        pthread_mutex_unlock(&fd->mutex);
        return EBADF;
    }
    *out_fd = fd->fd;
    pthread_mutex_unlock(&fd->mutex);
    return 0;
}

static CielAsyncOp *ciel_async_op_new(CielAsyncKind kind, CielAsyncFd *fd) {
    CielAsyncOp *op = (CielAsyncOp *)ciel_alloc(sizeof(CielAsyncOp));
    memset(op, 0, sizeof(CielAsyncOp));
    op->kind = kind;
    op->fd = fd;
    op->raw_fd = -1;
    op->self_root = ciel_root_pin(op);
    int rc = pthread_mutex_init(&op->mutex, NULL);
    if (rc != 0) {
        ciel_root_unpin(op->self_root);
        op->self_root = NULL;
        errno = rc;
        return NULL;
    }
    rc = pthread_cond_init(&op->cond, NULL);
    if (rc != 0) {
        ciel_root_unpin(op->self_root);
        op->self_root = NULL;
        errno = rc;
        return NULL;
    }
    return op;
}

static void ciel_async_op_unpin(CielAsyncOp *op) {
    CielRoot *root = op->self_root;
    op->self_root = NULL;
    ciel_root_unpin(root);
}

static void ciel_async_op_clear_route_locked(CielAsyncOp *op) {
    op->route_task_id = 0;
    op->route_operation_id = 0;
    op->route_generation = 0;
}

static void ciel_async_send_notification_locked(CielAsyncOp *op) {
    if (!op->complete || op->canceled || !op->notify_set || op->notify_sent ||
        op->notify_actor == NULL || op->notify_message == NULL)
        return;
    CielActor *actor = op->notify_actor;
    void *message = op->notify_message;
    op->notify_sent = 1;
    op->notify_message = NULL;
    pthread_mutex_unlock(&op->mutex);
    int32_t rc = ciel_actor_send(actor, message);
    pthread_mutex_lock(&op->mutex);
    if (rc != 0)
        op->error = rc;
}

static void ciel_async_complete(CielAsyncOp *op, int error, CielBytes *bytes,
                                size_t written) {
    pthread_mutex_lock(&op->mutex);
    if (!op->canceled) {
        op->error = error;
        op->bytes = bytes;
        op->written = written;
    }
    CielTaskWaitNode *waiters = op->waiters;
    op->waiters = NULL;
    op->complete = 1;
    pthread_cond_broadcast(&op->cond);
    ciel_async_send_notification_locked(op);
    pthread_mutex_unlock(&op->mutex);
    ciel_task_schedule_waiters(waiters);
}

static void ciel_async_cancel_source(CielAsyncOp *op) {
    dispatch_source_t source = NULL;
    pthread_mutex_lock(&op->mutex);
    source = op->source;
    op->source = NULL;
    pthread_mutex_unlock(&op->mutex);
    if (source != NULL)
        dispatch_source_cancel(source);
}

static void ciel_async_listener_clear_pending(CielAsyncOp *op) {
    CielAsyncTcpListener *listener = op->listener;
    if (listener == NULL)
        return;
    pthread_mutex_lock(&listener->mutex);
    if (listener->pending_accept == op)
        listener->pending_accept = NULL;
    pthread_mutex_unlock(&listener->mutex);
}

static int ciel_async_take_raw_fd(CielAsyncOp *op) {
    pthread_mutex_lock(&op->mutex);
    int fd = op->raw_fd;
    op->raw_fd = -1;
    pthread_mutex_unlock(&op->mutex);
    return fd;
}

static void ciel_async_complete_stream(CielAsyncOp *op, int error,
                                       CielAsyncFd *stream) {
    if (op->kind == CIEL_ASYNC_ACCEPT)
        ciel_async_listener_clear_pending(op);
    pthread_mutex_lock(&op->mutex);
    int canceled = op->canceled;
    CielTaskWaitNode *waiters = op->waiters;
    op->waiters = NULL;
    if (!canceled) {
        op->error = error;
        op->result_fd = stream;
    }
    op->complete = 1;
    pthread_cond_broadcast(&op->cond);
    ciel_async_send_notification_locked(op);
    pthread_mutex_unlock(&op->mutex);
    ciel_task_schedule_waiters(waiters);
    if (canceled && stream != NULL)
        (void)ciel_async_close(stream);
}

CielAsyncOp *ciel_async_read_bytes(CielAsyncFd *fd, size_t max_len) {
    dispatch_io_t channel = NULL;
    int32_t rc = ciel_async_fd_snapshot(fd, &channel);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    CielAsyncOp *op = ciel_async_op_new(CIEL_ASYNC_READ, fd);
    if (op == NULL)
        return NULL;
    op->written = max_len;
    CielBytes *bytes = ciel_bytes_new(max_len);
    bytes->len = 0;
    op->bytes = bytes;
    dispatch_io_read(
        channel, 0, max_len, ciel_async_queue(),
        ^(bool done, dispatch_data_t data, int error) {
          int32_t attach_rc = ciel_runtime_enter_callback();
          if (attach_rc != 0) {
              if (done)
                  ciel_async_complete(op, attach_rc, bytes, 0);
              return;
          }
          size_t data_size = data == NULL ? 0 : dispatch_data_get_size(data);
          if (data_size > 0) {
              dispatch_data_apply(data, ^bool(dispatch_data_t region,
                                              size_t offset, const void *buffer,
                                              size_t size) {
                (void)region;
                (void)offset;
                size_t remaining = max_len - bytes->len;
                size_t copy = size < remaining ? size : remaining;
                if (copy > 0)
                    memcpy(bytes->data + bytes->len, buffer, copy);
                bytes->len += copy;
                return bytes->len < max_len;
              });
          }
          if (done)
              ciel_async_complete(op, error, bytes, 0);
          ciel_runtime_leave_callback();
        });
    return op;
}

CielAsyncOp *ciel_async_write_bytes(CielAsyncFd *fd, CielBytes *bytes) {
    if (bytes == NULL) {
        errno = EINVAL;
        return NULL;
    }
    dispatch_io_t channel = NULL;
    int32_t rc = ciel_async_fd_snapshot(fd, &channel);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    CielAsyncOp *op = ciel_async_op_new(CIEL_ASYNC_WRITE, fd);
    if (op == NULL)
        return NULL;
    op->write_bytes = bytes;
    dispatch_data_t data =
        dispatch_data_create(bytes->data, bytes->len, ciel_async_queue(), NULL);
    if (data == NULL) {
        errno = ENOMEM;
        return NULL;
    }
    dispatch_io_write(channel, 0, data, ciel_async_queue(),
                      ^(bool done, dispatch_data_t remaining_data, int error) {
                        int32_t attach_rc = ciel_runtime_enter_callback();
                        if (attach_rc != 0) {
                            if (done)
                                ciel_async_complete(op, attach_rc, NULL, 0);
                            return;
                        }
                        if (done) {
                            size_t remaining =
                                remaining_data == NULL
                                    ? 0
                                    : dispatch_data_get_size(remaining_data);
                            size_t written = bytes->len >= remaining
                                                 ? bytes->len - remaining
                                                 : 0;
                            ciel_async_complete(op, error, NULL, written);
                        }
                        ciel_runtime_leave_callback();
                      });
    return op;
}

CielAsyncOp *ciel_async_tcp_read_bytes(CielAsyncFd *fd, size_t max_len) {
    int raw = -1;
    int32_t rc = ciel_async_fd_raw_snapshot(fd, &raw);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    CielAsyncOp *op = ciel_async_op_new(CIEL_ASYNC_READ, fd);
    if (op == NULL)
        return NULL;
    CielBytes *bytes = ciel_bytes_new(max_len);
    bytes->len = 0;
    op->bytes = bytes;
    if (max_len == 0) {
        ciel_async_complete(op, 0, bytes, 0);
        return op;
    }
    ssize_t immediate = 0;
    do {
        immediate = read(raw, bytes->data, max_len);
    } while (immediate < 0 && errno == EINTR);
    if (immediate >= 0) {
        bytes->len = (size_t)immediate;
        ciel_async_complete(op, 0, bytes, 0);
        return op;
    }
    int immediate_err = errno == 0 ? EIO : errno;
    if (!(immediate_err == EAGAIN
#if defined(EWOULDBLOCK) && EWOULDBLOCK != EAGAIN
          || immediate_err == EWOULDBLOCK
#endif
          )) {
        ciel_async_complete(op, immediate_err, bytes, 0);
        return op;
    }
    dispatch_source_t source = dispatch_source_create(
        DISPATCH_SOURCE_TYPE_READ, (uintptr_t)raw, 0, ciel_async_net_queue());
    if (source == NULL) {
        errno = ENOMEM;
        return NULL;
    }
    op->source = source;
    dispatch_source_set_event_handler(source, ^{
      int32_t attach_rc = ciel_runtime_enter_callback();
      if (attach_rc != 0) {
          ciel_async_cancel_source(op);
          ciel_async_complete(op, attach_rc, bytes, 0);
          return;
      }
      ssize_t n = 0;
      do {
          n = read(raw, bytes->data, max_len);
      } while (n < 0 && errno == EINTR);
      if (n < 0) {
          int err = errno == 0 ? EIO : errno;
          if (err == EAGAIN
#if defined(EWOULDBLOCK) && EWOULDBLOCK != EAGAIN
              || err == EWOULDBLOCK
#endif
          ) {
              ciel_runtime_leave_callback();
              return;
          }
          ciel_async_cancel_source(op);
          ciel_async_complete(op, err, bytes, 0);
          ciel_runtime_leave_callback();
          return;
      }
      bytes->len = (size_t)n;
      ciel_async_cancel_source(op);
      ciel_async_complete(op, 0, bytes, 0);
      ciel_runtime_leave_callback();
    });
    dispatch_resume(source);
    return op;
}

CielAsyncOp *ciel_async_tcp_read_into(CielAsyncFd *fd, CielBytes *bytes) {
    if (bytes == NULL) {
        errno = EINVAL;
        return NULL;
    }
    int raw = -1;
    int32_t rc = ciel_async_fd_raw_snapshot(fd, &raw);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    CielAsyncOp *op = ciel_async_op_new(CIEL_ASYNC_READ, fd);
    if (op == NULL)
        return NULL;
    size_t cap = bytes->cap;
    bytes->len = 0;
    op->bytes = bytes;
    if (cap == 0) {
        ciel_async_complete(op, 0, bytes, 0);
        return op;
    }
    ssize_t immediate = 0;
    do {
        immediate = read(raw, bytes->data, cap);
    } while (immediate < 0 && errno == EINTR);
    if (immediate >= 0) {
        bytes->len = (size_t)immediate;
        ciel_async_complete(op, 0, bytes, 0);
        return op;
    }
    int immediate_err = errno == 0 ? EIO : errno;
    if (!(immediate_err == EAGAIN
#if defined(EWOULDBLOCK) && EWOULDBLOCK != EAGAIN
          || immediate_err == EWOULDBLOCK
#endif
          )) {
        ciel_async_complete(op, immediate_err, bytes, 0);
        return op;
    }
    dispatch_source_t source = dispatch_source_create(
        DISPATCH_SOURCE_TYPE_READ, (uintptr_t)raw, 0, ciel_async_net_queue());
    if (source == NULL) {
        errno = ENOMEM;
        return NULL;
    }
    op->source = source;
    dispatch_source_set_event_handler(source, ^{
      int32_t attach_rc = ciel_runtime_enter_callback();
      if (attach_rc != 0) {
          ciel_async_cancel_source(op);
          ciel_async_complete(op, attach_rc, bytes, 0);
          return;
      }
      ssize_t n = 0;
      do {
          n = read(raw, bytes->data, cap);
      } while (n < 0 && errno == EINTR);
      if (n < 0) {
          int err = errno == 0 ? EIO : errno;
          if (err == EAGAIN
#if defined(EWOULDBLOCK) && EWOULDBLOCK != EAGAIN
              || err == EWOULDBLOCK
#endif
          ) {
              ciel_runtime_leave_callback();
              return;
          }
          ciel_async_cancel_source(op);
          ciel_async_complete(op, err, bytes, 0);
          ciel_runtime_leave_callback();
          return;
      }
      bytes->len = (size_t)n;
      ciel_async_cancel_source(op);
      ciel_async_complete(op, 0, bytes, 0);
      ciel_runtime_leave_callback();
    });
    dispatch_resume(source);
    return op;
}

static size_t ciel_buffered_reader_unread_locked(CielBufferedReader *reader) {
    if (reader == NULL || reader->buffer == NULL ||
        reader->offset >= reader->buffer->len)
        return 0;
    return reader->buffer->len - reader->offset;
}

static int32_t ciel_buffered_reader_reserve_locked(CielBufferedReader *reader,
                                                   size_t needed) {
    if (reader == NULL || reader->buffer == NULL)
        return EINVAL;
    if (needed <= reader->buffer->cap)
        return 0;
    uint8_t *next = ciel_bytes_data_alloc(needed);
    if (reader->buffer->len > 0)
        memcpy(next, reader->buffer->data, reader->buffer->len);
    free(reader->buffer->data);
    reader->buffer->data = next;
    reader->buffer->cap = needed;
    return 0;
}

static void ciel_buffered_reader_compact_locked(CielBufferedReader *reader) {
    size_t unread = ciel_buffered_reader_unread_locked(reader);
    if (unread == 0) {
        reader->buffer->len = 0;
        reader->offset = 0;
        return;
    }
    if (reader->offset > 0)
        memmove(reader->buffer->data, reader->buffer->data + reader->offset,
                unread);
    reader->buffer->len = unread;
    reader->offset = 0;
}

static int32_t ciel_buffered_reader_append_locked(CielBufferedReader *reader,
                                                  const uint8_t *data,
                                                  size_t len) {
    if (reader == NULL || (data == NULL && len > 0))
        return EINVAL;
    if (len == 0)
        return 0;
    ciel_buffered_reader_compact_locked(reader);
    if (len > SIZE_MAX - reader->buffer->len)
        return EOVERFLOW;
    size_t needed = reader->buffer->len + len;
    int32_t rc = ciel_buffered_reader_reserve_locked(reader, needed);
    if (rc != 0)
        return rc;
    memcpy(reader->buffer->data + reader->buffer->len, data, len);
    reader->buffer->len = needed;
    return 0;
}

static int32_t ciel_buffered_reader_prepend_locked(CielBufferedReader *reader,
                                                   CielBytes *bytes) {
    if (reader == NULL || bytes == NULL)
        return EINVAL;
    if (bytes->len == 0)
        return 0;
    size_t unread = ciel_buffered_reader_unread_locked(reader);
    if (bytes->len > SIZE_MAX - unread)
        return EOVERFLOW;
    size_t needed = bytes->len + unread;
    int32_t rc = ciel_buffered_reader_reserve_locked(reader, needed);
    if (rc != 0)
        return rc;
    if (unread > 0)
        memmove(reader->buffer->data + bytes->len,
                reader->buffer->data + reader->offset, unread);
    memcpy(reader->buffer->data, bytes->data, bytes->len);
    reader->buffer->len = needed;
    reader->offset = 0;
    return 0;
}

static CielBytes *ciel_buffered_reader_take_locked(CielBufferedReader *reader,
                                                   size_t max_len,
                                                   int32_t *out_rc) {
    if (out_rc != NULL)
        *out_rc = 0;
    size_t unread = ciel_buffered_reader_unread_locked(reader);
    size_t n = unread < max_len ? unread : max_len;
    CielBytes *out = ciel_bytes_new(n);
    if (n > 0)
        memcpy(out->data, reader->buffer->data + reader->offset, n);
    reader->offset += n;
    if (reader->offset >= reader->buffer->len) {
        reader->offset = 0;
        reader->buffer->len = 0;
    }
    return out;
}

static int32_t ciel_buffered_reader_take_into_locked(CielBufferedReader *reader,
                                                     CielBytes *out,
                                                     size_t max_len) {
    if (reader == NULL || out == NULL || out->len > out->cap)
        return EINVAL;
    size_t unread = ciel_buffered_reader_unread_locked(reader);
    size_t out_remaining = out->cap - out->len;
    size_t n = unread < max_len ? unread : max_len;
    if (n > out_remaining)
        n = out_remaining;
    if (n > 0)
        memcpy(out->data + out->len, reader->buffer->data + reader->offset, n);
    out->len += n;
    reader->offset += n;
    if (reader->offset >= reader->buffer->len) {
        reader->offset = 0;
        reader->buffer->len = 0;
    }
    return 0;
}

CielBufferedReader *ciel_async_tcp_buffered_reader_new(CielAsyncFd *fd,
                                                       size_t capacity) {
    if (fd == NULL) {
        errno = EINVAL;
        return NULL;
    }
    CielBufferedReader *reader =
        (CielBufferedReader *)ciel_alloc(sizeof(CielBufferedReader));
    memset(reader, 0, sizeof(CielBufferedReader));
    reader->fd = fd;
    reader->capacity = capacity;
    reader->buffer = ciel_bytes_new(capacity);
    reader->buffer->len = 0;
    int rc = pthread_mutex_init(&reader->mutex, NULL);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    return reader;
}

CielAsyncFd *
ciel_async_tcp_buffered_reader_into_read_half(CielBufferedReader *reader) {
    if (reader == NULL) {
        errno = EINVAL;
        return NULL;
    }
    pthread_mutex_lock(&reader->mutex);
    if (reader->pending_read != NULL ||
        ciel_buffered_reader_unread_locked(reader) != 0) {
        pthread_mutex_unlock(&reader->mutex);
        errno = EALREADY;
        return NULL;
    }
    CielAsyncFd *fd = reader->fd;
    pthread_mutex_unlock(&reader->mutex);
    return fd;
}

static void ciel_async_complete_buffered_read(CielAsyncOp *op, int error,
                                              CielBytes *bytes, size_t n,
                                              size_t requested) {
    CielBufferedReader *reader = op == NULL ? NULL : op->buffered_reader;
    int32_t rc = 0;
    pthread_mutex_lock(&op->mutex);
    int canceled = op->canceled;
    CielTaskWaitNode *waiters = op->waiters;
    op->waiters = NULL;
    if (reader != NULL) {
        pthread_mutex_lock(&reader->mutex);
        if (reader->pending_read == op)
            reader->pending_read = NULL;
        if (error == 0 && bytes != NULL && n > 0) {
            if (canceled) {
                rc = ciel_buffered_reader_append_locked(reader, bytes->data, n);
            } else if (n > requested) {
                rc = ciel_buffered_reader_append_locked(
                    reader, bytes->data + requested, n - requested);
            }
        }
        pthread_mutex_unlock(&reader->mutex);
    }
    if (!canceled) {
        op->error = error == 0 && rc != 0 ? rc : error;
        if (bytes != NULL) {
            bytes->len = n < requested ? n : requested;
            op->bytes = bytes;
        }
    }
    op->complete = 1;
    pthread_cond_broadcast(&op->cond);
    ciel_async_send_notification_locked(op);
    pthread_mutex_unlock(&op->mutex);
    ciel_task_schedule_waiters(waiters);
}

static void ciel_async_complete_buffered_exact_read(CielAsyncOp *op,
                                                    int error) {
    CielBufferedReader *reader = op == NULL ? NULL : op->buffered_reader;
    pthread_mutex_lock(&op->mutex);
    int canceled = op->canceled;
    CielTaskWaitNode *waiters = op->waiters;
    op->waiters = NULL;
    if (reader != NULL) {
        pthread_mutex_lock(&reader->mutex);
        if (reader->pending_read == op)
            reader->pending_read = NULL;
        pthread_mutex_unlock(&reader->mutex);
    }
    if (!canceled)
        op->error = error;
    op->complete = 1;
    pthread_cond_broadcast(&op->cond);
    ciel_async_send_notification_locked(op);
    pthread_mutex_unlock(&op->mutex);
    ciel_task_schedule_waiters(waiters);
}

CielAsyncOp *ciel_async_tcp_read_buffered(CielBufferedReader *reader,
                                          size_t max_len) {
    if (reader == NULL) {
        errno = EINVAL;
        return NULL;
    }
    CielAsyncOp *op = ciel_async_op_new(CIEL_ASYNC_READ, reader->fd);
    if (op == NULL)
        return NULL;
    op->buffered_reader = reader;

    pthread_mutex_lock(&reader->mutex);
    if (reader->pending_read != NULL) {
        pthread_mutex_unlock(&reader->mutex);
        errno = EALREADY;
        return NULL;
    }
    if (max_len == 0 || ciel_buffered_reader_unread_locked(reader) > 0) {
        int32_t rc = 0;
        CielBytes *bytes =
            ciel_buffered_reader_take_locked(reader, max_len, &rc);
        pthread_mutex_unlock(&reader->mutex);
        if (rc != 0) {
            errno = rc;
            return NULL;
        }
        ciel_async_complete(op, 0, bytes, 0);
        return op;
    }
    reader->pending_read = op;
    size_t read_cap = reader->capacity > max_len ? reader->capacity : max_len;
    if (read_cap == 0)
        read_cap = 1;
    pthread_mutex_unlock(&reader->mutex);

    int raw = -1;
    int32_t rc = ciel_async_fd_raw_snapshot(reader->fd, &raw);
    if (rc != 0) {
        pthread_mutex_lock(&reader->mutex);
        if (reader->pending_read == op)
            reader->pending_read = NULL;
        pthread_mutex_unlock(&reader->mutex);
        errno = rc;
        return NULL;
    }

    CielBytes *bytes = ciel_bytes_new(read_cap);
    bytes->len = 0;
    op->bytes = bytes;
    ssize_t immediate = 0;
    do {
        immediate = read(raw, bytes->data, read_cap);
    } while (immediate < 0 && errno == EINTR);
    if (immediate >= 0) {
        ciel_async_complete_buffered_read(op, 0, bytes, (size_t)immediate,
                                          max_len);
        return op;
    }
    int immediate_err = errno == 0 ? EIO : errno;
    if (!(immediate_err == EAGAIN
#if defined(EWOULDBLOCK) && EWOULDBLOCK != EAGAIN
          || immediate_err == EWOULDBLOCK
#endif
          )) {
        ciel_async_complete_buffered_read(op, immediate_err, bytes, 0, max_len);
        return op;
    }
    dispatch_source_t source = dispatch_source_create(
        DISPATCH_SOURCE_TYPE_READ, (uintptr_t)raw, 0, ciel_async_net_queue());
    if (source == NULL) {
        pthread_mutex_lock(&reader->mutex);
        if (reader->pending_read == op)
            reader->pending_read = NULL;
        pthread_mutex_unlock(&reader->mutex);
        errno = ENOMEM;
        return NULL;
    }
    op->source = source;
    dispatch_source_set_event_handler(source, ^{
      int32_t attach_rc = ciel_runtime_enter_callback();
      if (attach_rc != 0) {
          ciel_async_cancel_source(op);
          ciel_async_complete_buffered_read(op, attach_rc, bytes, 0, max_len);
          return;
      }
      ssize_t n = 0;
      do {
          n = read(raw, bytes->data, read_cap);
      } while (n < 0 && errno == EINTR);
      if (n < 0) {
          int err = errno == 0 ? EIO : errno;
          if (err == EAGAIN
#if defined(EWOULDBLOCK) && EWOULDBLOCK != EAGAIN
              || err == EWOULDBLOCK
#endif
          ) {
              ciel_runtime_leave_callback();
              return;
          }
          ciel_async_cancel_source(op);
          ciel_async_complete_buffered_read(op, err, bytes, 0, max_len);
          ciel_runtime_leave_callback();
          return;
      }
      ciel_async_cancel_source(op);
      ciel_async_complete_buffered_read(op, 0, bytes, (size_t)n, max_len);
      ciel_runtime_leave_callback();
    });
    dispatch_resume(source);
    return op;
}

CielAsyncOp *ciel_async_tcp_read_exact_buffered(CielBufferedReader *reader,
                                                size_t len) {
    if (reader == NULL) {
        errno = EINVAL;
        return NULL;
    }
    CielAsyncOp *op = ciel_async_op_new(CIEL_ASYNC_READ, reader->fd);
    if (op == NULL)
        return NULL;
    op->buffered_reader = reader;
    CielBytes *bytes = ciel_bytes_new(len);
    bytes->len = 0;
    op->bytes = bytes;

    pthread_mutex_lock(&reader->mutex);
    if (reader->pending_read != NULL) {
        pthread_mutex_unlock(&reader->mutex);
        errno = EALREADY;
        return NULL;
    }
    int32_t rc = ciel_buffered_reader_take_into_locked(reader, bytes, len);
    if (rc != 0) {
        pthread_mutex_unlock(&reader->mutex);
        errno = rc;
        return NULL;
    }
    if (bytes->len == len) {
        pthread_mutex_unlock(&reader->mutex);
        ciel_async_complete(op, 0, bytes, 0);
        return op;
    }
    reader->pending_read = op;
    pthread_mutex_unlock(&reader->mutex);

    int raw = -1;
    rc = ciel_async_fd_raw_snapshot(reader->fd, &raw);
    if (rc != 0) {
        pthread_mutex_lock(&reader->mutex);
        if (reader->pending_read == op)
            reader->pending_read = NULL;
        (void)ciel_buffered_reader_prepend_locked(reader, bytes);
        pthread_mutex_unlock(&reader->mutex);
        errno = rc;
        return NULL;
    }

    int immediate_complete = 0;
    int immediate_error = 0;
    while (bytes->len < len) {
        ssize_t n = 0;
        do {
            n = read(raw, bytes->data + bytes->len, len - bytes->len);
        } while (n < 0 && errno == EINTR);
        if (n < 0) {
            int err = errno == 0 ? EIO : errno;
            if (err == EAGAIN
#if defined(EWOULDBLOCK) && EWOULDBLOCK != EAGAIN
                || err == EWOULDBLOCK
#endif
            ) {
                break;
            }
            immediate_error = err;
            immediate_complete = 1;
            break;
        }
        if (n == 0) {
            immediate_error = EPIPE;
            immediate_complete = 1;
            break;
        }
        bytes->len += (size_t)n;
    }
    if (bytes->len == len)
        immediate_complete = 1;
    if (immediate_complete) {
        ciel_async_complete_buffered_exact_read(op, immediate_error);
        return op;
    }

    dispatch_source_t source = dispatch_source_create(
        DISPATCH_SOURCE_TYPE_READ, (uintptr_t)raw, 0, ciel_async_net_queue());
    if (source == NULL) {
        pthread_mutex_lock(&reader->mutex);
        if (reader->pending_read == op)
            reader->pending_read = NULL;
        (void)ciel_buffered_reader_prepend_locked(reader, bytes);
        pthread_mutex_unlock(&reader->mutex);
        errno = ENOMEM;
        return NULL;
    }
    op->source = source;
    dispatch_source_set_event_handler(source, ^{
      int32_t attach_rc = ciel_runtime_enter_callback();
      if (attach_rc != 0) {
          ciel_async_cancel_source(op);
          ciel_async_complete_buffered_exact_read(op, attach_rc);
          return;
      }
      int complete = 0;
      int error = 0;
      pthread_mutex_lock(&op->mutex);
      if (!op->canceled) {
          while (bytes->len < len) {
              ssize_t n = 0;
              do {
                  n = read(raw, bytes->data + bytes->len, len - bytes->len);
              } while (n < 0 && errno == EINTR);
              if (n < 0) {
                  int err = errno == 0 ? EIO : errno;
                  if (err == EAGAIN
#if defined(EWOULDBLOCK) && EWOULDBLOCK != EAGAIN
                      || err == EWOULDBLOCK
#endif
                  ) {
                      break;
                  }
                  error = err;
                  complete = 1;
                  break;
              }
              if (n == 0) {
                  error = EPIPE;
                  complete = 1;
                  break;
              }
              bytes->len += (size_t)n;
          }
          if (bytes->len == len)
              complete = 1;
      }
      pthread_mutex_unlock(&op->mutex);
      if (complete) {
          ciel_async_cancel_source(op);
          ciel_async_complete_buffered_exact_read(op, error);
      }
      ciel_runtime_leave_callback();
    });
    dispatch_resume(source);
    return op;
}

CielAsyncOp *ciel_async_tcp_write_bytes(CielAsyncFd *fd, CielBytes *bytes) {
    if (bytes == NULL) {
        errno = EINVAL;
        return NULL;
    }
    int raw = -1;
    int32_t rc = ciel_async_fd_raw_snapshot(fd, &raw);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    CielAsyncOp *op = ciel_async_op_new(CIEL_ASYNC_WRITE, fd);
    if (op == NULL)
        return NULL;
    op->write_bytes = bytes;
    if (bytes->len == 0) {
        ciel_async_complete(op, 0, NULL, 0);
        return op;
    }
    size_t offset = 0;
    while (offset < bytes->len) {
        ssize_t n = write(raw, bytes->data + offset, bytes->len - offset);
        if (n < 0 && errno == EINTR)
            continue;
        if (n < 0) {
            int err = errno == 0 ? EIO : errno;
            if (err == EAGAIN
#if defined(EWOULDBLOCK) && EWOULDBLOCK != EAGAIN
                || err == EWOULDBLOCK
#endif
            ) {
                break;
            }
            ciel_async_complete(op, err, NULL, offset);
            return op;
        }
        if (n == 0)
            break;
        offset += (size_t)n;
    }
    if (offset >= bytes->len) {
        ciel_async_complete(op, 0, NULL, offset);
        return op;
    }
    dispatch_source_t source = dispatch_source_create(
        DISPATCH_SOURCE_TYPE_WRITE, (uintptr_t)raw, 0, ciel_async_net_queue());
    if (source == NULL) {
        errno = ENOMEM;
        return NULL;
    }
    op->source = source;
    __block size_t pending_offset = offset;
    dispatch_source_set_event_handler(source, ^{
      int32_t attach_rc = ciel_runtime_enter_callback();
      if (attach_rc != 0) {
          ciel_async_cancel_source(op);
          ciel_async_complete(op, attach_rc, NULL, pending_offset);
          return;
      }
      while (pending_offset < bytes->len) {
          ssize_t n = write(raw, bytes->data + pending_offset,
                            bytes->len - pending_offset);
          if (n < 0 && errno == EINTR)
              continue;
          if (n < 0) {
              int err = errno == 0 ? EIO : errno;
              if (err == EAGAIN
#if defined(EWOULDBLOCK) && EWOULDBLOCK != EAGAIN
                  || err == EWOULDBLOCK
#endif
              ) {
                  ciel_runtime_leave_callback();
                  return;
              }
              ciel_async_cancel_source(op);
              ciel_async_complete(op, err, NULL, pending_offset);
              ciel_runtime_leave_callback();
              return;
          }
          if (n == 0)
              break;
          pending_offset += (size_t)n;
      }
      if (pending_offset >= bytes->len) {
          ciel_async_cancel_source(op);
          ciel_async_complete(op, 0, NULL, pending_offset);
      }
      ciel_runtime_leave_callback();
    });
    dispatch_resume(source);
    return op;
}

CielAsyncTcpListener *ciel_async_tcp_listen(CielSocketAddr *addr) {
    if (addr == NULL) {
        errno = EINVAL;
        return NULL;
    }
    struct sockaddr *sa = (struct sockaddr *)&addr->storage;
    int fd = ciel_net_make_socket(sa);
    if (fd < 0)
        return NULL;
    int rc = ciel_fd_set_nonblocking(fd);
    if (rc != 0) {
        close(fd);
        errno = rc;
        return NULL;
    }
    int one = 1;
    (void)setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
    if (bind(fd, sa, addr->len) != 0) {
        int err = errno == 0 ? EIO : errno;
        close(fd);
        errno = err;
        return NULL;
    }
    if (listen(fd, CIEL_TCP_LISTEN_BACKLOG) != 0) {
        int err = errno == 0 ? EIO : errno;
        close(fd);
        errno = err;
        return NULL;
    }
    CielAsyncTcpListener *listener =
        (CielAsyncTcpListener *)ciel_alloc_uncollectable(
            sizeof(CielAsyncTcpListener));
    listener->fd = fd;
    listener->closed = 0;
    listener->pending_accept = NULL;
    listener->accepted_head = NULL;
    listener->accepted_tail = NULL;
    rc = pthread_mutex_init(&listener->mutex, NULL);
    if (rc != 0) {
        close(fd);
        errno = rc;
        return NULL;
    }
    return listener;
}

int32_t ciel_async_tcp_listener_addr(CielAsyncTcpListener *listener,
                                     CielSocketAddr **out) {
    if (listener == NULL || out == NULL)
        return EINVAL;
    pthread_mutex_lock(&listener->mutex);
    if (listener->closed) {
        pthread_mutex_unlock(&listener->mutex);
        return EBADF;
    }
    int fd = listener->fd;
    pthread_mutex_unlock(&listener->mutex);
    int32_t rc = 0;
    CielSocketAddr *addr = ciel_net_addr_from_fd(fd, 0, &rc);
    if (addr == NULL)
        return rc == 0 ? EIO : rc;
    *out = addr;
    return 0;
}

int32_t ciel_async_tcp_close_listener(CielAsyncTcpListener *listener) {
    if (listener == NULL)
        return EINVAL;
    CielAsyncOp *pending = NULL;
    CielAcceptedStreamNode *accepted = NULL;
    pthread_mutex_lock(&listener->mutex);
    if (listener->closed) {
        pthread_mutex_unlock(&listener->mutex);
        return 0;
    }
    listener->closed = 1;
    int fd = listener->fd;
    listener->fd = -1;
    pending = listener->pending_accept;
    listener->pending_accept = NULL;
    accepted = listener->accepted_head;
    listener->accepted_head = NULL;
    listener->accepted_tail = NULL;
    pthread_mutex_unlock(&listener->mutex);
    if (pending != NULL) {
        ciel_async_cancel_source(pending);
        ciel_async_complete_stream(pending, ECANCELED, NULL);
    }
    ciel_async_close_accepted_queue(accepted);
    if (close(fd) != 0)
        return errno == 0 ? EIO : errno;
    return 0;
}

static int32_t ciel_async_tcp_wrap_accepted(int accepted, CielAsyncFd **out) {
    if (out == NULL)
        return EINVAL;
    int32_t rc = ciel_fd_set_nonblocking(accepted);
    CielAsyncFd *stream = NULL;
    if (rc == 0) {
        stream = ciel_async_tcp_stream_new(accepted);
        if (stream == NULL)
            rc = errno == 0 ? ENOMEM : errno;
    }
    if (rc != 0) {
        close(accepted);
        return rc;
    }
    *out = stream;
    return 0;
}

CielAsyncOp *ciel_async_tcp_accept(CielAsyncTcpListener *listener) {
    if (listener == NULL) {
        errno = EINVAL;
        return NULL;
    }
    CielAsyncOp *op = ciel_async_op_new(CIEL_ASYNC_ACCEPT, NULL);
    if (op == NULL)
        return NULL;
    pthread_mutex_lock(&listener->mutex);
    if (listener->closed) {
        pthread_mutex_unlock(&listener->mutex);
        errno = EBADF;
        return NULL;
    }
    CielAsyncFd *queued = ciel_async_listener_pop_accepted_locked(listener);
    if (queued != NULL) {
        pthread_mutex_unlock(&listener->mutex);
        ciel_async_complete_stream(op, 0, queued);
        return op;
    }
    if (listener->pending_accept != NULL) {
        pthread_mutex_unlock(&listener->mutex);
        errno = EALREADY;
        return NULL;
    }
    int fd = listener->fd;
    dispatch_source_t source = dispatch_source_create(
        DISPATCH_SOURCE_TYPE_READ, (uintptr_t)fd, 0, ciel_async_net_queue());
    if (source == NULL) {
        pthread_mutex_unlock(&listener->mutex);
        errno = ENOMEM;
        return NULL;
    }
    op->listener = listener;
    op->source = source;
    listener->pending_accept = op;
    pthread_mutex_unlock(&listener->mutex);

    dispatch_source_set_event_handler(source, ^{
      int32_t attach_rc = ciel_runtime_enter_callback();
      if (attach_rc != 0) {
          ciel_async_cancel_source(op);
          ciel_async_complete_stream(op, attach_rc, NULL);
          return;
      }
      CielAsyncFd *first_stream = NULL;
      int first_error = 0;
      while (true) {
          int accepted;
          do {
              accepted = accept(fd, NULL, NULL);
          } while (accepted < 0 && errno == EINTR);
          if (accepted < 0) {
              int err = errno == 0 ? EIO : errno;
              if (err == EAGAIN
#if defined(EWOULDBLOCK) && EWOULDBLOCK != EAGAIN
                  || err == EWOULDBLOCK
#endif
              ) {
                  break;
              }
              first_error = err;
              break;
          }
          CielAsyncFd *stream = NULL;
          int32_t rc = ciel_async_tcp_wrap_accepted(accepted, &stream);
          if (rc != 0) {
              first_error = rc;
              break;
          }
          if (first_stream == NULL)
              first_stream = stream;
          else
              ciel_async_listener_enqueue_accepted(listener, stream);
      }
      if (first_stream != NULL) {
          ciel_async_cancel_source(op);
          ciel_async_complete_stream(op, 0, first_stream);
          ciel_runtime_leave_callback();
          return;
      }
      if (first_error != 0) {
          ciel_async_cancel_source(op);
          ciel_async_complete_stream(op, first_error, NULL);
      }
      ciel_runtime_leave_callback();
    });
    dispatch_resume(source);
    return op;
}

CielAsyncOp *ciel_async_tcp_connect(CielSocketAddr *addr) {
    if (addr == NULL) {
        errno = EINVAL;
        return NULL;
    }
    int fd = ciel_net_make_socket((struct sockaddr *)&addr->storage);
    if (fd < 0)
        return NULL;
    int32_t rc = ciel_fd_set_nonblocking(fd);
    if (rc != 0) {
        close(fd);
        errno = rc;
        return NULL;
    }
    CielAsyncOp *op = ciel_async_op_new(CIEL_ASYNC_CONNECT, NULL);
    if (op == NULL) {
        close(fd);
        return NULL;
    }
    int connect_rc = connect(fd, (struct sockaddr *)&addr->storage, addr->len);
    if (connect_rc == 0) {
        CielAsyncFd *stream = ciel_async_tcp_stream_new(fd);
        if (stream == NULL) {
            int err = errno == 0 ? ENOMEM : errno;
            close(fd);
            ciel_async_complete_stream(op, err, NULL);
        } else {
            ciel_async_complete_stream(op, 0, stream);
        }
        return op;
    }
    int err = errno == 0 ? EIO : errno;
    if (err != EINPROGRESS) {
        close(fd);
        ciel_async_complete_stream(op, err, NULL);
        return op;
    }

    dispatch_source_t source = dispatch_source_create(
        DISPATCH_SOURCE_TYPE_WRITE, (uintptr_t)fd, 0, ciel_async_net_queue());
    if (source == NULL) {
        close(fd);
        errno = ENOMEM;
        return NULL;
    }
    op->raw_fd = fd;
    op->source = source;
    dispatch_source_set_event_handler(source, ^{
      int32_t attach_rc = ciel_runtime_enter_callback();
      if (attach_rc != 0) {
          int raw = ciel_async_take_raw_fd(op);
          if (raw >= 0)
              close(raw);
          ciel_async_cancel_source(op);
          ciel_async_complete_stream(op, attach_rc, NULL);
          return;
      }
      int so_error = 0;
      socklen_t len = (socklen_t)sizeof(so_error);
      int raw = ciel_async_take_raw_fd(op);
      if (raw < 0) {
          ciel_runtime_leave_callback();
          return;
      }
      int32_t finish_rc = 0;
      if (getsockopt(raw, SOL_SOCKET, SO_ERROR, &so_error, &len) != 0)
          finish_rc = errno == 0 ? EIO : errno;
      else
          finish_rc = so_error;
      CielAsyncFd *stream = NULL;
      if (finish_rc == 0) {
          stream = ciel_async_tcp_stream_new(raw);
          if (stream == NULL)
              finish_rc = errno == 0 ? ENOMEM : errno;
      }
      if (finish_rc != 0)
          close(raw);
      ciel_async_cancel_source(op);
      ciel_async_complete_stream(op, finish_rc, stream);
      ciel_runtime_leave_callback();
    });
    dispatch_resume(source);
    return op;
}

CielAsyncOp *ciel_async_sleep_ms(uint64_t ms) {
    uint64_t max_ms = (uint64_t)INT64_MAX / 1000000ULL;
    if (ms > max_ms) {
        errno = EOVERFLOW;
        return NULL;
    }
    CielAsyncOp *op = ciel_async_op_new(CIEL_ASYNC_SLEEP, NULL);
    if (op == NULL)
        return NULL;
    if (ms == 0) {
        ciel_async_complete(op, 0, NULL, 0);
        return op;
    }
    dispatch_source_t source = dispatch_source_create(
        DISPATCH_SOURCE_TYPE_TIMER, 0, 0, ciel_async_queue());
    if (source == NULL) {
        errno = ENOMEM;
        return NULL;
    }
    op->source = source;
    int64_t delta_ns = (int64_t)(ms * 1000000ULL);
    dispatch_source_set_timer(source,
                              dispatch_time(DISPATCH_TIME_NOW, delta_ns),
                              DISPATCH_TIME_FOREVER, 1000000ULL);
    dispatch_source_set_event_handler(source, ^{
      int32_t attach_rc = ciel_runtime_enter_callback();
      if (attach_rc != 0) {
          ciel_async_cancel_source(op);
          ciel_async_complete(op, attach_rc, NULL, 0);
          return;
      }
      ciel_async_cancel_source(op);
      ciel_async_complete(op, 0, NULL, 0);
      ciel_runtime_leave_callback();
    });
    dispatch_resume(source);
    return op;
}

static int32_t ciel_async_finish_stream(CielAsyncOp *op, CielAsyncKind kind,
                                        CielAsyncFd **out) {
    if (op == NULL || out == NULL)
        return EINVAL;
    pthread_mutex_lock(&op->mutex);
    if (op->kind != kind) {
        pthread_mutex_unlock(&op->mutex);
        return EINVAL;
    }
    if (op->finished) {
        pthread_mutex_unlock(&op->mutex);
        return EALREADY;
    }
    if (op->canceled) {
        op->finished = 1;
        op->result_fd = NULL;
        ciel_async_op_unpin(op);
        pthread_mutex_unlock(&op->mutex);
        return ECANCELED;
    }
    if (!op->complete) {
        pthread_mutex_unlock(&op->mutex);
        return EAGAIN;
    }
    if (op->error != 0) {
        int err = op->error;
        op->finished = 1;
        op->result_fd = NULL;
        ciel_async_op_unpin(op);
        pthread_mutex_unlock(&op->mutex);
        return err;
    }
    if (op->result_fd == NULL) {
        op->finished = 1;
        ciel_async_op_unpin(op);
        pthread_mutex_unlock(&op->mutex);
        return EIO;
    }
    op->finished = 1;
    *out = op->result_fd;
    op->result_fd = NULL;
    ciel_async_op_unpin(op);
    pthread_mutex_unlock(&op->mutex);
    return 0;
}

int32_t ciel_async_tcp_finish_accept(CielAsyncOp *op, CielAsyncFd **out) {
    return ciel_async_finish_stream(op, CIEL_ASYNC_ACCEPT, out);
}

int32_t ciel_async_tcp_finish_connect(CielAsyncOp *op, CielAsyncFd **out) {
    return ciel_async_finish_stream(op, CIEL_ASYNC_CONNECT, out);
}

int32_t ciel_async_tcp_stream_local_addr(CielAsyncFd *stream,
                                         CielSocketAddr **out) {
    if (out == NULL)
        return EINVAL;
    int fd = -1;
    int32_t rc = ciel_async_fd_raw_snapshot(stream, &fd);
    if (rc != 0)
        return rc;
    CielSocketAddr *addr = ciel_net_addr_from_fd(fd, 0, &rc);
    if (addr == NULL)
        return rc == 0 ? EIO : rc;
    *out = addr;
    return 0;
}

int32_t ciel_async_tcp_stream_peer_addr(CielAsyncFd *stream,
                                        CielSocketAddr **out) {
    if (out == NULL)
        return EINVAL;
    int fd = -1;
    int32_t rc = ciel_async_fd_raw_snapshot(stream, &fd);
    if (rc != 0)
        return rc;
    CielSocketAddr *addr = ciel_net_addr_from_fd(fd, 1, &rc);
    if (addr == NULL)
        return rc == 0 ? EIO : rc;
    *out = addr;
    return 0;
}

int32_t ciel_async_tcp_shutdown_read(CielAsyncFd *stream) {
    int fd = -1;
    int32_t rc = ciel_async_fd_raw_snapshot(stream, &fd);
    if (rc != 0)
        return rc;
    if (shutdown(fd, SHUT_RD) != 0)
        return errno == ENOTCONN ? 0 : (errno == 0 ? EIO : errno);
    return 0;
}

int32_t ciel_async_tcp_shutdown_write(CielAsyncFd *stream) {
    int fd = -1;
    int32_t rc = ciel_async_fd_raw_snapshot(stream, &fd);
    if (rc != 0)
        return rc;
    if (shutdown(fd, SHUT_WR) != 0)
        return errno == ENOTCONN ? 0 : (errno == 0 ? EIO : errno);
    return 0;
}

static int32_t ciel_async_notify(CielAsyncOp *op, CielAsyncKind kind,
                                 CielActor *actor, void *message) {
    if (op == NULL || actor == NULL || message == NULL)
        return EINVAL;
    pthread_mutex_lock(&op->mutex);
    if (op->kind != kind) {
        pthread_mutex_unlock(&op->mutex);
        return EINVAL;
    }
    if (op->notify_set) {
        pthread_mutex_unlock(&op->mutex);
        return EALREADY;
    }
    if (op->canceled) {
        pthread_mutex_unlock(&op->mutex);
        return ECANCELED;
    }
    op->notify_actor = actor;
    op->notify_message = message;
    op->notify_set = 1;
    ciel_async_send_notification_locked(op);
    pthread_mutex_unlock(&op->mutex);
    return 0;
}

int32_t ciel_async_notify_read(CielAsyncOp *op, CielActor *actor,
                               void *message) {
    return ciel_async_notify(op, CIEL_ASYNC_READ, actor, message);
}

int32_t ciel_async_notify_write(CielAsyncOp *op, CielActor *actor,
                                void *message) {
    return ciel_async_notify(op, CIEL_ASYNC_WRITE, actor, message);
}

int32_t ciel_async_tcp_notify_accept(CielAsyncOp *op, CielActor *actor,
                                     void *message) {
    return ciel_async_notify(op, CIEL_ASYNC_ACCEPT, actor, message);
}

int32_t ciel_async_tcp_notify_connect(CielAsyncOp *op, CielActor *actor,
                                      void *message) {
    return ciel_async_notify(op, CIEL_ASYNC_CONNECT, actor, message);
}

int32_t ciel_async_notify_sleep(CielAsyncOp *op, CielActor *actor,
                                void *message) {
    return ciel_async_notify(op, CIEL_ASYNC_SLEEP, actor, message);
}

int32_t ciel_async_finish_read(CielAsyncOp *op, CielBytes **out) {
    if (op == NULL || out == NULL)
        return EINVAL;
    pthread_mutex_lock(&op->mutex);
    if (op->kind != CIEL_ASYNC_READ) {
        pthread_mutex_unlock(&op->mutex);
        return EINVAL;
    }
    if (op->finished) {
        pthread_mutex_unlock(&op->mutex);
        return EALREADY;
    }
    if (op->canceled) {
        op->finished = 1;
        op->bytes = NULL;
        ciel_async_op_unpin(op);
        pthread_mutex_unlock(&op->mutex);
        return ECANCELED;
    }
    if (!op->complete) {
        pthread_mutex_unlock(&op->mutex);
        return EAGAIN;
    }
    if (op->error != 0) {
        int err = op->error;
        op->finished = 1;
        op->bytes = NULL;
        ciel_async_op_unpin(op);
        pthread_mutex_unlock(&op->mutex);
        return err;
    }
    op->finished = 1;
    *out = op->bytes;
    op->bytes = NULL;
    ciel_async_op_unpin(op);
    pthread_mutex_unlock(&op->mutex);
    return 0;
}

int32_t ciel_async_finish_write(CielAsyncOp *op, size_t *written) {
    if (op == NULL || written == NULL)
        return EINVAL;
    pthread_mutex_lock(&op->mutex);
    if (op->kind != CIEL_ASYNC_WRITE) {
        pthread_mutex_unlock(&op->mutex);
        return EINVAL;
    }
    if (op->finished) {
        pthread_mutex_unlock(&op->mutex);
        return EALREADY;
    }
    if (op->canceled) {
        op->finished = 1;
        op->write_bytes = NULL;
        ciel_async_op_unpin(op);
        pthread_mutex_unlock(&op->mutex);
        return ECANCELED;
    }
    if (!op->complete) {
        pthread_mutex_unlock(&op->mutex);
        return EAGAIN;
    }
    if (op->error != 0) {
        int err = op->error;
        op->finished = 1;
        op->write_bytes = NULL;
        ciel_async_op_unpin(op);
        pthread_mutex_unlock(&op->mutex);
        return err;
    }
    op->finished = 1;
    *written = op->written;
    op->write_bytes = NULL;
    ciel_async_op_unpin(op);
    pthread_mutex_unlock(&op->mutex);
    return 0;
}

int32_t ciel_async_finish_sleep(CielAsyncOp *op) {
    if (op == NULL)
        return EINVAL;
    pthread_mutex_lock(&op->mutex);
    if (op->kind != CIEL_ASYNC_SLEEP) {
        pthread_mutex_unlock(&op->mutex);
        return EINVAL;
    }
    if (op->finished) {
        pthread_mutex_unlock(&op->mutex);
        return EALREADY;
    }
    if (op->canceled) {
        op->finished = 1;
        ciel_async_op_unpin(op);
        pthread_mutex_unlock(&op->mutex);
        return ECANCELED;
    }
    if (!op->complete) {
        pthread_mutex_unlock(&op->mutex);
        return EAGAIN;
    }
    if (op->error != 0) {
        int err = op->error;
        op->finished = 1;
        ciel_async_op_unpin(op);
        pthread_mutex_unlock(&op->mutex);
        return err;
    }
    op->finished = 1;
    ciel_async_op_unpin(op);
    pthread_mutex_unlock(&op->mutex);
    return 0;
}

int32_t ciel_async_cancel(CielAsyncOp *op) {
    if (op == NULL)
        return EINVAL;
    dispatch_source_t source = NULL;
    int raw_fd = -1;
    CielAsyncFd *result_fd = NULL;
    CielBufferedReader *buffered_reader = NULL;
    CielBytes *buffered_bytes = NULL;
    CielAsyncKind kind;
    CielTaskWaitNode *waiters = NULL;
    pthread_mutex_lock(&op->mutex);
    if (op->finished) {
        pthread_mutex_unlock(&op->mutex);
        return EALREADY;
    }
    kind = op->kind;
    buffered_reader = op->buffered_reader;
    if (buffered_reader != NULL && op->bytes != NULL) {
        buffered_bytes = op->bytes;
        op->bytes = NULL;
    }
    op->canceled = 1;
    ciel_async_op_clear_route_locked(op);
    op->notify_actor = NULL;
    op->notify_message = NULL;
    waiters = op->waiters;
    op->waiters = NULL;
    pthread_cond_broadcast(&op->cond);
    source = op->source;
    op->source = NULL;
    raw_fd = op->raw_fd;
    op->raw_fd = -1;
    result_fd = op->result_fd;
    op->result_fd = NULL;
    pthread_mutex_unlock(&op->mutex);
    if (buffered_reader != NULL) {
        pthread_mutex_lock(&buffered_reader->mutex);
        if (buffered_reader->pending_read == op)
            buffered_reader->pending_read = NULL;
        if (buffered_bytes != NULL)
            (void)ciel_buffered_reader_prepend_locked(buffered_reader,
                                                      buffered_bytes);
        pthread_mutex_unlock(&buffered_reader->mutex);
    }
    if (kind == CIEL_ASYNC_ACCEPT)
        ciel_async_listener_clear_pending(op);
    if ((kind == CIEL_ASYNC_READ || kind == CIEL_ASYNC_WRITE) &&
        op->fd != NULL && buffered_reader == NULL)
        (void)ciel_async_close(op->fd);
    if (source != NULL)
        dispatch_source_cancel(source);
    if (raw_fd >= 0)
        close(raw_fd);
    if (result_fd != NULL)
        (void)ciel_async_close(result_fd);
    ciel_task_schedule_waiters(waiters);
    return 0;
}

static atomic_uint_fast64_t ciel_next_future_task_id = 1;
static __thread uint32_t ciel_future_trampoline_depth = 0;
static __thread uint32_t ciel_future_trampoline_budget = 0;

#define CIEL_FUTURE_TRAMPOLINE_FAIRNESS_BUDGET 64

static uint64_t ciel_future_alloc_task_id(void) {
    uint64_t id = atomic_fetch_add_explicit(&ciel_next_future_task_id, 1,
                                            memory_order_relaxed);
    return id == 0 ? 1 : id;
}

CielFuture *ciel_future_new(size_t result_size, size_t result_align,
                            CielFutureRunFn run, void *ctx,
                            CielFutureCleanupFn cleanup) {
    if (run == NULL || result_align == 0) {
        errno = EINVAL;
        return NULL;
    }
    CielFuture *future = (CielFuture *)ciel_alloc(sizeof(CielFuture));
    memset(future, 0, sizeof(CielFuture));
    future->run = run;
    future->cleanup = cleanup;
    future->owner = ciel_resource_current_owner_or_root();
    future->ctx = ctx;
    future->result_size = result_size;
    future->result_align = result_align;
    future->state = CIEL_FUTURE_PENDING;
    future->task_id = ciel_future_alloc_task_id();
    int rc = pthread_mutex_init(&future->mutex, NULL);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    if (result_size > 0) {
        future->result = ciel_alloc(result_size);
        memset(future->result, 0, result_size);
    }
    return future;
}

CielFuture *ciel_future_from_handle(void *handle) {
    return (CielFuture *)handle;
}

static void ciel_select_cancel(CielSelectSet *set);
static void ciel_select_wait_until_ready(CielSelectSet *set);

static void ciel_future_run_cleanup(CielFuture *future, int32_t reason) {
    if (future == NULL || future->cleanup == NULL)
        return;
    pthread_mutex_lock(&future->mutex);
    if (future->cleanup_started) {
        pthread_mutex_unlock(&future->mutex);
        return;
    }
    future->cleanup_started = 1;
    CielFutureCleanupFn cleanup = future->cleanup;
    CielResourceOwner *owner = future->owner;
    void *ctx = future->ctx;
    pthread_mutex_unlock(&future->mutex);
    CielResourceOwner *previous = ciel_resource_set_current_owner(owner);
    cleanup(ctx, reason);
    ciel_resource_restore_current_owner(previous);
}

int32_t ciel_future_cancel(CielFuture *future) {
    if (future == NULL)
        return EINVAL;
    CielAsyncOp *op = NULL;
    CielSelectSet *select = NULL;
    CielAsyncChannel *channel = NULL;
    CielTaskGroup *group = NULL;
    pthread_mutex_lock(&future->mutex);
    if (future->state == CIEL_FUTURE_COMPLETE ||
        future->state == CIEL_FUTURE_FAILED) {
        pthread_mutex_unlock(&future->mutex);
        return EALREADY;
    }
    future->state = CIEL_FUTURE_FAILED;
    future->failure = ECANCELED;
    op = future->pending_op;
    select = future->pending_select;
    channel = future->pending_channel;
    group = future->pending_group;
    future->pending_op = NULL;
    future->pending_task = NULL;
    future->pending_select = NULL;
    future->pending_channel = NULL;
    future->pending_channel_mode = CIEL_PENDING_CHANNEL_NONE;
    future->pending_group = NULL;
    future->generation++;
    pthread_mutex_unlock(&future->mutex);
    if (op != NULL)
        (void)ciel_async_cancel(op);
    if (select != NULL)
        ciel_select_cancel(select);
    if (channel != NULL)
        ciel_async_channel_broadcast(channel);
    if (group != NULL)
        ciel_task_group_broadcast(group);
    ciel_future_run_cleanup(future, ECANCELED);
    return 0;
}

int32_t ciel_future_abort(CielFuture *future) {
    return ciel_future_cancel(future);
}

void ciel_future_bind_operation(CielFuture *future, CielAsyncOp *op) {
    if (future == NULL || op == NULL)
        return;
    pthread_mutex_lock(&future->mutex);
    uint64_t operation_id = ++future->next_operation_id;
    uint64_t generation = ++future->generation;
    if (operation_id == 0)
        operation_id = ++future->next_operation_id;
    if (generation == 0)
        generation = ++future->generation;
    future->pending_op = op;
    future->pending_task = NULL;
    future->pending_select = NULL;
    future->pending_channel = NULL;
    future->pending_channel_mode = CIEL_PENDING_CHANNEL_NONE;
    future->pending_group = NULL;
    pthread_mutex_unlock(&future->mutex);

    pthread_mutex_lock(&op->mutex);
    op->route_task_id = future->task_id;
    op->route_operation_id = operation_id;
    op->route_generation = generation;
    pthread_mutex_unlock(&op->mutex);
}

void ciel_future_clear_operation(CielFuture *future, CielAsyncOp *op) {
    if (future == NULL)
        return;
    pthread_mutex_lock(&future->mutex);
    if (future->pending_op == op) {
        future->pending_op = NULL;
        future->generation++;
    }
    pthread_mutex_unlock(&future->mutex);
    if (op != NULL) {
        pthread_mutex_lock(&op->mutex);
        ciel_async_op_clear_route_locked(op);
        pthread_mutex_unlock(&op->mutex);
    }
}

static void ciel_future_bind_task(CielFuture *future, CielTask *task) {
    if (future == NULL)
        return;
    pthread_mutex_lock(&future->mutex);
    future->pending_task = task;
    future->pending_op = NULL;
    future->pending_select = NULL;
    future->pending_channel = NULL;
    future->pending_channel_mode = CIEL_PENDING_CHANNEL_NONE;
    future->pending_group = NULL;
    pthread_mutex_unlock(&future->mutex);
}

static void ciel_future_clear_task(CielFuture *future, CielTask *task) {
    if (future == NULL)
        return;
    pthread_mutex_lock(&future->mutex);
    if (future->pending_task == task)
        future->pending_task = NULL;
    pthread_mutex_unlock(&future->mutex);
}

static void ciel_future_bind_select(CielFuture *future, CielSelectSet *set) {
    if (future == NULL)
        return;
    pthread_mutex_lock(&future->mutex);
    future->pending_select = set;
    future->pending_op = NULL;
    future->pending_task = NULL;
    future->pending_channel = NULL;
    future->pending_channel_mode = CIEL_PENDING_CHANNEL_NONE;
    future->pending_group = NULL;
    pthread_mutex_unlock(&future->mutex);
}

static void ciel_future_bind_channel(CielFuture *future,
                                     CielAsyncChannel *channel,
                                     CielPendingChannelMode mode) {
    if (future == NULL)
        return;
    pthread_mutex_lock(&future->mutex);
    future->pending_channel = channel;
    future->pending_channel_mode = mode;
    future->pending_op = NULL;
    future->pending_task = NULL;
    future->pending_select = NULL;
    future->pending_group = NULL;
    pthread_mutex_unlock(&future->mutex);
}

static void ciel_future_clear_channel(CielFuture *future,
                                      CielAsyncChannel *channel) {
    if (future == NULL)
        return;
    pthread_mutex_lock(&future->mutex);
    if (future->pending_channel == channel) {
        future->pending_channel = NULL;
        future->pending_channel_mode = CIEL_PENDING_CHANNEL_NONE;
    }
    pthread_mutex_unlock(&future->mutex);
}

static CIEL_MAYBE_UNUSED void
ciel_future_bind_task_group(CielFuture *future, CielTaskGroup *group) {
    if (future == NULL)
        return;
    pthread_mutex_lock(&future->mutex);
    future->pending_group = group;
    future->pending_op = NULL;
    future->pending_task = NULL;
    future->pending_select = NULL;
    future->pending_channel = NULL;
    future->pending_channel_mode = CIEL_PENDING_CHANNEL_NONE;
    pthread_mutex_unlock(&future->mutex);
}

static CIEL_MAYBE_UNUSED void
ciel_future_clear_task_group(CielFuture *future, CielTaskGroup *group) {
    if (future == NULL)
        return;
    pthread_mutex_lock(&future->mutex);
    if (future->pending_group == group)
        future->pending_group = NULL;
    pthread_mutex_unlock(&future->mutex);
}

static void ciel_async_wait_until_ready(CielAsyncOp *op) {
    pthread_mutex_lock(&op->mutex);
    while (!op->complete && !op->canceled)
        pthread_cond_wait(&op->cond, &op->mutex);
    pthread_mutex_unlock(&op->mutex);
}

int32_t ciel_future_poll(CielFuture *future, void *out);
int32_t ciel_future_poll_trampoline(CielFuture *future, void *out);
int32_t ciel_future_cancel(CielFuture *future);
int32_t ciel_future_abort(CielFuture *future);
void ciel_future_adopt_pending_operation(CielFuture *future, CielFuture *child);
void ciel_future_clear_pending_operation(CielFuture *future);
static void ciel_future_wait_until_ready(CielFuture *future);
static int ciel_task_register_future_pending_source(CielFuture *future,
                                                    CielTask *task);
static int ciel_select_register_task_waiter(CielSelectSet *set, CielTask *task);
static void ciel_select_wait_until_ready(CielSelectSet *set);
static void ciel_select_cancel(CielSelectSet *set);

static int ciel_future_has_pending_source(CielFuture *future) {
    if (future == NULL)
        return 0;
    pthread_mutex_lock(&future->mutex);
    int has_pending =
        future->pending_op != NULL || future->pending_task != NULL ||
        future->pending_select != NULL || future->pending_channel != NULL ||
        future->pending_group != NULL;
    pthread_mutex_unlock(&future->mutex);
    return has_pending;
}

int32_t ciel_future_run_to_completion(CielFuture *future, void *out) {
    for (;;) {
        int32_t rc = ciel_future_poll_trampoline(future, out);
        if (rc != EAGAIN)
            return rc;
        ciel_future_wait_until_ready(future);
    }
}

int32_t ciel_future_poll(CielFuture *future, void *out) {
    if (future == NULL)
        return EINVAL;
    pthread_mutex_lock(&future->mutex);
    if (future->state == CIEL_FUTURE_COMPLETE) {
        if (future->result_size > 0 && out != NULL)
            memcpy(out, future->result, future->result_size);
        pthread_mutex_unlock(&future->mutex);
        return 0;
    }
    if (future->state == CIEL_FUTURE_FAILED) {
        int32_t failure = future->failure == 0 ? EIO : future->failure;
        pthread_mutex_unlock(&future->mutex);
        return failure;
    }
    if (future->state == CIEL_FUTURE_RUNNING) {
        pthread_mutex_unlock(&future->mutex);
        return EALREADY;
    }
    future->state = CIEL_FUTURE_RUNNING;
    CielResourceOwner *owner = future->owner;
    pthread_mutex_unlock(&future->mutex);

    CielResourceOwner *previous = ciel_resource_set_current_owner(owner);
    int32_t rc = future->run(future->ctx, future->result);
    ciel_resource_restore_current_owner(previous);

    pthread_mutex_lock(&future->mutex);
    if (rc == 0) {
        future->state = CIEL_FUTURE_COMPLETE;
        future->pending_op = NULL;
        future->pending_task = NULL;
        future->pending_select = NULL;
        future->pending_channel = NULL;
        future->pending_channel_mode = CIEL_PENDING_CHANNEL_NONE;
        future->pending_group = NULL;
        if (future->result_size > 0 && out != NULL)
            memcpy(out, future->result, future->result_size);
    } else if (rc == EAGAIN) {
        future->state = CIEL_FUTURE_PENDING;
    } else {
        future->state = CIEL_FUTURE_FAILED;
        future->pending_op = NULL;
        future->pending_task = NULL;
        future->pending_select = NULL;
        future->pending_channel = NULL;
        future->pending_channel_mode = CIEL_PENDING_CHANNEL_NONE;
        future->pending_group = NULL;
        future->failure = rc;
    }
    pthread_mutex_unlock(&future->mutex);
    return rc;
}

void ciel_future_adopt_pending_operation(CielFuture *future,
                                         CielFuture *child) {
    if (future == NULL)
        return;
    CielAsyncOp *op = NULL;
    CielTask *task = NULL;
    CielSelectSet *select = NULL;
    CielAsyncChannel *channel = NULL;
    CielPendingChannelMode channel_mode = CIEL_PENDING_CHANNEL_NONE;
    CielTaskGroup *group = NULL;
    if (child != NULL) {
        pthread_mutex_lock(&child->mutex);
        op = child->pending_op;
        task = child->pending_task;
        select = child->pending_select;
        channel = child->pending_channel;
        channel_mode = child->pending_channel_mode;
        group = child->pending_group;
        pthread_mutex_unlock(&child->mutex);
    }
    pthread_mutex_lock(&future->mutex);
    future->pending_op = op;
    future->pending_task = task;
    future->pending_select = select;
    future->pending_channel = channel;
    future->pending_channel_mode = channel_mode;
    future->pending_group = group;
    pthread_mutex_unlock(&future->mutex);
}

void ciel_future_clear_pending_operation(CielFuture *future) {
    if (future == NULL)
        return;
    pthread_mutex_lock(&future->mutex);
    future->pending_op = NULL;
    future->pending_task = NULL;
    future->pending_select = NULL;
    future->pending_channel = NULL;
    future->pending_channel_mode = CIEL_PENDING_CHANNEL_NONE;
    future->pending_group = NULL;
    pthread_mutex_unlock(&future->mutex);
}

int32_t ciel_async_channel_send_poll(CielFuture *future,
                                     CielAsyncSender *sender,
                                     const void *value) {
    if (sender == NULL || sender->channel == NULL)
        return EINVAL;
    CielAsyncChannel *channel = sender->channel;
    pthread_mutex_lock(&channel->mutex);
    if (sender->closed || channel->live_receivers == 0) {
        pthread_mutex_unlock(&channel->mutex);
        ciel_future_clear_channel(future, channel);
        return EPIPE;
    }
    if (channel->len + channel->reserved >= channel->capacity) {
        ciel_future_bind_channel(future, channel, CIEL_PENDING_CHANNEL_SEND);
        pthread_mutex_unlock(&channel->mutex);
        return EAGAIN;
    }
    int32_t rc = ciel_async_channel_enqueue_locked(channel, value);
    CielTaskWaitNode *waiters =
        rc == 0 ? ciel_async_channel_take_recv_waiter_locked(channel) : NULL;
    pthread_mutex_unlock(&channel->mutex);
    ciel_task_schedule_waiters(waiters);
    ciel_future_clear_channel(future, channel);
    return rc;
}

int32_t ciel_async_channel_reserve_poll(CielFuture *future,
                                        CielAsyncSender *sender,
                                        CielAsyncSendPermit **out) {
    if (sender == NULL || sender->channel == NULL || out == NULL)
        return EINVAL;
    CielAsyncChannel *channel = sender->channel;
    pthread_mutex_lock(&channel->mutex);
    if (sender->closed || channel->live_receivers == 0) {
        pthread_mutex_unlock(&channel->mutex);
        ciel_future_clear_channel(future, channel);
        return EPIPE;
    }
    if (channel->len + channel->reserved >= channel->capacity) {
        ciel_future_bind_channel(future, channel, CIEL_PENDING_CHANNEL_SEND);
        pthread_mutex_unlock(&channel->mutex);
        return EAGAIN;
    }
    channel->reserved++;
    pthread_mutex_unlock(&channel->mutex);
    ciel_future_clear_channel(future, channel);
    CielAsyncSendPermit *permit =
        (CielAsyncSendPermit *)ciel_alloc(sizeof(CielAsyncSendPermit));
    permit->channel = channel;
    permit->used = 0;
    *out = permit;
    return 0;
}

int32_t ciel_async_channel_recv_poll(CielFuture *future,
                                     CielAsyncReceiver *receiver, void *out) {
    if (receiver == NULL || receiver->channel == NULL ||
        (out == NULL && receiver->channel->value_size > 0))
        return EINVAL;
    CielAsyncChannel *channel = receiver->channel;
    pthread_mutex_lock(&channel->mutex);
    void *value = ciel_async_channel_pop_locked(channel);
    if (value != NULL) {
        if (channel->value_size > 0)
            memcpy(out, value, channel->value_size);
        CielTaskWaitNode *waiters =
            ciel_async_channel_take_send_waiter_locked(channel);
        pthread_mutex_unlock(&channel->mutex);
        ciel_task_schedule_waiters(waiters);
        ciel_future_clear_channel(future, channel);
        return 0;
    }
    if (receiver->closed || channel->live_senders == 0) {
        pthread_mutex_unlock(&channel->mutex);
        ciel_future_clear_channel(future, channel);
        return EPIPE;
    }
    ciel_future_bind_channel(future, channel, CIEL_PENDING_CHANNEL_RECV);
    pthread_mutex_unlock(&channel->mutex);
    return EAGAIN;
}

int32_t ciel_future_await_channel_send(CielFuture *future,
                                       CielAsyncSender *sender,
                                       const void *value) {
    if (future == NULL || sender == NULL || sender->channel == NULL)
        return EINVAL;
    CielAsyncChannel *channel = sender->channel;
    pthread_mutex_lock(&channel->mutex);
    if (sender->closed || channel->live_receivers == 0) {
        pthread_mutex_unlock(&channel->mutex);
        ciel_future_clear_channel(future, channel);
        return EPIPE;
    }
    if (channel->len + channel->reserved >= channel->capacity) {
        ciel_future_bind_channel(future, channel, CIEL_PENDING_CHANNEL_SEND);
        pthread_mutex_unlock(&channel->mutex);
        return EAGAIN;
    }
    int32_t rc = ciel_async_channel_enqueue_locked(channel, value);
    CielTaskWaitNode *waiters =
        rc == 0 ? ciel_async_channel_take_recv_waiter_locked(channel) : NULL;
    pthread_mutex_unlock(&channel->mutex);
    ciel_task_schedule_waiters(waiters);
    ciel_future_clear_channel(future, channel);
    return rc;
}

int32_t ciel_future_await_channel_reserve(CielFuture *future,
                                          CielAsyncSender *sender,
                                          CielAsyncSendPermit **permit_out) {
    if (future == NULL || sender == NULL || sender->channel == NULL ||
        permit_out == NULL)
        return EINVAL;
    CielAsyncChannel *channel = sender->channel;
    pthread_mutex_lock(&channel->mutex);
    if (sender->closed || channel->live_receivers == 0) {
        pthread_mutex_unlock(&channel->mutex);
        ciel_future_clear_channel(future, channel);
        return EPIPE;
    }
    if (channel->len + channel->reserved >= channel->capacity) {
        ciel_future_bind_channel(future, channel, CIEL_PENDING_CHANNEL_SEND);
        pthread_mutex_unlock(&channel->mutex);
        return EAGAIN;
    }
    channel->reserved++;
    pthread_mutex_unlock(&channel->mutex);
    CielAsyncSendPermit *permit =
        (CielAsyncSendPermit *)ciel_alloc(sizeof(CielAsyncSendPermit));
    permit->channel = channel;
    permit->used = 0;
    *permit_out = permit;
    ciel_future_clear_channel(future, channel);
    return 0;
}

int32_t ciel_future_await_channel_recv(CielFuture *future,
                                       CielAsyncReceiver *receiver, void *out) {
    if (future == NULL || receiver == NULL || receiver->channel == NULL)
        return EINVAL;
    CielAsyncChannel *channel = receiver->channel;
    pthread_mutex_lock(&channel->mutex);
    if (receiver->closed) {
        pthread_mutex_unlock(&channel->mutex);
        ciel_future_clear_channel(future, channel);
        return EPIPE;
    }
    if (channel->head != NULL) {
        void *value = ciel_async_channel_pop_locked(channel);
        if (channel->value_size > 0 && out != NULL)
            memcpy(out, value, channel->value_size);
        CielTaskWaitNode *waiters =
            ciel_async_channel_take_send_waiter_locked(channel);
        pthread_mutex_unlock(&channel->mutex);
        ciel_task_schedule_waiters(waiters);
        ciel_future_clear_channel(future, channel);
        return 0;
    }
    if (channel->live_senders == 0) {
        pthread_mutex_unlock(&channel->mutex);
        ciel_future_clear_channel(future, channel);
        return EPIPE;
    }
    ciel_future_bind_channel(future, channel, CIEL_PENDING_CHANNEL_RECV);
    pthread_mutex_unlock(&channel->mutex);
    return EAGAIN;
}

static void ciel_task_wait_until_finished(CielTask *task) {
    if (task == NULL) {
        sched_yield();
        return;
    }
    pthread_mutex_lock(&task->mutex);
    while (!task->finished)
        pthread_cond_wait(&task->cond, &task->mutex);
    pthread_mutex_unlock(&task->mutex);
}

static int
ciel_async_channel_ready_for_mode_locked(CielAsyncChannel *channel,
                                         CielPendingChannelMode mode) {
    if (channel == NULL)
        return 1;
    switch (mode) {
    case CIEL_PENDING_CHANNEL_SEND:
        return channel->live_receivers == 0 ||
               channel->len + channel->reserved < channel->capacity;
    case CIEL_PENDING_CHANNEL_RECV:
        return channel->len > 0 || channel->live_senders == 0;
    case CIEL_PENDING_CHANNEL_NONE:
    default:
        return 1;
    }
}

static int ciel_task_register_future_pending_source(CielFuture *future,
                                                    CielTask *task) {
    if (future == NULL || task == NULL)
        return 1;
    CielAsyncOp *op = NULL;
    CielTask *pending_task = NULL;
    CielSelectSet *select = NULL;
    CielAsyncChannel *channel = NULL;
    CielPendingChannelMode channel_mode = CIEL_PENDING_CHANNEL_NONE;
    CielTaskGroup *group = NULL;
    pthread_mutex_lock(&future->mutex);
    op = future->pending_op;
    pending_task = future->pending_task;
    select = future->pending_select;
    channel = future->pending_channel;
    channel_mode = future->pending_channel_mode;
    group = future->pending_group;
    int future_ready = future->state == CIEL_FUTURE_COMPLETE ||
                       future->state == CIEL_FUTURE_FAILED;
    pthread_mutex_unlock(&future->mutex);
    if (future_ready)
        return 1;

    if (op != NULL) {
        pthread_mutex_lock(&op->mutex);
        int ready = op->complete || op->canceled || op->finished;
        int32_t rc = 0;
        if (!ready)
            rc = ciel_task_waiter_push(&op->waiters, task);
        pthread_mutex_unlock(&op->mutex);
        return ready || rc != 0;
    }
    if (channel != NULL) {
        pthread_mutex_lock(&channel->mutex);
        int ready =
            ciel_async_channel_ready_for_mode_locked(channel, channel_mode);
        int32_t rc = 0;
        if (!ready) {
            switch (channel_mode) {
            case CIEL_PENDING_CHANNEL_SEND:
                rc = ciel_task_waiter_push(&channel->send_waiters, task);
                break;
            case CIEL_PENDING_CHANNEL_RECV:
                rc = ciel_task_waiter_push(&channel->recv_waiters, task);
                break;
            case CIEL_PENDING_CHANNEL_NONE:
            default:
                ready = 1;
                break;
            }
        }
        pthread_mutex_unlock(&channel->mutex);
        return ready || rc != 0;
    }
    if (select != NULL) {
        return ciel_select_register_task_waiter(select, task);
    }
    if (pending_task != NULL) {
        pthread_mutex_lock(&pending_task->mutex);
        int ready = pending_task->finished != 0;
        int32_t rc = 0;
        if (!ready)
            rc = ciel_task_waiter_push(&pending_task->waiters, task);
        pthread_mutex_unlock(&pending_task->mutex);
        return ready || rc != 0;
    }
    if (group != NULL) {
        pthread_mutex_lock(&group->mutex);
        int ready =
            group->done_head != NULL || group->closed || group->live_tasks == 0;
        int32_t rc = 0;
        if (!ready)
            rc = ciel_task_waiter_push(&group->waiters, task);
        pthread_mutex_unlock(&group->mutex);
        return ready || rc != 0;
    }
    return 1;
}

static int ciel_task_register_pending_source(CielTask *task) {
    if (task == NULL)
        return 1;
    return ciel_task_register_future_pending_source(task->future, task);
}

static void ciel_future_wait_until_ready(CielFuture *future) {
    if (future == NULL) {
        sched_yield();
        return;
    }
    pthread_mutex_lock(&future->mutex);
    CielAsyncOp *op = future->pending_op;
    CielTask *task = future->pending_task;
    CielSelectSet *select = future->pending_select;
    CielAsyncChannel *channel = future->pending_channel;
    CielTaskGroup *group = future->pending_group;
    pthread_mutex_unlock(&future->mutex);
    if (op == NULL) {
        if (select != NULL)
            ciel_select_wait_until_ready(select);
        else if (task != NULL)
            ciel_task_wait_until_finished(task);
        else if (channel != NULL) {
            pthread_mutex_lock(&channel->mutex);
            pthread_cond_wait(&channel->cond, &channel->mutex);
            pthread_mutex_unlock(&channel->mutex);
        } else if (group != NULL) {
            pthread_mutex_lock(&group->mutex);
            pthread_cond_wait(&group->cond, &group->mutex);
            pthread_mutex_unlock(&group->mutex);
        } else
            sched_yield();
        return;
    }
    ciel_async_wait_until_ready(op);
}

static void ciel_task_finish(CielTask *task, int32_t rc) {
    if (task == NULL)
        return;
    CielTaskWaitNode *waiters = NULL;
    CielRoot *root = NULL;
    CielResourceOwner *owner = NULL;
    pthread_mutex_lock(&task->mutex);
    uint8_t was_scheduled = task->scheduled;
    task->scheduled = 0;
    if (!task->finished) {
        task->finished = 1;
        task->rc = rc;
        waiters = task->waiters;
        task->waiters = NULL;
        owner = task->owner;
        task->owner = NULL;
        if (!was_scheduled) {
            root = task->self_root;
            task->self_root = NULL;
        }
    }
    pthread_cond_broadcast(&task->cond);
    pthread_mutex_unlock(&task->mutex);
    if (owner != NULL)
        (void)ciel_resource_owner_close(owner);
    ciel_task_schedule_waiters(waiters);
    ciel_root_unpin(root);
}

static void ciel_task_unpin_if_finished(CielTask *task) {
    if (task == NULL)
        return;
    CielRoot *root = NULL;
    pthread_mutex_lock(&task->mutex);
    if (task->finished && !task->scheduled && task->self_root != NULL) {
        root = task->self_root;
        task->self_root = NULL;
    }
    pthread_mutex_unlock(&task->mutex);
    ciel_root_unpin(root);
}

static int32_t ciel_task_wait_future_run(void *ctx_raw, void *out_raw) {
    CielTaskWait *wait = (CielTaskWait *)ctx_raw;
    if (wait == NULL || wait->task == NULL || wait->future == NULL)
        return EINVAL;
    CielTask *task = wait->task;
    pthread_mutex_lock(&task->mutex);
    uint8_t finished = task->finished;
    pthread_mutex_unlock(&task->mutex);
    if (!finished) {
        ciel_future_bind_task(wait->future, task);
        return EAGAIN;
    }
    ciel_future_clear_task(wait->future, task);
    return ciel_future_poll(task->future, out_raw);
}

static void ciel_task_run(void *ctx_raw) {
    CielTask *task = (CielTask *)ctx_raw;
    if (task == NULL)
        return;
    int32_t rc = ciel_thread_attach_persistent();
    if (rc == 0) {
        pthread_mutex_lock(&task->mutex);
        int finished = task->finished != 0;
        pthread_mutex_unlock(&task->mutex);
        if (!finished)
            rc = ciel_future_poll_trampoline(task->future, NULL);
    }
    if (rc == EAGAIN) {
        pthread_mutex_lock(&task->mutex);
        task->scheduled = 0;
        pthread_mutex_unlock(&task->mutex);
        if (ciel_task_register_pending_source(task))
            ciel_task_schedule(task);
    } else {
        ciel_task_finish(task, rc);
    }
    ciel_task_unpin_if_finished(task);
}

void *ciel_task_spawn(CielFuture *future) {
    if (future == NULL) {
        errno = EINVAL;
        return NULL;
    }
    CielTask *task = (CielTask *)ciel_alloc(sizeof(CielTask));
    memset(task, 0, sizeof(CielTask));
    task->future = future;
    int32_t owner_rc = 0;
    task->owner = ciel_resource_owner_new_child(
        ciel_resource_current_owner_or_root(), ciel_resource_default_limits(),
        &owner_rc);
    if (task->owner == NULL) {
        errno = owner_rc == 0 ? ENOMEM : owner_rc;
        return NULL;
    }
    int32_t detach_rc = ciel_resource_owner_detach(task->owner);
    if (detach_rc != 0) {
        (void)ciel_resource_owner_close(task->owner);
        task->owner = NULL;
        errno = detach_rc;
        return NULL;
    }
    pthread_mutex_lock(&future->mutex);
    future->owner = task->owner;
    pthread_mutex_unlock(&future->mutex);
    task->self_root = ciel_root_pin(task);
    int rc = pthread_mutex_init(&task->mutex, NULL);
    if (rc != 0) {
        (void)ciel_resource_owner_close(task->owner);
        task->owner = NULL;
        ciel_root_unpin(task->self_root);
        task->self_root = NULL;
        errno = rc;
        return NULL;
    }
    rc = pthread_cond_init(&task->cond, NULL);
    if (rc != 0) {
        (void)ciel_resource_owner_close(task->owner);
        task->owner = NULL;
        ciel_root_unpin(task->self_root);
        task->self_root = NULL;
        errno = rc;
        return NULL;
    }
    CielTaskWait *wait = (CielTaskWait *)ciel_alloc(sizeof(CielTaskWait));
    wait->task = task;
    wait->future = NULL;
    task->wait_future =
        ciel_future_new(future->result_size, future->result_align,
                        ciel_task_wait_future_run, wait, NULL);
    if (task->wait_future == NULL) {
        (void)ciel_resource_owner_close(task->owner);
        task->owner = NULL;
        ciel_root_unpin(task->self_root);
        task->self_root = NULL;
        return NULL;
    }
    wait->future = task->wait_future;
    ciel_task_schedule(task);
    return task;
}

CielFuture *ciel_task_future_from_handle(void *handle) {
    CielTask *task = (CielTask *)handle;
    if (task == NULL)
        return NULL;
    CielFuture *future = task->future;
    if (future == NULL)
        return NULL;
    CielTaskWait *wait = (CielTaskWait *)ciel_alloc(sizeof(CielTaskWait));
    wait->task = task;
    wait->future = NULL;
    CielFuture *wait_future =
        ciel_future_new(future->result_size, future->result_align,
                        ciel_task_wait_future_run, wait, NULL);
    if (wait_future == NULL)
        return NULL;
    wait->future = wait_future;
    return wait_future;
}

int32_t ciel_task_cancel(void *handle) {
    CielTask *task = (CielTask *)handle;
    if (task == NULL)
        return EINVAL;
    int32_t rc = ciel_future_abort(task->future);
    ciel_task_finish(task, ECANCELED);
    if (rc == EALREADY)
        return 0;
    return rc == 0 ? 0 : rc;
}

int32_t ciel_task_is_finished(void *handle, bool *out) {
    CielTask *task = (CielTask *)handle;
    if (task == NULL || out == NULL)
        return EINVAL;
    pthread_mutex_lock(&task->mutex);
    *out = task->finished != 0;
    pthread_mutex_unlock(&task->mutex);
    return 0;
}

static void ciel_task_group_broadcast(CielTaskGroup *group) {
    if (group == NULL)
        return;
    pthread_mutex_lock(&group->mutex);
    CielTaskWaitNode *waiters = group->waiters;
    group->waiters = NULL;
    pthread_cond_broadcast(&group->cond);
    pthread_mutex_unlock(&group->mutex);
    ciel_task_schedule_waiters(waiters);
}

CielTaskGroup *ciel_task_group_new(void) {
    CielTaskGroup *group = (CielTaskGroup *)ciel_alloc(sizeof(CielTaskGroup));
    memset(group, 0, sizeof(*group));
    int rc = pthread_mutex_init(&group->mutex, NULL);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    rc = pthread_cond_init(&group->cond, NULL);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    return group;
}

static void ciel_task_group_watch_run(void *ctx_raw) {
    CielTaskGroupWatch *watch = (CielTaskGroupWatch *)ctx_raw;
    if (watch == NULL || watch->group == NULL || watch->node == NULL) {
        if (watch != NULL)
            GC_FREE(watch);
        return;
    }
    if (ciel_thread_attach_persistent() != 0) {
        GC_FREE(watch);
        return;
    }
    CielTaskGroup *group = watch->group;
    CielTaskGroupTaskNode *task_node = watch->node;
    ciel_task_wait_until_finished(task_node->task);
    CielTaskWaitNode *waiters = NULL;
    pthread_mutex_lock(&group->mutex);
    if (!task_node->completed) {
        task_node->completed = 1;
        if (group->live_tasks > 0)
            group->live_tasks--;
        if (!group->closed) {
            CielTaskGroupDoneNode *done = (CielTaskGroupDoneNode *)ciel_alloc(
                sizeof(CielTaskGroupDoneNode));
            done->task = task_node->task;
            done->next = NULL;
            if (group->done_tail != NULL)
                group->done_tail->next = done;
            else
                group->done_head = done;
            group->done_tail = done;
        }
    }
    waiters = group->waiters;
    group->waiters = NULL;
    pthread_cond_broadcast(&group->cond);
    pthread_mutex_unlock(&group->mutex);
    GC_FREE(watch);
    ciel_task_schedule_waiters(waiters);
}

int32_t ciel_task_group_add(CielTaskGroup *group, void *task_handle) {
    if (group == NULL || task_handle == NULL)
        return EINVAL;
    CielTask *task = (CielTask *)task_handle;
    CielTaskGroupTaskNode *node =
        (CielTaskGroupTaskNode *)ciel_alloc(sizeof(CielTaskGroupTaskNode));
    node->task = task;
    node->completed = 0;
    pthread_mutex_lock(&group->mutex);
    if (group->closed) {
        pthread_mutex_unlock(&group->mutex);
        return EPIPE;
    }
    node->next = group->tasks;
    group->tasks = node;
    group->live_tasks++;
    pthread_mutex_unlock(&group->mutex);
    CielTaskGroupWatch *watch = (CielTaskGroupWatch *)ciel_alloc_uncollectable(
        sizeof(CielTaskGroupWatch));
    watch->group = group;
    watch->node = node;
    dispatch_async_f(ciel_select_waiter_queue(), watch,
                     ciel_task_group_watch_run);
    return 0;
}

void *ciel_task_group_next_task(CielTaskGroup *group) {
    if (group == NULL) {
        errno = EINVAL;
        return NULL;
    }
    pthread_mutex_lock(&group->mutex);
    for (;;) {
        if (group->done_head != NULL) {
            CielTaskGroupDoneNode *node = group->done_head;
            group->done_head = node->next;
            if (group->done_head == NULL)
                group->done_tail = NULL;
            CielTaskWaitNode *waiters = group->waiters;
            group->waiters = NULL;
            CielTask *task = node->task;
            pthread_mutex_unlock(&group->mutex);
            ciel_task_schedule_waiters(waiters);
            return task;
        }
        if (group->closed || group->live_tasks == 0) {
            pthread_mutex_unlock(&group->mutex);
            errno = EPIPE;
            return NULL;
        }
        pthread_cond_wait(&group->cond, &group->mutex);
    }
}

int32_t ciel_task_group_cancel_all(CielTaskGroup *group) {
    if (group == NULL)
        return EINVAL;
    pthread_mutex_lock(&group->mutex);
    group->cancel_all = 1;
    CielTaskGroupTaskNode *tasks = group->tasks;
    pthread_mutex_unlock(&group->mutex);
    for (CielTaskGroupTaskNode *node = tasks; node != NULL; node = node->next) {
        if (!node->completed)
            (void)ciel_task_cancel(node->task);
    }
    ciel_task_group_broadcast(group);
    return 0;
}

int32_t ciel_task_group_close(CielTaskGroup *group) {
    if (group == NULL)
        return EINVAL;
    pthread_mutex_lock(&group->mutex);
    group->closed = 1;
    group->live_tasks = 0;
    group->done_head = NULL;
    group->done_tail = NULL;
    CielTaskWaitNode *waiters = group->waiters;
    group->waiters = NULL;
    pthread_cond_broadcast(&group->cond);
    pthread_mutex_unlock(&group->mutex);
    ciel_task_schedule_waiters(waiters);
    return 0;
}

int32_t ciel_future_poll_trampoline(CielFuture *future, void *out) {
    uint32_t was_outermost = ciel_future_trampoline_depth == 0;
    if (was_outermost)
        ciel_future_trampoline_budget = CIEL_FUTURE_TRAMPOLINE_FAIRNESS_BUDGET;
    if (ciel_future_trampoline_budget == 0) {
        if (was_outermost)
            ciel_future_trampoline_budget = 0;
        return EAGAIN;
    }
    ciel_future_trampoline_depth++;
    ciel_future_trampoline_budget--;
    int32_t rc = ciel_future_poll(future, out);
    ciel_future_trampoline_depth--;
    if (was_outermost)
        ciel_future_trampoline_budget = 0;
    return rc;
}

int32_t ciel_future_run_to_completion_trampoline(CielFuture *future,
                                                 void *out) {
    return ciel_future_run_to_completion(future, out);
}

CielSelectSet *ciel_select_set_new(size_t capacity, int biased) {
    if (capacity == 0) {
        errno = EINVAL;
        return NULL;
    }
    CielSelectSet *set = (CielSelectSet *)ciel_alloc(sizeof(CielSelectSet));
    memset(set, 0, sizeof(CielSelectSet));
    set->cap = capacity;
    set->biased = biased != 0;
    set->winner = -1;
    set->arms = (CielSelectArm *)ciel_alloc(sizeof(CielSelectArm) * capacity);
    memset(set->arms, 0, sizeof(CielSelectArm) * capacity);
    int rc = pthread_mutex_init(&set->mutex, NULL);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    rc = pthread_cond_init(&set->cond, NULL);
    if (rc != 0) {
        errno = rc;
        return NULL;
    }
    return set;
}

int32_t ciel_select_set_push(CielSelectSet *set, CielFuture *future,
                             size_t result_size, size_t result_align) {
    if (set == NULL || future == NULL || result_align == 0)
        return EINVAL;
    pthread_mutex_lock(&set->mutex);
    if (set->len >= set->cap || set->started) {
        pthread_mutex_unlock(&set->mutex);
        return set->started ? EALREADY : EOVERFLOW;
    }
    CielSelectArm *arm = &set->arms[set->len++];
    arm->future = future;
    arm->result_size = result_size;
    arm->result_align = result_align;
    if (result_size > 0) {
        arm->result = ciel_alloc(result_size);
        memset(arm->result, 0, result_size);
    }
    pthread_mutex_unlock(&set->mutex);
    return 0;
}

static void ciel_select_cancel_losers(CielSelectSet *set, size_t winner) {
    for (size_t i = 0; i < set->len; i++) {
        if (i != winner && set->arms[i].future != NULL)
            (void)ciel_future_cancel(set->arms[i].future);
    }
}

static int ciel_select_claim(CielSelectSet *set, size_t index, int32_t rc) {
    int claimed = 0;
    CielTaskWaitNode *waiters = NULL;
    pthread_mutex_lock(&set->mutex);
    if (set->winner == -1) {
        set->winner = (ssize_t)index;
        set->winner_rc = rc;
        waiters = set->waiters;
        set->waiters = NULL;
        claimed = 1;
        pthread_cond_broadcast(&set->cond);
    }
    pthread_mutex_unlock(&set->mutex);
    ciel_task_schedule_waiters(waiters);
    return claimed;
}

static void ciel_select_waiter_run(void *ctx_raw) {
    CielSelectWaiter *waiter = (CielSelectWaiter *)ctx_raw;
    if (waiter == NULL || waiter->set == NULL) {
        if (waiter != NULL)
            GC_FREE(waiter);
        return;
    }
    if (ciel_thread_attach_persistent() != 0) {
        GC_FREE(waiter);
        return;
    }
    CielSelectSet *set = waiter->set;
    size_t index = waiter->index;
    if (index >= set->len) {
        GC_FREE(waiter);
        goto done;
    }
    CielSelectArm *arm = &set->arms[index];
    for (;;) {
        pthread_mutex_lock(&set->mutex);
        int stopped = set->winner != -1;
        pthread_mutex_unlock(&set->mutex);
        if (stopped) {
            GC_FREE(waiter);
            goto done;
        }
        int32_t rc = ciel_future_poll_trampoline(arm->future, arm->result);
        if (rc != EAGAIN) {
            pthread_mutex_lock(&set->mutex);
            arm->rc = rc;
            arm->completed = 1;
            pthread_mutex_unlock(&set->mutex);
            (void)ciel_select_claim(set, index, rc);
            GC_FREE(waiter);
            goto done;
        }
        if (ciel_future_has_pending_source(arm->future)) {
            ciel_future_wait_until_ready(arm->future);
        } else {
            sched_yield();
        }
    }
done:
    (void)0;
}

static int ciel_select_register_task_waiter(CielSelectSet *set,
                                            CielTask *task) {
    if (set == NULL || task == NULL)
        return 1;
    pthread_mutex_lock(&set->mutex);
    int ready = set->winner != -1;
    size_t len = set->len;
    pthread_mutex_unlock(&set->mutex);
    if (ready)
        return 1;

    int should_schedule = 0;
    for (size_t i = 0; i < len; i++) {
        pthread_mutex_lock(&set->mutex);
        int stopped = set->winner != -1;
        CielFuture *arm_future = i < set->len ? set->arms[i].future : NULL;
        pthread_mutex_unlock(&set->mutex);
        if (stopped)
            return 1;
        if (arm_future == NULL) {
            should_schedule = 1;
            continue;
        }
        if (ciel_task_register_future_pending_source(arm_future, task))
            should_schedule = 1;
    }
    return should_schedule;
}

static size_t ciel_select_fair_start(size_t len) {
    if (len == 0)
        return 0;
    pthread_mutex_lock(&ciel_select_fairness_mutex);
    size_t start = ciel_select_next_start++ % len;
    pthread_mutex_unlock(&ciel_select_fairness_mutex);
    return start;
}

static int32_t ciel_select_poll_immediate(CielSelectSet *set) {
    if (set == NULL)
        return EINVAL;
    size_t len = set->len;
    if (len == 0)
        return EINVAL;
    size_t start = set->biased ? 0 : ciel_select_fair_start(len);
    for (size_t offset = 0; offset < len; offset++) {
        size_t i = (start + offset) % len;
        CielSelectArm *arm = &set->arms[i];
        int32_t rc = ciel_future_poll_trampoline(arm->future, arm->result);
        if (rc != EAGAIN) {
            arm->rc = rc;
            arm->completed = 1;
            (void)ciel_select_claim(set, i, rc);
            return 0;
        }
    }
    return EAGAIN;
}

static int32_t ciel_select_finish_winner(CielSelectSet *set, size_t winner,
                                         void *out_raw) {
    if (set == NULL || out_raw == NULL || winner >= set->len)
        return EINVAL;
    CielSelectArm *arm = &set->arms[winner];
    pthread_mutex_lock(&set->mutex);
    int completed = arm->completed;
    int32_t rc = arm->rc;
    pthread_mutex_unlock(&set->mutex);
    if (!completed) {
        rc = ciel_future_poll_trampoline(arm->future, arm->result);
        if (rc == EAGAIN)
            return EAGAIN;
        pthread_mutex_lock(&set->mutex);
        arm->rc = rc;
        arm->completed = 1;
        pthread_mutex_unlock(&set->mutex);
    }
    ciel_select_cancel_losers(set, winner);
    if (rc != 0)
        return rc;
    ((CielSelectResult *)out_raw)->index = winner;
    return 0;
}

static int32_t ciel_select_start_waiters(CielSelectSet *set) {
    if (set == NULL)
        return EINVAL;
    pthread_mutex_lock(&set->mutex);
    if (set->started) {
        pthread_mutex_unlock(&set->mutex);
        return 0;
    }
    set->started = 1;
    size_t len = set->len;
    pthread_mutex_unlock(&set->mutex);
    for (size_t i = 0; i < len; i++) {
        CielSelectWaiter *waiter = (CielSelectWaiter *)ciel_alloc_uncollectable(
            sizeof(CielSelectWaiter));
        waiter->set = set;
        waiter->index = i;
        dispatch_async_f(ciel_select_waiter_queue(), waiter,
                         ciel_select_waiter_run);
    }
    return 0;
}

static void ciel_select_wait_until_ready(CielSelectSet *set) {
    if (set == NULL) {
        sched_yield();
        return;
    }
    int32_t rc = ciel_select_start_waiters(set);
    if (rc != 0)
        return;
    pthread_mutex_lock(&set->mutex);
    while (set->winner == -1)
        pthread_cond_wait(&set->cond, &set->mutex);
    pthread_mutex_unlock(&set->mutex);
}

static void ciel_select_cancel(CielSelectSet *set) {
    if (set == NULL)
        return;
    CielTaskWaitNode *waiters = NULL;
    pthread_mutex_lock(&set->mutex);
    if (set->winner < 0) {
        set->winner = -2;
        set->winner_rc = ECANCELED;
        waiters = set->waiters;
        set->waiters = NULL;
        pthread_cond_broadcast(&set->cond);
    }
    size_t len = set->len;
    pthread_mutex_unlock(&set->mutex);
    ciel_task_schedule_waiters(waiters);
    for (size_t i = 0; i < len; i++) {
        if (set->arms[i].future != NULL)
            (void)ciel_future_cancel(set->arms[i].future);
    }
}

static int32_t ciel_select_future_run(void *ctx_raw, void *out_raw) {
    CielSelectSet *set = (CielSelectSet *)ctx_raw;
    if (set == NULL || out_raw == NULL)
        return EINVAL;
    pthread_mutex_lock(&set->mutex);
    ssize_t winner = set->winner;
    pthread_mutex_unlock(&set->mutex);
    if (winner >= 0) {
        return ciel_select_finish_winner(set, (size_t)winner, out_raw);
    }
    if (winner == -2) {
        return ECANCELED;
    }
    int32_t rc = ciel_select_poll_immediate(set);
    if (rc != EAGAIN) {
        pthread_mutex_lock(&set->mutex);
        winner = set->winner;
        pthread_mutex_unlock(&set->mutex);
        if (winner >= 0) {
            return ciel_select_finish_winner(set, (size_t)winner, out_raw);
        }
        return rc;
    }
    ciel_future_bind_select(set->future, set);
    return EAGAIN;
}

static void ciel_select_future_cleanup(void *ctx_raw, int32_t reason) {
    (void)reason;
    ciel_select_cancel((CielSelectSet *)ctx_raw);
}

CielFuture *ciel_select_future_new(CielSelectSet *set) {
    if (set == NULL) {
        errno = EINVAL;
        return NULL;
    }
    CielFuture *future = ciel_future_new(
        sizeof(CielSelectResult), CIEL_ALIGNOF(CielSelectResult),
        ciel_select_future_run, set, ciel_select_future_cleanup);
    if (future == NULL)
        return NULL;
    set->future = future;
    return future;
}

CielSelectSet *ciel_select_future_set(CielFuture *future) {
    if (future == NULL)
        return NULL;
    return (CielSelectSet *)future->ctx;
}

void *ciel_select_winner_value(CielSelectSet *set, size_t index) {
    if (set == NULL || index >= set->len)
        return NULL;
    return set->arms[index].result;
}

int32_t ciel_future_await_sleep_ms(CielFuture *future, CielAsyncOp **slot,
                                   uint64_t ms) {
    if (slot == NULL)
        return EINVAL;
    CielAsyncOp *op = *slot;
    if (op == NULL) {
        op = ciel_async_sleep_ms(ms);
        if (op == NULL)
            return errno == 0 ? EIO : errno;
        *slot = op;
        ciel_future_bind_operation(future, op);
    }
    int32_t rc = ciel_async_finish_sleep(op);
    if (rc != EAGAIN) {
        ciel_future_clear_operation(future, op);
        *slot = NULL;
    }
    return rc;
}
