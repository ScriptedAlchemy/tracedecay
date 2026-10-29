use super::RemoteBrainTlsConfig;
use std::path::PathBuf;
use tracedecay_domain::errors::TraceDecayError;

#[test]
fn remote_tls_configuration_rejects_partial_and_wildcard_admission() {
    let path = PathBuf::from("remote.pem");
    let refusal =
        |listen: &str, key: Option<PathBuf>| match RemoteBrainTlsConfig::from_optional_parts(
            Some(listen.parse().unwrap()),
            Some(path.clone()),
            key,
        ) {
            Err(TraceDecayError::Config { message }) => message,
            Err(other) => panic!("unexpected refusal kind: {other:?}"),
            Ok(_) => panic!("{listen} must be refused"),
        };
    assert_eq!(
        refusal("127.0.0.1:7443", None),
        "Remote Brain TLS listener requires --remote-listen, --remote-tls-cert, and --remote-tls-key together"
    );
    assert_eq!(
        refusal("0.0.0.0:7443", Some(path.clone())),
        "Remote Brain TLS listener requires an explicit interface address; wildcard addresses are refused"
    );
    let admitted = RemoteBrainTlsConfig::from_optional_parts(
        Some("127.0.0.1:7443".parse().unwrap()),
        Some(path.clone()),
        Some(path.clone()),
    )
    .expect("explicit interface with both paths")
    .expect("configured listener");
    assert_eq!(admitted.listen(), "127.0.0.1:7443".parse().unwrap());
    assert_eq!(admitted.private_key(), path.as_path());
}
