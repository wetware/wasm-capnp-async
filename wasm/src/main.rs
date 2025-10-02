use capnp_rpc::{rpc_twoparty_capnp, twoparty, RpcSystem};
use std::io;
use std::task::{Context, Poll};

use futures::executor::LocalPool;
use futures::{pin_mut, future::{select, Either}};
// (no stream utilities needed)

use wasip2::cli::{stdin, stdout, stderr};
use wasip2::io::streams;

// Keep the generated Cap’n Proto bindings.
capnp::generated_code!(pub mod echo_capnp);

// Simple adapters over wasi:io/streams. We implement non-blocking reads (return
// Pending when no bytes are ready) and flush-safe writes so Cap'n Proto frames
// aren't truncated.

struct Wasip2Stdin {
    stream: streams::InputStream,
}

impl Wasip2Stdin {
    fn new(stream: streams::InputStream) -> Self { Self { stream } }
}

impl futures::io::AsyncRead for Wasip2Stdin {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        // Non-blocking read: try to read available bytes; if none, yield Pending and self-wake.
        let len = buf.len() as u64;
        match self.stream.read(len) {
            Ok(bytes) => {
                let n = bytes.len();
                if n == 0 {
                    // No data ready yet; yield and try again later.
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
                buf[..n].copy_from_slice(&bytes);
                Poll::Ready(Ok(n))
            }
            Err(e) => Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, format!("{e:?}")))),
        }
    }
}

struct Wasip2Stdout {
    stream: streams::OutputStream,
}

impl Wasip2Stdout {
    fn new(stream: streams::OutputStream) -> Self {
        Self { stream }
    }
}

impl futures::io::AsyncWrite for Wasip2Stdout {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        // Ensure we don't misreport partial writes: use blocking_write_and_flush so the
        // entire buffer is committed before returning. This avoids frame truncation that can
        // deadlock Cap'n Proto RPC on subsequent reads.
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        match self.stream.blocking_write_and_flush(buf) {
            Ok(()) => Poll::Ready(Ok(buf.len())),
            Err(e) => Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, format!("{e:?}")))),
        }
    }

    fn poll_flush(self: std::pin::Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Ensure any pending output is committed before proceeding.
        match self.stream.blocking_flush() {
            Ok(()) => Poll::Ready(Ok(())),
            Err(e) => Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, format!("{e:?}")))),
        }
    }

    fn poll_close(self: std::pin::Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Ensure all pending output is committed before close.
        match self.stream.blocking_flush() {
            Ok(()) => Poll::Ready(Ok(())),
            Err(e) => Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, format!("{e:?}")))),
        }
    }
}

fn log_stderr(msg: &str) {
    let stream = stderr::get_stderr();
    let _ = stream.blocking_write_and_flush(msg.as_bytes());
    let _ = stream.blocking_write_and_flush(b"\n");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {

    // Get wasi:cli stdin/stdout as WASIp2 streams.
    let stdin = Wasip2Stdin::new(stdin::get_stdin());
    let stdout = Wasip2Stdout::new(stdout::get_stdout());

    // Cap’n Proto two-party over these streams.
    let network = twoparty::VatNetwork::new(
        stdin,
        stdout,
        rpc_twoparty_capnp::Side::Client,
        Default::default(),
    );

    let mut rpc_system = RpcSystem::new(Box::new(network), None);

    let echoer_provider: echo_capnp::echoer_provider::Client =
        rpc_system.bootstrap(rpc_twoparty_capnp::Side::Server);

    // Drive everything on a single-threaded local pool, polling the rpc_system concurrently
    // with our request logic to ensure responses are processed.
    let mut pool = LocalPool::new();

    let request_logic = async move {
    log_stderr("guest: requesting echoer");
        let resp = echoer_provider.echoer_request().send().promise.await?;
        let echoer = resp.get()?.get_echoer()?;
    log_stderr("guest: got echoer");

        // Submit echo requests in order, store their promises by index.
        let count = 5usize;
        let mut promises: Vec<Option<_>> = Vec::with_capacity(count);
        let mut expected: Vec<String> = Vec::with_capacity(count);

        for i in 0..count {
            let mut echo_request = echoer.echo_request();
            let msg = format!("Hello from WASI! #{}", i);
            let mut buf = echo_request.get().init_msg(msg.len() as u32);
            buf.push_str(&msg);
            log_stderr(&format!("guest: submitting echo {}", i));
            let promise = echo_request.send().promise;
            promises.push(Some(promise));
            expected.push(msg);
        }

        // Re-order the (index, promise) groups and then read results in that different order.
        let order: [usize; 5] = [3, 1, 4, 0, 2];
        for idx in order {
            let promise = promises[idx]
                .take()
                .expect("promise should be present");
            let echo_response = promise.await?;
            let reply = echo_response.get()?.get_reply()?;
            let reply_str = std::str::from_utf8(reply)?.to_string();
            log_stderr(&format!("guest: read echo {} => {}", idx, reply_str));
            assert_eq!(reply_str, expected[idx], "reply mismatch for index {}", idx);
        }

        log_stderr("guest: all shuffled assertions passed");

        Ok::<(), Box<dyn std::error::Error>>(())
    };

    pool.run_until(async move {
        let rpc_fut = async move {
            if let Err(e) = rpc_system.await {
                log_stderr(&format!("rpc_system error: {e:?}"));
            }
        };

        pin_mut!(request_logic);
        pin_mut!(rpc_fut);

        match select(request_logic, rpc_fut).await {
            Either::Left((Ok(()), _rpc_remaining)) => Ok::<(), Box<dyn std::error::Error>>(()),
            Either::Left((Err(e), _)) => Err::<(), Box<dyn std::error::Error>>(e),
            Either::Right((_rpc_done, _req_remaining)) => {
                // RPC system ended before our work; treat as error
                Err::<(), Box<dyn std::error::Error>>("rpc_system terminated early".into())
            }
        }
    })?;

    Ok(())
}