#ifndef CIEL_ACTOR_H
#define CIEL_ACTOR_H

#include "ciel_base.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct CielActor CielActor;

typedef void (*CielActorDispatchFn)(CielActor* actor, void* state,
                                    void* handler, void* message,
                                    int32_t* failed);

int32_t ciel_actor_spawn(CielActor** out, void* state, void* handler,
                         CielActorDispatchFn dispatch);
int32_t ciel_actor_send(CielActor* actor, void* message);
int32_t ciel_actor_stop(CielActor* actor);
int32_t ciel_actor_join(CielActor* actor);

#ifdef __cplusplus
}
#endif

#endif
