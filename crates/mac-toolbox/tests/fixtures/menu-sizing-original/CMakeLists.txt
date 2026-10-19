cmake_minimum_required(VERSION 3.16)
project(MenuSizing LANGUAGES C CXX)
set(CMAKE_CXX_STANDARD 17)
add_application(MenuSizingProbe MenuSizingProbe.cc application.r CREATOR "MNSZ")
