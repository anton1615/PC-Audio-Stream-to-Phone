cmake_minimum_required(VERSION 3.22.1)

project("audiobtbridge")

# 設定 C++ 標準
set(CMAKE_CXX_STANDARD 17)

#  Oboe

find_package(oboe CONFIG REQUIRED)



# Enforce 16KB page alignment for our library
set(CMAKE_SHARED_LINKER_FLAGS "${CMAKE_SHARED_LINKER_FLAGS} -Wl,-z,max-page-size=16384 -Wl,-z,separate-code")
set(CMAKE_EXE_LINKER_FLAGS "${CMAKE_EXE_LINKER_FLAGS} -Wl,-z,max-page-size=16384 -Wl,-z,separate-code")

# -----------------------------------------------------------------------------
# ?﹝璓?Opus (?? FetchContent)
# -----------------------------------------------------------------------------
include(FetchContent)
FetchContent_Declare(
    opus
    GIT_REPOSITORY https://github.com/xiph/opus.git
    GIT_TAG        v1.4
)
# ?謚秋 Opus ?秋播?叟?隤?蹎Ⅹ賹?set(OPUS_STACK_PROTECTOR OFF CACHE BOOL "" FORCE)
FetchContent_MakeAvailable(opus)

add_library(
        audiobtbridge
        SHARED
        native-lib.cpp
)

target_include_directories(audiobtbridge PRIVATE
    ${opus_SOURCE_DIR}/include
)

find_library(log-lib log)

target_link_libraries(
        audiobtbridge
        ${log-lib}
        oboe::oboe_static
        opus
)
