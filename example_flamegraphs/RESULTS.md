# point-oc: normal vs. AN-encoded performance

Measured with the `point-oc-host` embedder driving the `point-oc` guest wasm,
using the host's `--bench` mode (cycles the scripted SCI-P conversation N times,
output suppressed) on the AN-encoding `wasmtime-an` fork.

- Host: built `--profile profiling` (release + line tables, unstripped) with
  rustc 1.93.0 (the fork's MSRV).
- Guest: `wasm-point-oc` for `wasm32-unknown-unknown`, profiled in **both** debug
  and release builds (the host loads whichever `.wasm` you pass as its first arg).
- Flamegraphs: `cargo flamegraph` (perf, 997 Hz, **frame-pointer** call-graph),
  with the host's `--perfmap` flag so the JIT-compiled wasm/AN frames symbolize
  by name. See "Note on `[unknown]` frames" below for why fp, not dwarf.
- Measured against `wasmtime-an` commit `14c30cb` (see "Fork update" below).

### Files here

| file | guest build | AN |
|---|---|---|
| `an-off.svg`         | debug   | off |
| `an-on.svg`          | debug   | on (A=65521) |
| `an-off-release.svg` | release | off |
| `an-on-release.svg`  | release | on (A=65521) |

## Throughput (clean runs, no profiler attached)

| Configuration            | debug guest | release guest |
|--------------------------|------------:|--------------:|
| AN-encoding **off**      | 351,659 tel/s (~2.8 µs) | **1,346,930 tel/s (~0.74 µs)** |
| AN-encoding **on**       |  58,223 tel/s (~17 µs)  |   **376,965 tel/s (~2.7 µs)** |
| **AN slowdown**          | **~6.0×**   | **~3.6×**     |

This is a **massive** improvement over the previous fork commit, where AN-on ran
at ~2,600 tel/s and the slowdown was ~144× (debug) / ~529× (release). The update
replaced the per-boundary whole-memory cross-check with verify-at-use (see "Fork
update"), collapsing AN overhead from ~96% of runtime to ~10%.

Note the relative penalty now *narrows* with a faster guest (6.0× debug → 3.6×
release) — the opposite of before. That is the signature of AN cost that scales
with **work actually done** (bytes touched per telegram) rather than with the
*size of linear memory*: a faster guest does the same per-telegram AN work over a
shorter baseline, but the AN work is now a roughly fixed *fraction* of each
telegram instead of a fixed huge sweep.

## Where the time goes

### AN-on — now guest-bound, AN overhead is a thin slice

With the whole-memory sweep gone, both AN-on graphs look almost like their AN-off
counterparts: the dominant cost is the **guest itself** (per-telegram
allocation + SCI (de)serialization), not the AN runtime. For the release guest
(`an-on-release.svg`), self-time:

| bucket | ~self-time | what |
|---|---:|---|
| guest allocator (`dlmalloc` malloc/free/realloc/unlink) | ~35% | the response `Vec<u8>` per telegram |
| guest SCI (de)serialization (`sci_rs` From/TryFrom, `Utf8Chunks::next`, `from_utf8_lossy`, `location_status`) | ~24% | telegram encode/decode + string-ID handling |
| `memory_copy` / mmap plumbing | ~8% | guest↔host buffer copies |
| **AN machinery** | **~10%** | see below |
| trampolines / trap plumbing / misc | remainder | |

The **AN machinery** is now three small, *targeted* costs — all O(bytes touched),
not O(memory size):

| AN cost | ~self-time | what it does |
|---|---:|---|
| `an_encode_range_from_raw`  | ~4.1% | re-encode **only the exact bytes** a host write touched (`Memory::write`) |
| `an_cross_check_range` (`…_range_parts`) | ~4.1% | verify-at-use: slot-compare **only the exact range** a host read borrowed |
| `an_resync_host_boundary`   | ~1.3% | post-host-call dirty-heal libcall (re-encodes only memories borrowed wholesale; nothing is whole-dirty here, so this is near-idle) |

There is **no** `an_check_host_boundary` / `an_cross_check_memory` /
`an_cross_check_parts` in the profile anymore — the whole-memory sweep was
removed. The guest-read half of verify-at-use (the inline `enc == A*raw`
assertion emitted per touched slot right after each `i32.load`) lives *inside* the
JIT'd guest code, so it shows up as part of the guest cost, not as a separate
runtime symbol.

### AN-off — baseline cost

- **Debug guest (`an-off.svg`):** dominated by host-side libc memory routines
  (`__memmove_*`, `__memset_*`, plus malloc/free) — SCI telegram serialization —
  with the slow debug guest layered on top.
- **Release guest (`an-off-release.svg`):** balanced, no single hotspot (which is
  why it's fast). Roughly: ~18% guest allocator (`dlmalloc`), ~12% guest telegram
  (de)serialization (`sci_rs` + `Utf8Chunks::next`), ~12% wasmtime per-call
  plumbing (export/memory resolution by name on every recv/send), ~8% memory
  copies. AN-on (release) is essentially this same profile plus the ~10% AN slice
  above.

## Interpretation

The AN integrity model changed from **verify-at-boundary** to **verify-at-use**.

- **Before:** every crossing of the host/guest boundary triggered a cross-check of
  the *entire* guest linear memory (`shadow == A*raw` over every 4-byte slot).
  Cost was O(linear-memory-size) per crossing and the point-oc loop crosses the
  boundary several times per telegram, so it dominated everything (~96%) and was
  completely independent of how much work the guest actually did.
- **After:** corruption is caught *where the data is used* instead. The guest's own
  `i32.load`s carry an inline per-slot `enc == A*raw` assertion, and the host-side
  accessors cross-check only the **exact byte range** they read
  (`an_cross_check_range`); host writes re-encode only the **exact range** they
  wrote (`an_encode_range_from_raw`). Cost is now O(bytes-actually-touched),
  which for point-oc is a kilobyte-ish telegram buffer, not the whole heap.

That single change is why AN-on went from ~2.6k to ~58k (debug) / ~377k
(release) tel/s. The remaining AN overhead (~10%) is genuine per-use work and
scales with traffic, not with memory footprint.

## Fork update (`fea3d57` → `14c30cb`)

- **What changed:** the host-boundary whole-memory cross-check was **removed** and
  replaced by verify-at-use. Per the fork's changelog: the pre-call
  whole-memory cross-check is gone; reads verify their exact range at the point of
  use (host `Memory::read`/`data`, component lifting, WASI wiggle reads, and the
  inline guest-load assertion), writes re-encode their exact range at the write
  site, and the post-call `an_resync_host_boundary` libcall only *dirty-heals*
  memories the host borrowed wholesale via `Memory::data_mut`.
- **Host source touched:** the fork also **removed `Config::an_load_validity_check`**
  — the load-side validity check is now mandatory/always-on under AN, so the toggle
  was redundant. `point-oc-host` previously called it behind a `--an-check` flag;
  that flag and its plumbing have been removed from `src/main.rs` (it no longer
  compiled, and the feature it gated is now unconditional). The four benchmark/
  flamegraph runs never used `--an-check`, so measured behaviour is unchanged.
- **Why throughput jumped ~140×:** the dominant cost on this workload *was* the
  O(linear-memory) per-crossing sweep. Removing it leaves only O(bytes-touched)
  range checks, so AN-on is now within a small constant factor of the unencoded
  baseline.

## Note on `[unknown]` frames

An early pass used perf's default **dwarf** call-graph and produced a single
`[unknown]` frame ~99% wide, plus bogus caller addresses like `0x3ffe0`. That is
*not* lost time and *not* a missing symbol. The cause is that DWARF stack
unwinding relies on `.eh_frame`/CFI, which exists for the native ELF but **not for
wasmtime's JIT-compiled code**. When the unwinder tries to walk *up* the stack
across a wasm/trampoline frame it has no CFI for, it fabricates garbage return
addresses that perf can't symbolize → `[unknown]`.

The fix is to unwind with **frame pointers** (`--call-graph fp`) instead: wasmtime
keeps frame pointers in its JIT code precisely so backtraces cross the wasm
boundary, and (Fedora's) glibc is also built with frame pointers. With fp the
bogus frames vanish, the JIT'd wasm frames symbolize via the perfmap
(`wasm[0]::array_to_wasm_trampoline[…]`, `point_oc::main`, …), and residual
`[unknown]` drops to ~2% (unavoidable prologue/epilogue/leaf samples).

## Reproduce

```sh
# one-time: rustup target add wasm32-unknown-unknown

# guest — .cargo/config.toml targets wasm32-unknown-unknown
( cd wasm-point-oc && cargo build )            # debug   -> target/.../debug/point-oc.wasm
( cd wasm-point-oc && cargo build --release )  # release -> target/.../release/point-oc.wasm

# host (profiling profile; rustc 1.93.0 is pinned for this dir via `rustup override`)
( cd point-oc-host && cargo build --profile profiling )

# clean timing (default arg = debug wasm; pass a path for the release wasm)
HOST=point-oc-host/target/profiling/point-oc-host
REL=wasm-point-oc/target/wasm32-unknown-unknown/release/point-oc.wasm
$HOST            --bench 4000000          # AN off, debug guest
$HOST       --an --bench 4000000          # AN on,  debug guest
$HOST $REL       --bench 14000000         # AN off, release guest
$HOST $REL  --an --bench 4000000          # AN on,  release guest
```

Or just run the provided script, which builds everything and writes all four SVGs
here:

```sh
./point-oc-host/make_flamegraphs.sh
```
