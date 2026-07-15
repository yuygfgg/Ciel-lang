#ifndef CIEL_CORE_H
#define CIEL_CORE_H

#include "ciel_base.h"

#ifdef __cplusplus
extern "C" {
#endif

void ciel_runtime_init(void);
void ciel_runtime_set_args(int argc, char** argv);
int ciel_env_args_len(size_t* out);
CielConstSlice_char ciel_env_arg_unchecked(size_t index);

CIEL_COLD CIEL_NORETURN void ciel_panic_at(const char* message, size_t len,
                                           const char* file, size_t line);
CIEL_COLD CIEL_NORETURN void ciel_panic(const char* message, size_t len);
int ciel_errno(void);
CIEL_MALLOC_LIKE CIEL_RETURNS_NONNULL char*
ciel_cstr_from_slice(const char* ptr, size_t len);
CielConstSlice_char ciel_diagnostic_text_copy(const char* ptr, size_t len);
size_t ciel_f32_to_string(float value, char* out, size_t cap);
size_t ciel_f64_to_string(double value, char* out, size_t cap);
int32_t ciel_parse_f32(const char* text, size_t len, float* out,
                       size_t* out_end, int32_t* out_range);
int32_t ciel_parse_f64(const char* text, size_t len, double* out,
                       size_t* out_end, int32_t* out_range);
int32_t ciel_time_monotonic_ms(uint64_t* out);
int32_t ciel_time_sleep_ms(uint64_t ms);

#ifdef __cplusplus
}
#endif

#endif
