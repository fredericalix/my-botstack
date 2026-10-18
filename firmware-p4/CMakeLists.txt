# botstack firmware for the Waveshare ESP32-P4-WIFI6-Touch-LCD-4C.
cmake_minimum_required(VERSION 3.16)
set(PROJECT_VER "0.1.0")
set(EXTRA_COMPONENT_DIRS "${CMAKE_CURRENT_LIST_DIR}/../firmware/components")
include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(botstack-p4)
