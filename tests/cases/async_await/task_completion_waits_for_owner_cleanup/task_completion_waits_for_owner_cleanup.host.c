#include <errno.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>

typedef struct JoinContext {
    CielTaskGroup *group;
} JoinContext;

typedef enum BarrierKind {
    BARRIER_TASK_WAIT,
    BARRIER_GROUP_JOIN,
} BarrierKind;

typedef struct BarrierRun {
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    BarrierKind kind;
    void *task;
    CielTaskGroup *group;
    int ready;
    int cleanup_started;
    int cleanup_released;
    int cleanup_finished;
    int done;
    int32_t setup_rc;
    int32_t rc;
} BarrierRun;

static const uint8_t cleanup_probe_resource_type;

static int32_t cleanup_probe_close(void *ptr) {
    BarrierRun *run = (BarrierRun *)ptr;
    pthread_mutex_lock(&run->mutex);
    run->cleanup_started = 1;
    pthread_cond_broadcast(&run->cond);
    while (!run->cleanup_released)
        pthread_cond_wait(&run->cond, &run->mutex);
    run->cleanup_finished = 1;
    pthread_cond_broadcast(&run->cond);
    pthread_mutex_unlock(&run->mutex);
    return 0;
}

static int32_t task_body_run(CielFuture *future, void *ctx_raw, void *out_raw) {
    (void)future;
    (void)out_raw;
    BarrierRun *run = (BarrierRun *)ctx_raw;
    CielResourceHandle handle;
    return ciel_resource_register_native(run, cleanup_probe_close,
                                         &cleanup_probe_resource_type, &handle);
}

static int32_t group_join_run(CielFuture *future, void *ctx_raw,
                              void *out_raw) {
    (void)out_raw;
    JoinContext *ctx = (JoinContext *)ctx_raw;
    return ciel_task_group_join_poll(future, ctx->group);
}

static int32_t unused_future_run(CielFuture *future, void *ctx_raw,
                                 void *out_raw) {
    (void)future;
    (void)ctx_raw;
    (void)out_raw;
    return EAGAIN;
}

static void barrier_run_publish_ready(BarrierRun *run, void *task,
                                      CielTaskGroup *group,
                                      int32_t setup_rc) {
    pthread_mutex_lock(&run->mutex);
    run->task = task;
    run->group = group;
    run->setup_rc = setup_rc;
    run->ready = 1;
    pthread_cond_broadcast(&run->cond);
    pthread_mutex_unlock(&run->mutex);
}

static void barrier_run_publish_done(BarrierRun *run, int32_t rc) {
    pthread_mutex_lock(&run->mutex);
    run->rc = rc;
    run->done = 1;
    pthread_cond_broadcast(&run->cond);
    pthread_mutex_unlock(&run->mutex);
}

static void *run_barrier(void *ctx_raw) {
    BarrierRun *run = (BarrierRun *)ctx_raw;
    int32_t attach_rc = ciel_runtime_enter_callback();
    if (attach_rc != 0) {
        barrier_run_publish_ready(run, NULL, NULL, attach_rc);
        barrier_run_publish_done(run, attach_rc);
        return NULL;
    }

    CielFuture *body = ciel_future_new(0, 1, task_body_run, run, NULL);
    void *task = body == NULL ? NULL : ciel_task_spawn(body);
    CielTaskGroup *group = NULL;
    int32_t rc = 0;
    if (task == NULL) {
        rc = errno == 0 ? ENOMEM : errno;
        barrier_run_publish_ready(run, task, group, rc);
    } else if (run->kind == BARRIER_TASK_WAIT) {
        CielFuture *wait = ciel_task_future_from_handle(task);
        int32_t setup_rc = wait == NULL ? ENOMEM : 0;
        barrier_run_publish_ready(run, task, group, setup_rc);
        rc = setup_rc == 0 ? ciel_future_run_to_completion(wait, NULL)
                           : setup_rc;
    } else {
        group = ciel_task_group_new();
        int32_t setup_rc = 0;
        if (group == NULL) {
            setup_rc = ENOMEM;
        } else {
            setup_rc = ciel_task_group_add(group, task);
            if (setup_rc == 0)
                setup_rc = ciel_task_group_close(group);
        }
        barrier_run_publish_ready(run, task, group, setup_rc);
        if (setup_rc == 0) {
            JoinContext join_ctx = {.group = group};
            CielFuture *join =
                ciel_future_new(0, 1, group_join_run, &join_ctx, NULL);
            rc = join == NULL ? ENOMEM
                              : ciel_future_run_to_completion(join, NULL);
        } else {
            rc = setup_rc;
        }
    }

    barrier_run_publish_done(run, rc);
    ciel_runtime_leave_callback();
    return NULL;
}

