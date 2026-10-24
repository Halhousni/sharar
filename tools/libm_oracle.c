/* Run msvcr110's maths functions on inputs and print the results' bits.
 *
 *   libm_oracle.exe <msvcr110.dll> <function> <inputs> <outputs>
 *
 * `inputs` has one line per call: one or two 16-digit hex words (the bits of the arguments).
 * `outputs` gets the bits of each result, one per line. Built with
 *   x86_64-w64-mingw32-gcc -O1 -o libm_oracle.exe tools/libm_oracle.c
 * and run under Wine, or on Windows, next to the real msvcr110.dll. */
#include <stdio.h>
#include <string.h>
#include <stdint.h>
#include <windows.h>

typedef double (*F1)(double);
typedef double (*F2)(double, double);

static double bits_to_d(uint64_t u) { double d; memcpy(&d, &u, 8); return d; }
static uint64_t d_to_bits(double d) { uint64_t u; memcpy(&u, &d, 8); return u; }

int main(int argc, char **argv)
{
    if (argc < 5) return 2;
    HMODULE m = LoadLibraryA(argv[1]);
    if (!m) { fprintf(stderr, "cannot load %s\n", argv[1]); return 3; }
    FARPROC f = GetProcAddress(m, !strcmp(argv[2], "hypot") ? "_hypot" : argv[2]);
    if (!f) { fprintf(stderr, "no %s\n", argv[2]); return 4; }
    FILE *in = fopen(argv[3], "r"), *out = fopen(argv[4], "w");
    if (!in || !out) return 5;
    int two = !strcmp(argv[2], "pow") || !strcmp(argv[2], "atan2") || !strcmp(argv[2], "fmod") || !strcmp(argv[2], "_hypot") || !strcmp(argv[2], "hypot");
    char line[128];
    while (fgets(line, sizeof line, in)) {
        unsigned long long a = 0, b = 0;
        int n = sscanf(line, "%llx %llx", &a, &b);
        if (n < 1) continue;
        double r = two ? ((F2)f)(bits_to_d(a), bits_to_d(b)) : ((F1)f)(bits_to_d(a));
        fprintf(out, "%016llx\n", (unsigned long long)d_to_bits(r));
    }
    fclose(out);
    return 0;
}
