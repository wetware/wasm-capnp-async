use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use std::fs;
use tokio;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use wasmer::{Module, Store};
use wasmer_wasix::{Pipe, WasiEnv, types::wasi};

use cap::{self, echo_capnp::echoer_provider};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let wasm_path = "wasm/target/wasm32-wasi/release/wasm-capnp-async.wasm";

    // Set up the RPC provider.
    let echoer_provider: echoer_provider::Client = cap::EchoerProvider::client();

    let (mut host_w, guest_r) = Pipe::channel();
    let (host_r, mut guest_w) = Pipe::channel();

    let network = twoparty::VatNetwork::new(
        // host_r.compat(),
        // host_w.compat(),
        host_r.compat(),
        host_w.compat_write(),
        rpc_twoparty_capnp::Side::Server,
        Default::default(),
    );

    let rpc_system = RpcSystem::new(Box::new(network), Some(echoer_provider.client));

    let local = tokio::task::LocalSet::new();

    local
        .run_until(async {
            let rpc_task = tokio::task::spawn_local(rpc_system);
            let _ = tokio::join!(rpc_task);
        })
        .await;

    // Spawn the RPC system to run in the background.
    let wasm_bytes = fs::read(wasm_path)?;

    // Create a Store.
    let mut store = Store::default();

    println!("Compiling module...");
    // Let's compile the Wasm module.
    let module = Module::new(&store, wasm_bytes)?;

    {
        // From the [docs](https://github.com/wasmerio/wasmer/blob/main/examples/wasi_pipes.rs):
        // We use a scope to make sure the runner is dropped
        // as soon as we are done with it; otherwise, it will keep the stdout pipe
        // open.
        WasiEnv::builder("capnp demo")
            .stdin(Box::new(guest_r))
            .stdout(Box::new(guest_w))
            .run_with_store(module, &mut store)?;
    }

    eprintln!("Ok");
    Ok(())
}
