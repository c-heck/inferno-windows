use std::marker::PhantomData;

use binary_serde::BinarySerde;
use bytebuffer::ByteBuffer;
use tracing::error;

use crate::{byte_utils::make_u16, device_info::DeviceInfo};

use super::req_resp::{Connection, HEADER_LENGTH};


pub const PACKET_SIZE_SOFT_LIMIT: usize = 800;
pub const PORT: u16 = 4440;

pub mod channels_and_flows_count {
  use binary_serde::{binary_serde_bitfield, BinarySerde, BitfieldBitOrder};

  pub const OPCODE: u16 = 0x1000;

  #[derive(Debug, Default, PartialEq, Eq)]
  #[binary_serde_bitfield(order = BitfieldBitOrder::LsbFirst)]
  pub struct Flags2 {
    #[bits(4)]
    pub unknown1_0: u8,
    #[bits(1)]
    pub supports_tx_channel_rename: bool,
    #[bits(1)]
    pub supports_tx_multicast: bool,
    #[bits(2)]
    pub unknown2_0: u8,
  }

  #[derive(Debug, BinarySerde, Default, PartialEq, Eq)]
  pub struct Response {
    pub unknown1_0: u8, // or 5
    pub flags2: Flags2,
    pub tx_channels_count: u16,
    pub rx_channels_count: u16,
    pub unknown2_4: u16, // or 1
    pub max_channels_in_flow: u16,
    pub unknown4_8: u16,
    pub max_tx_flows: u16,
    pub max_rx_flows: u16,
    pub unknown5_total_channels: u16,
    pub unknown6_1: u16,
    pub unknown7_1: u16,
    pub unknown8_0: [u16; 6],
  }
}

pub const GET_DEVICE_NAME_OPCODE: u16 = 0x1002;
pub const SET_DEVICE_NAME_OPCODE: u16 = 0x1001;

/// Decodes a 0x1001 request body: NUL-terminated name, empty/blank meaning "reset to default".
pub fn parse_device_name(content: &[u8]) -> Result<Option<String>, &'static str> {
  let end = content.iter().position(|&b| b == 0).unwrap_or(content.len());
  let name = std::str::from_utf8(&content[..end]).map_err(|_| "device name is not UTF-8")?;
  Ok((!name.is_empty()).then(|| name.to_owned()))
}

pub mod properties {
  use super::HEADER_LENGTH;
  use crate::device_info::DeviceInfo;

  pub const READ_OPCODE: u16 = 0x1100;
  pub const WRITE_OPCODE: u16 = 0x1101;
  pub const DIRECTORY_OPCODE: u16 = 0x1102;
  /// Network latency range offered to Dante Controller (Dante Virtual Soundcard range).
  pub const MIN_LATENCY_NS: u32 = 4_000_000;
  pub const MAX_LATENCY_NS: u32 = 20_000_000;
  const IDS: [u16; 14] = [
    0x0201, 0x0210, 0x0211, 0x0213, 0x0214, 0x0303, 0x0310, 0x0311,
    0x8204, 0x8205, 0x8301, 0x8302, 0x8306, 0x8021,
  ];
  /// Properties Dante Controller may write (configured TX and RX latency).
  const WRITABLE: [u16; 2] = [0x8205, 0x8301];

  pub fn directory() -> Vec<u8> {
    let mut result = vec![0, IDS.len() as u8];
    for id in IDS {
      result.extend_from_slice(&id.to_be_bytes());
      let flags: u16 = if WRITABLE.contains(&id) { 3 } else { 1 };
      result.extend_from_slice(&flags.to_be_bytes());
    }
    result
  }

