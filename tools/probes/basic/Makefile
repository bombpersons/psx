# Probe EXE for backend experiments. Needs mipsel-none-elf-gcc on PATH and the
# nugget tree from the pinned PCSX-Redux checkout (MIT licensed).
NUGGET ?= ../../../external/pcsx-redux/src/mips
TARGET = basic
TYPE = ps-exe
SRCS = basic.cpp
CXXFLAGS = -std=c++20
include $(NUGGET)/psyqo/psyqo.mk
