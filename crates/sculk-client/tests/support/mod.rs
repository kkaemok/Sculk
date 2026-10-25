use sculk_client::tls::{TlsSettings, TrustStore};
use std::sync::Arc;
use tokio_rustls::{
    rustls::{self, pki_types::PrivatePkcs8KeyDer},
    TlsAcceptor,
};
pub fn tls_pair() -> (TlsSettings, TlsAcceptor) {
    make_pair(false)
}
#[allow(dead_code)] // Used by TLS tests; shared support also compiles in session tests.
pub fn expired_pair() -> (TlsSettings, TlsAcceptor) {
    make_pair(true)
}
fn make_pair(expired: bool) -> (TlsSettings, TlsAcceptor) {
    let mut params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
    if expired {
        params.not_before = rcgen::date_time_ymd(2000, 1, 1);
        params.not_after = rcgen::date_time_ymd(2001, 1, 1);
    }
    let signing_key = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&signing_key).unwrap();
    let mut roots = TrustStore::empty();
    roots.add(cert.der().clone()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
        )
        .unwrap();
    (
        TlsSettings::with_roots(roots),
        TlsAcceptor::from(Arc::new(config)),
    )
}
