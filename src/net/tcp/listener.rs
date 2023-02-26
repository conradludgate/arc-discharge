use mio::Interest;

use crate::io::poll_evented::PollEvented;
use crate::net::tcp::TcpStream;

use crate::net::{to_socket_addrs, ToSocketAddrs};

use std::convert::TryFrom;
use std::fmt;
use std::io;
use std::net::{self, SocketAddr};

pub struct TcpListener {
    io: PollEvented<mio::net::TcpListener>,
}

impl TcpListener {
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<TcpListener> {
        let addrs = to_socket_addrs(addr)?;

        let mut last_err = None;

        for addr in addrs {
            match TcpListener::bind_addr(addr) {
                Ok(listener) => return Ok(listener),
                Err(e) => last_err = Some(e),
            }
        }

        Err(last_err.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "could not resolve to any address",
            )
        }))
    }

    fn bind_addr(addr: SocketAddr) -> io::Result<TcpListener> {
        let listener = mio::net::TcpListener::bind(addr)?;
        TcpListener::new(listener)
    }

    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        let (mio, addr) = self
            .io
            .registration()
            .async_io(Interest::READABLE, || self.io.accept())
            .await?;

        let stream = TcpStream::new(mio)?;
        Ok((stream, addr))
    }

    pub(crate) fn new(listener: mio::net::TcpListener) -> io::Result<TcpListener> {
        let io = PollEvented::new(listener)?;
        Ok(TcpListener { io })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.io.local_addr()
    }
}

impl TryFrom<net::TcpListener> for TcpListener {
    type Error = io::Error;
    #[track_caller]
    fn try_from(stream: net::TcpListener) -> Result<Self, Self::Error> {
        Self::new(mio::net::TcpListener::from_std(stream))
    }
}

impl fmt::Debug for TcpListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.io.fmt(f)
    }
}
