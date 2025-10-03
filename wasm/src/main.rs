use capnp_rpc::{rpc_twoparty_capnp, twoparty, RpcSystem};
use futures::executor::LocalPool;
use futures::{pin_mut, future::{select, Either}, stream::{FuturesUnordered, StreamExt}};
use std::io;
use std::task::{Context, Poll};
use wasip2::cli::{stdin, stdout, stderr};
use wasip2::io::streams;
use wasip2::random::random as wasi_random;

capnp::generated_code!(pub mod echo_capnp);

// Trying to use Cap'n Proto over the raw wasi:io/streams will not deadlock at some
// point and will not work. We need to implement non-blocking reads (return Pending
// when no bytes are ready) and flush-safe writes streams so capnp frames aren't truncated.

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

/// Submit `count` echo requests in order, then consume replies in a randomized order.
/// If `seed` is provided, the shuffle is reproducible; otherwise a WASI random seed is used.
async fn run_echo_batch(
    echoer: echo_capnp::echoer::Client,
    count: usize,
    seed: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Submit echo requests in order, store their promises by index.
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

    // Randomize the read order and then consume results accordingly.
    let s = seed.unwrap_or_else(seed_from_wasi);
    let order = shuffle_indices(count, s);

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

    log_stderr("guest: batch assertions passed");
    Ok(())
}


/// The main function will bootstrap `EchoerProvider` over stdin/stdout,
/// then spawn ${batch_count} tasks. Each task will perform a call to `EchoerProvider.echoer()`,
/// obtain an `Echoer` capability, then call `Echoer.echo("<message>"), wait for the response,
/// and assert the response matches the input. Each task will do this with different messages
/// ${call_count} amount of times.
/// 
/// The main idea is to force a high degree of concurrency and interleaving of requests and responses,
/// putting a lot of concurrent read/write pressure on the Cap'n Proto transport.
/// Execution will finish when all tasks complete successfully, or if any task fails.
/// Execution blocking would indicate a deadlock in the transport layer,
/// which means there is an issue in the implementation.
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

    // Configurable number of tasks per batch and number of batches to stress concurrency.
    let call_count: usize = 1000;
    let batch_count: usize = 10;
    // Optional fixed seed to make shuffles reproducible across runs; set Some(value) to fix.
    let fixed_seed: Option<u64> = None;

        // Launch all batches at once and await them asynchronously as they finish.
        let mut futs: FuturesUnordered<_> = (0..batch_count)
            .map(|b| {
                let e = echoer.clone();
                // Derive a per-batch seed if a fixed seed was provided; otherwise None -> WASI seed.
                let batch_seed = fixed_seed.map(|s| s ^ (b as u64).wrapping_mul(0x9E3779B97F4A7C15));
                async move {
                    log_stderr(&format!("guest: starting batch {} ({} tasks)", b, call_count));
                    let res = run_echo_batch(e, call_count, batch_seed).await;
                    (b, res)
                }
            })
            .collect();

        while let Some((i, r)) = futs.next().await {
            match r {
                Ok(()) => log_stderr(&format!("guest: batch {} completed", i)),
                Err(e) => {
                    log_stderr(&format!("guest: batch {} failed: {e}", i));
                    return Err(e);
                }
            }
        }

        log_stderr("guest: all batches completed successfully");

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


// I had some LLM generate the suffle functions, just know it works and it was not written
// by a human.

// Seed helpers and a tiny LCG for deterministic shuffles when desired.
fn seed_from_wasi() -> u64 {
    let bytes = wasi_random::get_random_bytes(8);
    if bytes.len() == 8 {
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&bytes);
        u64::from_le_bytes(arr)
    } else {
        0x9E3779B97F4A7C15u64 // fallback constant
    }
}

// Advance a 64-bit Linear Congruential Generator state and return the new value.
#[inline]
fn lcg_next(state: &mut u64) -> u64 {
    // Numerical Recipes LCG constants; sufficient for simple shuffle here.
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
    *state
}

// Produce a shuffled vector of indices [0, len) using Fisher-Yates with an LCG RNG.
fn shuffle_indices(len: usize, seed: u64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    if len <= 1 { return order; }
    let mut s = if seed == 0 { 1 } else { seed };
    for i in (1..len).rev() {
        let r = (lcg_next(&mut s) as usize) % (i + 1);
        order.swap(i, r);
    }
    order
}
