use rcgen::{
    CertificateParams, DnType, ExtendedKeyUsagePurpose, Issuer, KeyPair, KeyUsagePurpose,
    PublicKeyData,
};
use rustls::{
    ServerConfig,
    pki_types::PrivatePkcs8KeyDer,
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};
use std::{collections::HashMap, sync::Arc};
use time::{Duration, OffsetDateTime};
use tokio_rustls::TlsAcceptor;

#[derive(Debug)]
struct HostCertificate {
    host: String,
    key: Arc<CertifiedKey>,
}

impl ResolvesServerCert for HostCertificate {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        (hello.server_name() == Some(self.host.as_str())).then(|| self.key.clone())
    }
}

// One certificate/key per host per process; no leaf files or on-demand signing.
pub(crate) fn acceptors<'a>(
    ca_pem: &str,
    key_pem: &str,
    hosts: impl Iterator<Item = &'a str>,
) -> Result<HashMap<String, TlsAcceptor>, String> {
    let error = |_| "invalid CA certificate or signing key".to_owned();
    let mut certificates = rustls_pemfile::certs(&mut ca_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    if certificates.len() != 1 {
        return Err("provide exactly one CA certificate".into());
    }
    let ca = certificates.remove(0);
    let (_, parsed) =
        x509_parser::parse_x509_certificate(&ca).map_err(|_| "invalid CA certificate")?;
    let key = KeyPair::from_pem(key_pem)
        .map_err(|_| "invalid CA signing key (use unencrypted PKCS#8 PEM)")?;
    if !parsed.validity().is_valid()
        || !parsed
            .basic_constraints()
            .map_err(|_| "invalid CA constraints")?
            .is_some_and(|c| c.value.ca)
        || parsed
            .key_usage()
            .map_err(|_| "invalid CA key usage")?
            .is_some_and(|usage| !usage.value.key_cert_sign())
        || parsed.public_key().raw != key.subject_public_key_info()
    {
        return Err("CA must be valid, permit certificate signing, and match the key".into());
    }
    let expires = OffsetDateTime::from_unix_timestamp(parsed.validity().not_after.timestamp())
        .map_err(|_| "invalid CA expiry")?;
    let issuer = Issuer::from_ca_cert_pem(ca_pem, key).map_err(|_| "invalid CA issuer")?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let now = OffsetDateTime::now_utc();
    let mut result = HashMap::new();
    for host in hosts {
        let leaf_key = KeyPair::generate().map_err(|_| "could not generate leaf key")?;
        let mut params = CertificateParams::new(vec![host.into()])
            .map_err(|_| "invalid certificate hostname")?;
        params.distinguished_name.push(DnType::CommonName, host);
        params.use_authority_key_identifier_extension = true;
        params.not_before = now - Duration::days(1);
        params.not_after = (now + Duration::days(365)).min(expires);
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let certificate = params
            .signed_by(&leaf_key, &issuer)
            .map_err(|_| "could not sign leaf certificate")?;
        let key = CertifiedKey::from_der(
            vec![certificate.der().clone(), ca.clone()],
            PrivatePkcs8KeyDer::from(leaf_key.serialize_der()).into(),
            &provider,
        )
        .map_err(|_| "could not initialize leaf certificate")?;
        let resolver = Arc::new(HostCertificate {
            host: host.into(),
            key: Arc::new(key),
        });
        let mut config = ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|_| "could not initialize TLS versions")?
            .with_no_client_auth()
            .with_cert_resolver(resolver);
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        result.insert(host.into(), TlsAcceptor::from(Arc::new(config)));
    }
    Ok(result)
}
