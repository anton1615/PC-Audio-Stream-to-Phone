cmake_minimum_required(VERSION 3.22.1)

project("audiobtbridge")

# 設定 C++ 標準
set(CMAKE_CXX_STANDARD 17)

# Enforce 16KB page alignment for our library
set(CMAKE_SHARED_LINKER_FLAGS "${CMAKE_SHARED_LINKER_FLAGS} -Wl,-z,max-page-size=16384 -Wl,-z,common-page-size=16384 -Wl,-z,separate-code")
set(CMAKE_EXE_LINKER_FLAGS "${CMAKE_EXE_LINKER_FLAGS} -Wl,-z,max-page-size=16384 -Wl,-z,common-page-size=16384 -Wl,-z,separate-code")

include(FetchContent)

# -----------------------------------------------------------------------------
# Oboe (Build from source to ensure 16KB alignment and static STL)
# -----------------------------------------------------------------------------
FetchContent_Declare(
    oboe
    GIT_REPOSITORY https://github.com/google/oboe
    GIT_TAG        1.9.0
)
FetchContent_MakeAvailable(oboe)

# -----------------------------------------------------------------------------
# Opus
# -----------------------------------------------------------------------------
FetchContent_Declare(
    opus
    GIT_REPOSITORY https://github.com/xiph/opus.git
    GIT_TAG        v1.4
)
set(OPUS_STACK_PROTECTOR OFF CACHE BOOL "" FORCE)
FetchContent_MakeAvailable(opus)

add_library(
        audiobtbridge
        SHARED
        native-lib.cpp
)

target_include_directories(audiobtbridge PRIVATE
    ${opus_SOURCE_DIR}/include
    ${oboe_SOURCE_DIR}/include
)

find_library(log-lib log)

target_link_libraries(
        audiobtbridge
        ${log-lib}
        oboe
        opus
)
