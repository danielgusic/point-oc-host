//! Minimal wasmtime embedder ("host") for the `point-oc` guest module.
//!
//! `point-oc` is a railway point (switch) object controller compiled to wasm.
//! It imports three host functions from module `env`:
//!
//!   recv_msg(buf: *mut u8) -> usize    host writes the next SCI telegram into
//!                                       guest memory at `buf`, returns its length
//!   send_msg(msg: *const u8, len)      guest hands the host a response telegram
//!   move_point(cmd: i32) -> i32        physically move the switch
//!                                       cmd: 0 = Left, 1 = Right
//!                                       ret: 0 = EndPositionArrived, 1 = Trailed
//!
//! Its `main` is an infinite recv/handle/respond loop. We drive it by feeding a
//! scripted sequence of telegrams; when the inbox drains we make `recv_msg` trap
//! to unwind the loop, and treat that one trap as a clean shutdown.
//!
//! Usage:
//!   point-oc-host [WASM_PATH] [--an] [--an-constant N] [--bench N]
//!
//!   WASM_PATH       defaults to the debug build of the sibling crate
//!   --an            enable AN-encoding (Config::an_encoding)
//!   --an-constant N override the AN constant A
//!   --bench N       benchmark mode: cycle the scripted conversation until N
//!                   telegrams have been served, suppress per-telegram output,
//!                   and report wall time + telegrams/sec for the guest run

use std::collections::VecDeque;

use anyhow::Result;
use sci_rs::{
    SCIMessageType, SCITelegram,
    scip::{SCIPointLocation, SCIPointTargetLocation},
};
// Note: this fork's `wasmtime` has its own error type. Host closures must
// return `wasmtime::Result<T>`, so use wasmtime's `bail!`/`format_err!` there
// (there is a `From<wasmtime::Error> for anyhow::Error`, but not the reverse).
use wasmtime::{Caller, Config, Engine, Linker, Module, ProfilingStrategy, Store, bail, format_err};

const DEFAULT_WASM: &str = "../wasm-point-oc/target/wasm32-unknown-unknown/debug/point-oc.wasm";
// const DEFAULT_WASM: &str = "../wasm-point-oc/target/wasm32-unknown-unknown/release/point-oc.wasm";
const CONTROLLER: &str = "IXL"; // the interlocking talking to the point
const POINT: &str = "P01"; // must match the guest's SELF_ID

/// Everything the host functions need to share.
struct HostState {
    /// Telegrams still to be delivered to the guest, in order.
    inbox: VecDeque<Vec<u8>>,
    /// Bench mode: the script to cycle through once `inbox` is drained.
    script: Vec<Vec<u8>>,
    /// Bench mode: how many telegrams are still to be served from `script`.
    remaining: usize,
    /// Bench mode: index of the next `script` entry to serve.
    cursor: usize,
    /// Suppress per-telegram output (bench mode).
    quiet: bool,
    /// Set once the inbox is empty so the final trap is recognised as clean.
    drained: bool,
    /// How many physical moves the guest requested.
    moves: usize,
}

