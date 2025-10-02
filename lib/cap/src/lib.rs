use capnp::capability::Promise;
use capnp_rpc::pry;
use tracing::debug;

capnp::generated_code!(pub mod echo_capnp);

use echo_capnp::{echoer, echoer_provider};

pub struct Echoer;

impl echo_capnp::echoer::Server for Echoer {
    fn echo(
        &mut self,
        params: echoer::EchoParams,
        mut results: echoer::EchoResults,
    ) -> Promise<(), capnp::Error> {
        debug!("Received echo request");
        let msg = pry!(pry!(params.get()).get_msg());
        let msg_bytes = msg.as_bytes();
        let msg_str = std::str::from_utf8(msg_bytes);
        debug!(?msg_str, "Echoing message");
        results.get().set_reply(msg_bytes);
        debug!("Ended echo request");
        Promise::ok(())
    }
}

pub struct EchoerProvider {
    i: usize,
    echoers: Vec<echoer::Client>,
}

impl EchoerProvider {
    pub fn new() -> Self {
        let mut echoers: Vec<echoer::Client> = vec![];
        for _ in 0..10 {
            let echoer: echoer::Client = capnp_rpc::new_client(Echoer {});
            echoers.push(echoer);
        }
        Self {
            i: 0,
            echoers: echoers,
        }
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
    debug!("Received echoer request");
        
        // Round-robin selection of an Echoer client without risking out-of-bounds.
        // Use modulo over the number of echoers, then bump the counter.
        let len = self.echoers.len();
        let idx = self.i % len;
        let ec = self.echoers[idx].clone();
        self.i = self.i.wrapping_add(1);
        results.get().set_echoer(ec);
        debug!("Ended echoer request");
        Promise::ok(())
    }
}
