#ifndef CIEL_ASYNC_H
#define CIEL_ASYNC_H

#include "ciel_actor.h"
#include "ciel_io.h"
#include "ciel_net.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct CielAsyncChannel CielAsyncChannel;
typedef struct CielAsyncSender CielAsyncSender;
typedef struct CielAsyncReceiver CielAsyncReceiver;
typedef struct CielAsyncSendPermit CielAsyncSendPermit;
typedef struct CielAsyncFd CielAsyncFd;
typedef struct CielAsyncOp CielAsyncOp;
typedef struct CielAsyncTcpListener CielAsyncTcpListener;
typedef struct CielFuture CielFuture;
typedef struct CielTaskGroup CielTaskGroup;
typedef struct CielSelectSet CielSelectSet;
typedef struct CielBufferedReader CielBufferedReader;

typedef struct CielSelectResult {
    size_t index;
} CielSelectResult;

typedef int32_t (*CielFutureRunFn)(CielFuture* future, void* ctx, void* out);
typedef void (*CielFutureCleanupFn)(CielFuture* future, void* ctx,
                                    int32_t reason);

int ciel_async_timeout_errno(void);
int ciel_async_channel_closed_errno(void);
int ciel_async_again_errno(void);

int32_t ciel_async_channel_make(size_t value_size, size_t value_align,
                                size_t capacity, CielAsyncSender** sender_out,
                                CielAsyncReceiver** receiver_out);
CielAsyncSender* ciel_async_sender_clone(CielAsyncSender* sender);
CielAsyncReceiver* ciel_async_receiver_clone(CielAsyncReceiver* receiver);
int32_t ciel_async_sender_close(CielAsyncSender* sender);
int32_t ciel_async_receiver_close(CielAsyncReceiver* receiver);
int32_t ciel_async_channel_try_send(CielAsyncSender* sender, const void* value);
int32_t ciel_async_send_permit_send(CielAsyncSendPermit* permit,
                                    const void* value);
int32_t ciel_async_send_permit_release(CielAsyncSendPermit* permit);
int32_t ciel_async_channel_send_poll(CielFuture* future,
                                     CielAsyncSender* sender,
                                     const void* value);
int32_t ciel_async_channel_reserve_poll(CielFuture* future,
                                        CielAsyncSender* sender,
                                        CielAsyncSendPermit** permit_out);
int32_t ciel_async_channel_recv_poll(CielFuture* future,
                                     CielAsyncReceiver* receiver, void* out);
int32_t ciel_future_await_channel_send(CielFuture* future,
                                       CielAsyncSender* sender,
                                       const void* value);
int32_t ciel_future_await_channel_reserve(CielFuture* future,
                                          CielAsyncSender* sender,
                                          CielAsyncSendPermit** permit_out);
int32_t ciel_future_await_channel_recv(CielFuture* future,
                                       CielAsyncReceiver* receiver, void* out);

CielAsyncFd* ciel_async_open(int32_t mode, const char* path);
CielAsyncFd* ciel_async_from_raw_fd(int32_t raw);
int32_t ciel_async_fd_retain(CielAsyncFd* fd);
int32_t ciel_async_close(CielAsyncFd* fd);
CielAsyncOp* ciel_async_read_bytes(CielAsyncFd* fd, size_t max_len);
CielAsyncOp* ciel_async_write_bytes(CielAsyncFd* fd, const uint8_t* data,
                                    size_t len);
CielAsyncOp* ciel_async_tcp_read_bytes(CielAsyncFd* fd, size_t max_len);
CielAsyncOp* ciel_async_tcp_read_into(CielAsyncFd* fd, uint8_t* data,
                                      size_t cap);
CielAsyncOp* ciel_async_tcp_write_bytes(CielAsyncFd* fd, const uint8_t* data,
                                        size_t len);
CielBufferedReader* ciel_async_tcp_buffered_reader_new(CielAsyncFd* fd,
                                                       size_t capacity);
CielAsyncFd*
ciel_async_tcp_buffered_reader_into_read_half(CielBufferedReader* reader);
CielAsyncOp* ciel_async_tcp_read_buffered(CielBufferedReader* reader,
                                          size_t max_len);
CielAsyncOp* ciel_async_tcp_read_exact_buffered(CielBufferedReader* reader,
                                                size_t len);