  /// Extracts the requested latency from a 0x1101 write: records of (id, pointer) after a
  /// 2-byte preamble, pointers addressing big-endian u32 values relative to the packet start.
  pub fn parse_latency_write(content: &[u8]) -> Result<u32, &'static str> {
    let count = usize::from(*content.first().ok_or("empty ARC property write")?);
    let mut latency = None;
    for record in content.get(2..).unwrap_or_default().chunks_exact(4).take(count) {
      let id = u16::from_be_bytes([record[0], record[1]]);
      if !WRITABLE.contains(&id) {
        continue;
      }
      let pointer = usize::from(u16::from_be_bytes([record[2], record[3]]));
      let start = pointer.checked_sub(HEADER_LENGTH).ok_or("ARC property pointer inside header")?;
      let bytes = content.get(start..start + 4).ok_or("ARC property pointer out of range")?;
      let value = u32::from_be_bytes(bytes.try_into().unwrap());
      if latency.is_some_and(|previous| previous != value) {
        return Err("conflicting ARC latency values");
      }
      latency = Some(value);
    }
    latency.ok_or("ARC property write has no latency")
  }

  /// Reply to a latency write, in the layout Dante devices use.
  pub fn latency_write_reply(latency_ns: u32) -> Vec<u8> {
    let records: [(u16, Option<u16>); 4] = [(0x8205, None), (0x0211, Some(4)), (0x8301, None), (0x0310, Some(4))];
    let values_start = HEADER_LENGTH + 2 + records.len() * 4;
    let mut result = vec![records.len() as u8, 4];
    let mut values = vec![];
    for (id, inline) in records {
      result.extend_from_slice(&id.to_be_bytes());
      let field = inline.unwrap_or_else(|| (values_start + values.len()) as u16);
      result.extend_from_slice(&field.to_be_bytes());
      if inline.is_none() {
        values.extend_from_slice(&latency_ns.to_be_bytes());
      }
    }
    result.extend_from_slice(&values);
    result
  }

  pub fn read(request: &[u8], device: &DeviceInfo) -> Result<Vec<u8>, &'static str> {
    if request.len() < 2 || request[0] != 0 || request.len() != 2 + usize::from(request[1]) * 2 {
      return Err("invalid ARC property query extent");
    }
    let count = usize::from(request[1]);
    let mut result = vec![0; 2 + count * 4];
    result[0] = request[1];
    result[1] = request[1];
    for (index, bytes) in request[2..].chunks_exact(2).enumerate() {
      let id = u16::from_be_bytes(bytes.try_into().unwrap());
      let row = 2 + index * 4;
      let inline = match id {
        // Routing capability values observed in the O2RSlot4 property response.
        0x0201 => Some(1),
        0x0303 => Some(4),
        0x0210 | 0x0211 | 0x0214 | 0x0310 => Some(crate::device_server::flows_tx::FPP_MAX_ADVERTISED),
        0x0213 | 0x0311 => Some(crate::device_server::flows_tx::FPP_MIN),
        _ => None,
      };
      if let Some(value) = inline {
        result[row..row + 2].copy_from_slice(&id.to_be_bytes());
        result[row + 2..row + 4].copy_from_slice(&value.to_be_bytes());
        continue;
      }
      let value = match id {
        0x8204 | 0x8205 => device.tx_latency_ns.to_be_bytes().to_vec(),
        0x8301 => u32::try_from(device.latency_ns)
          .map_err(|_| "receive latency exceeds ARC property range")?.to_be_bytes().to_vec(),
        0x8302 => MAX_LATENCY_NS.to_be_bytes().to_vec(),
        0x8306 => MIN_LATENCY_NS.to_be_bytes().to_vec(),
        // The unnamed Dante audio domain is 16 zero bytes, distinct from the PTPv1 _DFLT subdomain.
        0x8021 => vec![0; 16],
        _ => {
          result[row + 2..row + 4].copy_from_slice(&id.to_be_bytes());
          continue;
        }
      };
      while (HEADER_LENGTH + result.len()) % 4 != 0 {
        result.push(0);
      }
      let offset = u16::try_from(HEADER_LENGTH + result.len()).map_err(|_| "ARC property offset overflow")?;
      result[row..row + 2].copy_from_slice(&id.to_be_bytes());
      result[row + 2..row + 4].copy_from_slice(&offset.to_be_bytes());
      result.extend_from_slice(&value);
    }
    Ok(result)
  }

  #[cfg(test)]
  mod tests {
    use super::*;
    use crate::device_server::settings::Settings;
    use std::{collections::BTreeMap, net::Ipv4Addr};

    #[test]
    fn property_records_report_real_latency_and_default_domain() {
      let settings = Settings::new("ARC test", "ARC", Some(Ipv4Addr::LOCALHOST),
        &BTreeMap::from([("RX_LATENCY_NS".into(), "10000000".into()),
          ("TX_LATENCY_NS".into(), "20000000".into())]));
      let reply = read(&[0, 4, 0x82, 5, 0x83, 1, 0x80, 0x21, 0xff, 0xff], &settings.self_info).unwrap();
      assert_eq!(&reply[..2], &[4, 4]);
      for (row, expected) in [(2, 20_000_000u32), (6, 10_000_000)] {
        let offset = usize::from(u16::from_be_bytes(reply[row + 2..row + 4].try_into().unwrap()));
        assert_eq!(offset % 4, 0);
        assert_eq!(&reply[offset - HEADER_LENGTH..offset - HEADER_LENGTH + 4], &expected.to_be_bytes());
      }
      let offset = usize::from(u16::from_be_bytes(reply[12..14].try_into().unwrap()));
      assert_eq!(&reply[offset - HEADER_LENGTH..offset - HEADER_LENGTH + 16], &[0; 16]);
      assert_eq!(&reply[14..18], &[0, 0, 0xff, 0xff]);
      assert_eq!(directory().len(), 2 + IDS.len() * 4);
      for invalid in [&[][..], &[0][..], &[0, 1][..], &[0, 0, 0][..], &[1, 0][..]] {
        assert!(read(invalid, &settings.self_info).is_err());
      }
    }

    #[test]
    fn latency_write_matches_controller_layout() {
      // netaudio/Controller 0x1101 body (after the 10-byte header): preamble, then 5 ms twice.
      let mut content = vec![0x05, 0x04, 0x82, 0x05, 0x00, 0x20, 0x02, 0x11, 0x00, 0x04, 0x83, 0x01,
        0x00, 0x24, 0x03, 0x10, 0x00, 0x04, 0x83, 0x02, 0x83, 0x06];
      content.extend_from_slice(&5_000_000u32.to_be_bytes());
      content.extend_from_slice(&5_000_000u32.to_be_bytes());
      assert_eq!(parse_latency_write(&content), Ok(5_000_000));
      content[26..30].copy_from_slice(&6_000_000u32.to_be_bytes());
      assert!(parse_latency_write(&content).is_err());
      assert!(parse_latency_write(&[]).is_err());
      assert!(parse_latency_write(&[1, 4, 0x82, 5, 0, 0x80]).is_err());

      let reply = latency_write_reply(10_000_000);
      assert_eq!(&reply[..18], &[4, 4, 0x82, 5, 0, 28, 2, 0x11, 0, 4, 0x83, 1, 0, 32, 3, 0x10, 0, 4]);
      assert_eq!(&reply[18..], &[0, 0x98, 0x96, 0x80, 0, 0x98, 0x96, 0x80]);

      let directory = directory();
      let flags = |id: u16| directory[2..].chunks_exact(4)
        .find(|r| r[..2] == id.to_be_bytes()).map(|r| u16::from_be_bytes([r[2], r[3]]));
      assert_eq!((flags(0x8205), flags(0x8301), flags(0x8302)), (Some(3), Some(3), Some(1)));
    }

    #[test]
    fn device_name_requests() {
      assert_eq!(super::super::parse_device_name(b"Studio-PC\0"), Ok(Some("Studio-PC".into())));
      assert_eq!(super::super::parse_device_name(b""), Ok(None));
      assert_eq!(super::super::parse_device_name(b"\0\0"), Ok(None));
      assert!(super::super::parse_device_name(&[0xff, 0]).is_err());
    }

    #[test]
    fn every_advertised_routing_property_has_a_value() {
      let settings = Settings::new("ARC test", "ARC", Some(Ipv4Addr::LOCALHOST), &BTreeMap::new());
      let mut request = vec![0, IDS.len() as u8];
      for id in IDS {
        request.extend_from_slice(&id.to_be_bytes());
      }
      let reply = read(&request, &settings.self_info).unwrap();
      assert_eq!(&reply[..2], &[IDS.len() as u8; 2]);
      for (index, id) in IDS.into_iter().enumerate() {
        let row = 2 + index * 4;
        assert_eq!(&reply[row..row + 2], &id.to_be_bytes());
      }
      for (id, expected) in [(0x0201, 1u16), (0x0303, 4)] {
        let reply = read(&[0, 1, (id >> 8) as u8, id as u8], &settings.self_info).unwrap();
        assert_eq!(&reply[4..6], &expected.to_be_bytes());
      }
    }
  }
}

