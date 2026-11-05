use std::net::SocketAddr;
use log::info;
use socket2::{Domain, Protocol, Socket, Type};

pub trait IoEngine: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn create_reuseport_listener(&self, addr: SocketAddr) -> std::io::Result<std::net::TcpListener>;
}

/// Linux-native io_uring runtime backend using monoio.
#[derive(Clone, Default)]
pub struct MonoioEngine;

impl MonoioEngine {
    pub fn new() -> Self {
        Self
    }
}

impl IoEngine for MonoioEngine {
    fn name(&self) -> &'static str {
        "monoio-io_uring"
    }

    fn create_reuseport_listener(&self, addr: SocketAddr) -> std::io::Result<std::net::TcpListener> {
        let domain = if addr.is_ipv6() { Domain::IPV6 } else { Domain::IPV4 };
        let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;

        // Set Linux socket options for high-throughput thread-per-core operation
        socket.set_reuse_address(true)?;
        #[cfg(target_os = "linux")]
        {
            socket.set_reuse_port(true)?;
        }
        socket.set_nodelay(true)?;
        socket.set_nonblocking(true)?;

        // Tune TCP buffer sizes for low-latency / high-throughput io_uring ring
        let _ = socket.set_recv_buffer_size(512 * 1024);
        let _ = socket.set_send_buffer_size(512 * 1024);

        let sock_addr: socket2::SockAddr = addr.into();
        socket.bind(&sock_addr)?;
        socket.listen(8192)?; // Deep backlog for burst connections

        info!("Created SO_REUSEPORT listener on {} via {}", addr, self.name());
        Ok(socket.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_listener() {
        let engine = MonoioEngine::new();
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = engine.create_reuseport_listener(addr);
        assert!(listener.is_ok());
    }
}
