#ifndef CIEL_RESOURCE_H
#define CIEL_RESOURCE_H

#include "ciel_base.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct CielResourceOwner CielResourceOwner;
typedef struct CielAsyncFd CielAsyncFd;
typedef struct CielAsyncOp CielAsyncOp;
typedef struct CielAsyncTcpListener CielAsyncTcpListener;
typedef int32_t (*CielResourceCloseFn)(void* ptr);

typedef struct CielResourceHandle {
    uint64_t owner_id;
    uint64_t resource_id;
    uint64_t generation;
} CielResourceHandle;

typedef struct CielResourceLimits {
    size_t max_resources;
    size_t max_child_owners;
    size_t max_pending_ops;
    size_t max_descriptors;
} CielResourceLimits;

typedef enum CielResourceKind {
    CIEL_RESOURCE_KIND_FILE = 1,
    CIEL_RESOURCE_KIND_TCP_LISTENER = 2,
    CIEL_RESOURCE_KIND_TCP_STREAM = 3,
    CIEL_RESOURCE_KIND_ASYNC_FD = 4,
    CIEL_RESOURCE_KIND_ASYNC_TCP_LISTENER = 5,
    CIEL_RESOURCE_KIND_ASYNC_OP = 6,
    CIEL_RESOURCE_KIND_NATIVE = 7,
} CielResourceKind;

CielResourceLimits ciel_resource_default_limits(void);
int32_t ciel_resource_scope_push_default(void);
int32_t ciel_resource_scope_push_limits(CielResourceLimits limits);
int32_t ciel_resource_scope_push_limits_raw(size_t max_resources,
                                            size_t max_child_owners,
                                            size_t max_pending_ops,
                                            size_t max_descriptors);
int32_t ciel_resource_scope_close_current(void);
int32_t ciel_resource_owner_enter_child_limits_raw(
    size_t max_resources, size_t max_child_owners, size_t max_pending_ops,
    size_t max_descriptors, uint64_t* out_owner_id,
    uint64_t* out_previous_owner_id);
int32_t ciel_resource_restore_owner(uint64_t owner_id);
int32_t ciel_resource_owner_close_id(uint64_t owner_id);

int32_t ciel_resource_register_fd(CielResourceKind kind, int fd, int borrowed,
                                  CielResourceHandle* out);
int32_t ciel_resource_close(CielResourceHandle handle);
int32_t ciel_resource_close_handle(uint64_t owner_id, uint64_t resource_id,
                                   uint64_t generation);
int32_t ciel_resource_transfer_to_parent(CielResourceHandle handle,
                                         CielResourceHandle* out);
int32_t ciel_resource_transfer_to_parent_handle(uint64_t owner_id,
                                                uint64_t resource_id,
                                                uint64_t generation,
                                                uint64_t* out_owner_id,
                                                uint64_t* out_resource_id,
                                                uint64_t* out_generation);
int32_t ciel_resource_transfer_to_current(CielResourceHandle handle,
                                          CielResourceHandle* out);
int32_t ciel_resource_transfer_to_current_handle(uint64_t owner_id,
                                                 uint64_t resource_id,
                                                 uint64_t generation,
                                                 uint64_t* out_owner_id,
                                                 uint64_t* out_resource_id,
                                                 uint64_t* out_generation);
int32_t ciel_resource_fd_snapshot(CielResourceHandle handle,
                                  CielResourceKind kind, int* out_fd);
int32_t ciel_resource_register_async_fd(CielAsyncFd* fd,
                                        CielResourceHandle* out);
int32_t ciel_resource_register_async_fd_handle(CielAsyncFd* fd,
                                               uint64_t* out_owner_id,
                                               uint64_t* out_resource_id,
                                               uint64_t* out_generation);
int32_t ciel_resource_async_fd_snapshot(CielResourceHandle handle,
                                        CielAsyncFd** out);
int32_t ciel_resource_async_fd_snapshot_handle(uint64_t owner_id,
                                               uint64_t resource_id,
                                               uint64_t generation,
                                               CielAsyncFd** out);
int32_t ciel_resource_register_async_listener(CielAsyncTcpListener* listener,
                                              CielResourceHandle* out);
int32_t ciel_resource_register_async_listener_handle(
    CielAsyncTcpListener* listener, uint64_t* out_owner_id,
    uint64_t* out_resource_id, uint64_t* out_generation);
int32_t ciel_resource_async_listener_snapshot(CielResourceHandle handle,
                                              CielAsyncTcpListener** out);
int32_t ciel_resource_async_listener_snapshot_handle(
    uint64_t owner_id, uint64_t resource_id, uint64_t generation,
    CielAsyncTcpListener** out);
int32_t ciel_resource_register_async_op(CielAsyncOp* op,
                                        CielResourceHandle* out);
int32_t ciel_resource_register_async_op_handle(CielAsyncOp* op,
                                               uint64_t* out_owner_id,
                                               uint64_t* out_resource_id,
                                               uint64_t* out_generation);
int32_t ciel_resource_async_op_snapshot(CielResourceHandle handle,
                                        CielAsyncOp** out);
int32_t ciel_resource_async_op_snapshot_handle(uint64_t owner_id,
                                               uint64_t resource_id,
                                               uint64_t generation,
                                               CielAsyncOp** out);
int32_t ciel_resource_register_native(void* ptr, CielResourceCloseFn close_fn,
                                      const void* native_type,
                                      CielResourceHandle* out);
int32_t ciel_resource_native_snapshot(CielResourceHandle handle,
                                      const void* native_type, void** out);

CielResourceOwner* ciel_resource_current_owner(void);
CielResourceOwner* ciel_resource_current_owner_or_root(void);
CielResourceOwner* ciel_resource_owner_new_child(CielResourceOwner* parent,
                                                 CielResourceLimits limits,
                                                 int32_t* out_rc);
CielResourceOwner* ciel_resource_set_current_owner(CielResourceOwner* owner);
void ciel_resource_restore_current_owner(CielResourceOwner* previous);
int32_t ciel_resource_owner_detach(CielResourceOwner* owner);
int32_t ciel_resource_owner_close(CielResourceOwner* owner);

#ifdef __cplusplus
}
#endif

#endif
