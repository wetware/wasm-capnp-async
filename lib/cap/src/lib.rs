use capnp::capability::Promise;
use capnp_rpc::{RpcSystem, pry, rpc_twoparty_capnp, twoparty};

capnp::generated_code!(pub mod echo_capnp);

use echo_capnp::{echoer, echoer_provider};

pub struct Echoer;

impl echo_capnp::echoer::Server for Echoer {
    fn echo(
        &mut self,
        params: echoer::EchoParams,
        mut results: echoer::EchoResults,
    ) -> Promise<(), capnp::Error> {
        let msg = pry!(pry!(params.get()).get_msg());
        results.get().set_reply(msg.as_bytes());
        Promise::ok(())
    }
}

pub struct EchoerProvider {
    echoers: Vec<echoer::Client>,
}

impl EchoerProvider {
    pub fn new() -> Self {
        let mut echoers: Vec<echoer::Client> = vec![];
        for _ in 0..10 {
            let echoer: echoer::Client = capnp_rpc::new_client(Echoer {});
            echoers.push(echoer);
        }
        Self { echoers: echoers }
    }

    pub fn client() -> echoer_provider::Client {
        let provider: echoer_provider::Client = capnp_rpc::new_client(EchoerProvider::new());
        provider
    }
}

impl echoer_provider::Server for EchoerProvider {
    fn echoer(
        &mut self,
        _params: echoer_provider::EchoerParams,
        mut results: echoer_provider::EchoerResults,
    ) -> Promise<(), capnp::Error> {
        let echoer: echoer::Client = capnp_rpc::new_client(Echoer {});
        results.get().set_echoer(echoer);
        Promise::ok(())
    }
}
