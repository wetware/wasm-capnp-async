use std::fs;
use std::io::Read;
use wasmer::{Module, Store};
use wasmer_wasix::{Pipe, WasiEnv};

pub fn run_wasi_wasm_file(wasm_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let wasm_bytes = fs::read(wasm_path)?;

    // Create a Store.
    let mut store = Store::default();

    println!("Compiling module...");
    // Let's compile the Wasm module.
    let module = Module::new(&store, wasm_bytes)?;

    let (mut stdin_w, stdin_r) = Pipe::channel();
    let (stdout_w, mut stdout_r) = Pipe::channel();

    {
        // From the [docs](https://github.com/wasmerio/wasmer/blob/main/examples/wasi_pipes.rs):
        // We use a scope to make sure the runner is dropped
        // as soon as we are done with it; otherwise, it will keep the stdout pipe
        // open.
        WasiEnv::builder("capnp demo")
            .stdin(Box::new(stdin_r))
            .stdout(Box::new(stdout_w))
            .run_with_store(module, &mut store)?;
    }

    eprintln!("Ok");
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    run_wasi_wasm_file("wasm/target/wasm32-wasip1/release/wasm.wasm")?;
    Ok(())
}
