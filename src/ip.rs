use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

pub const DEFAULT_PORT: u16 = 7110;

/// The unspecified Ipv4 address and an os-assigned port.
/// When bound to a local socket, allows communication with any reachable Ipv4 address.
/// Not recommended for use as a server's local socket, as clients must know which port to connect to.
/// This is the recommended address to use for a client's local socket.
pub const IPV4_WILDCARD: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);

/// The unspecified Ipv6 address and an os-assigned port.
/// When bound to a local socket, allows communication with any reachable Ipv6 address.
/// If the socket is configured as dual-stack, also allows communication with any reachable Ipv4 address.
/// Not recommended for use as a server's local socket, as clients must know which port to connect to.
/// This is the recommended address to use for a client's local socket.
pub const IPV6_WILDCARD: SocketAddr = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0);

/// The unspecified Ipv4 address and the default port (7110).
/// When bound to a local socket, allows communication with any reachable Ipv4 address.
/// This is the recommended address to use for a server's local socket.
pub const IPV4_WILDCARD_DEFAULT_PORT: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_PORT);

/// The unspecified Ipv6 address and the default port (7110).
/// When bound to a local socket, allows communication with any reachable Ipv6 address.
/// If the socket is configured as dual-stack, also allows communication with any reachable Ipv4 address.
/// This is the recommended address to use for a server's local socket.
pub const IPV6_WILDCARD_DEFAULT_PORT: SocketAddr =
    SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), DEFAULT_PORT);

/// The given Ipv4 address and the default port (7110).
/// When bound to a local socket, only allows communication with the specified Ipv4 addresses.
/// This is recommended for connecting to servers.
pub const fn ipv4_default_port(addr: Ipv4Addr) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(addr), DEFAULT_PORT)
}

/// The given Ipv6 address and the default port (7110).
/// When bound to a local socket, only allows communication with the specified Ipv6 addresses.
/// If the socket is configured as dual-stack, also allows communication with included Ipv4 addresses.
/// This is recommended for connecting to servers.
pub const fn ipv6_default_port(addr: Ipv6Addr) -> SocketAddr {
    SocketAddr::new(IpAddr::V6(addr), DEFAULT_PORT)
}
