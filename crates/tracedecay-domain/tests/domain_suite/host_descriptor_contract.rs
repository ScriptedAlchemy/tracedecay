use tracedecay_domain::NativeHostIdentityV1;

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
fn native_host_identity_wire_values_remain_stable() {
    assert_eq!(
        serde_json::to_string(&NativeHostIdentityV1::OpenCode).unwrap(),
        "\"open_code\""
    );
}
