use std::collections::BTreeSet;

use tracedecay_domain::{
    HostActivationPolicyV1, HostAssetRenderPolicyV1, HostHookMappingV1, HostKindV1,
    HostProjectRegistrationPathV1, NativeHostIdentityV1, host_descriptors_v1,
    stock_host_capabilities,
};

#[test]
fn descriptors_cover_each_stock_host_once_with_stable_identity() {
    let descriptors = host_descriptors_v1();
    assert_eq!(descriptors.len(), HostKindV1::ALL.len());
    assert_eq!(
        descriptors
            .iter()
            .map(|descriptor| descriptor.host())
            .collect::<Vec<_>>(),
        HostKindV1::ALL
    );
    assert_eq!(
        descriptors
            .iter()
            .map(|descriptor| descriptor.slug())
            .collect::<BTreeSet<_>>()
            .len(),
        HostKindV1::ALL.len()
    );

    for descriptor in descriptors {
        assert!(!descriptor.cli_id().is_empty());
        assert!(!descriptor.slug().is_empty());
        assert_eq!(
            descriptor.capabilities(),
            stock_host_capabilities(descriptor.host())
        );
        assert_eq!(
            descriptor
                .components()
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len(),
            descriptor.components().len()
        );
    }
}

#[test]
fn native_identities_preserve_provider_specific_hosts() {
    for host in HostKindV1::ALL {
        let descriptor = host.descriptor();
        match (host, host.native_identity(), descriptor.hook()) {
            (HostKindV1::ClineFamily, None, HostHookMappingV1::NotApplicable) => {}
            (
                HostKindV1::Cline | HostKindV1::RooCode | HostKindV1::Kilo,
                Some(identity),
                HostHookMappingV1::Unavailable(mapped),
            ) => {
                assert_eq!(identity, mapped);
                assert_eq!(identity.host_kind(), host);
            }
            (_, Some(identity), HostHookMappingV1::Native(mapped)) => {
                assert_eq!(identity, mapped);
                assert_eq!(identity.host_kind(), host);
            }
            state => panic!("incoherent native host projection: {state:?}"),
        }
    }
}

#[test]
fn native_hook_keys_preserve_exact_provider_variants() {
    let identities = [
        NativeHostIdentityV1::ClaudeCode,
        NativeHostIdentityV1::CursorDesktop,
        NativeHostIdentityV1::CursorCloud,
        NativeHostIdentityV1::Codex,
        NativeHostIdentityV1::Hermes,
        NativeHostIdentityV1::Kiro,
        NativeHostIdentityV1::Cline,
        NativeHostIdentityV1::RooCode,
        NativeHostIdentityV1::Kilo,
        NativeHostIdentityV1::KimiCode,
        NativeHostIdentityV1::OpenCode,
    ];
    assert_eq!(
        identities.map(NativeHostIdentityV1::hook_key),
        [
            "claude",
            "cursor-desktop",
            "cursor-cloud",
            "codex",
            "hermes",
            "kiro",
            "cline",
            "roo-code",
            "kilo",
            "kimi",
            "opencode",
        ]
    );
}

#[test]
fn activation_and_registration_never_invent_unsupported_routes() {
    for host in [HostKindV1::CursorCloud, HostKindV1::ClineFamily] {
        let descriptor = host.descriptor();
        assert!(descriptor.components().is_empty());
        assert_eq!(
            descriptor.asset_render_policy(),
            HostAssetRenderPolicyV1::Unavailable
        );
        assert_eq!(
            descriptor.activation_policy(),
            HostActivationPolicyV1::Unsupported
        );
        assert_eq!(
            descriptor.project_registration_path(),
            HostProjectRegistrationPathV1::Unavailable
        );
        assert_eq!(descriptor.project_registration_path().relative_path(), None);
    }

    let kimi = HostKindV1::KimiCode.descriptor();
    assert_eq!(
        kimi.asset_render_policy(),
        HostAssetRenderPolicyV1::StagedManualPlugin
    );
    assert_eq!(
        kimi.activation_policy(),
        HostActivationPolicyV1::ManualHostInstall
    );
    assert_eq!(
        kimi.project_registration_path().relative_path(),
        Some(".kimi-code")
    );
}

#[test]
fn native_host_identity_wire_values_remain_stable() {
    assert_eq!(
        serde_json::to_string(&NativeHostIdentityV1::OpenCode).unwrap(),
        "\"open_code\""
    );
}
