#include "internal.h"

typedef enum {
    CIEL_NET_SLOT_FREE = 0,
    CIEL_NET_SLOT_LISTENER = 1,
    CIEL_NET_SLOT_STREAM = 2,
} CielNetSlotKind;

typedef struct CielNetSlot {
    int fd;
    uint32_t generation;
    CielNetSlotKind kind;
    uint32_t next_free;
} CielNetSlot;

static pthread_mutex_t ciel_net_table_mutex = PTHREAD_MUTEX_INITIALIZER;
static CielNetSlot *ciel_net_slots = NULL;
static uint32_t ciel_net_slot_count = 0;
static uint32_t ciel_net_slot_cap = 0;
static uint32_t ciel_net_free_head = UINT32_MAX;

static int32_t ciel_net_table_grow(void) {
    uint32_t old_cap = ciel_net_slot_cap;
    uint32_t new_cap = old_cap == 0 ? 16 : old_cap * 2;
    CielNetSlot *next = (CielNetSlot *)GC_MALLOC_UNCOLLECTABLE(
        sizeof(CielNetSlot) * (size_t)new_cap);
    if (next == NULL)
        return ENOMEM;
    memset(next, 0, sizeof(CielNetSlot) * (size_t)new_cap);
    if (ciel_net_slots != NULL) {
        memcpy(next, ciel_net_slots, sizeof(CielNetSlot) * (size_t)old_cap);
        GC_FREE(ciel_net_slots);
    }
    ciel_net_slots = next;
    ciel_net_slot_cap = new_cap;
    return 0;
}

static int32_t ciel_net_slot_insert_locked(int fd, CielNetSlotKind kind,
                                           uint32_t *out_slot,
                                           uint32_t *out_generation) {
    uint32_t slot;
    if (ciel_net_free_head != UINT32_MAX) {
        slot = ciel_net_free_head;
        ciel_net_free_head = ciel_net_slots[slot].next_free;
    } else {
        if (ciel_net_slot_count == ciel_net_slot_cap) {
            int32_t rc = ciel_net_table_grow();
            if (rc != 0)
                return rc;
        }
        slot = ciel_net_slot_count++;
        if (ciel_net_slots[slot].generation == 0)
            ciel_net_slots[slot].generation = 1;
    }
    ciel_net_slots[slot].fd = fd;
    ciel_net_slots[slot].kind = kind;
    ciel_net_slots[slot].next_free = UINT32_MAX;
    *out_slot = slot;
    *out_generation = ciel_net_slots[slot].generation;
    return 0;
}

static int32_t ciel_net_resolve_locked(uint32_t slot, uint32_t generation,
                                       CielNetSlotKind kind,
                                       CielNetSlot **out) {
    if (out == NULL)
        return EINVAL;
    if (slot >= ciel_net_slot_count)
        return EBADF;
    CielNetSlot *entry = &ciel_net_slots[slot];
    if (entry->generation != generation || entry->kind != kind)
        return EBADF;
    *out = entry;
    return 0;
}

static int32_t ciel_net_fd_snapshot(uint32_t slot, uint32_t generation,
                                    CielNetSlotKind kind, int *fd) {
    if (fd == NULL)
        return EINVAL;
    pthread_mutex_lock(&ciel_net_table_mutex);
    CielNetSlot *entry = NULL;
    int32_t rc = ciel_net_resolve_locked(slot, generation, kind, &entry);
    if (rc == 0)
        *fd = entry->fd;
    pthread_mutex_unlock(&ciel_net_table_mutex);
    return rc;
}

