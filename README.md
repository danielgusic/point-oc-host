**Note:** This was created with the help of AI.

# point-oc-host

A minimal embedder ("host") intended to use [wasmtime-an](https://github.com/danielgusic/wasmtime-an/) that runs the
[`point-oc`](https://github.com/ctiedt/wasm-point-oc) guest module so it can be exercised against the
AN-encoding wasmtime fork.

## What `point-oc` is

`point-oc` is a railway **point (switch) object controller** compiled to wasm.
It speaks the SCI-P protocol (via the `sci-rs` crate) and imports three host
functions from module `env`:

| import | signature | meaning |
|---|---|---|
| `recv_msg` | `(buf: *mut u8) -> usize` | host writes the next SCI telegram into guest memory at `buf`, returns its length |
| `send_msg` | `(msg: *const u8, len: usize)` | guest hands the host a response telegram |
| `move_point` | `(cmd: i32) -> i32` | move the switch; `cmd` 0=Left 1=Right, ret 0=EndPositionArrived 1=Trailed |

Its `main` is an infinite recv → handle → respond loop. On a `LocationStatus`
request it replies with its current location; on a `ChangeLocation` command it
calls `move_point`, updates state (Bumped if trailed), and replies.

## What this host does

- Defines the three `env` host functions (reusing `sci-rs` to build/parse
  telegrams).
- Feeds a scripted conversation: query → throw right → query → throw left →
  query. When the inbox drains, `recv_msg` traps to unwind the guest loop; that
  one trap is treated as a clean shutdown.
- Links the **fork's** `wasmtime` crate by path so `Config::an_encoding(true)`
  is available (crates.io wasmtime has no such method).

## Build & run

This host expects the guest crate checked out as a **sibling directory** named
`wasm-point-oc` (the default WASM path is `../wasm-point-oc/...`). Lay the two
repos out side by side:

```sh
git clone https://github.com/danielgusic/point-oc-host
git clone https://github.com/ctiedt/wasm-point-oc
# parent/
# ├── point-oc-host   (this repo)
# └── wasm-point-oc   (the guest)
```

Then build the guest to wasm and run the host:

```sh
# one-time: the guest compiles to wasm
rustup target add wasm32-unknown-unknown

# build the guest first (plain `cargo build` targets wasm via .cargo/config.toml)
( cd ../wasm-point-oc && cargo build )

# the host pulls the AN-encoding wasmtime fork on first build (a few minutes).
# requires a recent Rust toolchain (edition 2024).
cargo run                          # AN-encoding off
cargo run -- --an                  # AN-encoding on (default A=65521)
cargo run -- --an --an-constant 1  # AN on, identity constant
cargo run -- --an --an-check       # AN on + load-side validity check
cargo run -- --bench 100           # Run the scripted conversation 100 times
cargo run -- /path/to/other.wasm   # run a different guest
```

## Disassembling the compiled module

This needs the `wasmtime` CLI built from the AN-encoding fork (the crates.io
CLI has no `an-encoding` option). Build it once from a checkout of the fork:

```sh
git clone https://github.com/danielgusic/wasmtime-an
( cd wasmtime-an && cargo build -p wasmtime-cli )
WT=wasmtime-an/target/debug/wasmtime

# precompile (AN on) to a .cwasm
$WT compile -C an-encoding=y -C cache=n \
    ../wasm-point-oc/target/wasm32-unknown-unknown/debug/point-oc.wasm \
    -o /tmp/point-oc.cwasm

$WT objdump /tmp/point-oc.cwasm                 # native disassembly, all funcs
$WT objdump --funcs all /tmp/point-oc.cwasm     # with per-function headers
$WT explore ...point-oc.wasm -o explore.html    # interactive wasm <-> native
$WT compile -C an-encoding=y --emit-clif /tmp/clif ...  # Cranelift IR
```
