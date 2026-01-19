cmake_minimum_required(VERSION 3.22.1)

project("audiobtbridge")

# 設定 C++ 標準
set(CMAKE_CXX_STANDARD 17)

# 尋找 Oboe
find_package(oboe CONFIG REQUIRED)

# -----------------------------------------------------------------------------
# 下載並編譯 Opus (透過 FetchContent)
# -----------------------------------------------------------------------------
include(FetchContent)
FetchContent_Declare(
    opus
    GIT_REPOSITORY https://github.com/xiph/opus.git
    GIT_TAG        v1.4
)
# 停用 Opus 不需要的一些組件以加速編譯
set(OPUS_STACK_PROTECTOR OFF CACHE BOOL "" FORCE)
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
        oboe::oboe
        opus
)