static int32_t ciel_net_slot_close(uint32_t slot, uint32_t generation,
                                   CielNetSlotKind kind) {
    pthread_mutex_lock(&ciel_net_table_mutex);
    CielNetSlot *entry = NULL;
    int32_t rc = ciel_net_resolve_locked(slot, generation, kind, &entry);
    if (rc != 0) {
        pthread_mutex_unlock(&ciel_net_table_mutex);
        return rc;
    }
    int fd = entry->fd;
    entry->fd = -1;
    entry->kind = CIEL_NET_SLOT_FREE;
    entry->generation =
        entry->generation == UINT32_MAX ? 1 : entry->generation + 1;
    entry->next_free = ciel_net_free_head;
    ciel_net_free_head = slot;
    pthread_mutex_unlock(&ciel_net_table_mutex);
    if (close(fd) != 0)
        return errno == 0 ? EIO : errno;
    return 0;
}

static int32_t ciel_net_gai_error(int rc) {
    switch (rc) {
    case 0:
        return 0;
    case EAI_MEMORY:
        return ENOMEM;
    case EAI_FAMILY:
        return EAFNOSUPPORT;
    case EAI_NONAME:
        return ENOENT;
    case EAI_SERVICE:
        return EINVAL;
    case EAI_AGAIN:
        return EAGAIN;
    default:
        return EIO;
    }
}

static CielSocketAddr *ciel_net_addr_copy(const struct sockaddr *addr,
                                          socklen_t len) {
    if (addr == NULL || len == 0 || len > sizeof(struct sockaddr_storage)) {
        errno = EINVAL;
        return NULL;
    }
    CielSocketAddr *out =
        (CielSocketAddr *)ciel_alloc_uncollectable(sizeof(CielSocketAddr));
    memcpy(&out->storage, addr, len);
    out->len = len;
    return out;
}

CielSocketAddr *ciel_net_addr_from_fd(int fd, int peer, int32_t *out_rc) {
    struct sockaddr_storage storage;
    socklen_t len = (socklen_t)sizeof(storage);
    int rc = peer ? getpeername(fd, (struct sockaddr *)&storage, &len)
                  : getsockname(fd, (struct sockaddr *)&storage, &len);
    if (rc != 0) {
        if (out_rc != NULL)
            *out_rc = errno == 0 ? EIO : errno;
        return NULL;
    }
    CielSocketAddr *addr = ciel_net_addr_copy((struct sockaddr *)&storage, len);
    if (addr == NULL && out_rc != NULL)
        *out_rc = errno == 0 ? ENOMEM : errno;
    return addr;
}

int ciel_net_make_socket(const struct sockaddr *addr) {
    int fd = socket(addr->sa_family, SOCK_STREAM, 0);
    if (fd < 0)
        return -1;
#if defined(SO_NOSIGPIPE)
    int one = 1;
    (void)setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof(one));
#endif
    return fd;
}

int32_t ciel_fd_set_nonblocking(int fd) {
    int flags = fcntl(fd, F_GETFL, 0);
    if (flags < 0)
        return errno == 0 ? EIO : errno;
    if (fcntl(fd, F_SETFL, flags | O_NONBLOCK) != 0)
        return errno == 0 ? EIO : errno;
    return 0;
}

static int32_t ciel_net_parse_port(const char *text, size_t len,
                                   char *out_service, size_t out_service_cap) {
    if (text == NULL || len == 0 || out_service == NULL ||
        out_service_cap == 0 || len >= out_service_cap)
        return EINVAL;
    uint32_t value = 0;
    for (size_t i = 0; i < len; i++) {
        unsigned char ch = (unsigned char)text[i];
        if (ch < '0' || ch > '9')
            return EINVAL;
        value = value * 10U + (uint32_t)(ch - '0');
        if (value > 65535U)
            return ERANGE;
    }
    memcpy(out_service, text, len);
    out_service[len] = '\0';
    return 0;
}

