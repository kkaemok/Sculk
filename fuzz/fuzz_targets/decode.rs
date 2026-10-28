#![no_main]
use libfuzzer_sys::fuzz_target;
use tokio_util::codec::Decoder;
fuzz_target!(|data: &[u8]| {
    if data.len() > 2 * sculk_protocol::DEFAULT_MAX_BODY {
        return;
    }
    let mut codec = sculk_protocol::LocoCodec::default();
    let mut input = bytes::BytesMut::from(data);
    while let Ok(Some(packet)) = codec.decode(&mut input) {
        let event = sculk_client::messaging::Event::from_packet(packet.clone());
        if let sculk_client::messaging::Event::Message(message) = event {
            let _ = (message.text(), message.attachment(), message.previous_id());
        }
        if packet.command.as_str() == "SYNCMSG" {
            let request = sculk_client::synchronization::Request::new(
                sculk_client::read_only::RoomId::new(9_007_199_254_740_993).unwrap(),
                sculk_client::synchronization::Cursor::default(),
                sculk_client::messaging::MessageId::new(i64::MAX).unwrap(),
                100,
            )
            .unwrap();
            if let Ok(page) = sculk_client::synchronization::Page::parse(request, packet) {
                for message in page.messages() {
                    let _ = (message.text(), message.attachment(), message.previous_id());
                }
            }
        }
    }
    let _ = codec.decode_eof(&mut input);
});
