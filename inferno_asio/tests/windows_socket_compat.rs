use socket2::{Domain, Protocol, SockAddr, Socket, Type};

#[test]
fn tcp_boolean_options_roundtrip() {
    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).unwrap();
    for enabled in [true, false] {
        socket.set_nodelay(enabled).unwrap();
        assert_eq!(socket.nodelay().unwrap(), enabled);
    }
}

#[test]
fn unix_socket_paths_preserve_utf8_bytes() {
    let path = std::path::Path::new("inferno-\u{00e9}.sock");
    let address = SockAddr::unix(path).unwrap();
    let bytes = unsafe {
        std::slice::from_raw_parts(address.as_ptr().cast::<u8>(), address.len() as usize)
    };
    // Windows SOCKADDR_UN starts with a 16-bit address family.
    let encoded = path.to_str().unwrap().as_bytes();
    assert_eq!(&bytes[2..bytes.len() - 1], encoded);
    assert_eq!(bytes.last(), Some(&0));
}