pub mod get_device_names {
  use binary_serde::BinarySerde;

  pub const OPCODE: u16 = 0x1003;

  #[derive(Debug, BinarySerde, Default)]
  pub struct ResponseHeader {
    pub unknown1_0: u16,
    pub unknown2_0: u16, // was 0x14
    pub unknown3_0: u16, // was 0x20
    pub board_name_offset: u16,
    pub revision_string_offset: u16,
    pub unknown4_0: u16, // was 0x500
    pub friendly_hostname_offset1: u16,
    pub factory_hostname_offset: u16,
    pub friendly_hostname_offset2: u16,
    pub unknown5_0: [u16; 6], // was [0, 0, 4, 0, 4, 0]
    pub start_code: u16, // 0x2729
    pub unknown6_0: u16,
    pub unknown_opcode_1102: u16,
    pub unknown7_0: u16,
  }
}

#[derive(Debug, BinarySerde, Default)]
pub struct CommonChannelsDescriptor {
  pub sample_rate: u32,
  pub unknown1_1: u8,
  pub unknown2_1: u8,
  pub bits_per_sample_1: u16,
  pub unknown3_400: u16,
  pub bits_per_sample_2: u16,
  pub bits_per_sample_3: u16,
  pub pcm_type: u16,
}

impl CommonChannelsDescriptor {
  pub fn new(self_info: &DeviceInfo) -> Self {
    Self {
      sample_rate: self_info.sample_rate,
      unknown1_1: 1,
      unknown2_1: 1,
      bits_per_sample_1: self_info.bits_per_sample.into(),
      unknown3_400: 0x400,
      bits_per_sample_2: self_info.bits_per_sample.into(),
      bits_per_sample_3: self_info.bits_per_sample.into(),
      pcm_type: self_info.pcm_type.into(),
    }
  }
}

