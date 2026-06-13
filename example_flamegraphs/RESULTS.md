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
| AN-encoding **off**      | 355,294 tel/s (~2.8 µs) | **1,368,415 tel/s (~0.73 µs)** |
| AN-encoding **on**       |   2,466 tel/s (~405 µs) |     **2,574 tel/s (~389 µs)** |
| **AN slowdown**          | **~144×**   | **~532×**     |

(For reference, AN-on with the identity constant `A=1` runs at ~2,502 tel/s —
just as slow as `A=65521`. The cost is *not* the AN arithmetic, but the
integrity machinery that runs regardless of the constant.)

The key takeaway: optimizing the guest makes the **baseline ~3.8× faster but does
nothing for AN-on**, because the AN cost is a cross-check whose size is set by the
*linear-memory bytes*, not the guest's code quality. So the relative penalty
*widens* from ~144× (debug) to ~532× (release).

## Where the time goes

### AN-on — the cross-check dominates (and is guest-independent)

Both AN-on graphs are almost entirely `an_check_host_boundary` →
`an_cross_check_memory` → `an_cross_check_parts`: **88.6%** (debug) / **96.1%**
(release). Inside that loop the self-time splits two ways — and the split barely
moves between guest builds, confirming the bottleneck is the AN runtime, not the
guest:

| half of the cross-check loop | debug | release |
|------------------------------|------:|--------:|
| compare body (`an_cross_check_parts`: unaligned `u32` load, `A*raw` multiply, compare) | ~56% | 53.9% |
| iterator (`split_at_checked` / `ChunksExact<u8>::next` + the 8-byte shadow load) | ~44% | 42.2% |

Both halves are the **comparison**. The "sync"/re-encode side
(`an_sweep_whole_dirty` / `an_encode_full_memory_from_raw`) is **~0%**, because the
host writes guest memory via `Memory::write`, which re-encodes only the exact
bytes written (`an_encode_range_from_raw`, ~0.2%) and never marks the memory
whole-dirty.

### AN-off — baseline cost

- **Debug guest (`an-off.svg`):** dominated by host-side libc memory routines
  (`__memmove_*` ~72%, `__memset_*` ~13%, plus malloc/free) — SCI telegram
  serialization — with the slow debug guest layered on top.
- **Release guest (`an-off-release.svg`):** balanced, no single hotspot (which is
  why it's fast). Self-time roughly: ~18% guest allocator (`dlmalloc`
  malloc/free/realloc — the response `Vec<u8>` per telegram), ~12% guest telegram
  (de)serialization (`sci_rs` From/TryFrom + `Utf8Chunks::next` on the string
  IDs), ~12% wasmtime per-call plumbing (`get_export_by_index_mut` / `get_memory`
  / `defined_memory_index` — the host closures resolve the exported `memory` *by
  name* on every recv/send; caching the handle would remove this), and ~8% memory
  copies (`memory_copy` / `__memmove`).

## Interpretation

With AN-encoding on, **every crossing of the host/guest boundary** (each
`recv_msg` / `send_msg` / `move_point` call) triggers a cross-check of the
*entire* guest linear memory (`an_check_host_boundary`, walking every 4-byte slot
and asserting `shadow == A * raw`). The point-oc loop crosses the boundary several
times per telegram, so this O(memory-size) sweep per crossing dominates
everything. The AN arithmetic itself is cheap; the per-boundary full-memory sweep
is the bottleneck (confirmed by the identical `A=1` result and by it being
unchanged across guest builds).

## Note on `[unknown]` frames

An early pass used perf's default **dwarf** call-graph and produced a single
`[unknown]` frame ~99% wide, plus bogus caller addresses like `0x3ffe0`. That is
*not* lost time and *not* a missing symbol: self-time (`perf report --no-children`)
always attributed ~88% to `an_check_host_boundary`. The cause is that DWARF stack
unwinding relies on `.eh_frame`/CFI, which exists for the native ELF but **not for
wasmtime's JIT-compiled code**. When the unwinder tries to walk *up* the stack
across a wasm/trampoline frame it has no CFI for, it fabricates garbage return
addresses that perf can't symbolize → `[unknown]`.

The fix is to unwind with **frame pointers** (`--call-graph fp`) instead: wasmtime
keeps frame pointers in its JIT code precisely so backtraces cross the wasm
boundary, and (Fedora's) glibc is also built with frame pointers. With fp the
bogus frames vanish, the JIT'd wasm frames symbolize via the perfmap
(`wasm[0]::array_to_wasm_trampoline[21]`, `point_oc::main`, …), and residual
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
$HOST       --an --bench 20000            # AN on,  debug guest
$HOST $REL       --bench 14000000         # AN off, release guest
$HOST $REL  --an --bench 20000            # AN on,  release guest

# flamegraphs (run from point-oc-host/) — frame-pointer unwinding for clean JIT stacks
REL=../wasm-point-oc/target/wasm32-unknown-unknown/release/point-oc.wasm
cargo flamegraph --profile profiling -c "record -F 997 --call-graph fp -g" \
  -o example_flamegraphs/an-off.svg          --       --bench 4000000  --perfmap
cargo flamegraph --profile profiling -c "record -F 997 --call-graph fp -g" \
  -o example_flamegraphs/an-on.svg           --  --an --bench 20000    --perfmap
cargo flamegraph --profile profiling -c "record -F 997 --call-graph fp -g" \
  -o example_flamegraphs/an-off-release.svg  -- "$REL"      --bench 14000000 --perfmap
cargo flamegraph --profile profiling -c "record -F 997 --call-graph fp -g" \
  -o example_flamegraphs/an-on-release.svg   -- "$REL" --an --bench 20000    --perfmap
```
