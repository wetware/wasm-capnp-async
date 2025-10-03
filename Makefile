.PHONY: clean run

all: clean build

build: build-host build-guest

build-host:
	cargo build

build-guest:
	cargo build -p wasm --manifest-path wasm/Cargo.toml --target wasm32-wasip2 --release

clean: clean-host clean-guest

clean-host:
	cargo clean

clean-guest:
	cd wasm && cargo clean

run:
	cargo run

# There must be a way cleaner way of configuring the logger but my hacking
# session has resulted in me running adding this mumbo jumbo to my run command.
trace:
	@RUST_BACKTRACE=full \
	RUST_LOG=info,wasmtime=trace,wasmtime_wasi=trace,capnp_rpc=trace,capnp=trace,wasmtime::runtime=info,wasmtime_internal_cranelift=info,cranelift_codegen=info,wasmtime_environ=info,wasmtime_internal_unwinder=info \
	cargo run
