#include <errno.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>

typedef struct FailureContext {
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    int block_body;
    int body_started;
    int release_body;
    int cleanup_started;
    int release_cleanup;
    int32_t registration_rc;
} FailureContext;

typedef struct JoinContext {
    CielTaskGroup *group;
} JoinContext;

static const uint8_t failure_resource_type;

static int32_t failure_resource_close(void *ptr) {
    FailureContext *ctx = (FailureContext *)ptr;
    pthread_mutex_lock(&ctx->mutex);
    ctx->cleanup_started = 1;
    pthread_cond_broadcast(&ctx->cond);
    while (!ctx->release_cleanup)
        pthread_cond_wait(&ctx->cond, &ctx->mutex);
    pthread_mutex_unlock(&ctx->mutex);
    return 0;
}

static int32_t failure_task_run(CielFuture *future, void *ctx_raw,
                                void *out_raw) {
    (void)future;
    (void)out_raw;
    FailureContext *ctx = (FailureContext *)ctx_raw;
    CielResourceHandle handle;
    int32_t rc = ciel_resource_register_native(ctx, failure_resource_close,
                                               &failure_resource_type, &handle);

    pthread_mutex_lock(&ctx->mutex);
    ctx->registration_rc = rc;
    ctx->body_started = 1;
    pthread_cond_broadcast(&ctx->cond);
    while (ctx->block_body && !ctx->release_body)
        pthread_cond_wait(&ctx->cond, &ctx->mutex);
    pthread_mutex_unlock(&ctx->mutex);
    return rc == 0 ? EIO : rc;
}

static int32_t ready_task_run(CielFuture *future, void *ctx_raw,
                              void *out_raw) {
    (void)future;
    (void)ctx_raw;
    (void)out_raw;
    return 0;
}

static int32_t group_join_run(CielFuture *future, void *ctx_raw,
                              void *out_raw) {
    (void)out_raw;
    JoinContext *ctx = (JoinContext *)ctx_raw;
    return ciel_task_group_join_poll(future, ctx->group);
}

static int32_t wait_for_task(void *task, int32_t expected_rc) {
    CielFuture *wait = ciel_task_future_from_handle(task);
    if (wait == NULL)
        return ENOMEM;
    int32_t rc = ciel_future_run_to_completion(wait, NULL);
    return rc == expected_rc ? 0 : EINVAL;
}

static void wait_for_body(FailureContext *ctx) {
    pthread_mutex_lock(&ctx->mutex);
    while (!ctx->body_started)
        pthread_cond_wait(&ctx->cond, &ctx->mutex);
    pthread_mutex_unlock(&ctx->mutex);
}

static void release_body(FailureContext *ctx) {
    pthread_mutex_lock(&ctx->mutex);
    ctx->release_body = 1;
    pthread_cond_broadcast(&ctx->cond);
    pthread_mutex_unlock(&ctx->mutex);
}

static void wait_for_cleanup(FailureContext *ctx) {
    pthread_mutex_lock(&ctx->mutex);
    while (!ctx->cleanup_started)
        pthread_cond_wait(&ctx->cond, &ctx->mutex);
    pthread_mutex_unlock(&ctx->mutex);
}

static void release_cleanup(FailureContext *ctx) {
    pthread_mutex_lock(&ctx->mutex);
    ctx->release_cleanup = 1;
    pthread_cond_broadcast(&ctx->cond);
    pthread_mutex_unlock(&ctx->mutex);
}

static int32_t join_group(CielTaskGroup *group) {
    JoinContext ctx = {.group = group};
    CielFuture *join = ciel_future_new(0, 1, group_join_run, &ctx, NULL);
    return join == NULL ? ENOMEM : ciel_future_run_to_completion(join, NULL);
}

static void cancel_and_wait_for_task(void *task) {
    if (task == NULL)
        return;
    (void)ciel_task_cancel(task);
    CielFuture *wait = ciel_task_future_from_handle(task);
    if (wait != NULL)
        (void)ciel_future_run_to_completion(wait, NULL);
}

static int run_case(bool add_before_settle) {
    int result = 0;
    bool group_closed = false;
    FailureContext failure_ctx = {
        .mutex = PTHREAD_MUTEX_INITIALIZER,
        .cond = PTHREAD_COND_INITIALIZER,
        .block_body = add_before_settle,
    };
    CielTaskGroup *group = ciel_task_group_new();
    CielFuture *failure =
        ciel_future_new(0, 1, failure_task_run, &failure_ctx, NULL);
    void *failure_task = failure == NULL ? NULL : ciel_task_spawn(failure);
    CielFuture *ready = NULL;
    void *ready_task = NULL;
    if (group == NULL || failure_task == NULL) {
        result = 1;
        goto cleanup;
    }

    wait_for_body(&failure_ctx);
    if (failure_ctx.registration_rc != 0) {
        result = 2;
        goto cleanup;
    }
    if (add_before_settle) {
        if (ciel_task_group_add(group, failure_task) != 0) {
            result = 3;
            goto cleanup;
        }
        release_body(&failure_ctx);
        wait_for_cleanup(&failure_ctx);
    } else {
        wait_for_cleanup(&failure_ctx);
        if (ciel_task_group_add(group, failure_task) != 0) {
            result = 4;
            goto cleanup;
        }
    }

    ready = ciel_future_new(0, 1, ready_task_run, NULL, NULL);
    ready_task = ready == NULL ? NULL : ciel_task_spawn(ready);
    if (ready_task == NULL || ciel_task_group_add(group, ready_task) != 0) {
        result = 5;
        goto cleanup;
    }
    if (wait_for_task(ready_task, 0) != 0) {
        result = 6;
        goto cleanup;
    }

    void *completed = NULL;
    if (ciel_task_group_try_next_task(group, &completed) != 0 ||
        completed != ready_task) {
        result = 7;
        goto cleanup;
    }
    if (ciel_task_group_close(group) != 0) {
        result = 8;
        goto cleanup;
    }
    group_closed = true;

    bool finished = true;
    if (ciel_task_is_finished(failure_task, &finished) != 0 || finished) {
        result = 9;
        goto cleanup;
    }
    release_cleanup(&failure_ctx);
    int32_t join_rc = join_group(group);
    int32_t failure_rc = wait_for_task(failure_task, EIO);
    if (join_rc != 0 || failure_rc != 0) {
        result = 10;
        goto cleanup;
    }

    completed = NULL;
    if (ciel_task_group_try_next_task(group, &completed) != 0 ||
        completed != failure_task) {
        result = 11;
        goto cleanup;
    }
    completed = (void *)(uintptr_t)1;
    if (ciel_task_group_try_next_task(group, &completed) != EPIPE ||
        completed != NULL) {
        result = 12;
        goto cleanup;
    }

cleanup:
    release_body(&failure_ctx);
    release_cleanup(&failure_ctx);
    if (result != 0) {
        if (group != NULL && !group_closed &&
            ciel_task_group_close(group) == 0)
            group_closed = true;
        cancel_and_wait_for_task(ready_task);
        cancel_and_wait_for_task(failure_task);
        if (group != NULL && group_closed)
            (void)join_group(group);
    }
    return result;
}

int main(void) {
    ciel_runtime_init();
    int before_rc = run_case(true);
    if (before_rc != 0)
        return before_rc;
    int during_rc = run_case(false);
    return during_rc == 0 ? 0 : 20 + during_rc;
}
