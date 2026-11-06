#ifndef SLED_H
#define SLED_H

#include <stdint.h>

/* The pad's buttons this game reads. */
enum { PAD_LEFT = 1, PAD_RIGHT = 2, PAD_CROSS = 4 };

typedef struct {
    int32_t x;     /* 20.12 */
    int32_t speed; /* 20.12 per tick */
} Sled;

/* The sled at rest at the top of the hill, at start_x. */
Sled sled_start(int32_t start_x);

/* One tick with the pad's buttons held. */
void sled_tick(Sled *s, unsigned pad);

#endif
