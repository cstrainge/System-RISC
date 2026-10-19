cmake_minimum_required(VERSION 3.16)
project(StandardFile LANGUAGES C CXX)
set(CMAKE_CXX_STANDARD 17)
add_application(StandardFileProbe StandardFileProbe.cc application.r CREATOR "SFPR")
