# FLUX C / C++ Headers — Build Notes

## Files

| File | Purpose |
|------|---------|
| `flux.h`   | C99 header — declares the full C ABI. Use this for pure-C projects. |
| `flux.hpp` | C++17 header-only wrapper — includes `flux.h` and provides RAII/idiomatic C++ interface. |

## Library name

| Platform | Shared library | Import library (link-time) |
|----------|---------------|---------------------------|
| Windows  | `flux_core_v1.dll` | `flux_core_v1.dll.lib` |
| Linux    | `libflux_core_v1.so` | — (link directly) |

Built by: `cargo build -p flux-core --release`
Output: `target/release/`

---

## Compiling with GCC / Clang (Linux)

```bash
# C
gcc -std=c99 -I include -L target/release \
    -lflux_core_v1 examples/example_usage.c -o example_c

# C++
g++ -std=c++17 -I include -L target/release \
    -lflux_core_v1 examples/example_usage.cpp -o example_cpp

# Runtime: tell the loader where the .so lives
export LD_LIBRARY_PATH=target/release
./example_cpp
```

## Compiling with MinGW (Windows)

MinGW's linker requires a GNU-format import library (`.dll.a`), not the MSVC
`.dll.lib` produced by Rust.  Generate it once from the provided def file:

```bash
# 1. Generate the MinGW import library (one-time step after each build)
dlltool -d include/flux_core_v1.def \
        -l target/release/libflux_core_v1.dll.a

# 2. Compile — source files must come BEFORE the import library
# C
gcc -std=c99 -I include \
    examples/example_usage.c target/release/libflux_core_v1.dll.a \
    -o example_c.exe

# C++
g++ -std=c++17 -I include \
    examples/example_usage.cpp target/release/libflux_core_v1.dll.a \
    -o example_cpp.exe

# 3. At runtime, flux_core_v1.dll must be next to the .exe or on PATH
cp target/release/flux_core_v1.dll .
./example_cpp.exe
```

The def file `include/flux_core_v1.def` is committed alongside the headers.
If the exported symbols ever change, update it to match.

## Compiling with MSVC (Windows)

```bat
REM C
cl /std:c17 /I include examples\example_usage.c ^
   /link /LIBPATH:target\release flux_core_v1.dll.lib

REM C++
cl /std:c++17 /I include examples\example_usage.cpp ^
   /link /LIBPATH:target\release flux_core_v1.dll.lib

REM Copy DLL next to the .exe before running
copy target\release\flux_core_v1.dll .
example_usage.exe
```

---

## Windows DLL placement note

`flux_core_v1.dll` must be findable at runtime.  Options:
1. Copy it next to the executable.
2. Add `target\release` to the system `PATH`.
3. Install it to a directory already on `PATH` (e.g. `C:\Windows\System32` — not recommended for development).

---

## API overview (C)

```c
#include "flux.h"

// Query version
const char *ver = flux_v1_get_version();

// Compress in memory
FluxOptions opts; memset(&opts, 0, sizeof(opts)); opts.level = FLUX_LEVEL_BALANCED;
uint8_t out[BIG_ENOUGH]; size_t out_len = sizeof(out);
FluxResult r = flux_v1_compress(src, src_len, out, &out_len, opts, NULL, NULL);

// Decompress in memory
uint8_t raw[ORIGINAL_SIZE]; size_t raw_len = sizeof(raw);
r = flux_v1_decompress(out, out_len, raw, &raw_len, opts);

// Archive a directory
r = flux_v1_compress_directory("my_data", "archive.flx", opts, NULL, NULL);
```

## API overview (C++)

```cpp
#include "flux.hpp"

auto ver = flux::version();

flux::Options opts;
opts.level = flux::Level_Balanced;

auto compressed = flux::compress(src_vec, opts);
auto recovered  = flux::decompress(compressed, src_vec.size(), opts);

flux::compress_directory("my_data", "archive.flx", opts);
```

All C++ functions throw `flux::Error` (derives from `std::runtime_error`) on failure.
