use std::str;

use capnp;
use capnp_rpc::{pry, rpc_twoparty_capnp, twoparty, RpcSystem};

capnp::generated_code!(pub mod echo_capnp);

struct Echoer;

impl echo_capnp::echoer::Server for Echoer {
    fn echo(
        &mut self,
        params: echo_capnp::echoer::EchoParams,
        mut results: echo_capnp::echoer::EchoResults,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        let msg = pry!(pry!(params.get()).get_msg());
        results.get().set_reply(msg.as_bytes());
        capnp::capability::Promise::ok(())
    }
}

struct EchoerProvider {
    echoers: Vec<echo_capnp::echoer::Client>,
}

impl EchoerProvider {
    fn new() -> Self {
        let mut echoers: Vec<echo_capnp::echoer::Client> = vec![];
        for _ in 0..10 {
            let echoer: echo_capnp::echoer::Client = capnp_rpc::new_client(Echoer {});
            echoers.push(echoer);
        }
        Self { echoers: echoers }
    }

    fn client(/* TODO pass pipe */) -> echo_capnp::echoer_provider::Client {
        let provider: echo_capnp::echoer_provider::Client =
            capnp_rpc::new_client(EchoerProvider::new());
        provider
    }
}

impl echo_capnp::echoer_provider::Server for EchoerProvider {
    fn echoer(
        &mut self,
        _params: echo_capnp::echoer_provider::EchoerParams,
        mut results: echo_capnp::echoer_provider::EchoerResults,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        let echoer: echo_capnp::echoer::Client = capnp_rpc::new_client(Echoer {});
        results.get().set_echoer(echoer);
        capnp::capability::Promise::ok(())
    }
}