pub mod get_receive_channels {
  use binary_serde::BinarySerde;

  pub const OPCODE: u16 = 0x3000;

  #[derive(Debug, BinarySerde, Default)]
  pub struct ChannelDescriptor {
    pub channel_id: u16,
    pub unknown1_6: u16,
    pub common_descriptor_offset: u16,
    pub tx_channel_name_offset: u16,
    pub tx_hostname_offset: u16,
    pub friendly_name_offset: u16,
    pub subscription_status: u32, // TODO. 0x01010009 if subscribed currently, 0x00000001 if not found but remembers subscription or in progress
    pub unknown2_0: u32,
  }
}

pub mod get_transmit_channels {
  use binary_serde::BinarySerde;

  pub const OPCODE: u16 = 0x2000;

  #[derive(Debug, BinarySerde, Default)]
  pub struct ChannelDescriptor {
    pub channel_id: u16,
    pub unknown1_7: u16,
    pub common_descriptor_offset: u16,
    pub name_offset: u16,
  }
}

pub mod get_transmit_channels_friendly_names {
  use binary_serde::BinarySerde;

  pub const OPCODE: u16 = 0x2010;

  #[derive(Debug, BinarySerde, Default)]
  pub struct ChannelDescriptor {
    pub channel_id_1: u16,
    pub channel_id_2: u16,
    pub friendly_name_offset: u16,
  }
}

