#include "sled.h"
#include "fixed.h"

/* A push a tick, 0.1875 units per tick. */
#define PUSH 0x300
/* The snow keeps 15/16 of the speed (0.12). */
#define SNOW 0xF00
/* The brake halves it (2.0, 20.12). */
#define BRAKE 0x2000

Sled sled_start(int32_t start_x) {
    Sled s = {start_x, 0};
    return s;
}

void sled_tick(Sled *s, unsigned pad) {
    if (pad & PAD_RIGHT) s->speed = fx_add(s->speed, PUSH);
    if (pad & PAD_LEFT) s->speed = fx_sub(s->speed, PUSH);
    if (pad & PAD_CROSS) s->speed = fx_div(s->speed, BRAKE, 12);
    s->speed = fx_mul(s->speed, SNOW, 12);
    s->x = fx_add(s->x, s->speed);
}
