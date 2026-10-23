// Compile .s2s files with Spike2's own compiler.dll and print the code words.
//   compile_oracle.exe file.s2s ...
//   CMDS=commands.txt compile_oracle.exe file.s2s ...
// Output, one line per file: "WORDS n w0 w1 ..." or "ERR <code>".
//
// Built for the Windows x64 target (x86_64-w64-mingw32-gcc -O1) and run under Wine
// beside compiler.dll and SpikeUtl.dll, which are CED's and not in this repository.
//
// With CMDS set, the file lists commands to register before compiling, one per line:
//   name min max n word0 ... word(n-1)
// where the words are the descriptor compiler.dll reads (return type, argument lists
// closed by 5, the whole closed by 7). They are registered in file order, each with
// CBaseCmd::IXCmd, which is what a command's initialiser does, so the compiler numbers
// them 0x7a upward as it does when runtime.s2l has loaded. runtime.s2l itself cannot be
// loaded outside Spike2: it imports from SONVIEW.exe.
#include <windows.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static char *lines[20000];
static int nlines;

static int __cdecl getline_cb(char *buf, int line, void *view) {
    (void)view;
    if (line < 0 || line >= nlines) return -1;
    strcpy(buf, lines[line]);
    return (int)strlen(lines[line]);
}

typedef void *(*cp_ctor_t)(void *self, void *ctl);
typedef void (*cp_src_t)(void *self, void *fn, void *view);
typedef void (*cp_car_t)(void *self, char b);
typedef void (*cp_opt_t)(void *self, int opt, short v);
typedef void *(*da_ctor_t)(void *self, void *cp);
typedef void (*cp_setda_t)(void *self, void *da);
typedef void *(*mgr_ctor_t)(void *self, void *params, void *head, void *hwnd);
typedef short (*mgr_car_t)(void *self);


static int load(const char *path) {
    FILE *f = fopen(path, "rb");
    if (!f) return -1;
    static char buf[1 << 20];
    size_t n = fread(buf, 1, sizeof buf - 1, f);
    buf[n] = 0;
    fclose(f);
    nlines = 0;
    char *p = buf;
    while (*p && nlines < 20000) {
        char *e = strpbrk(p, "\r\n");
        lines[nlines++] = p;
        if (!e) break;
        if (*e == '\r' && e[1] == '\n') { *e = 0; p = e + 2; }
        else { *e = 0; p = e + 1; }
    }
    return 0;
}

int main(int argc, char **argv) {
    HMODULE u = LoadLibraryA("SpikeUtl.dll");
    HMODULE c = LoadLibraryA("compiler.dll"); 
    typedef void *(*bc_t)(void *, char);
    typedef short (*ix_t)(void *, const char *, short, short, const short *);
    bc_t bc = (bc_t)GetProcAddress(c, "??0CBaseCmd@@QEAA@_N@Z");
    ix_t ix = (ix_t)GetProcAddress(c, "?IXCmd@CBaseCmd@@IEAAFPEBDFFPEBF@Z");
    if (!bc || !ix) { printf("ERR cmdsym\n"); return 1; }
    if (getenv("CMDS")) {
        FILE *cf = fopen(getenv("CMDS"), "r"); char name[128]; int mn, mx, n, cnt = 0;
        while (fscanf(cf, "%127s %d %d %d", name, &mn, &mx, &n) == 4) {
            short *d = (short *)malloc(2 * (n + 4));
            for (int i = 0; i < n; i++) { int w; fscanf(cf, "%d", &w); d[i] = (short)w; }
            void *obj = calloc(1, 0x1000);
            bc(obj, 0);
            short r = ix(obj, strdup(name), (short)mn, (short)mx, d);
            if (r) fprintf(stderr, "IXCmd %s -> %d\n", name, r);
            cnt++;
        }
        fprintf(stderr, "registered %d commands\n", cnt);
    }
    if (!u || !c) { printf("ERR load\n"); return 1; }
    { void **g = (void **)((char *)u + 0x11fd38); if (!*g) *g = calloc(1, 0x1000); }
    cp_ctor_t cp_ctor = (cp_ctor_t)GetProcAddress(u, "??0CompileParams@@QEAA@PEAVCScriptControl@@@Z");
    cp_src_t cp_src = (cp_src_t)GetProcAddress(u, "?SetCompilerSource@CompileParams@@QEAAXP6AHPEADHPEAX@Z1@Z");
    cp_car_t cp_car = (cp_car_t)GetProcAddress(u, "?SetCompileAndRun@CompileParams@@QEAAX_N@Z");
    da_ctor_t da_ctor = (da_ctor_t)GetProcAddress(u, "??0CDebugAccessor@@QEAA@PEAVCompileParams@@@Z");
    cp_setda_t cp_setda = (cp_setda_t)GetProcAddress(u, "?SetDebugAccessor@CompileParams@@QEAAXPEAVCDebugAccessor@@@Z");
    mgr_ctor_t mgr_ctor = (mgr_ctor_t)GetProcAddress(c, "??0CScriptMgr@@QEAA@PEAVCompileParams@@PEAVCHeadCmd@@PEAUHWND__@@@Z");
    mgr_car_t mgr_car = (mgr_car_t)GetProcAddress(c, "?CompileAndRun@CScriptMgr@@QEAAFXZ");
    if (!cp_ctor || !cp_src || !cp_car || !da_ctor || !cp_setda || !mgr_ctor || !mgr_car) { printf("ERR sym\n"); return 1; }
    void *headp = NULL;
    if (getenv("CMDS")) {
        typedef void *(*hc_t)(void *);
        hc_t hc = (hc_t)GetProcAddress(c, "??0CHeadCmd@@QEAA@XZ");
        if (hc) { headp = calloc(1, 0x1000); hc(headp); }
    }
    for (int a = 1; a < argc; a++) {
        if (load(argv[a])) { printf("ERR open\n"); continue; }
        void *ctl = calloc(1, 0x8000), *cp = calloc(1, 0x4000), *mgr = calloc(1, 0x4000), *da = calloc(1, 0x4000);
        cp_ctor(cp, ctl);
        cp_src(cp, (void *)getline_cb, (void *)1);
        da_ctor(da, cp);
        cp_setda(cp, da);
        cp_car(cp, 0);
        mgr_ctor(mgr, cp, headp, NULL);
        if (getenv("CMDS")) { short (*is)(void*) = (short(*)(void*))GetProcAddress(c, "?IScriptMgr@CScriptMgr@@QEAAFXZ"); if (is) is(mgr); }
        short r = mgr_car(mgr);
        unsigned char *st = (unsigned char *)mgr + 0x60;
        unsigned short *b = *(unsigned short **)(st + 0x238);
        unsigned short *e = *(unsigned short **)(st + 0x240);
        if (r != 0 || !b) { printf("ERR %d\n", r); fflush(stdout); continue; }
        printf("WORDS %d", (int)(e - b));
        for (unsigned short *q = b; q < e; q++) printf(" %u", *q);
        printf("\n");
        fflush(stdout);
    }
    return 0;
}