CielAsyncOp* ciel_async_sleep_ms(uint64_t ms);
CielAsyncOp* ciel_async_tcp_accept(CielAsyncTcpListener* listener);
CielAsyncOp* ciel_async_tcp_connect(CielSocketAddr* addr);
int32_t ciel_async_notify_read(CielAsyncOp* op, CielActor* actor,
                               void* message);
int32_t ciel_async_notify_write(CielAsyncOp* op, CielActor* actor,
                                void* message);
int32_t ciel_async_notify_sleep(CielAsyncOp* op, CielActor* actor,
                                void* message);
int32_t ciel_async_finish_read(CielAsyncOp* op, uint8_t** out, size_t* len,
                               size_t* cap);
int32_t ciel_async_finish_write(CielAsyncOp* op, size_t* written);
int32_t ciel_async_finish_sleep(CielAsyncOp* op);
int32_t ciel_async_cancel(CielAsyncOp* op);

CielAsyncTcpListener* ciel_async_tcp_listen(CielSocketAddr* addr);
int32_t ciel_async_tcp_listener_addr(CielAsyncTcpListener* listener,
                                     CielSocketAddr** out);
int32_t ciel_async_tcp_close_listener(CielAsyncTcpListener* listener);
int32_t ciel_async_tcp_finish_accept(CielAsyncOp* op, CielAsyncFd** out);
int32_t ciel_async_tcp_finish_connect(CielAsyncOp* op, CielAsyncFd** out);
int32_t ciel_async_tcp_notify_accept(CielAsyncOp* op, CielActor* actor,
                                     void* message);
int32_t ciel_async_tcp_notify_connect(CielAsyncOp* op, CielActor* actor,
                                      void* message);
int32_t ciel_async_tcp_stream_local_addr(CielAsyncFd* stream,
                                         CielSocketAddr** out);
int32_t ciel_async_tcp_stream_peer_addr(CielAsyncFd* stream,
                                        CielSocketAddr** out);
int32_t ciel_async_tcp_shutdown_read(CielAsyncFd* stream);
int32_t ciel_async_tcp_shutdown_write(CielAsyncFd* stream);

CielFuture* ciel_future_new(size_t result_size, size_t result_align,
                            CielFutureRunFn run, void* ctx,
                            CielFutureCleanupFn cleanup);
CielFuture* ciel_future_from_handle(void* handle);
CielFuture* ciel_task_future_from_handle(void* handle);
int32_t ciel_future_cancel(CielFuture* future);
int32_t ciel_future_abort(CielFuture* future);
void ciel_future_bind_operation(CielFuture* future, CielAsyncOp* op);
void ciel_future_clear_operation(CielFuture* future, CielAsyncOp* op);
int32_t ciel_future_run_to_completion(CielFuture* future, void* out);
int32_t ciel_future_poll(CielFuture* future, void* out);
void ciel_future_adopt_pending_operation(CielFuture* future, CielFuture* child);
void ciel_future_clear_pending_operation(CielFuture* future);
int32_t ciel_future_poll_trampoline(CielFuture* future, void* out);
int32_t ciel_future_run_to_completion_trampoline(CielFuture* future, void* out);
void* ciel_task_spawn(CielFuture* future);
int32_t ciel_task_cancel(void* handle);
int32_t ciel_task_is_finished(void* handle, bool* out);
CielTaskGroup* ciel_task_group_new(void);
int32_t ciel_task_group_add(CielTaskGroup* group, void* task_handle);
int32_t ciel_task_group_next_task_poll(CielFuture* future, CielTaskGroup* group,
                                       void** out_task);
int32_t ciel_task_group_cancel_all(CielTaskGroup* group);
int32_t ciel_task_group_close(CielTaskGroup* group);
CielSelectSet* ciel_select_set_new(size_t capacity, int biased);
int32_t ciel_select_set_push(CielSelectSet* set, CielFuture* future,
                             size_t result_size, size_t result_align);
CielFuture* ciel_select_future_new(CielSelectSet* set);
CielSelectSet* ciel_select_future_set(CielFuture* future);
void* ciel_select_winner_value(CielSelectSet* set, size_t index);
int32_t ciel_future_await_sleep_ms(CielFuture* future, CielAsyncOp** slot,
                                   uint64_t ms);

#ifdef __cplusplus
}
#endif

#endif
