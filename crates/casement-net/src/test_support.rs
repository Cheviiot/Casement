//! Private loopback transport used by network regression tests.

use std::sync::Arc;

pub(crate) async fn loopback_pair() -> (
    quinn::Endpoint,
    quinn::Endpoint,
    quinn::Connection,
    quinn::Connection,
) {
    let dir = tempfile::tempdir().unwrap();
    let server_identity = crate::Identity::load_or_create(&dir.path().join("server")).unwrap();
    let server_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(
        crate::tls::server_config(&server_identity).unwrap(),
    )
    .unwrap();
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(server_crypto));
    server_config.transport_config(Arc::new(crate::transport_config()));
    let server = quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let address = server.local_addr().unwrap();
    let server_accept = server.clone();
    let accepted =
        tokio::spawn(async move { server_accept.accept().await.unwrap().await.unwrap() });

    let client_identity = crate::Identity::load_or_create(&dir.path().join("client")).unwrap();
    let seen = crate::tls::SeenKey::default();
    let client_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(
        crate::tls::client_config(
            &client_identity,
            crate::tls::HostKeyVerifier::new(None, seen),
        )
        .unwrap(),
    )
    .unwrap();
    let mut client_config = quinn::ClientConfig::new(Arc::new(client_crypto));
    client_config.transport_config(Arc::new(crate::transport_config()));
    let mut client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    client.set_default_client_config(client_config);
    let connection = client
        .connect(address, crate::tls::SERVER_NAME)
        .unwrap()
        .await
        .unwrap();
    let peer = accepted.await.unwrap();
    (client, server, connection, peer)
}