static int wait_for_barrier_start(BarrierRun *run) {
    pthread_mutex_lock(&run->mutex);
    while (!run->ready)
        pthread_cond_wait(&run->cond, &run->mutex);
    if (run->setup_rc != 0 || run->task == NULL) {
        pthread_mutex_unlock(&run->mutex);
        return 1;
    }
    while (!run->cleanup_started && !run->done)
        pthread_cond_wait(&run->cond, &run->mutex);
    int cleanup_started = run->cleanup_started;
    pthread_mutex_unlock(&run->mutex);
    return cleanup_started ? 0 : 1;
}

static void release_cleanup(BarrierRun *run) {
    pthread_mutex_lock(&run->mutex);
    run->cleanup_released = 1;
    pthread_cond_broadcast(&run->cond);
    pthread_mutex_unlock(&run->mutex);
}

static void wait_for_task_to_stop(void *task) {
    if (task == NULL)
        return;
    CielFuture *wait = ciel_task_future_from_handle(task);
    if (wait != NULL)
        (void)ciel_future_run_to_completion(wait, NULL);
}

static int run_cleanup_barrier_test(BarrierKind kind) {
    int result = 0;
    bool thread_created = false;
    BarrierRun run = {
        .mutex = PTHREAD_MUTEX_INITIALIZER,
        .cond = PTHREAD_COND_INITIALIZER,
        .kind = kind,
    };
    pthread_t thread;
    if (pthread_create(&thread, NULL, run_barrier, &run) != 0) {
        result = 1;
        goto cleanup;
    }
    thread_created = true;
    if (wait_for_barrier_start(&run) != 0) {
        result = 2;
        goto cleanup;
    }

    bool finished = true;
    if (run.task == NULL || ciel_task_is_finished(run.task, &finished) != 0 ||
        finished) {
        result = 3;
        goto cleanup;
    }

    if (kind == BARRIER_GROUP_JOIN) {
        CielFuture *join_probe =
            ciel_future_new(0, 1, unused_future_run, NULL, NULL);
        if (join_probe == NULL || run.group == NULL ||
            ciel_task_group_join_poll(join_probe, run.group) != EAGAIN) {
            result = 4;
            goto cleanup;
        }
    }

    pthread_mutex_lock(&run.mutex);
    int returned_early = run.done;
    pthread_mutex_unlock(&run.mutex);
    if (returned_early) {
        result = 5;
        goto cleanup;
    }

cleanup:
    release_cleanup(&run);
    if (result != 0) {
        if (run.group != NULL)
            (void)ciel_task_group_close(run.group);
        if (run.task != NULL)
            (void)ciel_task_cancel(run.task);
    }
    if (thread_created && pthread_join(thread, NULL) != 0 && result == 0)
        result = 6;
    wait_for_task_to_stop(run.task);
    if (result == 0) {
        pthread_mutex_lock(&run.mutex);
        int invalid_completion = run.rc != 0 || !run.cleanup_finished;
        pthread_mutex_unlock(&run.mutex);
        if (invalid_completion)
            result = 7;
    }
    return result;
}

int main(void) {
    ciel_runtime_init();
    int task_rc = run_cleanup_barrier_test(BARRIER_TASK_WAIT);
    if (task_rc != 0)
        return task_rc;
    int group_rc = run_cleanup_barrier_test(BARRIER_GROUP_JOIN);
    if (group_rc != 0)
        return 10 + group_rc;
    return 0;
}
