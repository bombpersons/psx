/* The sled port. With --psx it speaks psx's port protocol on stdin and
 * stdout: one JSON request a line, one reply a line (psx guide --topic
 * languages). The requests are read with a few string searches, enough for
 * the fields this game uses; a bigger port would vendor a JSON library. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "sled.h"

/* The integer after "key": in line, or fallback when there is none. */
static long number(const char *line, const char *key, long fallback) {
    char quoted[64];
    snprintf(quoted, sizeof quoted, "\"%s\":", key);
    const char *at = strstr(line, quoted);
    return at ? strtol(at + strlen(quoted), NULL, 10) : fallback;
}

/* The buttons a tick request holds, by name. */
static unsigned buttons(const char *line) {
    const char *list = strstr(line, "\"buttons\":[");
    const char *end = list ? strchr(list, ']') : NULL;
    unsigned pad = 0;
    if (!end) return 0;
    static const struct { const char *name; unsigned bit; } names[] = {
        {"\"LEFT\"", PAD_LEFT}, {"\"RIGHT\"", PAD_RIGHT}, {"\"CROSS\"", PAD_CROSS}};
    for (size_t i = 0; i < sizeof names / sizeof names[0]; i++) {
        const char *b = strstr(list, names[i].name);
        if (b && b < end) pad |= names[i].bit;
    }
    return pad;
}

static int serve(void) {
    char line[65536];
    Sled sled = sled_start(0);
    int launched = 0;
    while (fgets(line, sizeof line, stdin)) {
        if (strncmp(line, "{\"hello\":", 9) == 0) {
            printf("{\"kinds\":[\"hill\"]}\n");
        } else if (strncmp(line, "{\"launch\":", 10) == 0) {
            if (!strstr(line, "\"kind\":\"hill\"")) {
                printf("{\"error\":\"no such kind\"}\n");
            } else {
                sled = sled_start((int32_t)number(line, "start_x", 0));
                launched = 1;
                printf("{\"ok\":true}\n");
            }
        } else if (strncmp(line, "{\"tick\":", 8) == 0) {
            if (launched) sled_tick(&sled, buttons(line));
            printf("{}\n");
        } else if (strncmp(line, "{\"observe\":", 11) == 0) {
            printf("{\"values\":{\"sled_x\":%ld,\"sled_speed\":%ld}}\n", (long)sled.x, (long)sled.speed);
        } else if (strncmp(line, "{\"screen\":", 10) == 0) {
            printf("{\"none\":true}\n");
        } else {
            printf("{\"error\":\"unknown request\"}\n");
        }
        fflush(stdout);
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "--psx") == 0) return serve();
    fprintf(stderr, "usage: sled --psx (the port protocol; this example has no window)\n");
    return 2;
}
