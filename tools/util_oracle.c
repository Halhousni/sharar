/* Run a function of a Windows DLL on arguments and print the bits of its result.
 *
 *   util_oracle.exe <dll> <symbol> <signature> <inputs> <outputs>
 *
 * The signature names the argument kinds and the result, e.g. `dd>d` (two doubles, a double) or
 * `ii>d`. `inputs` has one line per call, a hex word per argument (a double's bits, or an int).
 * Built with x86_64-w64-mingw32-gcc -O1 and run under Wine next to the DLL's own dependencies. */
#include <stdio.h>
#include <string.h>
#include <stdint.h>
#include <windows.h>

static double bits_to_d(uint64_t u) { double d; memcpy(&d, &u, 8); return d; }
static uint64_t d_to_bits(double d) { uint64_t u; memcpy(&u, &d, 8); return u; }

int main(int argc, char **argv)
{
    if (argc < 6) return 2;
    HMODULE m = LoadLibraryA(argv[1]);
    if (!m) { fprintf(stderr, "cannot load %s (%lu)\n", argv[1], GetLastError()); return 3; }
    FARPROC f = GetProcAddress(m, argv[2]);
    if (!f) { fprintf(stderr, "no %s\n", argv[2]); return 4; }
    const char *sig = argv[3];
    FILE *in = fopen(argv[4], "r"), *out = fopen(argv[5], "w");
    if (!in || !out) return 5;
    char line[256];
    while (fgets(line, sizeof line, in)) {
        unsigned long long w[4] = {0, 0, 0, 0};
        int n = sscanf(line, "%llx %llx %llx %llx", &w[0], &w[1], &w[2], &w[3]);
        if (n < 1) continue;
        double r = 0;
        if (!strcmp(sig, "d>d")) r = ((double (*)(double))f)(bits_to_d(w[0]));
        else if (!strcmp(sig, "dd>d")) r = ((double (*)(double, double))f)(bits_to_d(w[0]), bits_to_d(w[1]));
        else if (!strcmp(sig, "ddd>d")) r = ((double (*)(double, double, double))f)(bits_to_d(w[0]), bits_to_d(w[1]), bits_to_d(w[2]));
        else if (!strcmp(sig, "ii>d")) r = ((double (*)(int, int))f)((int)w[0], (int)w[1]);
        else { fprintf(stderr, "signature %s\n", sig); return 6; }
        fprintf(out, "%016llx\n", (unsigned long long)d_to_bits(r));
    }
    fclose(out);
    return 0;
}
