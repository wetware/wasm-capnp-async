# Wasm and Cap'n Proto integration in Rust

Run Cap'n Proto clients and servers within WASM programs, directly over stdin and stdout.

## Proof of Concept

This project sets up a WASM runtime in a Linux/MacOS host
and provides Cap'n Proto capabilities to guest WASM processes through
their stdin/stout by:

- Configuring WASIp2 in the guest; then carefully handling its polling capabilities to obtain non-blocking streams.
- Mapping async pipes to the host side of the WASM process stdin/stdout.
- Bootstrapping object capabilities over a transport based on the async pipes built upon the now async stdin/stdout.

The WASM guest will:

1. Bootstrap an `EchoerProvider` capability.
2. Ask the `EchoerProvider` for an `Echoer` capability by calling a capnp method: `EchoerProvider.echoer()`.
This will return a new `Echoer` capability.
3. Call the `echo` method of the newly obtained `Echoer` and verify the result: `Echoer.echo("<some message>")`.

Steps 2 and 3 are performed many times concurrently, producing multiple (different) `Echoer` objects
and verifying that the transport is capable of handling multiple concurrent read/write requests
under pressure.

## Usage

Build the project with `make`, then run it with `make run`.
