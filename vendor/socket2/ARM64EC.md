This is socket2 0.5.10 from crates.io, retaining its MIT/Apache-2.0 licenses.
Downstream changes are windows-sys 0.52 -> 0.61 in Cargo.toml, spelling
the removed BOOLEAN alias as u8, and adapting SOCKADDR_UN's signed path bytes
without changing their bit patterns.
The newer Windows bindings recognize Rust's arm64ec target architecture.
The 0.5 API is retained for trust-dns-proto's mDNS support.
