use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use futures::io::{BufReader, BufWriter};
use std::io;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;
use tokio;
use tokio::task;
use tokio::time;
use wasip1::{
    ERRNO_SUCCESS, Errno, FD_STDIN, FD_STDOUT, FDFLAGS_NONBLOCK, Fd, fd_fdstat_get,
    fd_fdstat_set_flags,
};

capnp::generated_code!(pub mod echo_capnp);

fn eventtype_tag(t: wasip1::Eventtype) -> u8 {
    if t == wasip1::EVENTTYPE_CLOCK
        || t == wasip1::EVENTTYPE_FD_READ
        || t == wasip1::EVENTTYPE_FD_WRITE
    {
        return t.raw();
    } else {
        255
    }
}

fn set_nonblocking(fd: Fd) -> io::Result<()> {
    let err: Errno = unsafe {
        // let mut flags = wasip1::fd_fdstat_get(fd)?.flags;
        let mut flags = fd_fdstat_get(fd).unwrap().fs_flags;
        flags |= FDFLAGS_NONBLOCK;
        fd_fdstat_set_flags(fd, flags).unwrap();
        ERRNO_SUCCESS
    };
    match err {
        ERRNO_SUCCESS => Ok(()),
        _ => Err(io::Error::new(io::ErrorKind::Other, err)),
    }
}

struct NonBlockingStdin {
    inner: std::io::Stdin,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl NonBlockingStdin {
    fn new() -> Self {
        Self {
            inner: std::io::stdin(),
            waker: Arc::new(Mutex::new(None)),
        }
    }

    fn schedule_read_wake(waker: Arc<Mutex<Option<Waker>>>) {
        task::spawn(async move {
            loop {
                // Non-blocking readiness check using WASI poll_oneoff with zero-time clock.
                let ready = unsafe {
                    // Build FD_READ and zero-time CLOCK subscriptions.
                    let mut sub_fd: wasip1::Subscription = std::mem::zeroed();
                    sub_fd.userdata = 1;
                    sub_fd.u.tag = eventtype_tag(wasip1::EVENTTYPE_FD_READ);
                    sub_fd.u.u.fd_read = wasip1::SubscriptionFdReadwrite {
                        file_descriptor: FD_STDIN,
                    };

                    let mut sub_clk: wasip1::Subscription = std::mem::zeroed();
                    sub_clk.userdata = 2;
                    sub_clk.u.tag = eventtype_tag(wasip1::EVENTTYPE_CLOCK);
                    sub_clk.u.u.clock = wasip1::SubscriptionClock {
                        id: wasip1::CLOCKID_MONOTONIC,
                        timeout: 0,
                        precision: 0,
                        flags: 0,
                    };

                    let subs = [sub_fd, sub_clk];
                    let mut evs: [wasip1::Event; 2] = std::mem::zeroed();
                    let n = match wasip1::poll_oneoff(subs.as_ptr(), evs.as_mut_ptr(), subs.len()) {
                        Ok(n) => n,
                        Err(_) => 0,
                    };

                    // Ready if any FD_READ event succeeded.
                    (0..n).any(|i| {
                        evs[i].type_ == wasip1::EVENTTYPE_FD_READ
                            && evs[i].error == wasip1::ERRNO_SUCCESS
                    })
                };

                if ready {
                    if let Some(w) = waker.lock().unwrap().clone() {
                        w.wake();
                    }
                    break;
                } else {
                    time::sleep(Duration::from_millis(1)).await;
                }
            }
        });
    }
}

impl futures::io::AsyncRead for NonBlockingStdin {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match this.inner.read(buf) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                {
                    let mut guard = this.waker.lock().unwrap();
                    *guard = Some(cx.waker().clone());
                }
                Self::schedule_read_wake(this.waker.clone());
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

struct NonBlockingStdout {
    inner: std::io::Stdout,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl NonBlockingStdout {
    fn new() -> Self {
        Self {
            inner: std::io::stdout(),
            waker: Arc::new(Mutex::new(None)),
        }
    }

    fn schedule_write_wake(waker: Arc<Mutex<Option<Waker>>>) {
        task::spawn(async move {
            loop {
                // Non-blocking readiness check using WASI poll_oneoff with zero-time clock.
                let ready = unsafe {
                    // Build FD_WRITE and zero-time CLOCK subscriptions.
                    let mut sub_fd: wasip1::Subscription = std::mem::zeroed();
                    sub_fd.userdata = 1;
                    sub_fd.u.tag = eventtype_tag(wasip1::EVENTTYPE_FD_WRITE);
                    sub_fd.u.u.fd_write = wasip1::SubscriptionFdReadwrite {
                        file_descriptor: FD_STDOUT,
                    };

                    let mut sub_clk: wasip1::Subscription = std::mem::zeroed();
                    sub_clk.userdata = 2;
                    sub_clk.u.tag = eventtype_tag(wasip1::EVENTTYPE_CLOCK);
                    sub_clk.u.u.clock = wasip1::SubscriptionClock {
                        id: wasip1::CLOCKID_MONOTONIC,
                        timeout: 0,
                        precision: 0,
                        flags: 0,
                    };

                    let subs = [sub_fd, sub_clk];
                    let mut evs: [wasip1::Event; 2] = std::mem::zeroed();
                    let n = match wasip1::poll_oneoff(subs.as_ptr(), evs.as_mut_ptr(), subs.len()) {
                        Ok(n) => n,
                        Err(_) => 0,
                    };

                    // Ready if any FD_WRITE event succeeded.
                    (0..n).any(|i| {
                        evs[i].type_ == wasip1::EVENTTYPE_FD_WRITE
                            && evs[i].error == wasip1::ERRNO_SUCCESS
                    })
                };

                if ready {
                    if let Some(w) = waker.lock().unwrap().clone() {
                        w.wake();
                    }
                    break;
                } else {
                    time::sleep(Duration::from_millis(1)).await;
                }
            }
        });
    }
}

impl futures::io::AsyncWrite for NonBlockingStdout {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match this.inner.write(buf) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                {
                    let mut guard = this.waker.lock().unwrap();
                    *guard = Some(cx.waker().clone());
                }
                Self::schedule_write_wake(this.waker.clone());
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }

    fn poll_flush(self: std::pin::Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match this.inner.flush() {
            Ok(()) => Poll::Ready(Ok(())),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                Self::schedule_write_wake(this.waker.clone());
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }

    fn poll_close(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}
}