static int32_t ciel_net_parse_endpoint(const char *text, size_t text_len,
                                       char **out_host, char *out_service,
                                       size_t out_service_cap) {
    if (text == NULL || text_len == 0 || out_host == NULL ||
        out_service == NULL)
        return EINVAL;

    size_t host_start = 0;
    size_t host_len = 0;
    size_t port_start = 0;
    size_t port_len = 0;

    if (text[0] == '[') {
        size_t close = 1;
        while (close < text_len && text[close] != ']')
            close++;
        if (close >= text_len || close + 1 >= text_len ||
            text[close + 1] != ':')
            return EINVAL;
        host_start = 1;
        host_len = close - 1;
        port_start = close + 2;
        port_len = text_len - port_start;
    } else {
        size_t colon = text_len;
        for (size_t i = 0; i < text_len; i++) {
            if (text[i] == ':') {
                if (colon != text_len)
                    return EINVAL;
                colon = i;
            }
        }
        if (colon == text_len)
            return EINVAL;
        host_start = 0;
        host_len = colon;
        port_start = colon + 1;
        port_len = text_len - port_start;
    }

    if (host_len == 0 || port_len == 0)
        return EINVAL;
    int32_t rc = ciel_net_parse_port(text + port_start, port_len, out_service,
                                     out_service_cap);
    if (rc != 0)
        return rc;

    char *host = (char *)ciel_alloc_atomic_array(sizeof(char), host_len + 1);
    memcpy(host, text + host_start, host_len);
    host[host_len] = '\0';
    *out_host = host;
    return 0;
}

static int32_t ciel_net_addr_from_name(const char *host, const char *service,
                                       int flags, CielSocketAddr **out) {
    if (host == NULL || service == NULL || out == NULL)
        return EINVAL;
    struct addrinfo hints;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    hints.ai_flags = flags;

    struct addrinfo *results = NULL;
    int rc = getaddrinfo(host, service, &hints, &results);
    if (rc != 0)
        return ciel_net_gai_error(rc);

    int32_t out_rc = ENOENT;
    for (struct addrinfo *it = results; it != NULL; it = it->ai_next) {
        if (it->ai_family != AF_INET && it->ai_family != AF_INET6)
            continue;
        CielSocketAddr *addr = ciel_net_addr_copy(it->ai_addr, it->ai_addrlen);
        if (addr == NULL) {
            out_rc = errno == 0 ? ENOMEM : errno;
            break;
        }
        *out = addr;
        out_rc = 0;
        break;
    }
    freeaddrinfo(results);
    return out_rc;
}

int32_t ciel_net_parse_addr(const char *text, size_t text_len,
                            CielSocketAddr **out) {
    char service[6];
    char *host = NULL;
    int32_t rc = ciel_net_parse_endpoint(text, text_len, &host, service,
                                         sizeof(service));
    if (rc != 0)
        return rc;
#if defined(AI_NUMERICSERV)
    int flags = AI_NUMERICHOST | AI_NUMERICSERV;
#else
    int flags = AI_NUMERICHOST;
#endif
    return ciel_net_addr_from_name(host, service, flags, out);
}

int32_t ciel_net_resolve_tcp(const char *host, size_t host_len, uint16_t port,
                             CielSocketAddr **out) {
    if (host == NULL || out == NULL)
        return EINVAL;
    char *host_c = ciel_cstr_from_slice(host, host_len);
    char service[6];
    int n = snprintf(service, sizeof(service), "%u", (unsigned)port);
    if (n < 0 || (size_t)n >= sizeof(service))
        return EOVERFLOW;
    return ciel_net_addr_from_name(host_c, service, 0, out);
}

int32_t ciel_net_addr_family(CielSocketAddr *addr, int32_t *out) {
    if (addr == NULL || out == NULL)
        return EINVAL;
    switch (((struct sockaddr *)&addr->storage)->sa_family) {
    case AF_INET:
        *out = 4;
        return 0;
    case AF_INET6:
        *out = 6;
        return 0;
    default:
        return EAFNOSUPPORT;
    }
}

