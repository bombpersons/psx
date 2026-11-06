# The sled port: `make` builds build/sled; `make test VECTORS=DIR` runs the
# arithmetic against `psx port vectors --out DIR`.
CC ?= cc
CFLAGS ?= -std=c99 -O2 -Wall -Wextra -Werror

build/sled: src/main.c src/sled.c src/sled.h src/fixed.h | build
	$(CC) $(CFLAGS) -o $@ src/main.c src/sled.c

build/test_fixed: src/test_fixed.c src/fixed.h | build
	$(CC) $(CFLAGS) -o $@ src/test_fixed.c

build:
	mkdir -p build

VECTORS ?= ../.psx/port/vectors

test: build/test_fixed
	build/test_fixed $(VECTORS)/fixed.jsonl

.PHONY: test
