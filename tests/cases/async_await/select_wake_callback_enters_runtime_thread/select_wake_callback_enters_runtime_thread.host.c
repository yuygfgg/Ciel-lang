#define GC_NO_THREAD_REDIRECTS 1
#define GC_THREADS 1

#include <errno.h>
#include <gc/gc.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <time.h>

typedef struct SelectContext {
    CielAsyncReceiver *receiver;
    CielFuture *future;
    CielSelectResult result;
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    pthread_t blocking_thread;
    int pending;
    int done;
    int32_t rc;
    atomic_int completion_registered;
    atomic_int completion_on_blocking_thread;
} SelectContext;

static int32_t select_arm_run(CielFuture *future, void *ctx_raw,
                              void *out_raw) {
    SelectContext *ctx = (SelectContext *)ctx_raw;
    int32_t rc = ciel_async_channel_recv_poll(future, ctx->receiver, out_raw);
    if (rc == EAGAIN) {
        pthread_mutex_lock(&ctx->mutex);
        ctx->pending = 1;
        pthread_cond_broadcast(&ctx->cond);
        pthread_mutex_unlock(&ctx->mutex);
    } else {
        atomic_store(&ctx->completion_registered, GC_thread_is_registered());
        atomic_store(&ctx->completion_on_blocking_thread,
                     pthread_equal(pthread_self(), ctx->blocking_thread));
    }
    return rc;
}

static void *run_select(void *ctx_raw) {
    SelectContext *ctx = (SelectContext *)ctx_raw;
    pthread_mutex_lock(&ctx->mutex);
    ctx->blocking_thread = pthread_self();
    pthread_mutex_unlock(&ctx->mutex);

    int32_t rc = ciel_runtime_enter_callback();
    if (rc == 0) {
        rc = ciel_future_run_to_completion(ctx->future, &ctx->result);
        ciel_runtime_leave_callback();
    }

    pthread_mutex_lock(&ctx->mutex);
    ctx->rc = rc;
    ctx->done = 1;
    pthread_cond_broadcast(&ctx->cond);
    pthread_mutex_unlock(&ctx->mutex);
    return NULL;
}

int main(void) {
    ciel_runtime_init();

    int result = 0;
    bool thread_created = false;
    bool pending = false;
    int value = 17;
    CielAsyncSender *sender = NULL;
    CielAsyncReceiver *receiver = NULL;
    CielFuture *arm = NULL;
    CielSelectSet *set = NULL;
    CielFuture *select_future = NULL;
    pthread_t thread;
    struct timespec registration_delay = {.tv_nsec = 20000000};
    SelectContext ctx = {
        .mutex = PTHREAD_MUTEX_INITIALIZER,
        .cond = PTHREAD_COND_INITIALIZER,
        .completion_registered = ATOMIC_VAR_INIT(-1),
        .completion_on_blocking_thread = ATOMIC_VAR_INIT(-1),
    };

    int32_t rc = ciel_async_channel_make(sizeof(int), _Alignof(int), 1, &sender,
                                         &receiver);
    if (rc != 0) {
        result = 1;
        goto cleanup;
    }
    ctx.receiver = receiver;

    arm =
        ciel_future_new(sizeof(int), _Alignof(int), select_arm_run, &ctx, NULL);
    set = ciel_select_set_new(1, 0);
    if (arm == NULL || set == NULL ||
        ciel_select_set_push(set, arm, sizeof(int), _Alignof(int)) != 0) {
        result = 2;
        goto cleanup;
    }
    select_future = ciel_select_future_new(set);
    if (select_future == NULL) {
        result = 3;
        goto cleanup;
    }
    ctx.future = select_future;

    if (pthread_create(&thread, NULL, run_select, &ctx) != 0) {
        result = 4;
        goto cleanup;
    }
    thread_created = true;

    pthread_mutex_lock(&ctx.mutex);
    while (!ctx.pending && !ctx.done)
        pthread_cond_wait(&ctx.cond, &ctx.mutex);
    pending = ctx.pending;
    pthread_mutex_unlock(&ctx.mutex);
    if (!pending) {
        result = 7;
        goto cleanup;
    }

    nanosleep(&registration_delay, NULL);
    if (ciel_async_channel_try_send(sender, &value) != 0) {
        result = 5;
        goto cleanup;
    }

cleanup:
    if (result != 0) {
        if (sender != NULL) {
            (void)ciel_async_sender_close(sender);
            sender = NULL;
        }
        if (receiver != NULL) {
            (void)ciel_async_receiver_close(receiver);
            receiver = NULL;
        }
    }
    if (thread_created && pthread_join(thread, NULL) != 0 && result == 0)
        result = 6;

    if (result == 0) {
        if (ctx.rc != 0 || ctx.result.index != 0 || ctx.result.arm_rc != 0) {
            result = 7;
        } else {
            int *winner = (int *)ciel_select_winner_value(set, 0);
            if (winner == NULL || *winner != value)
                result = 8;
            else if (atomic_load(&ctx.completion_on_blocking_thread) != 0)
                result = 9;
            else if (atomic_load(&ctx.completion_registered) != 1)
                result = 10;
        }
    }

    if (sender != NULL)
        (void)ciel_async_sender_close(sender);
    if (receiver != NULL)
        (void)ciel_async_receiver_close(receiver);
    return result;
}
