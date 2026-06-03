/**
 * example_usage.cpp — C++17 demo of the FLUX compression C++ wrapper
 *
 * Build (Linux):
 *   g++ -std=c++17 -I ../include -L ../target/release \
 *       -lflux_core_v1 example_usage.cpp -o example_usage_cpp
 *   LD_LIBRARY_PATH=../target/release ./example_usage_cpp
 *
 * Build (Windows, MinGW):
 *   dlltool -d ../include/flux_core_v1.def -l ../target/release/libflux_core_v1.dll.a
 *   g++ -std=c++17 -I ../include \
 *       example_usage.cpp ../target/release/libflux_core_v1.dll.a -o example_usage_cpp.exe
 *   copy ..\target\release\flux_core_v1.dll . && example_usage_cpp.exe
 *
 * Build (Windows, MSVC):
 *   cl /std:c++17 /I ..\include example_usage.cpp \
 *      /link /LIBPATH:..\target\release flux_core_v1.dll.lib
 */

#include "flux.hpp"

#include <algorithm>
#include <cassert>
#include <cstdint>
#include <iostream>
#include <string>
#include <vector>

int main() {
    std::cout << "FLUX engine version: " << flux::version() << '\n';

    /* ---------------------------------------------------------------
     * Build source data (highly compressible repetitive content)
     * ------------------------------------------------------------- */
    std::string src_text =
        "Hello, FLUX! This is a round-trip test. "
        "FLUX FLUX FLUX FLUX FLUX FLUX FLUX FLUX FLUX FLUX "
        "FLUX FLUX FLUX FLUX FLUX FLUX FLUX FLUX FLUX FLUX "
        "repetition greatly helps compression ratios. "
        "ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMNOPQRSTUVWXYZ";

    std::vector<uint8_t> src(src_text.begin(), src_text.end());

    /* ---------------------------------------------------------------
     * Configure options
     * ------------------------------------------------------------- */
    flux::Options opts;
    opts.level        = flux::Level_Balanced;
    opts.thread_count = 0;  /* auto */
    opts.block_size   = 0;  /* auto */
    /* opts.password = "s3cr3t"; */  /* uncomment to enable encryption */

    try {
        /* -----------------------------------------------------------
         * Compress with optional progress callback
         * --------------------------------------------------------- */
        flux::ProgressFn on_progress = [](const flux::ProgressEvent& ev) {
            if (ev.bytes_total > 0) {
                double pct = 100.0
                    * static_cast<double>(ev.bytes_processed)
                    / static_cast<double>(ev.bytes_total);
                std::cout << "  [progress] " << pct
                          << "% ratio=" << ev.compression_ratio << '\n';
            }
        };

        auto compressed = flux::compress(src, opts, on_progress);

        std::cout << "Compressed " << src.size()
                  << " bytes -> " << compressed.size()
                  << " bytes (ratio "
                  << static_cast<double>(compressed.size()) / static_cast<double>(src.size())
                  << ")\n";

        /* -----------------------------------------------------------
         * Query original size from archive header
         * --------------------------------------------------------- */
        std::size_t orig_sz = flux::get_decompressed_size(compressed, opts);
        std::cout << "Archive reports original size: " << orig_sz << " bytes\n";
        assert(orig_sz == src.size());

        /* -----------------------------------------------------------
         * ErrBufferTooSmall path
         * --------------------------------------------------------- */
        {
            uint8_t tiny[1];
            std::size_t tiny_len = sizeof(tiny);
            FluxOptions c_opts = opts.to_c();
            FluxResult small_r = flux_v1_decompress(
                compressed.data(), compressed.size(),
                tiny, &tiny_len,
                c_opts);
            if (small_r != FLUX_ERR_BUFFER_SMALL) {
                std::cerr << "Expected FLUX_ERR_BUFFER_SMALL, got " << small_r << '\n';
                return 1;
            }
            std::cout << "ErrBufferTooSmall test PASSED (required: " << tiny_len << ")\n";
        }

        /* -----------------------------------------------------------
         * Decompress — size is queried automatically
         * --------------------------------------------------------- */
        auto recovered = flux::decompress(compressed, opts);

        /* -----------------------------------------------------------
         * Byte-perfect verification
         * --------------------------------------------------------- */
        if (recovered != src) {
            std::cerr << "Round-trip FAILED: data mismatch!\n";
            return 1;
        }
        std::cout << "Round-trip OK -- decompressed " << recovered.size()
                  << " bytes, byte-perfect.\n";

        /* -----------------------------------------------------------
         * Directory compression (paths are illustrative — commented out)
         * --------------------------------------------------------- */
        /*
        flux::Options dir_opts;
        dir_opts.level = flux::Level_Maximum;
        flux::compress_directory(
            "my_data",
            "archive.flx",
            dir_opts,
            [](const flux::ProgressEvent& ev) {
                std::cout << "  Archiving: " << ev.current_file << '\n';
            }
        );
        std::cout << "Directory archived successfully.\n";
        */

    } catch (const flux::Error& e) {
        std::cerr << "FLUX error (code " << static_cast<int>(e.code())
                  << "): " << e.what() << '\n';
        return 1;
    } catch (const std::exception& e) {
        std::cerr << "Unexpected error: " << e.what() << '\n';
        return 1;
    }

    return 0;
}
