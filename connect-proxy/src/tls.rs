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
    let mut certificates = rustls_pemfile::certs(&mut ca_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("invalid CA certificate PEM: {error}"))?;
    if certificates.len() != 1 {
        return Err(format!(
            "provide exactly one CA certificate; found {}",
            certificates.len()
        ));
    }
    let ca = certificates.remove(0);
    let (_, parsed) = x509_parser::parse_x509_certificate(&ca)
        .map_err(|error| format!("invalid CA certificate: {error}"))?;
    let key = KeyPair::from_pem(key_pem).map_err(|error| {
        // PEM parse errors may quote input. Keep private key bytes out of logs.
        let reason = match error {
            rcgen::Error::PemError(_) => "malformed PEM encoding".into(),
            error => error.to_string(),
        };
        format!("invalid CA signing key (use unencrypted PKCS#8 PEM): {reason}")
    })?;
    if !parsed.validity().is_valid() {
        return Err(format!(
            "CA certificate is not currently valid (not before {}, not after {}); check certificate dates and host clock",
            parsed.validity().not_before,
            parsed.validity().not_after
        ));
    }
    if !parsed
        .basic_constraints()
        .map_err(|error| format!("invalid CA constraints: {error}"))?
        .is_some_and(|c| c.value.ca)
    {
        return Err("CA certificate must have basicConstraints CA:TRUE".into());
    }
    if parsed
        .key_usage()
        .map_err(|error| format!("invalid CA key usage: {error}"))?
        .is_some_and(|usage| !usage.value.key_cert_sign())
    {
        return Err("CA certificate key usage must permit certificate signing".into());
    }
    if parsed.public_key().raw != key.subject_public_key_info() {
        return Err("CA certificate public key does not match the signing key".into());
    }
    let expires = OffsetDateTime::from_unix_timestamp(parsed.validity().not_after.timestamp())
        .map_err(|error| format!("invalid CA expiry: {error}"))?;
    let issuer = Issuer::from_ca_cert_pem(ca_pem, key)
        .map_err(|error| format!("invalid CA issuer: {error}"))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let now = OffsetDateTime::now_utc();
    let mut result = HashMap::new();
    for host in hosts {
        let leaf_key = KeyPair::generate()
            .map_err(|error| format!("host {host:?}: could not generate leaf key: {error}"))?;
        let mut params = CertificateParams::new(vec![host.into()])
            .map_err(|error| format!("host {host:?}: invalid certificate hostname: {error}"))?;
        params.distinguished_name.push(DnType::CommonName, host);
        params.use_authority_key_identifier_extension = true;
        params.not_before = now - Duration::days(1);
        params.not_after = (now + Duration::days(365)).min(expires);
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let certificate = params
            .signed_by(&leaf_key, &issuer)
            .map_err(|error| format!("host {host:?}: could not sign leaf certificate: {error}"))?;
        let key = CertifiedKey::from_der(
            vec![certificate.der().clone(), ca.clone()],
            PrivatePkcs8KeyDer::from(leaf_key.serialize_der()).into(),
            &provider,
        )
        .map_err(|error| {
            format!("host {host:?}: could not initialize leaf certificate: {error}")
        })?;
        let resolver = Arc::new(HostCertificate {
            host: host.into(),
            key: Arc::new(key),
        });
        let mut config = ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|error| format!("host {host:?}: could not initialize TLS versions: {error}"))?
            .with_no_client_auth()
            .with_cert_resolver(resolver);
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        result.insert(host.into(), TlsAcceptor::from(Arc::new(config)));
    }
    Ok(result)
}
