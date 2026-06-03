/*
 * example_usage.c — minimal C demo of the FLUX compression C API
 *
 * Build (Linux):
 *   gcc -std=c99 -I ../include -L ../target/release \
 *       -lflux_core_v1 example_usage.c -o example_usage_c
 *   LD_LIBRARY_PATH=../target/release ./example_usage_c
 *
 * Build (Windows, MinGW):
 *   dlltool -d ../include/flux_core_v1.def -l ../target/release/libflux_core_v1.dll.a
 *   gcc -std=c99 -I ../include \
 *       example_usage.c ../target/release/libflux_core_v1.dll.a -o example_usage_c.exe
 *   copy ..\target\release\flux_core_v1.dll . && example_usage_c.exe
 *
 * Build (Windows, MSVC):
 *   cl /std:c17 /I ..\include example_usage.c \
 *      /link /LIBPATH:..\target\release flux_core_v1.dll.lib
 */

#include "flux.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* -------------------------------------------------------------------------
 * Optional progress callback
 * ---------------------------------------------------------------------- */
static void on_progress(FluxProgressEvent ev, void *user_data) {
    (void)user_data;
    if (ev.bytes_total > 0) {
        double pct = 100.0 * (double)ev.bytes_processed / (double)ev.bytes_total;
        printf("  [progress] %.1f%%  ratio=%.3f\n", pct, (double)ev.compression_ratio);
    }
}

int main(void) {
    int ok = 1;

    /* ------------------------------------------------------------------
     * 1. Version check
     * ---------------------------------------------------------------- */
    printf("FLUX engine version: %s\n", flux_v1_get_version());

    /* ------------------------------------------------------------------
     * 2. Prepare source data (highly compressible repetitive content)
     * ---------------------------------------------------------------- */
    const char src_text[] =
        "Hello, FLUX! This is a round-trip test. "
        "FLUX FLUX FLUX FLUX FLUX FLUX FLUX FLUX FLUX FLUX "
        "FLUX FLUX FLUX FLUX FLUX FLUX FLUX FLUX FLUX FLUX "
        "repetition greatly helps compression ratios. "
        "ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const size_t src_len = sizeof(src_text) - 1; /* exclude NUL terminator */

    /* ------------------------------------------------------------------
     * 3. Build FluxOptions
     *    Always zero-initialise before setting fields (field order changed
     *    in v1.2; memset is future-proof regardless of field layout).
     * ---------------------------------------------------------------- */
    FluxOptions opts;
    memset(&opts, 0, sizeof(opts));
    opts.level        = FLUX_LEVEL_BALANCED;
    opts.password     = NULL;   /* no encryption */
    opts.thread_count = 0;      /* auto */
    opts.block_size   = 0;      /* auto */

    /* ------------------------------------------------------------------
     * 4. Compress
     * ---------------------------------------------------------------- */
    size_t   comp_cap = src_len + src_len / 8 + 4096;
    uint8_t *comp_buf = (uint8_t *)malloc(comp_cap);
    if (!comp_buf) { fputs("malloc failed\n", stderr); return 1; }

    size_t     comp_len = comp_cap;
    FluxResult r = flux_v1_compress(
        (const uint8_t *)src_text, src_len,
        comp_buf, &comp_len,
        opts, on_progress, NULL
    );

    if (r != FLUX_OK) {
        fprintf(stderr, "flux_v1_compress failed: code %d\n", (int)r);
        free(comp_buf);
        return 1;
    }
    printf("Compressed %llu bytes -> %llu bytes (ratio %.3f)\n",
           (unsigned long long)src_len,
           (unsigned long long)comp_len,
           (double)comp_len / (double)src_len);

    /* ------------------------------------------------------------------
     * 5. Query decompressed size from the archive header
     * ---------------------------------------------------------------- */
    size_t decomp_size = 0;
    r = flux_v1_get_decompressed_size(comp_buf, comp_len, opts, &decomp_size);
    if (r != FLUX_OK) {
        fprintf(stderr, "flux_v1_get_decompressed_size failed: code %d\n", (int)r);
        free(comp_buf);
        return 1;
    }
    printf("Archive reports original size: %llu bytes\n", (unsigned long long)decomp_size);

    if (decomp_size != src_len) {
        fprintf(stderr, "Size mismatch: expected %llu, got %llu\n",
                (unsigned long long)src_len, (unsigned long long)decomp_size);
        free(comp_buf); return 1;
    }

    /* ------------------------------------------------------------------
     * 6. ErrBufferTooSmall path: call with a 1-byte output buffer
     * ---------------------------------------------------------------- */
    uint8_t tiny_buf[1];
    size_t  tiny_len = sizeof(tiny_buf);
    r = flux_v1_decompress(comp_buf, comp_len, tiny_buf, &tiny_len, opts);
    if (r != FLUX_ERR_BUFFER_SMALL) {
        fprintf(stderr,
                "Expected FLUX_ERR_BUFFER_SMALL with 1-byte buffer, got %d\n", (int)r);
        free(comp_buf); return 1;
    }
    printf("ErrBufferTooSmall test PASSED (required size: %llu)\n",
           (unsigned long long)tiny_len);

    /* ------------------------------------------------------------------
     * 7. Decompress into exact-size buffer
     * ---------------------------------------------------------------- */
    uint8_t *decomp_buf = (uint8_t *)malloc(decomp_size + 1);
    if (!decomp_buf) { fputs("malloc failed\n", stderr); free(comp_buf); return 1; }

    size_t out_len = decomp_size;
    r = flux_v1_decompress(comp_buf, comp_len, decomp_buf, &out_len, opts);
    if (r != FLUX_OK) {
        fprintf(stderr, "flux_v1_decompress failed: code %d\n", (int)r);
        free(comp_buf); free(decomp_buf); return 1;
    }
    decomp_buf[out_len] = '\0';

    /* ------------------------------------------------------------------
     * 8. Verify round-trip
     * ---------------------------------------------------------------- */
    if (out_len != src_len || memcmp(decomp_buf, src_text, src_len) != 0) {
        fputs("Round-trip FAILED: decompressed data does not match source!\n", stderr);
        ok = 0;
    } else {
        printf("Round-trip OK -- decompressed %llu bytes, byte-perfect.\n",
               (unsigned long long)out_len);
    }

    flux_v1_free_result(r);
    free(comp_buf);
    free(decomp_buf);
    return ok ? 0 : 1;
}