int32_t ciel_net_addr_port(CielSocketAddr *addr, uint16_t *out) {
    if (addr == NULL || out == NULL)
        return EINVAL;
    struct sockaddr *sa = (struct sockaddr *)&addr->storage;
    if (sa->sa_family == AF_INET) {
        *out = ntohs(((struct sockaddr_in *)sa)->sin_port);
        return 0;
    }
    if (sa->sa_family == AF_INET6) {
        *out = ntohs(((struct sockaddr_in6 *)sa)->sin6_port);
        return 0;
    }
    return EAFNOSUPPORT;
}

int32_t ciel_net_addr_write(CielSocketAddr *addr, char *out, size_t cap,
                            size_t *written) {
    if (addr == NULL || written == NULL || (out == NULL && cap > 0))
        return EINVAL;
    char host[INET6_ADDRSTRLEN];
    uint16_t port = 0;
    struct sockaddr *sa = (struct sockaddr *)&addr->storage;
    const void *source = NULL;
    int family = sa->sa_family;
    if (family == AF_INET) {
        struct sockaddr_in *in = (struct sockaddr_in *)sa;
        source = &in->sin_addr;
        port = ntohs(in->sin_port);
    } else if (family == AF_INET6) {
        struct sockaddr_in6 *in6 = (struct sockaddr_in6 *)sa;
        source = &in6->sin6_addr;
        port = ntohs(in6->sin6_port);
    } else {
        return EAFNOSUPPORT;
    }
    if (inet_ntop(family, source, host, sizeof(host)) == NULL)
        return errno == 0 ? EIO : errno;

    char tmp[INET6_ADDRSTRLEN + 10];
    int n = 0;
    if (family == AF_INET6)
        n = snprintf(tmp, sizeof(tmp), "[%s]:%u", host, (unsigned)port);
    else
        n = snprintf(tmp, sizeof(tmp), "%s:%u", host, (unsigned)port);
    if (n < 0)
        return EIO;
    size_t need = (size_t)n;
    if (need >= sizeof(tmp))
        return EOVERFLOW;
    *written = need;
    if (cap <= need)
        return ENOSPC;
    memcpy(out, tmp, need);
    out[need] = '\0';
    return 0;
}

int32_t ciel_net_tcp_listen(CielSocketAddr *addr, uint32_t *out_slot,
                            uint32_t *out_generation) {
    if (addr == NULL || out_slot == NULL || out_generation == NULL)
        return EINVAL;
    struct sockaddr *sa = (struct sockaddr *)&addr->storage;
    int fd = ciel_net_make_socket(sa);
    if (fd < 0)
        return errno == 0 ? EIO : errno;
    int one = 1;
    (void)setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
    if (bind(fd, sa, addr->len) != 0) {
        int err = errno == 0 ? EIO : errno;
        close(fd);
        return err;
    }
    if (listen(fd, CIEL_TCP_LISTEN_BACKLOG) != 0) {
        int err = errno == 0 ? EIO : errno;
        close(fd);
        return err;
    }
    pthread_mutex_lock(&ciel_net_table_mutex);
    int32_t rc = ciel_net_slot_insert_locked(fd, CIEL_NET_SLOT_LISTENER,
                                             out_slot, out_generation);
    pthread_mutex_unlock(&ciel_net_table_mutex);
    if (rc != 0)
        close(fd);
    return rc;
}

int32_t ciel_net_tcp_accept(uint32_t listener_slot,
                            uint32_t listener_generation, uint32_t *out_slot,
                            uint32_t *out_generation) {
    if (out_slot == NULL || out_generation == NULL)
        return EINVAL;
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(listener_slot, listener_generation,
                                      CIEL_NET_SLOT_LISTENER, &fd);
    if (rc != 0)
        return rc;
    int accepted;
    do {
        accepted = accept(fd, NULL, NULL);
    } while (accepted < 0 && errno == EINTR);
    if (accepted < 0)
        return errno == 0 ? EIO : errno;
#if defined(SO_NOSIGPIPE)
    int one = 1;
    (void)setsockopt(accepted, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof(one));
#endif
    pthread_mutex_lock(&ciel_net_table_mutex);
    rc = ciel_net_slot_insert_locked(accepted, CIEL_NET_SLOT_STREAM, out_slot,
                                     out_generation);
    pthread_mutex_unlock(&ciel_net_table_mutex);
    if (rc != 0)
        close(accepted);
    return rc;
}

