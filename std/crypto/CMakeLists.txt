cmake_minimum_required(VERSION 3.16)
project(ciel_std_crypto C)

set(CMAKE_EXPORT_COMPILE_COMMANDS ON)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

if(NOT CIEL_RUNTIME_INCLUDE_DIR)
    message(FATAL_ERROR "CIEL_RUNTIME_INCLUDE_DIR must be provided by the Ciel driver")
endif()
get_filename_component(CIEL_RUNTIME_INCLUDE_DIR "${CIEL_RUNTIME_INCLUDE_DIR}" ABSOLUTE)

add_library(ciel_std_crypto STATIC
    native/botan.c
)

target_include_directories(ciel_std_crypto
    PUBLIC
        ${CMAKE_CURRENT_SOURCE_DIR}/include
        ${CIEL_RUNTIME_INCLUDE_DIR}
)

find_package(PkgConfig REQUIRED)
pkg_check_modules(BOTAN REQUIRED IMPORTED_TARGET botan-3)
target_link_libraries(ciel_std_crypto PUBLIC PkgConfig::BOTAN)

set_target_properties(ciel_std_crypto PROPERTIES
    ARCHIVE_OUTPUT_DIRECTORY "${CMAKE_CURRENT_BINARY_DIR}"
    ARCHIVE_OUTPUT_DIRECTORY_DEBUG "${CMAKE_CURRENT_BINARY_DIR}"
    ARCHIVE_OUTPUT_DIRECTORY_RELEASE "${CMAKE_CURRENT_BINARY_DIR}"
    ARCHIVE_OUTPUT_DIRECTORY_RELWITHDEBINFO "${CMAKE_CURRENT_BINARY_DIR}"
    ARCHIVE_OUTPUT_DIRECTORY_MINSIZEREL "${CMAKE_CURRENT_BINARY_DIR}"
)
