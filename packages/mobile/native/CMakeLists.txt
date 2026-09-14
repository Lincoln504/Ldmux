# The JNI shim is built by scripts/build-libghostty-vt.sh into
# composeApp/src/androidMain/jniLibs/<abi>/. This CMake file exists so AGP's
# externalNativeBuild can take over later; for now the prebuilt .so is what is
# packaged.
cmake_minimum_required(VERSION 3.22)
project(ldmux_terminal C)

set(GHOSTTY_PREBUILT "${CMAKE_CURRENT_SOURCE_DIR}/prebuilt/${ANDROID_ABI}")

add_library(ldmux_terminal SHARED ldmux_terminal.c)
target_include_directories(ldmux_terminal PRIVATE "${GHOSTTY_PREBUILT}/include")
target_link_libraries(ldmux_terminal PRIVATE
  "${GHOSTTY_PREBUILT}/lib/libghostty-vt.a"
  log
  m
)