static int32_t ciel_net_connect_fd(const struct sockaddr *addr, socklen_t len,
                                   int *out_fd) {
    int fd = ciel_net_make_socket(addr);
    if (fd < 0)
        return errno == 0 ? EIO : errno;
    while (connect(fd, addr, len) != 0) {
        if (errno == EINTR)
            continue;
        int err = errno == 0 ? EIO : errno;
        close(fd);
        return err;
    }
    *out_fd = fd;
    return 0;
}

static int32_t ciel_net_insert_connected_fd(int fd, uint32_t *out_slot,
                                            uint32_t *out_generation) {
    pthread_mutex_lock(&ciel_net_table_mutex);
    int32_t rc = ciel_net_slot_insert_locked(fd, CIEL_NET_SLOT_STREAM, out_slot,
                                             out_generation);
    pthread_mutex_unlock(&ciel_net_table_mutex);
    if (rc != 0)
        close(fd);
    return rc;
}

int32_t ciel_net_tcp_connect(CielSocketAddr *addr, uint32_t *out_slot,
                             uint32_t *out_generation) {
    if (addr == NULL || out_slot == NULL || out_generation == NULL)
        return EINVAL;
    int fd = -1;
    int32_t rc =
        ciel_net_connect_fd((struct sockaddr *)&addr->storage, addr->len, &fd);
    if (rc != 0)
        return rc;
    return ciel_net_insert_connected_fd(fd, out_slot, out_generation);
}

int32_t ciel_net_tcp_connect_host(const char *host, size_t host_len,
                                  uint16_t port, uint32_t *out_slot,
                                  uint32_t *out_generation) {
    if (host == NULL || out_slot == NULL || out_generation == NULL)
        return EINVAL;
    char *host_c = ciel_cstr_from_slice(host, host_len);
    char service[6];
    int n = snprintf(service, sizeof(service), "%u", (unsigned)port);
    if (n < 0 || (size_t)n >= sizeof(service))
        return EOVERFLOW;

    struct addrinfo hints;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    struct addrinfo *results = NULL;
    int rc = getaddrinfo(host_c, service, &hints, &results);
    if (rc != 0)
        return ciel_net_gai_error(rc);

    int32_t last = ENOENT;
    for (struct addrinfo *it = results; it != NULL; it = it->ai_next) {
        if (it->ai_family != AF_INET && it->ai_family != AF_INET6)
            continue;
        int fd = -1;
        last = ciel_net_connect_fd(it->ai_addr, it->ai_addrlen, &fd);
        if (last == 0) {
            last = ciel_net_insert_connected_fd(fd, out_slot, out_generation);
            freeaddrinfo(results);
            return last;
        }
    }
    freeaddrinfo(results);
    return last;
}

intptr_t ciel_net_tcp_read(uint32_t stream_slot, uint32_t stream_generation,
                           void *buf, size_t count) {
    if (buf == NULL && count > 0) {
        errno = EINVAL;
        return -1;
    }
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(stream_slot, stream_generation,
                                      CIEL_NET_SLOT_STREAM, &fd);
    if (rc != 0) {
        errno = rc;
        return -1;
    }
    ssize_t n;
    do {
        n = recv(fd, buf, count, 0);
    } while (n < 0 && errno == EINTR);
    return (intptr_t)n;
}

