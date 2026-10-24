use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use rcgen::{
    BasicConstraints, Certificate, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer,
    KeyPair, KeyUsagePurpose,
};

pub(super) struct TlsIdentity {
    pub certificate: Certificate,
    pub key: KeyPair,
    pub root_pem: String,
}

pub(super) fn tls_identity() -> &'static TlsIdentity {
    static IDENTITY: OnceLock<TlsIdentity> = OnceLock::new();
    IDENTITY.get_or_init(generate_identity)
}

fn generate_identity() -> TlsIdentity {
    let now = SystemTime::now();
    let mut root = CertificateParams::new(Vec::<String>::new()).unwrap();
    root.not_before = (now - Duration::from_secs(86_400)).into();
    root.not_after = (now + Duration::from_secs(86_400)).into();
    root.distinguished_name
        .push(rcgen::DnType::CommonName, "TraceDecay redirect test root");
    root.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    root.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let root_key = KeyPair::generate().unwrap();
    let root_certificate = root.self_signed(&root_key).unwrap();
    let issuer = Issuer::new(root, root_key);
    let mut leaf =
        CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()]).unwrap();
    leaf.not_before = (now - Duration::from_secs(86_400)).into();
    leaf.not_after = (now + Duration::from_secs(86_400)).into();
    leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let key = KeyPair::generate().unwrap();
    let certificate = leaf.signed_by(&key, &issuer).unwrap();
    TlsIdentity {
        certificate,
        key,
        root_pem: root_certificate.pem(),
    }
}
