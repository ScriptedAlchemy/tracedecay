use super::remote_https::{RemoteBrainHttpsConfigV1, RemoteBrainHttpsStateV1};

#[test]
fn remote_brain_https_is_unconfigured_by_default() {
    let config = RemoteBrainHttpsConfigV1::default();

    assert_eq!(config.version, 1);
    assert_eq!(config.state(), RemoteBrainHttpsStateV1::Unconfigured);
}