fn main() -> Result<()> {
    let mut wasm_path = DEFAULT_WASM.to_string();
    let mut an = false;
    let mut an_constant: Option<u64> = None;
    let mut bench: Option<usize> = None;
    let mut perfmap = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--an" => an = true,
            "--an-constant" => {
                an_constant = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--an-constant needs a value"))?
                        .parse()?,
                );
            }
            "--bench" => {
                bench = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--bench needs a telegram count"))?
                        .parse()?,
                );
            }
            "--perfmap" => perfmap = true,
            "-h" | "--help" => {
                println!(
                    "usage: point-oc-host [WASM_PATH] [--an] [--an-constant N] [--bench N] [--perfmap]"
                );
                return Ok(());
            }
            other => wasm_path = other.to_string(),
        }
    }

    let mut config = Config::new();
    if an {
        config.an_encoding(true);
        if let Some(a) = an_constant {
            config.an_constant(a);
        }
    }
    if perfmap {
        // Emit /tmp/perf-<pid>.map so an external sampler (samply) can
        // symbolize the JIT-compiled wasm frames in the flamegraph.
        config.profiler(ProfilingStrategy::PerfMap);
    }

    println!(
        "host: loading {wasm_path}\nhost: AN-encoding {}{}\n",
        if an { "ON" } else { "off" },
        an_constant.map(|a| format!(" (A={a})")).unwrap_or_default(),
    );

    let engine = Engine::new(&config)?;
    let module = Module::from_file(&engine, &wasm_path)?;

    let mut linker = Linker::new(&engine);

    // recv_msg(buf) -> len : pop the next scripted telegram into guest memory.
    linker.func_wrap(
        "env",
        "recv_msg",
        |mut caller: Caller<'_, HostState>, buf: i32| -> wasmtime::Result<i32> {
            let state = caller.data_mut();
            let bytes = if let Some(bytes) = state.inbox.pop_front() {
                bytes
            } else if state.remaining > 0 {
                // Bench mode: cycle through the script without materializing
                // millions of telegrams up front.
                let bytes = state.script[state.cursor].clone();
                state.cursor = (state.cursor + 1) % state.script.len();
                state.remaining -= 1;
                bytes
            } else {
                state.drained = true;
                bail!("inbox drained — unwinding the guest loop");
            };
            let mem = caller
                .get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| format_err!("guest has no exported memory"))?;
            mem.write(&mut caller, buf as usize, &bytes)?;
            if !caller.data().quiet {
                println!("  → recv  {}", describe(&bytes));
            }
            Ok(bytes.len() as i32)
        },
    )?;

    // send_msg(ptr, len) : read a response telegram out of guest memory.
    linker.func_wrap(
        "env",
        "send_msg",
        |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> wasmtime::Result<()> {
            let mem = caller
                .get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| format_err!("guest has no exported memory"))?;
            let mut bytes = vec![0u8; len as usize];
            mem.read(&caller, ptr as usize, &mut bytes)?;
            if !caller.data().quiet {
                println!("  ← send  {}", describe(&bytes));
            }
            Ok(())
        },
    )?;

    // move_point(cmd) -> result : pretend the switch always reaches its end position.
    // Return 1 instead of 0 here to exercise the guest's Trailed -> PointBumped path.
    linker.func_wrap(
        "env",
        "move_point",
        |mut caller: Caller<'_, HostState>, cmd: i32| -> i32 {
            caller.data_mut().moves += 1;
            if !caller.data().quiet {
                let dir = if cmd == 0 { "Left" } else { "Right" };
                println!("  ⚙ move_point({dir}) => EndPositionArrived");
            }
            0 // 0 = EndPositionArrived, 1 = Trailed
        },
    )?;

    let state = match bench {
        Some(n) => HostState {
            inbox: VecDeque::new(),
            script: build_script().into(),
            remaining: n,
            cursor: 0,
            quiet: true,
            drained: false,
            moves: 0,
        },
        None => HostState {
            inbox: build_script(),
            script: Vec::new(),
            remaining: 0,
            cursor: 0,
            quiet: false,
            drained: false,
            moves: 0,
        },
    };
    let mut store = Store::new(&engine, state);

    let instance = linker.instantiate(&mut store, &module)?;
    let entry = instance.get_typed_func::<(i32, i32), i32>(&mut store, "main")?;

    println!("host: starting guest\n");
    let start = std::time::Instant::now();
    let result = entry.call(&mut store, (0, 0));
    let elapsed = start.elapsed();
    match result {
        Ok(code) => println!("\nhost: guest main returned {code} (loop exited on its own?)"),
        Err(err) => {
            if store.data().drained {
                println!(
                    "\nhost: ✓ run complete — {} physical move(s), inbox fully processed",
                    store.data().moves
                );
                if let Some(n) = bench {
                    println!(
                        "host: bench — {n} telegrams in {elapsed:.3?} ({:.0} telegrams/sec)",
                        n as f64 / elapsed.as_secs_f64()
                    );
                }
            } else {
                // A real trap (e.g. AN-encoding mismatch). Surface it.
                eprintln!("\nhost: ✗ guest trapped: {err:?}");
                return Err(err.into());
            }
        }
    }

    Ok(())
}

/// The scripted conversation the controller plays toward the point.
fn build_script() -> VecDeque<Vec<u8>> {
    let telegrams = vec![
        // Ask for the current location (guest ignores the payload here).
        SCITelegram::location_status(CONTROLLER, POINT, SCIPointLocation::PointNoTargetLocation),
        // Throw the point to the right, then re-query.
        SCITelegram::change_location(
            CONTROLLER,
            POINT,
            SCIPointTargetLocation::PointLocationChangeToRight,
        ),
        SCITelegram::location_status(CONTROLLER, POINT, SCIPointLocation::PointNoTargetLocation),
        // Throw it back to the left, then re-query.
        SCITelegram::change_location(
            CONTROLLER,
            POINT,
            SCIPointTargetLocation::PointLocationChangeToLeft,
        ),
        SCITelegram::location_status(CONTROLLER, POINT, SCIPointLocation::PointNoTargetLocation),
    ];
    telegrams.into_iter().map(<Vec<u8>>::from).collect()
}

/// Decode an SCI-P telegram into a one-line human description.
fn describe(bytes: &[u8]) -> String {
    let t = match SCITelegram::try_from(bytes) {
        Ok(t) => t,
        Err(e) => return format!("<unparseable {} bytes: {e}>", bytes.len()),
    };
    let body = if t.message_type == SCIMessageType::scip_change_location() {
        format!("ChangeLocation -> {}", target_name(t.payload.data[0]))
    } else if t.message_type == SCIMessageType::scip_location_status() {
        format!("LocationStatus = {}", location_name(t.payload.data[0]))
    } else {
        format!("msgtype {:#06x}", u16::from(t.message_type))
    };
    let s = t.sender.trim_end_matches('_');
    let r = t.receiver.trim_end_matches('_');
    format!("[{s} -> {r}] {body}")
}

fn target_name(v: u8) -> &'static str {
    match v {
        0x01 => "Right",
        0x02 => "Left",
        _ => "?",
    }
}

fn location_name(v: u8) -> &'static str {
    match v {
        0x01 => "Right",
        0x02 => "Left",
        0x03 => "NoTarget",
        0x04 => "Bumped",
        _ => "?",
    }
}
