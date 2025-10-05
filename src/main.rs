use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use std::fs;
use std::thread;
use tokio::io::DuplexStream;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::*;
use wasmtime_wasi::cli::{AsyncStdinStream, AsyncStdoutStream};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use cap::{self, echo_capnp::echoer_provider};
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

const BUFFER_SIZE: usize = 32 * 1024 * 1024;

pub struct ComponentRunStates {
    // These two are required basically as a standard way to enable the impl of IoView and
    // WasiView.
    // impl of WasiView is required by [`wasmtime_wasi::p2::add_to_linker_sync`]
    pub wasi_ctx: WasiCtx,
    pub resource_table: ResourceTable,
}

impl WasiView for ComponentRunStates {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.resource_table,
        }
    }
}

/// The main function will:
/// 1. Set up async pipes, map them to the guest stdin/stdout
/// 2. Map the guest stderr to host tracing
/// 3. Spawn the Cap'n Proto provider on a dedicated thread
/// 4. Bootstrap the capability over the async pipes
/// 5. Spawn the guest process
/// 6. Wait for the guest to exit
#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize global tracing subscriber before any Wasmer/Cap'n Proto activity.
    {
        // Use RUST_LOG if set; otherwise default to info with useful module hints.
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new(
                "info,wasmtime=info,wasmtime_wasi=info,capnp_rpc=info,wasm_capnp_async=info",
            )
        });
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .init();
    }

    let host_span = tracing::info_span!("host");
    let _host_enter = host_span.enter();
    let wasm_path = "wasm/target/wasm32-wasip2/release/wasm.wasm";

    // Create pipes for WASI stdio and host/provider RPC network.
    // Use larger pipe buffers to reduce backpressure interactions between read/write sides.
    let (host_w, guest_r): (DuplexStream, DuplexStream) = tokio::io::duplex(BUFFER_SIZE);
    let (host_r, guest_w): (DuplexStream, DuplexStream) = tokio::io::duplex(BUFFER_SIZE);

    // Wrap guest-side ends in WASI-compatible async stdio streams.
    let guest_r_async = AsyncStdinStream::new(guest_r);
    let guest_w_async = AsyncStdoutStream::new(BUFFER_SIZE, guest_w);

    // Separate stderr so we can capture and map it to host tracing.
    let (guest_stderr_host_r, guest_stderr_guest_w): (DuplexStream, DuplexStream) =
        tokio::io::duplex(BUFFER_SIZE);
    let guest_e_async = AsyncStdoutStream::new(BUFFER_SIZE, guest_stderr_guest_w);

    // Spawn a task to read guest stderr lines and log them via tracing at info level.
    let mut stderr_reader = BufReader::new(guest_stderr_host_r);
    let stderr_task = tokio::spawn(async move {
        let mut line = String::new();
        loop {
            line.clear();
            match stderr_reader.read_line(&mut line).await {
                Ok(0) => break, // EOF
                Ok(_) => {
                    let msg = line.trim_end_matches(['\n', '\r']);
                    info!(target: "guest", "{}", msg);
                }
                Err(e) => {
                    warn!(error = %e, target = "guest", "error reading guest stderr");
                    break;
                }
            }
        }
    });

    // Create a readiness channel so the main thread waits until the provider is listening.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    // Spawn the Cap'n Proto provider on a dedicated background thread with its own
    // single-threaded Tokio runtime. This keeps the RPC system on one thread,
    // while the Wasm module runs on the main thread.
    info!("Spawning RPC provider thread");
    let provider_handle = thread::Builder::new()
        .name("rpc-provider".to_string())
        .spawn(move || {
            let provider_span =
                tracing::info_span!("rpc_provider", side = "server", transport = "pipe");
            let _provider_enter = provider_span.enter();
            info!("building single-threaded Tokio runtime for provider");
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build Tokio runtime for provider");
            info!("provider runtime built; entering event loop");

            rt.block_on(async move {
                // Set up the RPC provider inside the provider thread so we don't have to
                // move non-Send types across threads.
                info!("initializing echoer_provider client");
                let echoer_provider: echoer_provider::Client = cap::EchoerProvider::client();

                info!("constructing twoparty VatNetwork (server side)");
                let network = twoparty::VatNetwork::new(
                    host_r.compat(),
                    host_w.compat_write(),
                    rpc_twoparty_capnp::Side::Server,
                    Default::default(),
                );
                debug!("VatNetwork constructed");

                info!("starting RpcSystem");
                let rpc_system = RpcSystem::new(Box::new(network), Some(echoer_provider.client));

                // Signal to the main thread that the provider is ready to accept connections.
                let _ = ready_tx.send(());
                debug!("provider readiness signal sent");

                // Drive the RPC system until the connection closes (e.g., when the Wasm exits).
                info!("RpcSystem running; awaiting shutdown");
                match rpc_system.await {
                    Ok(()) => info!("RpcSystem completed"),
                    Err(e) => warn!(error = %e, "RpcSystem terminated with error"),
                }
            });
        })
        .expect("failed to spawn provider thread");

    // Wait for the provider thread to be ready before running the Wasm guest.
    info!("waiting for RPC provider readiness");
    let _ = ready_rx.await;
    info!("RPC provider is ready");

    // Load and run the Wasm guest in the main thread.
    let wasm_span = tracing::info_span!("wasm_runtime", path = %wasm_path);
    let _wasm_enter = wasm_span.enter();
    info!(path = %wasm_path, "loading Wasm bytes");
    let wasm_bytes = fs::read(wasm_path)?;
    debug!(len = wasm_bytes.len(), "loaded Wasm bytes");

    // Create a Store.
    info!("setting up WASM engine");
    let mut config = Config::new();
    config.async_support(true);
    let engine = Engine::new(&config)?;
    let mut linker = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;

    // Wire the async stdio streams into WASI and inherit host args.
    let wasi = WasiCtx::builder()
        .stdin(guest_r_async)
        .stdout(guest_w_async)
        .stderr(guest_e_async)
        .inherit_args()
        .build();
    let state = ComponentRunStates {
        wasi_ctx: wasi,
        resource_table: ResourceTable::new(),
    };
    let mut store = Store::new(&engine, state);

    info!("compiling WASM module");
    // Instantiate it as a normal component
    let component = Component::from_binary(&engine, &wasm_bytes)?;
    let instance = linker.instantiate_async(&mut store, &component).await?;
    // Get the index for the exported interface
    let interface_idx = instance
        .get_export_index(&mut store, None, "wasi:cli/run@0.2.0")
        .expect("Cannot get `wasi:cli/run@0.2.0` interface");
    // Get the index for the exported function in the exported interface
    let parent_export_idx = Some(&interface_idx);
    let func_idx = instance
        .get_export_index(&mut store, parent_export_idx, "run")
        .expect("Cannot get `run` function in `wasi:cli/run@0.2.0` interface");
    let func = instance
        .get_func(&mut store, func_idx)
        .expect("Unreachable since we've got func_idx");
    let typed = func.typed::<(), (Result<(), ()>,)>(&store)?;
    let (result,) = typed.call_async(&mut store, ()).await?;
    // Required, see documentation of TypedFunc::call
    typed.post_return_async(&mut store).await?;
    if result.is_err() {
        warn!(?result, "Wasm guest exited with error");
    } else {
        info!("Wasm guest exited cleanly");
    };

    // Proactively drop the Wasm instance and store to close WASI stdio resources
    // (guest_r_async/guest_w_async). This signals EOF to the provider's transport
    // so its RpcSystem can shut down cleanly.
    info!("Shutting down WASM store and closing guest stdio");
    // Dropping the store will close WASI resources (guest stdio), allowing the
    // provider's transport to observe EOF and exit.
    drop(store);

    // Ensure the provider thread terminates cleanly after the guest exits and
    // its stdio has been closed.
    info!("Wasm guest finished; joining provider thread");
    let _ = provider_handle.join();

    // Ensure the stderr mapping task has finished.
    let _ = stderr_task.await;

    info!("Ok");
    Ok(())
}
