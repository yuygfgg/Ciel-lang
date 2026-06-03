#ifndef CIEL_BASE_H
#define CIEL_BASE_H

#if !defined(__linux__) && !defined(__APPLE__)
#error "Ciel runtime currently supports only Linux and macOS targets"
#endif

#include <errno.h>
#include <limits.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>

#if defined(__GNUC__) || defined(__clang__)
#define CIEL_MAYBE_UNUSED __attribute__((unused))
#define CIEL_COLD __attribute__((cold))
#define CIEL_NORETURN __attribute__((noreturn))
#define CIEL_MALLOC_LIKE __attribute__((malloc))
#define CIEL_ALLOC_SIZE1 __attribute__((alloc_size(1)))
#define CIEL_ALLOC_SIZE2 __attribute__((alloc_size(1, 2)))
#define CIEL_ALLOC_SIZE_ARG2 __attribute__((alloc_size(2)))
#define CIEL_RETURNS_NONNULL __attribute__((returns_nonnull))
#else
#define CIEL_MAYBE_UNUSED
#define CIEL_COLD
#define CIEL_NORETURN
#define CIEL_MALLOC_LIKE
#define CIEL_ALLOC_SIZE1
#define CIEL_ALLOC_SIZE2
#define CIEL_ALLOC_SIZE_ARG2
#define CIEL_RETURNS_NONNULL
#endif

#define CIEL_PANIC_EXIT_CODE 101

#if defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L
#define CIEL_ALIGNOF(T) _Alignof(T)
#elif defined(__GNUC__) || defined(__clang__)
#define CIEL_ALIGNOF(T) __alignof__(T)
#else
#define CIEL_ALIGNOF(T) sizeof(void*)
#endif

typedef struct {
    uint8_t* ptr;
    size_t len;
} CielSlice_u8;

typedef struct {
    char* ptr;
    size_t len;
} CielSlice_char;

typedef struct {
    const char* ptr;
    size_t len;
} CielConstSlice_char;

#define CIEL_CONST_STR(S)                                                      \
    ((CielConstSlice_char){.ptr = (S), .len = sizeof(S) - 1})

#endif
