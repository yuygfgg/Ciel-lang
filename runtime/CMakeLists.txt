cmake_minimum_required(VERSION 3.16)
project(ciel_runtime C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_library(ciel_runtime STATIC
    src/core.c
    src/gc.c
    src/checks.c
    src/net_tcp.c
    src/crypto_botan.c
    src/actor.c
    src/atomic.c
    src/channel.c
    src/io.c
    src/bytes.c
    src/async.c
)

target_include_directories(ciel_runtime
    PUBLIC
        ${CMAKE_CURRENT_SOURCE_DIR}/include
    PRIVATE
        ${CMAKE_CURRENT_SOURCE_DIR}/src
)

target_compile_definitions(ciel_runtime PRIVATE
    GC_THREADS=1
    GC_NO_THREAD_REDIRECTS=1
)

find_package(PkgConfig QUIET)
if(PkgConfig_FOUND)
    pkg_check_modules(BDWGC QUIET bdw-gc)
    if(BDWGC_FOUND)
        target_include_directories(ciel_runtime PRIVATE ${BDWGC_INCLUDE_DIRS})
        target_compile_options(ciel_runtime PRIVATE ${BDWGC_CFLAGS_OTHER})
    endif()

    pkg_check_modules(BOTAN QUIET botan-3)
    if(BOTAN_FOUND)
        target_include_directories(ciel_runtime PRIVATE ${BOTAN_INCLUDE_DIRS})
        target_compile_options(ciel_runtime PRIVATE ${BOTAN_CFLAGS_OTHER})
    endif()
endif()

if(NOT WIN32)
    target_compile_options(ciel_runtime PRIVATE -fblocks)
endif()

set_target_properties(ciel_runtime PROPERTIES
    ARCHIVE_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}"
    ARCHIVE_OUTPUT_DIRECTORY_DEBUG "${CMAKE_BINARY_DIR}"
    ARCHIVE_OUTPUT_DIRECTORY_RELEASE "${CMAKE_BINARY_DIR}"
    ARCHIVE_OUTPUT_DIRECTORY_RELWITHDEBINFO "${CMAKE_BINARY_DIR}"
    ARCHIVE_OUTPUT_DIRECTORY_MINSIZEREL "${CMAKE_BINARY_DIR}"
)
