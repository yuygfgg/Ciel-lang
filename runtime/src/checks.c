#include "internal.h"

#ifndef NDEBUG
#define CIEL_DEFINE_BINOP(NAME, BUILTIN, OP, SUFFIX, C_TY, U_TY)               \
    C_TY ciel_##NAME##_##SUFFIX(C_TY lhs, C_TY rhs, char *file, size_t line) { \
        C_TY out;                                                              \
        if (BUILTIN(lhs, rhs, &out))                                           \
            ciel_panic_at("integer overflow", 16, file, line);                 \
        return out;                                                            \
    }
#else
#define CIEL_DEFINE_BINOP(NAME, BUILTIN, OP, SUFFIX, C_TY, U_TY)               \
    C_TY ciel_##NAME##_##SUFFIX(C_TY lhs, C_TY rhs, char *file, size_t line) { \
        (void)file;                                                            \
        (void)line;                                                            \
        return (C_TY)((U_TY)lhs OP(U_TY) rhs);                                 \
    }
#endif

#ifndef NDEBUG
#define CIEL_DEFINE_SIGNED_NEG(SUFFIX, C_TY, U_TY, MIN_VALUE)                  \
    C_TY ciel_neg_##SUFFIX(C_TY value, char *file, size_t line) {              \
        if (value == (C_TY)MIN_VALUE)                                          \
            ciel_panic_at("integer overflow", 16, file, line);                 \
        return (C_TY)(((U_TY)0) - (U_TY)value);                                \
    }
#else
#define CIEL_DEFINE_SIGNED_NEG(SUFFIX, C_TY, U_TY, MIN_VALUE)                  \
    C_TY ciel_neg_##SUFFIX(C_TY value, char *file, size_t line) {              \
        (void)file;                                                            \
        (void)line;                                                            \
        return (C_TY)(((U_TY)0) - (U_TY)value);                                \
    }
#endif

#ifndef NDEBUG
#define CIEL_SIGNED_DIV_OVERFLOW_CHECK(C_TY, MIN_VALUE, lhs, rhs, file, line)  \
    do {                                                                       \
        if ((lhs) == (C_TY)MIN_VALUE && (rhs) == (C_TY) - 1)                   \
            ciel_panic_at("integer overflow", 16, file, line);                 \
    } while (0)
#else
#define CIEL_SIGNED_DIV_OVERFLOW_CHECK(C_TY, MIN_VALUE, lhs, rhs, file, line)  \
    do {                                                                       \
        (void)file;                                                            \
        (void)line;                                                            \
    } while (0)
#endif

#define CIEL_DEFINE_SIGNED_DIV_REM(SUFFIX, C_TY, MIN_VALUE)                    \
    C_TY ciel_div_##SUFFIX(C_TY lhs, C_TY rhs, char *file, size_t line) {      \
        if (rhs == 0)                                                          \
            ciel_panic_at("division by zero", 16, file, line);                 \
        CIEL_SIGNED_DIV_OVERFLOW_CHECK(C_TY, MIN_VALUE, lhs, rhs, file, line); \
        if (lhs == (C_TY)MIN_VALUE && rhs == (C_TY) - 1)                       \
            return lhs;                                                        \
        return lhs / rhs;                                                      \
    }                                                                          \
    C_TY ciel_rem_##SUFFIX(C_TY lhs, C_TY rhs, char *file, size_t line) {      \
        if (rhs == 0)                                                          \
            ciel_panic_at("division by zero", 16, file, line);                 \
        CIEL_SIGNED_DIV_OVERFLOW_CHECK(C_TY, MIN_VALUE, lhs, rhs, file, line); \
        if (lhs == (C_TY)MIN_VALUE && rhs == (C_TY) - 1)                       \
            return 0;                                                          \
        return lhs % rhs;                                                      \
    }

#define CIEL_DEFINE_UNSIGNED_DIV_REM(SUFFIX, C_TY)                             \
    C_TY ciel_div_##SUFFIX(C_TY lhs, C_TY rhs, char *file, size_t line) {      \
        if (rhs == 0)                                                          \
            ciel_panic_at("division by zero", 16, file, line);                 \
        (void)file;                                                            \
        (void)line;                                                            \
        return lhs / rhs;                                                      \
    }                                                                          \
    C_TY ciel_rem_##SUFFIX(C_TY lhs, C_TY rhs, char *file, size_t line) {      \
        if (rhs == 0)                                                          \
            ciel_panic_at("division by zero", 16, file, line);                 \
        (void)file;                                                            \
        (void)line;                                                            \
        return lhs % rhs;                                                      \
    }

#define CIEL_DEFINE_SHIFTS(SUFFIX, C_TY, U_TY, BITS)                           \
    C_TY ciel_shl_##SUFFIX(C_TY lhs, uintmax_t rhs, char *file, size_t line) { \
        if (rhs >= (uintmax_t)(BITS))                                          \
            ciel_panic_at("shift count out of range", 24, file, line);         \
        return (C_TY)((U_TY)lhs << rhs);                                       \
    }                                                                          \
    C_TY ciel_shr_##SUFFIX(C_TY lhs, uintmax_t rhs, char *file, size_t line) { \
        if (rhs >= (uintmax_t)(BITS))                                          \
            ciel_panic_at("shift count out of range", 24, file, line);         \
        return (C_TY)(lhs >> rhs);                                             \
    }

