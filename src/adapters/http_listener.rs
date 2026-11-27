//! Admission control for the HTTP listeners. `axum::serve` accepts every connection and keeps an
//! idle one open for as long as the client likes, so idle sockets could use up the process's file
//! descriptors and leave chain saves failing. This listener caps open connections and closes one
//! that makes no progress.

use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
    time::{Instant, Sleep},
};

/// Open connections on the public wallet endpoint.
pub const MAX_PUBLIC_HTTP_CONNECTIONS: usize = 256;
/// Open connections on the operator API, which serves the local UI.
pub const MAX_OPERATOR_HTTP_CONNECTIONS: usize = 128;
/// A connection that neither reads nor writes a byte for this long is closed. It also bounds how
/// long a client may take to send its headers. Requests answer well within it.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Errors such as running out of file descriptors return at once; pause instead of spinning.
const ACCEPT_ERROR_DELAY: Duration = Duration::from_millis(100);

pub struct AdmittedListener {
    listener: TcpListener,
    permits: Arc<Semaphore>,
    idle_timeout: Duration,
}

impl AdmittedListener {
    pub fn new(listener: TcpListener, max_connections: usize) -> Self {
        Self {
            listener,
            permits: Arc::new(Semaphore::new(max_connections)),
            idle_timeout: IDLE_TIMEOUT,
        }
    }
}

impl axum::serve::Listener for AdmittedListener {
    type Io = AdmittedStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (stream, address) = match self.listener.accept().await {
                Ok(accepted) => accepted,
                Err(error) => {
                    eprintln!("accepting an HTTP connection failed: {error}");
                    tokio::time::sleep(ACCEPT_ERROR_DELAY).await;
                    continue;
                }
            };
            // Over the cap the connection is closed at once, so it holds no descriptor.
            let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() else {
                continue;
            };
            return (
                AdmittedStream {
                    stream,
                    _permit: permit,
                    idle_timeout: self.idle_timeout,
                    deadline: Box::pin(tokio::time::sleep(self.idle_timeout)),
                },
                address,
            );
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

/// A connection that holds its slot until it closes and fails once it stops making progress.
pub struct AdmittedStream {
    stream: TcpStream,
    _permit: OwnedSemaphorePermit,
    idle_timeout: Duration,
    deadline: Pin<Box<Sleep>>,
}

impl AdmittedStream {
    fn progressed(&mut self) {
        let next = Instant::now() + self.idle_timeout;
        self.deadline.as_mut().reset(next);
    }

    /// Called when the stream is not ready: an error once the idle deadline has passed.
    fn check_idle(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.deadline.as_mut().poll(context) {
            Poll::Ready(()) => Poll::Ready(Err(io::ErrorKind::TimedOut.into())),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncRead for AdmittedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let filled = buffer.filled().len();
        match Pin::new(&mut self.stream).poll_read(context, buffer) {
            Poll::Ready(Ok(())) => {
                if buffer.filled().len() > filled {
                    self.progressed();
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => self.check_idle(context),
        }
    }
}

impl AsyncWrite for AdmittedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.stream).poll_write(context, buffer) {
            Poll::Ready(Ok(written)) => {
                if written > 0 {
                    self.progressed();
                }
                Poll::Ready(Ok(written))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => self.check_idle(context).map(|result| result.map(|()| 0)),
        }
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.stream).poll_write_vectored(context, buffers) {
            Poll::Ready(Ok(written)) => {
                if written > 0 {
                    self.progressed();
                }
                Poll::Ready(Ok(written))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => self.check_idle(context).map(|result| result.map(|()| 0)),
        }
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(context)
    }
}

/// Raises the soft open-file limit to the hard limit, so the connection caps above leave room
/// for P2P sessions and database files. Some systems start processes with only 256 or 1024.
#[cfg(unix)]
pub fn raise_open_file_limit() {
    // macOS refuses a soft limit above OPEN_MAX even when the hard limit is unlimited.
    const CEILING: libc::rlim_t = 10_240;
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit and setrlimit only read and write the struct passed to them.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
            return;
        }
        let wanted = limit.rlim_max.min(CEILING);
        if limit.rlim_cur < wanted {
            limit.rlim_cur = wanted;
            if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                eprintln!(
                    "raising the open-file limit failed: {}",
                    io::Error::last_os_error()
                );
            }
        }
    }
}

#[cfg(not(unix))]
pub fn raise_open_file_limit() {}

#[cfg(test)]
mod tests {
    use axum::serve::Listener;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    async fn listener(max_connections: usize, idle_timeout: Duration) -> AdmittedListener {
        let mut listener = AdmittedListener::new(
            TcpListener::bind("127.0.0.1:0").await.unwrap(),
            max_connections,
        );
        listener.idle_timeout = idle_timeout;
        listener
    }

    #[tokio::test]
    async fn connections_over_the_cap_are_closed_until_one_ends() {
        let mut listener = listener(2, IDLE_TIMEOUT).await;
        let address = listener.local_addr().unwrap();
        let first_client = TcpStream::connect(address).await.unwrap();
        let (first, _) = listener.accept().await;
        let _second_client = TcpStream::connect(address).await.unwrap();
        let (_second, _) = listener.accept().await;

        let mut refused = TcpStream::connect(address).await.unwrap();
        let accepting = tokio::spawn(async move {
            let accepted = listener.accept().await;
            (listener, accepted)
        });
        // The listener closes the third connection without admitting it.
        let mut byte = [0; 1];
        assert_eq!(refused.read(&mut byte).await.unwrap(), 0);
        assert!(!accepting.is_finished());

        drop((first, first_client));
        let mut admitted = TcpStream::connect(address).await.unwrap();
        let (_listener, (mut stream, _)) = accepting.await.unwrap();
        admitted.write_all(b"x").await.unwrap();
        assert_eq!(stream.read(&mut byte).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn a_silent_connection_times_out_and_progress_keeps_one_open() {
        let idle = Duration::from_millis(300);
        let mut listener = listener(4, idle).await;
        let address = listener.local_addr().unwrap();

        let _silent_client = TcpStream::connect(address).await.unwrap();
        let (mut silent, _) = listener.accept().await;
        let mut byte = [0; 1];
        let error = silent.read(&mut byte).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);

        let mut talking_client = TcpStream::connect(address).await.unwrap();
        let (mut talking, _) = listener.accept().await;
        for _ in 0..4 {
            tokio::time::sleep(idle / 2).await;
            talking_client.write_all(b"x").await.unwrap();
            assert_eq!(talking.read(&mut byte).await.unwrap(), 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_open_file_limit_is_raised_towards_the_hard_limit() {
        raise_open_file_limit();
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit only writes the struct passed to it.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
            0
        );
        assert!(limit.rlim_cur >= limit.rlim_max.min(10_240));
    }
}
