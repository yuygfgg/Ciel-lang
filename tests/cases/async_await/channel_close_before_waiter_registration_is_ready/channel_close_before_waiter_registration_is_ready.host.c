#include <errno.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <time.h>

typedef enum PendingMode {
    PENDING_SEND,
    PENDING_RESERVE,
    PENDING_RECV,
} PendingMode;

typedef struct PendingContext {
    CielAsyncSender *sender;
    CielAsyncReceiver *receiver;
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    PendingMode mode;
    int value;
    int polled;
    int release;
} PendingContext;

static int32_t pending_run(CielFuture *future, void *ctx_raw, void *out_raw) {
    (void)out_raw;
    PendingContext *ctx = (PendingContext *)ctx_raw;
    int32_t rc = EINVAL;
    if (ctx->mode == PENDING_SEND) {
        rc = ciel_async_channel_send_poll(future, ctx->sender, &ctx->value);
    } else if (ctx->mode == PENDING_RESERVE) {
        CielAsyncSendPermit *permit = NULL;
        rc = ciel_async_channel_reserve_poll(future, ctx->sender, &permit);
        if (rc == 0 && permit != NULL)
            (void)ciel_async_send_permit_release(permit);
    } else {
        rc = ciel_async_channel_recv_poll(future, ctx->receiver, &ctx->value);
    }
    if (rc != EAGAIN)
        return rc;

    pthread_mutex_lock(&ctx->mutex);
    ctx->polled = 1;
    pthread_cond_broadcast(&ctx->cond);
    while (!ctx->release)
        pthread_cond_wait(&ctx->cond, &ctx->mutex);
    pthread_mutex_unlock(&ctx->mutex);
    return EAGAIN;
}

static struct timespec deadline_after_seconds(time_t seconds) {
    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += seconds;
    return deadline;
}

static int wait_until_polled(PendingContext *ctx) {
    struct timespec deadline = deadline_after_seconds(2);
    pthread_mutex_lock(&ctx->mutex);
    while (!ctx->polled) {
        int rc = pthread_cond_timedwait(&ctx->cond, &ctx->mutex, &deadline);
        if (rc == ETIMEDOUT) {
            pthread_mutex_unlock(&ctx->mutex);
            return ETIMEDOUT;
        }
        if (rc != 0) {
            pthread_mutex_unlock(&ctx->mutex);
            return rc;
        }
    }
    pthread_mutex_unlock(&ctx->mutex);
    return 0;
}

static int wait_until_finished(void *task) {
    struct timespec delay = {.tv_nsec = 1000000};
    for (size_t attempt = 0; attempt < 2000; attempt++) {
        bool finished = false;
        int32_t rc = ciel_task_is_finished(task, &finished);
        if (rc != 0)
            return rc;
        if (finished)
            return 0;
        nanosleep(&delay, NULL);
    }
    return ETIMEDOUT;
}

static int run_pending_case(PendingMode mode) {
    int result = 0;
    CielAsyncSender *sender = NULL;
    CielAsyncReceiver *receiver = NULL;
    void *task = NULL;
    PendingContext ctx = {
        .mutex = PTHREAD_MUTEX_INITIALIZER,
        .cond = PTHREAD_COND_INITIALIZER,
        .mode = mode,
        .value = 17,
    };

    int32_t rc = ciel_async_channel_make(sizeof(int), _Alignof(int), 1, &sender,
                                         &receiver);
    if (rc != 0)
        return 1;
    ctx.sender = sender;
    ctx.receiver = receiver;
    if (mode != PENDING_RECV &&
        ciel_async_channel_try_send(sender, &ctx.value) != 0) {
        result = 2;
        goto cleanup;
    }

    CielFuture *future = ciel_future_new(0, 1, pending_run, &ctx, NULL);
    if (future == NULL) {
        result = 3;
        goto cleanup;
    }
    task = ciel_task_spawn(future);
    if (task == NULL) {
        result = 4;
        goto cleanup;
    }
    if (wait_until_polled(&ctx) != 0) {
        result = 5;
        goto cleanup;
    }

    rc = mode == PENDING_RECV ? ciel_async_receiver_close(receiver)
                              : ciel_async_sender_close(sender);
    if (rc != 0) {
        result = 6;
        goto cleanup;
    }
    pthread_mutex_lock(&ctx.mutex);
    ctx.release = 1;
    pthread_cond_broadcast(&ctx.cond);
    pthread_mutex_unlock(&ctx.mutex);

    if (wait_until_finished(task) != 0) {
        result = 7;
        goto cleanup;
    }
    CielFuture *task_future = ciel_task_future_from_handle(task);
    if (task_future == NULL || ciel_future_poll(task_future, NULL) != EPIPE)
        result = 8;

cleanup:
    pthread_mutex_lock(&ctx.mutex);
    ctx.release = 1;
    pthread_cond_broadcast(&ctx.cond);
    pthread_mutex_unlock(&ctx.mutex);
    if (task != NULL && result != 0) {
        (void)ciel_task_cancel(task);
        (void)wait_until_finished(task);
    }
    if (sender != NULL)
        (void)ciel_async_sender_close(sender);
    if (receiver != NULL)
        (void)ciel_async_receiver_close(receiver);
    pthread_cond_destroy(&ctx.cond);
    pthread_mutex_destroy(&ctx.mutex);
    return result;
}

int main(void) {
    ciel_runtime_init();
    int result = run_pending_case(PENDING_SEND);
    if (result != 0)
        return result;
    result = run_pending_case(PENDING_RESERVE);
    if (result != 0)
        return 20 + result;
    result = run_pending_case(PENDING_RECV);
    if (result != 0)
        return 40 + result;
    return 0;
}