pub mod rename_tx_channels {
  pub const OPCODE: u16 = 0x2013;
  
  #[derive(Debug, binary_serde::BinarySerde, Default)]
  pub struct SingleChannelRenameRequest {
    pub unknown1_0: u16,
    pub channel_id: u16,
    pub new_name_offset: u16,
  }
}

pub mod rename_rx_channels {
  pub const OPCODE: u16 = 0x3001;

  #[derive(Debug, binary_serde::BinarySerde, Default)]
  pub struct SingleChannelRenameRequest {
    pub channel_id: u16,
    pub new_name_offset: u16,
  }
}

#[derive(Debug, binary_serde::BinarySerde, Default)]
pub struct DestinationSocketDescriptor {
  pub unknown1_8002: u16,
  pub port: u16,
  pub addr: [u8; 4],
}

pub mod query_tx_flows {
  pub const OPCODE: u16 = 0x2200;

  #[derive(Debug, binary_serde::BinarySerde, Default)]
  pub struct NamesDescriptor {
    pub unknown1_a00: u16,
    pub unknown2_1: u16,
    pub remote_hostname_offset: u16,
    pub remote_rx_flow_name_offset: u16,
    pub unknown3_10: u16, // or 0x3c ???
    pub local_tx_flow_name_offset: u16,
    pub unknown4_0: [u8; 8], // in multicast flows first 4B are latency_ns
  }

  #[derive(Debug, binary_serde::BinarySerde, Default)]
  pub struct FlowDescriptorHeader {
    pub flow_id: u16,
    pub flow_type: u16, // 0x11 for unicast, 2 for multicast
    pub sample_rate: u32,
    pub unknown1_0: u16,
    pub bits_per_sample: u16,
    pub unknown2_1: u16,
    pub channels_count: u16,
    pub receiver_socket_descriptor_offset: u16,
  }

  #[derive(Debug, binary_serde::BinarySerde, Default)]
  pub struct FlowDescriptorFooter {
    pub names_descriptor_offset: u16,
  }
}

pub mod create_multicast_tx_flow {
  pub const OPCODE: u16 = 0x2201;

  #[derive(Debug, binary_serde::BinarySerde, Default)]
  pub struct FlowDescriptorHeader {
    pub flow_id: u16,
    pub flow_type: u16,
    pub unknown1_0: [u8; 10],
    pub channels_count: u16,
  }

  #[derive(Debug, binary_serde::BinarySerde, Default)]
  pub struct FlowDescriptorFooter {
    pub mostly_zeros_offset: u16,
  }

  #[derive(Debug, binary_serde::BinarySerde, Default)]
  pub struct MostlyZeros {
    pub unknown1_a00: u16,
    pub unknown2_0: [u8; 14],
    pub unknown3_1: u16,
    pub unknown4_0: u16,
  }
}

pub mod delete_multicast_tx_flow {
  pub const OPCODE: u16 = 0x2202;
}


pub mod query_rx_flows {
  pub const OPCODE: u16 = 0x3200;

  #[derive(Debug, binary_serde::BinarySerde, Default)]
  pub struct Descriptor2 {
    pub unknown1_9: u16,
    pub unknown2_1: u16,
    pub unknown3_800: u16,
    pub unknown4_0: u16,
    pub latency_ns: u32,
    pub unknown5_0: u32,
  }

  #[derive(Debug, binary_serde::BinarySerde, Default)]
  pub struct FlowDescriptorHeader {
    pub flow_id: u16,
    pub unknown1_1: u16,
    pub sample_rate: u32,
    pub unknown2_0: u16,
    pub bits_per_sample: u16,
    pub unknown3_1: u16,
    pub channels_count: u16,
    pub words_per_bitmask: u16,
    pub receiver_socket_descriptor_offset: u16,
  }

