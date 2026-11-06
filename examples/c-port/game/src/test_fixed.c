/* fixed.h against psx's test vectors (psx port vectors): every 32-bit,
 * 12-fraction-bit add, sub, mul and div case. */
#include <stdio.h>
#include <string.h>

#include "fixed.h"

int main(int argc, char **argv) {
    FILE *f = argc > 1 ? fopen(argv[1], "r") : NULL;
    if (!f) {
        fprintf(stderr, "usage: test_fixed FIXED.jsonl (psx port vectors --out DIR)\n");
        return 2;
    }
    char line[4096], op[32];
    long a, b, out;
    int cases = 0, failed = 0;
    while (fgets(line, sizeof line, f)) {
        /* The vectors' keys are in alphabetical order. */
        if (sscanf(line, "{\"a\":[\"i32\",12,%ld],\"b\":[\"i32\",12,%ld],\"op\":\"%31[a-z_]\",\"out\":%ld}", &a, &b, op,
                   &out) != 4)
            continue;
        int32_t got;
        if (strcmp(op, "add") == 0) got = fx_add((int32_t)a, (int32_t)b);
        else if (strcmp(op, "sub") == 0) got = fx_sub((int32_t)a, (int32_t)b);
        else if (strcmp(op, "mul") == 0) got = fx_mul((int32_t)a, (int32_t)b, 12);
        else if (strcmp(op, "div") == 0) got = fx_div((int32_t)a, (int32_t)b, 12);
        else continue;
        cases++;
        if (got != out) {
            if (failed++ < 10) fprintf(stderr, "%s(%ld, %ld): expected %ld, got %ld\n", op, a, b, out, (long)got);
        }
    }
    fclose(f);
    printf("%d cases, %d failed\n", cases, failed);
    return failed || cases == 0;
}
