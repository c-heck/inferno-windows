use crate::broadcast::ServiceBuilder;
use std::{
	net::{IpAddr, Ipv4Addr, Ipv6Addr},
	str::FromStr,
};
use trust_dns_client::serialize::binary::{BinEncodable, BinEncoder};

#[test]
fn dns_nsec_records_decode_without_panicking() {
    // One mDNS NSEC answer for x.local, asserting that an A record exists.
    let packet = [
        0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0,
        1, b'x', 5, b'l', b'o', b'c', b'a', b'l', 0,
        0, 47, 0x80, 1, 0, 0, 0, 120, 0, 5, 0xc0, 12, 0, 1, 0x40,
    ];
    let parsed = trust_dns_client::op::Message::from_vec(&packet).unwrap();
    assert_eq!(parsed.answers()[0].record_type(), trust_dns_client::rr::RecordType::NSEC);
    assert!(matches!(parsed.answers()[0].data(),
        Some(trust_dns_client::rr::RData::DNSSEC(_))));
}

#[test]
fn test_dns_parser_backwards_compatibility() {
	let dns_message = ServiceBuilder::new("_venner-test._udp.local", "helloworld", 1337)
		.unwrap()
		.add_ip_address(IpAddr::V4(Ipv4Addr::from_str("192.168.1.69").unwrap()))
		.add_ip_address(IpAddr::V6(Ipv6Addr::from_str("fe80::18e4:b943:8756:d855").unwrap()))
		.add_txt("key=value")
		.add_txt_truncated("key2=value2")
		.build()
		.unwrap()
		.dns_response()
		.unwrap();

	println!("========== OURS ==========\n{dns_message:#?}\n");

	let mut buf = Vec::with_capacity(4096);
	dns_message.emit(&mut BinEncoder::new(&mut buf)).unwrap();

	println!("========== THEIRS ==========\n{:#?}", dns_parser::Packet::parse(&buf).unwrap());
	let parsed = trust_dns_client::op::Message::from_vec(&buf).unwrap();
	assert_eq!(parsed.answers().len(), 3);
	assert!(parsed.answers().iter().any(|record| matches!(
		record.data(),
		Some(trust_dns_client::rr::RData::PTR(name))
			if name.0.to_utf8() == "helloworld._venner-test._udp.local."
	)));
	assert!(parsed.additionals().iter().any(|record| matches!(
		record.data(),
		Some(trust_dns_client::rr::RData::A(address))
			if address.0 == Ipv4Addr::new(192, 168, 1, 69)
	)));
	assert!(parsed.additionals().iter().any(|record| matches!(
		record.data(),
		Some(trust_dns_client::rr::RData::AAAA(address))
			if address.0 == Ipv6Addr::from_str("fe80::18e4:b943:8756:d855").unwrap()
	)));
}

#[test]
fn address_only_dns_response_preserves_address_records() {
	let message = ServiceBuilder::new("_venner-test._udp.local", "address-only", 0)
		.unwrap()
		.add_ip_address(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 69)))
		.add_ip_address(IpAddr::V6(Ipv6Addr::LOCALHOST))
		.build().unwrap().dns_response().unwrap();
	let bytes = message.to_vec().unwrap();
	let response = trust_dns_client::op::DnsResponse::new(
		trust_dns_client::op::Message::from_vec(&bytes).unwrap(), bytes.clone()
	);
	assert_eq!(response.answers().len(), 2);
	assert_eq!(response.as_buffer(), bytes);
	assert!(response.answers().iter().any(|record| matches!(record.data(),
		Some(trust_dns_client::rr::RData::A(address)) if address.0 == Ipv4Addr::new(192, 168, 1, 69)
	)));
	assert!(response.answers().iter().any(|record| matches!(record.data(),
		Some(trust_dns_client::rr::RData::AAAA(address)) if address.0 == Ipv6Addr::LOCALHOST
	)));
}

#[test]
fn test_readme_version() {
	let readme = std::fs::read_to_string("README.md").unwrap();
	let version = env!("CARGO_PKG_VERSION");
	assert!(readme.contains(format!("searchlight = \"{}\"", version).as_str()));
}
