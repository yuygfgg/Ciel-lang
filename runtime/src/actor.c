#include "internal.h"

typedef struct CielActorJob {
    CielActor *actor;
    void *value;
} CielActorJob;

struct CielActor {
    dispatch_queue_t queue;
    dispatch_group_t jobs;
    dispatch_semaphore_t lifecycle_lock;
    CielResourceOwner *owner;
    void *state;
    void *handler;
    CielActorDispatchFn dispatch;
    int closing;
    int joined;
    int failed;
    int join_result;
};

static char ciel_actor_queue_key;

static void ciel_actor_lock(CielActor *actor) {
    dispatch_semaphore_wait(actor->lifecycle_lock, DISPATCH_TIME_FOREVER);
}

static void ciel_actor_unlock(CielActor *actor) {
    dispatch_semaphore_signal(actor->lifecycle_lock);
}

static void ciel_actor_job_run(void *raw) {
    CielActorJob *job = (CielActorJob *)raw;
    CielActor *actor = job->actor;
    void *message = job->value;
    int32_t attach_rc = ciel_runtime_enter_callback();
    int32_t failed = attach_rc != 0;
    if (attach_rc == 0) {
        CielResourceOwner *previous =
            ciel_resource_set_current_owner(actor->owner);
        actor->dispatch(actor, actor->state, actor->handler, message, &failed);
        ciel_resource_restore_current_owner(previous);
        ciel_runtime_leave_callback();
    }

    if (failed) {
        ciel_actor_lock(actor);
        actor->failed = 1;
        actor->closing = 1;
        ciel_actor_unlock(actor);
    }
    if (message != NULL)
        GC_FREE(message);
    job->value = NULL;
    dispatch_group_leave(actor->jobs);
    GC_FREE(job);
}

int32_t ciel_actor_spawn_with_owner(CielActor **out, void *state, void *handler,
                                    CielActorDispatchFn dispatch,
                                    CielResourceOwner *owner) {
    if (out == NULL || state == NULL || handler == NULL || dispatch == NULL ||
        owner == NULL)
        return EINVAL;
    ciel_runtime_init();
    CielActor *actor = (CielActor *)ciel_alloc_uncollectable(sizeof(CielActor));
    memset(actor, 0, sizeof(*actor));
    actor->owner = owner;
    actor->state = state;
    actor->handler = handler;
    actor->dispatch = dispatch;
    actor->queue = dispatch_queue_create("ciel.actor", DISPATCH_QUEUE_SERIAL);
    if (actor->queue == NULL) {
        (void)ciel_resource_owner_close(owner);
        return ENOMEM;
    }
    dispatch_queue_set_specific(actor->queue, &ciel_actor_queue_key, actor,
                                NULL);
    actor->jobs = dispatch_group_create();
    if (actor->jobs == NULL) {
        (void)ciel_resource_owner_close(owner);
        return ENOMEM;
    }
    actor->lifecycle_lock = dispatch_semaphore_create(1);
    if (actor->lifecycle_lock == NULL) {
        (void)ciel_resource_owner_close(owner);
        return ENOMEM;
    }
    *out = actor;
    return 0;
}

int32_t ciel_actor_spawn(CielActor **out, void *state, void *handler,
                         CielActorDispatchFn dispatch) {
    if (out == NULL || state == NULL || handler == NULL || dispatch == NULL)
        return EINVAL;
    int32_t owner_rc = 0;
    CielResourceOwner *owner = ciel_resource_owner_new_child(
        ciel_resource_current_owner_or_root(), ciel_resource_default_limits(),
        &owner_rc);
    if (owner == NULL)
        return owner_rc == 0 ? ENOMEM : owner_rc;
    int32_t detach_rc = ciel_resource_owner_detach(owner);
    if (detach_rc != 0) {
        (void)ciel_resource_owner_close(owner);
        return detach_rc;
    }
    return ciel_actor_spawn_with_owner(out, state, handler, dispatch, owner);
}

int32_t ciel_actor_send(CielActor *actor, void *message) {
    if (actor == NULL || message == NULL)
        return EINVAL;
    CielActorJob *job =
        (CielActorJob *)ciel_alloc_uncollectable(sizeof(CielActorJob));
    job->actor = actor;
    job->value = message;
    ciel_actor_lock(actor);
    if (actor->closing) {
        ciel_actor_unlock(actor);
        GC_FREE(message);
        GC_FREE(job);
        return EPIPE;
    }
    dispatch_group_enter(actor->jobs);
    ciel_actor_unlock(actor);
    dispatch_async_f(actor->queue, job, ciel_actor_job_run);
    return 0;
}

int32_t ciel_actor_stop(CielActor *actor) {
    if (actor == NULL)
        return EINVAL;
    ciel_actor_lock(actor);
    actor->closing = 1;
    ciel_actor_unlock(actor);
    return 0;
}

int32_t ciel_actor_join(CielActor *actor) {
    if (actor == NULL)
        return EINVAL;
    if (dispatch_get_specific(&ciel_actor_queue_key) == actor)
        return EDEADLK;
    ciel_actor_lock(actor);
    if (actor->joined) {
        int result = actor->join_result;
        ciel_actor_unlock(actor);
        return result;
    }
    actor->closing = 1;
    ciel_actor_unlock(actor);

    dispatch_group_wait(actor->jobs, DISPATCH_TIME_FOREVER);
    CielResourceOwner *owner = NULL;
    ciel_actor_lock(actor);
    if (!actor->joined) {
        actor->joined = 1;
        actor->join_result = actor->failed ? EIO : 0;
        owner = actor->owner;
        actor->owner = NULL;
    }
    int result = actor->join_result;
    ciel_actor_unlock(actor);
    if (owner != NULL) {
        int32_t close_rc = ciel_resource_owner_close(owner);
        if (result == 0 && close_rc != 0)
            result = close_rc;
    }
    return result;
}