  #[derive(Debug, binary_serde::BinarySerde, Default)]
  pub struct FlowDescriptorFooter {
    pub descriptor2_offset: u16,
  }
}

pub mod set_channels_subscriptions {
  pub const OPCODE: u16 = 0x3010;

  #[derive(Debug, binary_serde::BinarySerde, Default)]
  pub struct SingleChannelSubscriptionRequest {
    pub local_channel_id: u16,
    pub tx_channel_name_offset: u16,
    pub tx_hostname_offset: u16,
  }
}

pub fn serialize_items<InItem, OutItem>(
  space_items: u8,
  source: impl IntoIterator<Item = InItem>,
  mut transform: impl FnMut(InItem, &mut ByteBuffer) -> Option<OutItem>
) -> (bool, Vec<u8>)
  where OutItem: BinarySerde {
  let mut bytes = ByteBuffer::new();
  bytes.write_bytes(&[0u8; HEADER_LENGTH]);
  bytes.write_u8(space_items);
  bytes.write_u8(0);
  if space_items == 0 {
    return (false, bytes.as_bytes()[HEADER_LENGTH..].into());
  }
  let space_items: usize = space_items.into();
  bytes.write_bytes(&vec![0u8; space_items*OutItem::SERIALIZED_SIZE]);
  
  let mut source = source.into_iter().peekable();
  let mut item_pos = 2 + HEADER_LENGTH;
  let mut actual_items = 0;
  let mut have_more = false;

  let mut tmp_buffer = vec![0u8; OutItem::SERIALIZED_SIZE];
  while let Some(in_item) = source.next() {
    if actual_items >= space_items {
      have_more = true;
      break;
    }
    let out_item = if let Some(item) = transform(in_item, &mut bytes) {
      item
    } else {
      continue;
    };
    out_item.binary_serialize(&mut tmp_buffer, binary_serde::Endianness::Big);
    let prev_pos = bytes.get_wpos();
    bytes.set_wpos(item_pos);
    bytes.write_bytes(&tmp_buffer);
    bytes.set_wpos(prev_pos);
    item_pos += OutItem::SERIALIZED_SIZE;
    actual_items += 1;
    if prev_pos >= PACKET_SIZE_SOFT_LIMIT {
      have_more = source.peek().is_some();
      break;
    }
  }
  bytes.set_wpos(1 + HEADER_LENGTH);
  bytes.write_u8(actual_items.try_into().unwrap_or_else(|_| {
    error!("actual_items exceeds u8 max");
    255
  }));
  (have_more, bytes.as_bytes()[HEADER_LENGTH..].into())
}

pub fn extract_start_index(request_payload: &[u8]) -> Option<usize> {
  if request_payload.len() < 4 || (request_payload[2]|request_payload[3])==0 {
    error!("got invalid paginate request, payload: {request_payload:?}");
    return None;
  }
  Some((make_u16(request_payload[2], request_payload[3]) - 1).into())
}

pub fn paginate_make_response<InItem, OutItem>(
  connection: &mut Connection,
  request_payload: &[u8],
  space_items: u8,
  source: impl IntoIterator<Item = InItem>,
  transform: impl FnMut(InItem, &mut ByteBuffer) -> Option<OutItem>
) -> (u16, Vec<u8>)
  where OutItem: BinarySerde {
  let start_index = match extract_start_index(request_payload) {
    Some(v) => v,
    None => {
      error!("unable to extract start index from request payload {}", hex::encode(request_payload));
      return (0xFFFF /* TODO */, vec![]);
    }
  };
  let (have_more, bytes) = serialize_items(space_items, source.into_iter().skip(start_index), transform);
  let code = if have_more { 0x8112 } else { 1 };
  (code, bytes)
}

pub async fn paginate_respond<InItem, OutItem>(
  connection: &mut Connection,
  request_payload: &[u8],
  space_items: u8,
  source: impl IntoIterator<Item = InItem>,
  transform: impl FnMut(InItem, &mut ByteBuffer) -> Option<OutItem>
)
  where OutItem: BinarySerde {
  let (code, bytes) = paginate_make_response(connection, request_payload, space_items, source, transform);
  connection.respond_with_code(code, &bytes).await;
}