#define CIEL_DEFINE_UNSIGNED_SHIFTS(SUFFIX, C_TY, BITS)                        \
    C_TY ciel_shl_##SUFFIX(C_TY lhs, uintmax_t rhs, char *file, size_t line) { \
        if (rhs >= (uintmax_t)(BITS))                                          \
            ciel_panic_at("shift count out of range", 24, file, line);         \
        return (C_TY)(lhs << rhs);                                             \
    }                                                                          \
    C_TY ciel_shr_##SUFFIX(C_TY lhs, uintmax_t rhs, char *file, size_t line) { \
        if (rhs >= (uintmax_t)(BITS))                                          \
            ciel_panic_at("shift count out of range", 24, file, line);         \
        return (C_TY)(lhs >> rhs);                                             \
    }

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, i8, int8_t, uint8_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, i8, int8_t, uint8_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, i8, int8_t, uint8_t)
CIEL_DEFINE_SIGNED_NEG(i8, int8_t, uint8_t, INT8_MIN)
CIEL_DEFINE_SIGNED_DIV_REM(i8, int8_t, INT8_MIN)
CIEL_DEFINE_SHIFTS(i8, int8_t, uint8_t, 8)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, i16, int16_t, uint16_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, i16, int16_t, uint16_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, i16, int16_t, uint16_t)
CIEL_DEFINE_SIGNED_NEG(i16, int16_t, uint16_t, INT16_MIN)
CIEL_DEFINE_SIGNED_DIV_REM(i16, int16_t, INT16_MIN)
CIEL_DEFINE_SHIFTS(i16, int16_t, uint16_t, 16)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, i32, int32_t, uint32_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, i32, int32_t, uint32_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, i32, int32_t, uint32_t)
CIEL_DEFINE_SIGNED_NEG(i32, int32_t, uint32_t, INT32_MIN)
CIEL_DEFINE_SIGNED_DIV_REM(i32, int32_t, INT32_MIN)
CIEL_DEFINE_SHIFTS(i32, int32_t, uint32_t, 32)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, i64, int64_t, uint64_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, i64, int64_t, uint64_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, i64, int64_t, uint64_t)
CIEL_DEFINE_SIGNED_NEG(i64, int64_t, uint64_t, INT64_MIN)
CIEL_DEFINE_SIGNED_DIV_REM(i64, int64_t, INT64_MIN)
CIEL_DEFINE_SHIFTS(i64, int64_t, uint64_t, 64)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, u8, uint8_t, uint8_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, u8, uint8_t, uint8_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, u8, uint8_t, uint8_t)
CIEL_DEFINE_UNSIGNED_DIV_REM(u8, uint8_t)
CIEL_DEFINE_UNSIGNED_SHIFTS(u8, uint8_t, 8)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, u16, uint16_t, uint16_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, u16, uint16_t, uint16_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, u16, uint16_t, uint16_t)
CIEL_DEFINE_UNSIGNED_DIV_REM(u16, uint16_t)
CIEL_DEFINE_UNSIGNED_SHIFTS(u16, uint16_t, 16)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, u32, uint32_t, uint32_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, u32, uint32_t, uint32_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, u32, uint32_t, uint32_t)
CIEL_DEFINE_UNSIGNED_DIV_REM(u32, uint32_t)
CIEL_DEFINE_UNSIGNED_SHIFTS(u32, uint32_t, 32)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, u64, uint64_t, uint64_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, u64, uint64_t, uint64_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, u64, uint64_t, uint64_t)
CIEL_DEFINE_UNSIGNED_DIV_REM(u64, uint64_t)
CIEL_DEFINE_UNSIGNED_SHIFTS(u64, uint64_t, 64)

CIEL_DEFINE_BINOP(add, __builtin_add_overflow, +, usize, size_t, size_t)
CIEL_DEFINE_BINOP(sub, __builtin_sub_overflow, -, usize, size_t, size_t)
CIEL_DEFINE_BINOP(mul, __builtin_mul_overflow, *, usize, size_t, size_t)
CIEL_DEFINE_UNSIGNED_DIV_REM(usize, size_t)
CIEL_DEFINE_UNSIGNED_SHIFTS(usize, size_t, sizeof(size_t) * CHAR_BIT)

#undef CIEL_DEFINE_BINOP
#undef CIEL_DEFINE_SIGNED_NEG
#undef CIEL_SIGNED_DIV_OVERFLOW_CHECK
#undef CIEL_DEFINE_SIGNED_DIV_REM
#undef CIEL_DEFINE_UNSIGNED_DIV_REM
#undef CIEL_DEFINE_SHIFTS
#undef CIEL_DEFINE_UNSIGNED_SHIFTS

size_t ciel_bounds_check(size_t index, size_t len, char *file, size_t line) {
    if (index >= len)
        ciel_panic_at("index out of bounds", 19, file, line);
    return index;
}

size_t ciel_slice_range_check(size_t start, size_t end, size_t len, char *file,
                              size_t line) {
    if (start > end || end > len)
        ciel_panic_at("slice range out of bounds", 25, file, line);
    return start;
}
