use bson::doc;
use bytes::{Bytes, BytesMut};
use proptest::prelude::*;
use sculk_protocol::{Command, Error, LocoCodec, Packet, DEFAULT_MAX_BODY, HEADER_LEN};
use tokio_util::codec::{Decoder, Encoder};
fn packet() -> Packet {
    Packet::from_document(0x12345678, Command::new("PING").unwrap(), &doc! {}).unwrap()
}
fn wire() -> BytesMut {
    let mut b = BytesMut::new();
    LocoCodec::default().encode(packet(), &mut b).unwrap();
    b
}
#[test]
fn golden_fixture_and_roundtrip() {
    let fixture = include_bytes!("../../../tests/fixtures/ping.bin");
    assert_eq!(&wire()[..], fixture);
    assert_eq!(
        LocoCodec::default()
            .decode(&mut BytesMut::from(&fixture[..]))
            .unwrap(),
        Some(packet())
    );
}
#[test]
fn every_fragment_boundary_and_concatenation() {
    let bytes = wire();
    for split in 0..bytes.len() {
        let mut codec = LocoCodec::default();
        let mut input = BytesMut::from(&bytes[..split]);
        assert!(codec.decode(&mut input).unwrap().is_none());
        assert_eq!(&input[..], &bytes[..split]);
        input.extend_from_slice(&bytes[split..]);
        input.extend_from_slice(&bytes);
        assert_eq!(codec.decode(&mut input).unwrap(), Some(packet()));
        assert_eq!(codec.decode(&mut input).unwrap(), Some(packet()));
        assert!(input.is_empty());
    }
}
#[test]
fn rejects_untrusted_lengths_without_reserving() {
    for length in [0, 4, DEFAULT_MAX_BODY as u32 + 1, u32::MAX] {
        let mut input = wire();
        input.truncate(HEADER_LEN);
        input[18..22].copy_from_slice(&length.to_le_bytes());
        let capacity = input.capacity();
        assert!(matches!(
            LocoCodec::default().decode(&mut input),
            Err(Error::Length(_))
        ));
        assert_eq!(capacity, input.capacity());
    }
}
#[test]
fn malformed_headers_and_bson_are_fatal() {
    for (offset, value) in [(6, 0xff), (8, 0), (17, 1), (26, 1), (22, 6)] {
        let mut input = wire();
        input[offset] = value;
        assert!(
            LocoCodec::default().decode(&mut input).is_err(),
            "offset {offset}"
        );
    }
    assert!(Command::new("").is_err());
    assert!(Command::new("TOO_LONG_1234").is_err());
    assert!(Command::new("bad\0name").is_err());
}
#[test]
fn eof_distinguishes_clean_and_partial() {
    for end in 1..wire().len() {
        assert!(matches!(
            LocoCodec::default().decode_eof(&mut BytesMut::from(&wire()[..end])),
            Err(Error::Truncated)
        ));
    }
    assert!(LocoCodec::default()
        .decode_eof(&mut BytesMut::new())
        .unwrap()
        .is_none());
}
#[test]
fn unknown_commands_status_and_boundary_limits() {
    let mut p = packet();
    p.command = Command::new("UNKNOWN1234").unwrap();
    p.status = 65535;
    let mut buf = BytesMut::new();
    let mut codec = LocoCodec::new(5).unwrap();
    codec.encode(p.clone(), &mut buf).unwrap();
    assert_eq!(codec.decode(&mut buf).unwrap(), Some(p));
    assert!(LocoCodec::new(4).is_err());
    assert!(LocoCodec::new(DEFAULT_MAX_BODY + 1).is_err());
    let large = Packet::from_document(1, Command::new("PING").unwrap(), &doc! {"x": "a"}).unwrap();
    assert!(codec.encode(large, &mut buf).is_err());
}
#[test]
fn nested_documents_and_arrays_are_bounded() {
    let mut d = doc! { "value": 1 };
    for _ in 0..40 {
        d = doc! { "nested": [d] };
    }
    assert!(matches!(
        Packet::from_document(1, Command::new("NESTED").unwrap(), &d),
        Err(Error::Bson)
    ));
    assert!(Packet::from_bytes(
        1,
        0,
        Command::new("PING").unwrap(),
        Bytes::from_static(&[5, 0, 0, 0, 1])
    )
    .is_err());
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    #[test]
    fn parser_never_panics_or_grows_on_arbitrary_input(data in prop::collection::vec(any::<u8>(), 0..4096)) {
        let mut buffer = BytesMut::from(data.as_slice());
        let capacity = buffer.capacity();
        let _ = LocoCodec::default().decode(&mut buffer);
        prop_assert_eq!(buffer.capacity(), capacity);
    }
    #[test]
    fn bson_roundtrip(id in any::<u32>(), text in ".{0,256}", n in any::<i64>()) {
        let document = doc! { "text": text, "n": n };
        let p = Packet::from_document(id, Command::new("TEST").unwrap(), &document).unwrap();
        let mut b = BytesMut::new(); let mut codec = LocoCodec::default();
        codec.encode(p.clone(), &mut b).unwrap();
        let result = codec.decode(&mut b).unwrap().unwrap();
        prop_assert_eq!(result.document().unwrap(), document);
        prop_assert_eq!(result, p);
    }
}

#[test]
fn accepts_exact_maximum_body_and_rejects_one_byte_more() {
    let document = doc! {"x": "x".repeat(DEFAULT_MAX_BODY - 13)};
    let p = Packet::from_document(1, Command::new("BOUNDARY").unwrap(), &document).unwrap();
    assert_eq!(p.body().len(), DEFAULT_MAX_BODY);
    let mut bytes = BytesMut::new();
    let mut codec = LocoCodec::default();
    codec.encode(p.clone(), &mut bytes).unwrap();
    assert_eq!(codec.decode(&mut bytes).unwrap(), Some(p));
    let too_large = doc! {"x": "x".repeat(DEFAULT_MAX_BODY - 12)};
    assert!(matches!(
        Packet::from_document(1, Command::new("BOUNDARY").unwrap(), &too_large),
        Err(Error::Length(_))
    ));
}
