#include <errno.h>
#include <stdbool.h>
#include <stdint.h>

typedef struct SleepContext {
    CielAsyncOp *op;
} SleepContext;

static int32_t ready_task_run(CielFuture *future, void *ctx_raw,
                              void *out_raw) {
    (void)future;
    (void)ctx_raw;
    (void)out_raw;
    return 0;
}

static int32_t sleep_task_run(CielFuture *future, void *ctx_raw,
                              void *out_raw) {
    (void)out_raw;
    SleepContext *ctx = (SleepContext *)ctx_raw;
    return ciel_future_await_sleep_ms(future, &ctx->op, 10000);
}

static void sleep_task_cleanup(CielFuture *future, void *ctx_raw,
                               int32_t reason) {
    (void)future;
    (void)reason;
    SleepContext *ctx = (SleepContext *)ctx_raw;
    if (ctx->op != NULL)
        (void)ciel_async_cancel(ctx->op);
    ctx->op = NULL;
}

static int32_t wait_for_task(void *task, int32_t expected_rc) {
    CielFuture *wait = ciel_task_future_from_handle(task);
    if (wait == NULL)
        return ENOMEM;
    int32_t rc = ciel_future_run_to_completion(wait, NULL);
    return rc == expected_rc ? 0 : rc == 0 ? EINVAL : rc;
}

static void cancel_and_wait_for_task(void *task) {
    if (task == NULL)
        return;
    (void)ciel_task_cancel(task);
    CielFuture *wait = ciel_task_future_from_handle(task);
    if (wait != NULL)
        (void)ciel_future_run_to_completion(wait, NULL);
}

int main(void) {
    ciel_runtime_init();

    int result = 0;
    bool group_closed = false;
    bool second_group_closed = false;
    void *drained = (void *)(uintptr_t)1;
    void *ready_task = NULL;
    void *sleep_task = NULL;
    CielTaskGroup *closed_group = NULL;
    SleepContext sleep_ctx = {0};
    CielTaskGroup *group = ciel_task_group_new();
    if (group == NULL) {
        result = 1;
        goto cleanup;
    }
    if (ciel_task_group_try_next_task(group, &drained) != EAGAIN ||
        drained != NULL) {
        result = 2;
        goto cleanup;
    }

    CielFuture *ready = ciel_future_new(0, 1, ready_task_run, NULL, NULL);
    ready_task = ready == NULL ? NULL : ciel_task_spawn(ready);
    if (ready_task == NULL || ciel_task_group_add(group, ready_task) != 0) {
        result = 3;
        goto cleanup;
    }
    if (wait_for_task(ready_task, 0) != 0) {
        result = 4;
        goto cleanup;
    }
    bool cancelled = true;
    if (ciel_task_is_cancelled(ready_task, &cancelled) != 0 || cancelled) {
        result = 5;
        goto cleanup;
    }
    if (ciel_task_group_close(group) != 0) {
        result = 6;
        goto cleanup;
    }
    group_closed = true;

    if (ciel_task_group_try_next_task(group, &drained) != 0 ||
        drained != ready_task) {
        result = 7;
        goto cleanup;
    }
    drained = (void *)(uintptr_t)1;
    if (ciel_task_group_try_next_task(group, &drained) != EPIPE ||
        drained != NULL) {
        result = 8;
        goto cleanup;
    }

    closed_group = ciel_task_group_new();
    CielFuture *sleep =
        ciel_future_new(0, 1, sleep_task_run, &sleep_ctx, sleep_task_cleanup);
    sleep_task = sleep == NULL ? NULL : ciel_task_spawn(sleep);
    if (closed_group == NULL || sleep_task == NULL ||
        ciel_task_group_add(closed_group, sleep_task) != 0) {
        result = 9;
        goto cleanup;
    }
    cancelled = true;
    if (ciel_task_is_cancelled(sleep_task, &cancelled) != 0 || cancelled) {
        result = 10;
        goto cleanup;
    }
    if (ciel_task_group_close(closed_group) != 0) {
        result = 11;
        goto cleanup;
    }
    second_group_closed = true;
    if (ciel_task_cancel(sleep_task) != 0) {
        result = 11;
        goto cleanup;
    }
    if (wait_for_task(sleep_task, ECANCELED) != 0) {
        result = 12;
        goto cleanup;
    }
    if (ciel_task_is_cancelled(sleep_task, &cancelled) != 0 || !cancelled) {
        result = 13;
        goto cleanup;
    }

    drained = (void *)(uintptr_t)1;
    if (ciel_task_group_try_next_task(closed_group, &drained) != EPIPE ||
        drained != NULL) {
        result = 14;
        goto cleanup;
    }

cleanup:
    if (group != NULL && !group_closed)
        (void)ciel_task_group_close(group);
    if (closed_group != NULL && !second_group_closed)
        (void)ciel_task_group_close(closed_group);
    if (result != 0) {
        cancel_and_wait_for_task(sleep_task);
        cancel_and_wait_for_task(ready_task);
    }
    return result;
}
