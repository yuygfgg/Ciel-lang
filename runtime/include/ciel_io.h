#ifndef CIEL_IO_H
#define CIEL_IO_H

#include "ciel_base.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct CielBytes CielBytes;

int ciel_io_open_read(const char* path);
int ciel_io_open_write(const char* path);
int ciel_io_open_append(const char* path);

int32_t ciel_file_open(int32_t mode, const char* path, uint64_t* out_owner,
                       uint64_t* out_resource, uint64_t* out_generation);
int32_t ciel_file_close(uint64_t owner, uint64_t resource, uint64_t generation);
ssize_t ciel_file_read(uint64_t owner, uint64_t resource, uint64_t generation,
                       void* buf, size_t count);
ssize_t ciel_file_write(uint64_t owner, uint64_t resource, uint64_t generation,
                        const void* buf, size_t count);
int32_t ciel_file_stdout(uint64_t* out_owner, uint64_t* out_resource,
                         uint64_t* out_generation);
int32_t ciel_file_stderr(uint64_t* out_owner, uint64_t* out_resource,
                         uint64_t* out_generation);

CielBytes* ciel_bytes_copy(const uint8_t* ptr, size_t len);
CielBytes* ciel_bytes_copy_chars(const char* ptr, size_t len);
CielBytes* ciel_bytes_concat(const uint8_t* left, size_t left_len,
                             const uint8_t* right, size_t right_len);
CielBytes* ciel_bytes_prepend(const uint8_t* prefix, size_t prefix_len,
                              CielBytes* bytes);
CielBytes* ciel_bytes_slice(CielBytes* bytes, size_t offset, size_t len);
size_t ciel_bytes_len(CielBytes* bytes);
size_t ciel_bytes_capacity(CielBytes* bytes);
int32_t ciel_bytes_copy_to(CielBytes* bytes, uint8_t* out, size_t cap,
                           size_t* copied);
int32_t ciel_bytes_copy_to_chars(CielBytes* bytes, char* out, size_t cap,
                                 size_t* copied);

#ifdef __cplusplus
}
#endif

#endif