pub struct ItemsInPacketIterator<'a, T> {
  items_bytes: &'a [u8],
  item_start: usize,
  _t: PhantomData<T>,
}

impl<'a, T: BinarySerde> Iterator for ItemsInPacketIterator<'a, T> {
  type Item = T;
  fn next(&mut self) -> Option<Self::Item> {
    loop {
      let item_start = self.item_start;
      let item_end = item_start + T::SERIALIZED_SIZE;
      self.item_start = item_end;
      if item_end > self.items_bytes.len() {
        return None;
      }
      match T::binary_deserialize(&self.items_bytes[item_start..item_end], binary_serde::Endianness::Big) {
        Ok(item) => {
          return Some(item);
        },
        Err(e) => {
          error!("unable to deserialize item in incoming packet: {e:?}, item: {}, all items: {}", hex::encode(&self.items_bytes[item_start..item_end]), hex::encode(&self.items_bytes));
        }
      }
    }
  }
}

pub fn deserialize_items<'a, T: BinarySerde>(payload: &'a [u8]) -> ItemsInPacketIterator<'a, T> {
  let num_items: usize = (*payload.get(1).unwrap_or(&0)).into();
  let num_items = num_items.min(payload.len() / T::SERIALIZED_SIZE);
  ItemsInPacketIterator::<'a, T> {
    items_bytes: &payload[2..][..num_items*T::SERIALIZED_SIZE],
    item_start: 0,
    _t: Default::default()
  }
}

#[cfg(test)]
mod pagination_tests {
  use super::*;

  #[test]
  fn counts_descriptor_that_crosses_packet_limit() {
    let (more, bytes) = serialize_items(32, 1u16..=32, |channel_id, bytes| {
      bytes.write_bytes(&[0; 40]);
      Some(channel_id)
    });
    assert!(more);
    // Header + counts + reserved descriptors + 19 * 40 bytes crosses 800.
    assert_eq!(bytes[1], 19);
    for index in 0..usize::from(bytes[1]) {
      assert_eq!(&bytes[2 + index * 2..4 + index * 2], &(index as u16 + 1).to_be_bytes());
    }
  }

  #[test]
  fn final_descriptor_at_packet_limit_does_not_request_another_page() {
    let (more, bytes) = serialize_items(1, [1u16], |channel_id, bytes| {
      bytes.write_bytes(&[0; PACKET_SIZE_SOFT_LIMIT]);
      Some(channel_id)
    });
    assert!(!more);
    assert_eq!(&bytes[..4], &[1, 1, 0, 1]);
  }

  #[test]
  fn channel_pages_preserve_ten_and_thirty_two_channel_ids_and_offsets() {
    for count in [10u16, 32] {
      let mut next = 1u16;
      let mut ids = Vec::new();
      loop {
        let (more, bytes) = serialize_items(count as u8, next..=count, |channel_id, bytes| {
          let name_offset = bytes.get_wpos() as u16;
          bytes.write_bytes(format!("Channel {channel_id:02} with long name\0").as_bytes());
          Some(get_transmit_channels::ChannelDescriptor {
            channel_id,
            unknown1_7: 7,
            common_descriptor_offset: 0,
            name_offset,
          })
        });
        assert!(bytes[1] > 0);
        for descriptor in deserialize_items::<get_transmit_channels::ChannelDescriptor>(&bytes) {
          assert_eq!(descriptor.channel_id, next);
          let offset = usize::from(descriptor.name_offset) - HEADER_LENGTH;
          assert!(offset < bytes.len());
          assert!(bytes[offset..].contains(&0));
          ids.push(descriptor.channel_id);
          next += 1;
        }
        if !more { break; }
      }
      assert_eq!(ids, (1..=count).collect::<Vec<_>>());
    }
  }
}
