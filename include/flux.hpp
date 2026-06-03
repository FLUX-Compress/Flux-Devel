/**
 * flux.hpp  —  C++17 RAII wrapper for the FLUX compression engine
 *
 * Header-only.  Include this file and link against flux_core_v1.
 * (flux.h is included automatically; do not include it separately.)
 *
 * Linking:
 *   Windows (MinGW): g++ -std=c++17 -I include example.cpp -L lib -l:flux_core_v1.dll.lib
 *   Windows (MSVC):  cl /std:c++17 /I include example.cpp /link flux_core_v1.dll.lib
 *   Linux:           g++ -std=c++17 -I include example.cpp -lflux_core_v1
 *
 * Error handling:
 *   All functions throw flux::Error (std::runtime_error subclass) on failure.
 *   Catch it via `catch (const flux::Error& e)`.
 *
 * Quick start:
 *
 *   #include "flux.hpp"
 *   #include <iostream>
 *
 *   int main() {
 *       std::vector<uint8_t> src = {1, 2, 3, 4, 5};
 *
 *       flux::Options opts;
 *       opts.level = flux::Level_Balanced;
 *
 *       auto compressed = flux::compress(src, opts);
 *       auto recovered  = flux::decompress(compressed, opts);  // size auto-queried
 *
 *       assert(recovered == src);   // byte-perfect round-trip
 *
 *       // Archive a directory:
 *       flux::compress_directory("my_data", "archive.flx", opts);
 *   }
 */

#pragma once
#ifndef FLUX_HPP
#define FLUX_HPP

#include "flux.h"

#include <cstdint>
#include <cstring>
#include <filesystem>
#include <functional>
#include <stdexcept>
#include <string>
#include <vector>

namespace flux {

/* =========================================================================
 * Level constants (aliases for the C enum)
 * ========================================================================= */
constexpr FluxCompressionLevel Level_Tiny     = FLUX_LEVEL_TINY;
constexpr FluxCompressionLevel Level_Fast     = FLUX_LEVEL_FAST;
constexpr FluxCompressionLevel Level_Balanced = FLUX_LEVEL_BALANCED;
constexpr FluxCompressionLevel Level_Maximum  = FLUX_LEVEL_MAXIMUM;
constexpr FluxCompressionLevel Level_Extreme  = FLUX_LEVEL_EXTREME;

/* =========================================================================
 * Error type
 * =========================================================================
 * Thrown by all flux:: functions on failure.
 *
 *   try { auto c = flux::compress(src, opts); }
 *   catch (const flux::Error& e) {
 *       std::cerr << e.what() << "  (code " << e.code() << ")\n";
 *   }
 * ========================================================================= */
class Error : public std::runtime_error {
public:
    explicit Error(FluxResult r, const std::string& msg = {})
        : std::runtime_error(msg.empty() ? default_message(r) : msg)
        , result_(r) {}

    /** The raw FluxResult code that caused this exception. */
    FluxResult code() const noexcept { return result_; }

private:
    FluxResult result_;

    static std::string default_message(FluxResult r) {
        switch (r) {
            case FLUX_OK:                return "ok";
            case FLUX_ERR_GENERIC:       return "flux: generic error";
            case FLUX_ERR_INVALID_PARAM: return "flux: invalid parameter";
            case FLUX_ERR_NULL_POINTER:  return "flux: null pointer";
            case FLUX_ERR_IO:            return "flux: I/O error";
            case FLUX_ERR_CORRUPT_DATA:  return "flux: corrupt or invalid archive";
            case FLUX_ERR_WRONG_PASS:    return "flux: wrong password";
            case FLUX_ERR_BUFFER_SMALL:  return "flux: output buffer too small";
            default:                     return "flux: unknown error";
        }
    }
};

namespace detail {
inline void check(FluxResult r) {
    if (r != FLUX_OK) throw Error(r);
}
} /* namespace detail */

/* =========================================================================
 * Options
 * =========================================================================
 * Thin C++ wrapper around FluxOptions.  Owns the password string so callers
 * don't have to manage raw char* lifetimes.
 * ========================================================================= */

/**
 * flux::Options — configuration for compress / decompress calls.
 *
 * Safe to copy and move.  On copy the password string is deep-copied so
 * each Options object owns independent storage.
 *
 *   flux::Options opts;
 *   opts.level    = flux::Level_Maximum;
 *   opts.password = "s3cr3t";   // leave empty for no encryption
 */
class Options {
public:
    FluxCompressionLevel level        = FLUX_LEVEL_BALANCED;
    uint32_t             thread_count = 0;   /* 0 = auto */
    uint32_t             block_size   = 0;   /* 0 = auto */
    std::string          password;           /* empty = no encryption */

