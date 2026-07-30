use super::remote_https::{
    RemoteBrainHttpsConfigV1, RemoteBrainHttpsEnablementV1, RemoteBrainHttpsError,
    RemoteBrainHttpsStateV1,
};

#[test]
fn remote_brain_https_is_unconfigured_by_default() {
    let config = RemoteBrainHttpsConfigV1::default();

    assert_eq!(config.version, 1);
    assert_eq!(config.state(), RemoteBrainHttpsStateV1::Unconfigured);
}

#[test]
fn enabled_remote_brain_listener_rejects_plaintext_advertised_endpoint() {
    let config = RemoteBrainHttpsConfigV1 {
        enablement: RemoteBrainHttpsEnablementV1::Enabled,
        bind_address: Some("127.0.0.1:0".to_owned()),
        advertised_endpoint: Some("http://remote.example".to_owned()),
        certificate_chain_path: Some("test-cert.pem".into()),
        private_key_path: Some("test-key.pem".into()),
        ..RemoteBrainHttpsConfigV1::default()
    };

    assert!(matches!(
        config.validate_enabled(),
        Err(RemoteBrainHttpsError::InvalidAdvertisedEndpoint)
    ));
}

#[test]
fn enabled_remote_brain_listener_requires_client_authority_roots() {
    let config = RemoteBrainHttpsConfigV1 {
        enablement: RemoteBrainHttpsEnablementV1::Enabled,
        bind_address: Some("127.0.0.1:0".to_owned()),
        advertised_endpoint: Some("https://remote.example".to_owned()),
        certificate_chain_path: Some("test-cert.pem".into()),
        private_key_path: Some("test-key.pem".into()),
        ..RemoteBrainHttpsConfigV1::default()
    };

    assert!(matches!(
        config.validate_enabled(),
        Err(RemoteBrainHttpsError::MissingField("client_ca_bundle_path"))
    ));
}

#[test]
fn enabled_remote_brain_listener_retains_client_authority_roots() {
    let config = RemoteBrainHttpsConfigV1 {
        enablement: RemoteBrainHttpsEnablementV1::Enabled,
        bind_address: Some("127.0.0.1:0".to_owned()),
        advertised_endpoint: Some("https://remote.example".to_owned()),
        certificate_chain_path: Some("test-cert.pem".into()),
        private_key_path: Some("test-key.pem".into()),
        client_ca_bundle_path: Some("test-client-ca.pem".into()),
        ..RemoteBrainHttpsConfigV1::default()
    };

    let binding = config.validate_enabled().unwrap();
    assert_eq!(
        binding.client_ca_bundle_path,
        std::path::PathBuf::from("test-client-ca.pem")
    );
}
