#ifndef CIEL_RUNTIME_INTERNAL_H
#define CIEL_RUNTIME_INTERNAL_H

#if !defined(GC_THREADS)
#define GC_THREADS 1
#endif
#if !defined(GC_NO_THREAD_REDIRECTS)
#define GC_NO_THREAD_REDIRECTS 1
#endif

#include "ciel_runtime.h"

#include <errno.h>
#include <gc/gc.h>
#include <limits.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include <arpa/inet.h>
#include <dispatch/dispatch.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <sys/socket.h>
#include <unistd.h>

#define CIEL_TCP_LISTEN_BACKLOG 16384

struct CielRoot {
    void* ptr;
    struct CielRoot* next;
};

struct CielSocketAddr {
    struct sockaddr_storage storage;
    socklen_t len;
};

CIEL_MALLOC_LIKE CIEL_ALLOC_SIZE1 CIEL_RETURNS_NONNULL void*
ciel_alloc_uncollectable(size_t size);
int32_t ciel_thread_attach_persistent(void);
int ciel_file_open_mode_flags(int32_t mode);
CielSocketAddr* ciel_net_addr_from_fd(int fd, int peer, int32_t* out_rc);
int ciel_net_make_socket(const struct sockaddr* addr);
int32_t ciel_fd_set_nonblocking(int fd);
void ciel_resource_runtime_init(void);
void ciel_resource_close_root_at_shutdown(void);

#endif
