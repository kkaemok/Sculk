//! Offline demonstration only: no KakaoTalk connection or login.
use futures_util::{SinkExt, StreamExt};
use sculk_client::{Client, Config};
use sculk_protocol::{Command, LocoCodec, Packet};
use tokio_util::codec::Framed;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (local, remote) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
        let mut framed = Framed::new(remote, LocoCodec::default());
        while let Some(Ok(packet)) = framed.next().await {
            framed.send(packet).await.unwrap();
        }
    });
    let client = Client::from_stream(local, Config::default())?;
    let packet = Packet::from_document(0, Command::new("PING")?, &bson::doc! { "offline": true })?;
    let response = client.request(packet).await?;
    println!(
        "Offline mock response: id={}, command={}",
        response.id,
        response.command.as_str()
    );
    client.disconnect().await;
    server.await?;
    Ok(())
}
