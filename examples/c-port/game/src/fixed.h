/* The original's integer arithmetic on 32-bit words (psx guide --topic
 * languages). Everything is computed in uint32_t, where C defines
 * overflow, and converted back; a right shift of a negative number is
 * written out, since C leaves it to the compiler. */
#ifndef FIXED_H
#define FIXED_H

#include <stdint.h>

/* The low 32 bits of v as a signed word, as a store keeps them. */
static inline int32_t wrap32(uint32_t v) {
    return v <= INT32_MAX ? (int32_t)v : (int32_t)(v - 0x80000000u) - INT32_MAX - 1;
}

/* a >> k, arithmetic: rounds toward minus infinity. */
static inline int32_t sra(int32_t a, unsigned k) {
    uint32_t u = (uint32_t)a >> k;
    if (a < 0 && k > 0) u |= ~(UINT32_MAX >> k);
    return wrap32(u);
}

static inline int32_t fx_add(int32_t a, int32_t b) { return wrap32((uint32_t)a + (uint32_t)b); }
static inline int32_t fx_sub(int32_t a, int32_t b) { return wrap32((uint32_t)a - (uint32_t)b); }

/* a * b with b's g fraction bits: the product's low 32 bits, shifted. */
static inline int32_t fx_mul(int32_t a, int32_t b, unsigned g) {
    return sra(wrap32((uint32_t)a * (uint32_t)b), g);
}

/* (a << g) / b, truncating toward zero, with the R3000's results for a
 * zero divisor and for INT32_MIN / -1. */
static inline int32_t fx_div(int32_t a, int32_t b, unsigned g) {
    int32_t n = wrap32((uint32_t)a << g);
    if (b == 0) return n >= 0 ? -1 : 1;
    if (n == INT32_MIN && b == -1) return INT32_MIN;
    return n / b;
}

#endif
