cmake_minimum_required(VERSION 3.16)
project(ciel_runtime C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

if(NOT CIEL_RUNTIME_INCLUDE_DIR)
    set(CIEL_RUNTIME_INCLUDE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/include")
endif()
get_filename_component(CIEL_RUNTIME_INCLUDE_DIR "${CIEL_RUNTIME_INCLUDE_DIR}" ABSOLUTE)

add_library(ciel_runtime STATIC
    src/core.c
    src/resource.c
    src/gc.c
    src/checks.c
    src/net_tcp.c
    src/actor.c
    src/io.c
    src/bytes.c
    src/async.c
)

target_include_directories(ciel_runtime
    PUBLIC
        ${CIEL_RUNTIME_INCLUDE_DIR}
    PRIVATE
        ${CMAKE_CURRENT_SOURCE_DIR}/src
)

target_compile_definitions(ciel_runtime PRIVATE
    GC_THREADS=1
    GC_NO_THREAD_REDIRECTS=1
)

find_package(Threads REQUIRED)
target_link_libraries(ciel_runtime PUBLIC Threads::Threads)

find_package(PkgConfig REQUIRED)
pkg_check_modules(BDWGC REQUIRED IMPORTED_TARGET bdw-gc)
target_link_libraries(ciel_runtime PUBLIC PkgConfig::BDWGC)

if(NOT APPLE AND NOT WIN32)
    pkg_check_modules(DISPATCH QUIET IMPORTED_TARGET libdispatch)
    if(DISPATCH_FOUND)
        target_link_libraries(ciel_runtime PUBLIC PkgConfig::DISPATCH BlocksRuntime)
    else()
        target_link_libraries(ciel_runtime PUBLIC dispatch BlocksRuntime)
    endif()
endif()

if(NOT WIN32)
    target_compile_options(ciel_runtime PUBLIC -fblocks)
endif()

set_target_properties(ciel_runtime PROPERTIES
    ARCHIVE_OUTPUT_DIRECTORY "${CMAKE_CURRENT_BINARY_DIR}"
    ARCHIVE_OUTPUT_DIRECTORY_DEBUG "${CMAKE_CURRENT_BINARY_DIR}"
    ARCHIVE_OUTPUT_DIRECTORY_RELEASE "${CMAKE_CURRENT_BINARY_DIR}"
    ARCHIVE_OUTPUT_DIRECTORY_RELWITHDEBINFO "${CMAKE_CURRENT_BINARY_DIR}"
    ARCHIVE_OUTPUT_DIRECTORY_MINSIZEREL "${CMAKE_CURRENT_BINARY_DIR}"
)
