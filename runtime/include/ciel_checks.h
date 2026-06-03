#ifndef CIEL_CHECKS_H
#define CIEL_CHECKS_H

#include "ciel_base.h"

#ifdef __cplusplus
extern "C" {
#endif

#define CIEL_DECLARE_SIGNED_CHECKS(SUFFIX, C_TY)                               \
    C_TY ciel_add_##SUFFIX(C_TY lhs, C_TY rhs, char* file, size_t line);       \
    C_TY ciel_sub_##SUFFIX(C_TY lhs, C_TY rhs, char* file, size_t line);       \
    C_TY ciel_mul_##SUFFIX(C_TY lhs, C_TY rhs, char* file, size_t line);       \
    C_TY ciel_neg_##SUFFIX(C_TY value, char* file, size_t line);               \
    C_TY ciel_div_##SUFFIX(C_TY lhs, C_TY rhs, char* file, size_t line);       \
    C_TY ciel_rem_##SUFFIX(C_TY lhs, C_TY rhs, char* file, size_t line);       \
    C_TY ciel_shl_##SUFFIX(C_TY lhs, uintmax_t rhs, char* file, size_t line);  \
    C_TY ciel_shr_##SUFFIX(C_TY lhs, uintmax_t rhs, char* file, size_t line)

#define CIEL_DECLARE_UNSIGNED_CHECKS(SUFFIX, C_TY)                             \
    C_TY ciel_add_##SUFFIX(C_TY lhs, C_TY rhs, char* file, size_t line);       \
    C_TY ciel_sub_##SUFFIX(C_TY lhs, C_TY rhs, char* file, size_t line);       \
    C_TY ciel_mul_##SUFFIX(C_TY lhs, C_TY rhs, char* file, size_t line);       \
    C_TY ciel_div_##SUFFIX(C_TY lhs, C_TY rhs, char* file, size_t line);       \
    C_TY ciel_rem_##SUFFIX(C_TY lhs, C_TY rhs, char* file, size_t line);       \
    C_TY ciel_shl_##SUFFIX(C_TY lhs, uintmax_t rhs, char* file, size_t line);  \
    C_TY ciel_shr_##SUFFIX(C_TY lhs, uintmax_t rhs, char* file, size_t line)

CIEL_DECLARE_SIGNED_CHECKS(i8, int8_t);
CIEL_DECLARE_SIGNED_CHECKS(i16, int16_t);
CIEL_DECLARE_SIGNED_CHECKS(i32, int32_t);
CIEL_DECLARE_SIGNED_CHECKS(i64, int64_t);
CIEL_DECLARE_UNSIGNED_CHECKS(u8, uint8_t);
CIEL_DECLARE_UNSIGNED_CHECKS(u16, uint16_t);
CIEL_DECLARE_UNSIGNED_CHECKS(u32, uint32_t);
CIEL_DECLARE_UNSIGNED_CHECKS(u64, uint64_t);
CIEL_DECLARE_UNSIGNED_CHECKS(usize, size_t);

#undef CIEL_DECLARE_SIGNED_CHECKS
#undef CIEL_DECLARE_UNSIGNED_CHECKS

size_t ciel_bounds_check(size_t index, size_t len, char* file, size_t line);
size_t ciel_slice_range_check(size_t start, size_t end, size_t len, char* file,
                              size_t line);

#ifdef __cplusplus
}
#endif

#endif
