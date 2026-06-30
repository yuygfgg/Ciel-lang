#ifndef CIEL_IO_H
#define CIEL_IO_H

#include "ciel_base.h"

#ifdef __cplusplus
extern "C" {
#endif

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

#ifdef __cplusplus
}
#endif

#endif
