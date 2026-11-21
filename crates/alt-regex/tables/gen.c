// prints src/tables.rs: the character classes and the upper-case mapping
// of the GNU C library this is run with, in a UTF-8 locale
// usage: gen [locale [unicode version of the locale's data]]
#include <gnu/libc-version.h>
#include <locale.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>
#include <wctype.h>

#define LAST 0x10ffff

static int valid(unsigned c) { return c < 0xd800 || c > 0xdfff; }

static void class(const char *name, const char *rust) {
    wctype_t type = wctype(name);
    if (!type) { fprintf(stderr, "no class %s\n", name); exit(1); }
    printf("\n#[rustfmt::skip]\npub static %s: &[(u32, u32)] = &[", rust);
    int open = 0, count = 0;
    unsigned from = 0;
    for (unsigned c = 0; c <= LAST + 1; c++) {
        int in = c <= LAST && valid(c) && iswctype((wint_t)c, type);
        if (in && !open) { from = c; open = 1; }
        // a run goes on over the surrogates, which are no characters
        if (!in && open && !(c >= 0xd800 && c <= 0xdfff)) {
            unsigned to = c - 1;
            if (to >= 0xd800 && to <= 0xdfff) to = 0xd7ff;
            printf("%s(0x%x, 0x%x),", count % 6 ? " " : "\n    ", from, to);
            count++; open = 0;
        }
    }
    printf("\n];\n");
}

int main(int argc, char **argv) {
    const char *locale = argc > 1 ? argv[1] : "C.UTF-8";
    if (!setlocale(LC_ALL, locale)) { fprintf(stderr, "no locale %s\n", locale); return 1; }
    const char *unicode = argc > 2 ? argv[2] : "?";
    printf("// CODEGEN: tables/gen.c\n");
    printf("// source: the GNU C library %s, its locale %s (Unicode %s)\n",
           gnu_get_libc_version(), locale, unicode);
    printf("// regenerate where that library is the C library:\n");
    printf("//   sh crates/alt-regex/tables/gen.sh %s > crates/alt-regex/src/tables.rs\n", unicode);
    printf("\n//! The character classes and the upper-case letters of a UTF-8 locale.\n");
    // digit and xdigit hold ASCII only, and alnum is alpha and digit
    for (unsigned c = 0x80; c <= LAST; c++) {
        if (!valid(c)) continue;
        if (iswdigit(c) || iswxdigit(c)) { fprintf(stderr, "digit outside ASCII: %x\n", c); return 1; }
        if (!!iswalnum(c) != !!iswalpha(c)) { fprintf(stderr, "alnum is not alpha: %x\n", c); return 1; }
    }
    class("alpha", "ALPHA"); class("upper", "UPPER"); class("lower", "LOWER");
    class("space", "SPACE"); class("blank", "BLANK"); class("punct", "PUNCT");
    class("print", "PRINT"); class("graph", "GRAPH"); class("cntrl", "CNTRL");
    printf("\n/// Each character with an upper-case form other than itself, and that form.\n");
    printf("#[rustfmt::skip]\npub static TO_UPPER: &[(u32, u32)] = &[");
    int count = 0;
    for (unsigned c = 0; c <= LAST; c++) {
        if (!valid(c)) continue;
        unsigned u = towupper(c);
        if (u != c) { printf("%s(0x%x, 0x%x),", count % 6 ? " " : "\n    ", c, u); count++; }
    }
    printf("\n];\n");
    return 0;
}
