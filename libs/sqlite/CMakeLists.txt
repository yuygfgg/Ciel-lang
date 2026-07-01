cmake_minimum_required(VERSION 3.16)
project(ciel_lib_sqlite C)

set(CMAKE_EXPORT_COMPILE_COMMANDS ON)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

if(NOT DEFINED CIEL_RUNTIME_INCLUDE_DIR)
    message(FATAL_ERROR "CIEL_RUNTIME_INCLUDE_DIR is required")
endif()

find_package(Threads REQUIRED)

add_library(ciel_lib_sqlite STATIC
    native/sqlite3.c
    native/sqlite_wrap.c
)

target_include_directories(ciel_lib_sqlite
    PUBLIC
        ${CMAKE_CURRENT_SOURCE_DIR}/native
    PRIVATE
        ${CIEL_RUNTIME_INCLUDE_DIR}
)

target_compile_definitions(ciel_lib_sqlite PRIVATE
    SQLITE_THREADSAFE=1
    SQLITE_OMIT_LOAD_EXTENSION
    SQLITE_ENABLE_FTS5
    SQLITE_ENABLE_RTREE
    SQLITE_ENABLE_MATH_FUNCTIONS
)

target_link_libraries(ciel_lib_sqlite PUBLIC
    Threads::Threads
    ${CMAKE_DL_LIBS}
)

if(UNIX AND NOT APPLE)
    target_link_libraries(ciel_lib_sqlite PUBLIC m)
endif()
