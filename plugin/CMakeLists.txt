cmake_minimum_required(VERSION 3.22)

project(FLint VERSION 0.2.0 LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Locate JUCE: use local JUCE_DIR (defaults to C:/JUCE if present), otherwise fetch JUCE 8.0.12.
set(JUCE_DIR "C:/JUCE" CACHE PATH "Path to local JUCE checkout")

if(EXISTS "${JUCE_DIR}/CMakeLists.txt")
    message(STATUS "FLint: Using local JUCE at ${JUCE_DIR}")
    add_subdirectory("${JUCE_DIR}" "${CMAKE_CURRENT_BINARY_DIR}/JUCE" EXCLUDE_FROM_ALL)
else()
    message(STATUS "FLint: Local JUCE not found at ${JUCE_DIR}; fetching JUCE 8.0.12 via FetchContent")
    include(FetchContent)
    FetchContent_Declare(
        JUCE
        GIT_REPOSITORY https://github.com/juce-framework/JUCE.git
        GIT_TAG        8.0.12
        GIT_SHALLOW    TRUE
    )
    FetchContent_MakeAvailable(JUCE)
endif()

juce_add_plugin(FLint
    COMPANY_NAME                "FLint"
    IS_SYNTH                    FALSE
    NEEDS_MIDI_INPUT            FALSE
    NEEDS_MIDI_OUTPUT           FALSE
    IS_MIDI_EFFECT              FALSE
    EDITOR_WANTS_KEYBOARD_FOCUS TRUE
    COPY_PLUGIN_AFTER_BUILD     FALSE
    PLUGIN_MANUFACTURER_CODE    Flnt
    PLUGIN_CODE                 Flnt
    FORMATS                     VST3 Standalone
    PRODUCT_NAME                "FLint"
)

juce_generate_juce_header(FLint)

target_sources(FLint
    PRIVATE
        Source/PluginProcessor.cpp
        Source/PluginProcessor.h
        Source/PluginEditor.cpp
        Source/PluginEditor.h
)

target_compile_definitions(FLint
    PUBLIC
        JUCE_WEB_BROWSER=0
        JUCE_USE_CURL=0
        JUCE_VST3_CAN_REPLACE_VST2=0
)

target_link_libraries(FLint
    PRIVATE
        juce::juce_audio_utils
        juce::juce_audio_processors
        juce::juce_gui_extra
    PUBLIC
        juce::juce_recommended_config_flags
        juce::juce_recommended_lto_flags
        juce::juce_recommended_warning_flags
)