intptr_t ciel_net_tcp_write(uint32_t stream_slot, uint32_t stream_generation,
                            const void *buf, size_t count) {
    if (buf == NULL && count > 0) {
        errno = EINVAL;
        return -1;
    }
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(stream_slot, stream_generation,
                                      CIEL_NET_SLOT_STREAM, &fd);
    if (rc != 0) {
        errno = rc;
        return -1;
    }
#if defined(MSG_NOSIGNAL)
    int flags = MSG_NOSIGNAL;
#else
    int flags = 0;
#endif
    ssize_t n;
    do {
        n = send(fd, buf, count, flags);
    } while (n < 0 && errno == EINTR);
    return (intptr_t)n;
}

int32_t ciel_net_tcp_shutdown_read(uint32_t stream_slot,
                                   uint32_t stream_generation) {
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(stream_slot, stream_generation,
                                      CIEL_NET_SLOT_STREAM, &fd);
    if (rc != 0)
        return rc;
    if (shutdown(fd, SHUT_RD) != 0)
        return errno == ENOTCONN ? 0 : (errno == 0 ? EIO : errno);
    return 0;
}

int32_t ciel_net_tcp_shutdown_write(uint32_t stream_slot,
                                    uint32_t stream_generation) {
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(stream_slot, stream_generation,
                                      CIEL_NET_SLOT_STREAM, &fd);
    if (rc != 0)
        return rc;
    if (shutdown(fd, SHUT_WR) != 0)
        return errno == ENOTCONN ? 0 : (errno == 0 ? EIO : errno);
    return 0;
}

int32_t ciel_net_tcp_shutdown(uint32_t stream_slot,
                              uint32_t stream_generation) {
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(stream_slot, stream_generation,
                                      CIEL_NET_SLOT_STREAM, &fd);
    if (rc != 0)
        return rc;
    if (shutdown(fd, SHUT_RDWR) != 0)
        return errno == ENOTCONN ? 0 : (errno == 0 ? EIO : errno);
    return 0;
}

int32_t ciel_net_tcp_close(uint32_t stream_slot, uint32_t stream_generation) {
    return ciel_net_slot_close(stream_slot, stream_generation,
                               CIEL_NET_SLOT_STREAM);
}

int32_t ciel_net_listener_close(uint32_t listener_slot,
                                uint32_t listener_generation) {
    return ciel_net_slot_close(listener_slot, listener_generation,
                               CIEL_NET_SLOT_LISTENER);
}

int32_t ciel_net_listener_addr(uint32_t listener_slot,
                               uint32_t listener_generation,
                               CielSocketAddr **out) {
    if (out == NULL)
        return EINVAL;
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(listener_slot, listener_generation,
                                      CIEL_NET_SLOT_LISTENER, &fd);
    if (rc != 0)
        return rc;
    CielSocketAddr *addr = ciel_net_addr_from_fd(fd, 0, &rc);
    if (addr == NULL)
        return rc == 0 ? EIO : rc;
    *out = addr;
    return 0;
}

int32_t ciel_net_stream_local_addr(uint32_t stream_slot,
                                   uint32_t stream_generation,
                                   CielSocketAddr **out) {
    if (out == NULL)
        return EINVAL;
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(stream_slot, stream_generation,
                                      CIEL_NET_SLOT_STREAM, &fd);
    if (rc != 0)
        return rc;
    CielSocketAddr *addr = ciel_net_addr_from_fd(fd, 0, &rc);
    if (addr == NULL)
        return rc == 0 ? EIO : rc;
    *out = addr;
    return 0;
}

int32_t ciel_net_stream_peer_addr(uint32_t stream_slot,
                                  uint32_t stream_generation,
                                  CielSocketAddr **out) {
    if (out == NULL)
        return EINVAL;
    int fd = -1;
    int32_t rc = ciel_net_fd_snapshot(stream_slot, stream_generation,
                                      CIEL_NET_SLOT_STREAM, &fd);
    if (rc != 0)
        return rc;
    CielSocketAddr *addr = ciel_net_addr_from_fd(fd, 1, &rc);
    if (addr == NULL)
        return rc == 0 ? EIO : rc;
    *out = addr;
    return 0;
}
