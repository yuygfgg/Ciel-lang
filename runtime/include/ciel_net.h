#ifndef CIEL_NET_H
#define CIEL_NET_H

#include "ciel_base.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct CielSocketAddr CielSocketAddr;

int32_t ciel_net_parse_addr(const char* text, size_t text_len,
                            CielSocketAddr** out);
int32_t ciel_net_resolve_tcp(const char* host, size_t host_len, uint16_t port,
                             CielSocketAddr** out);
int32_t ciel_net_addr_family(CielSocketAddr* addr, int32_t* out);
int32_t ciel_net_addr_port(CielSocketAddr* addr, uint16_t* out);
int32_t ciel_net_addr_write(CielSocketAddr* addr, char* out, size_t cap,
                            size_t* written);
int32_t ciel_net_tcp_listen(CielSocketAddr* addr, uint32_t* out_slot,
                            uint32_t* out_generation);
int32_t ciel_net_tcp_accept(uint32_t listener_slot,
                            uint32_t listener_generation, uint32_t* out_slot,
                            uint32_t* out_generation);
int32_t ciel_net_tcp_connect(CielSocketAddr* addr, uint32_t* out_slot,
                             uint32_t* out_generation);
int32_t ciel_net_tcp_connect_host(const char* host, size_t host_len,
                                  uint16_t port, uint32_t* out_slot,
                                  uint32_t* out_generation);
intptr_t ciel_net_tcp_read(uint32_t stream_slot, uint32_t stream_generation,
                           void* buf, size_t count);
intptr_t ciel_net_tcp_write(uint32_t stream_slot, uint32_t stream_generation,
                            const void* buf, size_t count);
int32_t ciel_net_tcp_shutdown_read(uint32_t stream_slot,
                                   uint32_t stream_generation);
int32_t ciel_net_tcp_shutdown_write(uint32_t stream_slot,
                                    uint32_t stream_generation);
int32_t ciel_net_tcp_shutdown(uint32_t stream_slot, uint32_t stream_generation);
int32_t ciel_net_tcp_close(uint32_t stream_slot, uint32_t stream_generation);
int32_t ciel_net_listener_close(uint32_t listener_slot,
                                uint32_t listener_generation);
int32_t ciel_net_listener_addr(uint32_t listener_slot,
                               uint32_t listener_generation,
                               CielSocketAddr** out);
int32_t ciel_net_stream_local_addr(uint32_t stream_slot,
                                   uint32_t stream_generation,
                                   CielSocketAddr** out);
int32_t ciel_net_stream_peer_addr(uint32_t stream_slot,
                                  uint32_t stream_generation,
                                  CielSocketAddr** out);

#ifdef __cplusplus
}
#endif

#endif