    /**
     * Convert to the C struct expected by the API.
     * The returned FluxOptions is valid as long as this Options object is
     * alive and the password field has not been modified.
     */
    FluxOptions to_c() const noexcept {
        FluxOptions o{};
        o.password     = password.empty() ? nullptr : password.c_str();
        o.thread_count = thread_count;
        o.block_size   = block_size;
        o.level        = level;
        return o;
    }
};

/* =========================================================================
 * Progress event (C++ mirror of FluxProgressEvent)
 * ========================================================================= */

/**
 * flux::ProgressEvent — passed to a ProgressFn during compress / decompress.
 *
 * current_file is empty if not applicable.
 */
struct ProgressEvent {
    uint64_t    bytes_processed   = 0;
    uint64_t    bytes_total       = 0;
    std::string current_file;
    float       compression_ratio = 0.f;
    uint32_t    detected_stride   = 0;
    float       buffer_fill_level = 0.f;

    static ProgressEvent from_c(const FluxProgressEvent& c) {
        ProgressEvent e;
        e.bytes_processed   = c.bytes_processed;
        e.bytes_total       = c.bytes_total;
        e.current_file      = c.current_file ? c.current_file : "";
        e.compression_ratio = c.compression_ratio;
        e.detected_stride   = c.detected_stride;
        e.buffer_fill_level = c.buffer_fill_level;
        return e;
    }
};

/** Signature for C++ progress callbacks. */
using ProgressFn = std::function<void(const ProgressEvent&)>;

namespace detail {

/* Trampoline: bridges the C callback ABI into a C++ std::function. */
inline void progress_trampoline(FluxProgressEvent ev, void* ud) {
    auto* fn = static_cast<ProgressFn*>(ud);
    (*fn)(ProgressEvent::from_c(ev));
}

} /* namespace detail */

/* =========================================================================
 * Free functions
 * ========================================================================= */

/**
 * version — return the engine version string (e.g. "0.1.0").
 */
inline std::string version() {
    return flux_v1_get_version();
}

/**
 * get_decompressed_size — query the original size stored in an archive header.
 *
 * Reads only the header (does NOT decompress data).  Use the result to
 * pre-allocate an exact buffer for decompress().
 *
 * For encrypted archives, opts.password must be set.
 *
 * @throws flux::Error on failure (wrong password, corrupt archive, I/O error).
 */
inline std::size_t get_decompressed_size(
    const std::vector<uint8_t>& compressed,
    const Options&              opts = {}
) {
    FluxOptions c_opts = opts.to_c();
    std::size_t sz = 0;
    FluxResult r = flux_v1_get_decompressed_size(
        compressed.data(), compressed.size(), c_opts, &sz);
    detail::check(r);
    return sz;
}

/**
 * compress — compress an in-memory buffer.
 *
 * @param input     Source data.
 * @param opts      Compression options.
 * @param progress  Optional progress callback.  Pass {} to disable.
 *
 * @return  Compressed archive bytes as a byte vector.
 * @throws  flux::Error on any failure.
 *
 * The output buffer is sized conservatively.  If the library reports
 * FLUX_ERR_BUFFER_SMALL the capacity is doubled and the call is retried
 * once before throwing.
 */
inline std::vector<uint8_t> compress(
    const std::vector<uint8_t>& input,
    const Options&              opts     = {},
    ProgressFn                  progress = {}
) {
    FluxOptions c_opts = opts.to_c();

    std::size_t capacity = input.size() + input.size() / 8 + 4096;
    std::vector<uint8_t> output(capacity);
    std::size_t out_len = capacity;

    FluxProgressCallback cb_ptr  = progress ? detail::progress_trampoline : nullptr;
    void*                cb_data = progress ? static_cast<void*>(&progress) : nullptr;

    FluxResult r = flux_v1_compress(
        input.data(), input.size(),
        output.data(), &out_len,
        c_opts, cb_ptr, cb_data);

    if (r == FLUX_ERR_BUFFER_SMALL) {
        /* Library told us the exact required size — use it directly. */
        capacity = out_len;
        output.resize(capacity);
        r = flux_v1_compress(
            input.data(), input.size(),
            output.data(), &out_len,
            c_opts, cb_ptr, cb_data);
    }

    detail::check(r);
    output.resize(out_len);
    return output;
}

/**
 * decompress — decompress a FLUX buffer, querying the original size automatically.
 *
 * Calls flux_v1_get_decompressed_size to allocate the exact output buffer,
 * then decompresses.  For encrypted archives, opts.password must be set.
 *
 * @param compressed  Archive bytes produced by compress().
 * @param opts        Options; set opts.password for encrypted archives.
 * @param progress    (unused — decompress does not yet report progress)
 *
 * @return  Decompressed data.
 * @throws  flux::Error on failure (corrupt data, wrong password, etc.).
 */
inline std::vector<uint8_t> decompress(
    const std::vector<uint8_t>& compressed,
    const Options&              opts     = {},
    ProgressFn                  /*progress*/ = {}
) {
    /* Query the exact original size from the archive header. */
    std::size_t original_size = get_decompressed_size(compressed, opts);

    std::vector<uint8_t> output(original_size);
    std::size_t out_len = original_size;

    FluxOptions c_opts = opts.to_c();
    FluxResult r = flux_v1_decompress(
        compressed.data(), compressed.size(),
        output.data(), &out_len,
        c_opts);
    detail::check(r);
    output.resize(out_len);
    return output;
}

/**
 * decompress — decompress with a caller-supplied original size.
 *
 * Use this overload when you already know the original size (e.g. stored
 * alongside the archive) and want to skip the header-query round-trip.
 *
 * @param compressed    Archive bytes produced by compress().
 * @param original_size Expected decompressed byte count.
 * @param opts          Options; set opts.password for encrypted archives.
 *
 * @throws flux::Error on failure.
 */
inline std::vector<uint8_t> decompress(
    const std::vector<uint8_t>& compressed,
    std::size_t                  original_size,
    const Options&               opts = {}
) {
    std::vector<uint8_t> output(original_size);
    std::size_t out_len = original_size;

    FluxOptions c_opts = opts.to_c();
    FluxResult r = flux_v1_decompress(
        compressed.data(), compressed.size(),
        output.data(), &out_len,
        c_opts);
    detail::check(r);
    output.resize(out_len);
    return output;
}

/**
 * compress_directory — compress an entire directory tree into an archive file.
 *
 * @param src_dir      Path to the source directory (or single file).
 * @param dest_archive Path for the output archive file.
 * @param opts         Compression options.
 * @param progress     Optional progress callback.
 *
 * @throws flux::Error on any failure.
 */
inline void compress_directory(
    const std::filesystem::path& src_dir,
    const std::filesystem::path& dest_archive,
    const Options&               opts     = {},
    ProgressFn                   progress = {}
) {
    std::string src  = src_dir.string();
    std::string dest = dest_archive.string();
    FluxOptions c_opts = opts.to_c();

    FluxProgressCallback cb_ptr  = progress ? detail::progress_trampoline : nullptr;
    void*                cb_data = progress ? static_cast<void*>(&progress) : nullptr;

    FluxResult r = flux_v1_compress_directory(
        src.c_str(), dest.c_str(), c_opts, cb_ptr, cb_data);
    detail::check(r);
}

} /* namespace flux */

#endif /* FLUX_HPP */
