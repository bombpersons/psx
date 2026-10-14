# GPU rasterisation probe: draws gp0_stream.h once, then idles, so the VRAM of
# any emulator running it can be diffed against `psx gpu-replay gp0_stream.bin`.
# Needs mipsel-none-elf-gcc on PATH and the nugget tree from the pinned
# PCSX-Redux checkout (MIT licensed). Regenerate the stream with gen_stream.py.
NUGGET ?= ../../../external/pcsx-redux/src/mips
TARGET = gpu
TYPE = ps-exe
SRCS = gpu.cpp
CXXFLAGS = -std=c++20
include $(NUGGET)/psyqo/psyqo.mk
