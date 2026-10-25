#![no_main]
use libfuzzer_sys::fuzz_target;
use tokio_util::codec::Decoder;
fuzz_target!(|data: &[u8]| {
    if data.len() > 2 * sculk_protocol::DEFAULT_MAX_BODY {
        return;
    }
    let mut codec = sculk_protocol::LocoCodec::default();
    let mut input = bytes::BytesMut::from(data);
    while let Ok(Some(_)) = codec.decode(&mut input) {}
    let _ = codec.decode_eof(&mut input);
});
