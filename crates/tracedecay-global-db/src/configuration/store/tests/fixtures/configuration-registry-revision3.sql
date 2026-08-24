-- Real registry-revision-3 configuration store, canonically initialized by
-- revision-3 code (tip aae945bf54) and dumped with `sqlite3 .dump`. The
-- dump's transaction wrapper (PRAGMA foreign_keys=OFF / BEGIN / COMMIT) is
-- stripped so the engine's exact-SQL guard can execute the batch; every
-- schema object and row is byte-exact dump output.
CREATE TABLE configuration_format (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    format_revision INTEGER NOT NULL CHECK (format_revision = 1)
);
INSERT INTO configuration_format VALUES(1,1);
CREATE TABLE configuration_revisions (
    revision_id TEXT PRIMARY KEY,
    parent_revision_id TEXT,
    -- A forward rollback can intentionally reproduce a prior immutable
    -- snapshot under a new revision, so snapshot identity is not revision
    -- identity and must not be globally unique.
    snapshot_id TEXT NOT NULL,
    effective_behavior_digest TEXT NOT NULL,
    resolution_provenance_digest TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    operation_kind TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY(parent_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);
INSERT INTO configuration_revisions VALUES('configuration.revision.root',NULL,'tracedecay.configuration.snapshot.v1.2ead3cb21bd717c2de40d8e5ab8fd906f32ce85eb1b3962041d4905a05c22acd','sha256:b15ea8f712f362f865bef9a3738cad55d00d05be0673ebd069dc91ed235fabd1','sha256:058e02666c6d74af8f38ec9f1d60b93879086690a8915a6cef72f8824e35b3a3','actor.configuration.fixture','canonical_initialization',1);
CREATE TABLE configuration_entries (
    revision_id TEXT NOT NULL,
    key TEXT NOT NULL,
    layer_kind TEXT NOT NULL,
    layer_id TEXT,
    schema_revision INTEGER NOT NULL,
    typed_value TEXT NOT NULL,
    PRIMARY KEY(revision_id, key, layer_kind, layer_id),
    FOREIGN KEY(revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);
INSERT INTO configuration_entries VALUES('configuration.revision.root','analyzer.settings.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"analyzer_settings","value":{"schema_version":1,"selections":[]}},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','context_scout.settings.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"context_scout_settings","value":{"schema_version":1,"state":"disabled","mode":"deterministic","limits":{"max_candidates":32,"max_evidence":16,"max_text_bytes":4096,"max_model_input_tokens":2048,"max_model_output_tokens":256},"model_path":null}},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','diagnostics.prewarm.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"boolean","value":false},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','feedback.proximity.risk_threshold','default',NULL,1,'{"schema_version":1,"value":{"kind":"unsigned","value":7000},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','index.exclude.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"string_list","value":[".git/**",".tracedecay/**","bin/**","**/*.min.*",".cache/**","**/.cache/**",".gradle/**","**/.gradle/**",".next/**","**/.next/**",".turbo/**","**/.turbo/**",".venv/**","**/.venv/**",".worktrees/**","**/.worktrees/**","__pycache__/**","**/__pycache__/**","build/**","**/build/**","coverage/**","**/coverage/**","dist/**","**/dist/**","node_modules/**","**/node_modules/**","out/**","**/out/**","target/**","**/target/**","vendor/**","**/vendor/**","venv/**","**/venv/**"]},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','index.extract_docstrings.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"boolean","value":true},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','index.git_ignore.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"boolean","value":true},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','index.include.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"string_list","value":[]},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','index.max_file_size.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"unsigned","value":1048576},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','index.track_call_sites.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"boolean","value":true},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','query.default_collection.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"default_collection","value":null},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','scope.access_rules.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"access_rules","value":[]},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','scope.source_bindings.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"source_bindings","value":[]},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','semantic.runtime.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"text","value":"{\"selected_model\":\"JinaEmbeddingsV2BaseCode\",\"auto_download\":true,\"active_profile\":null,\"rollback_profile\":null,\"resources\":{\"max_model_bytes\":734003200,\"max_tokenizer_bytes\":67108864,\"max_resident_bytes\":2147483648,\"max_threads\":4,\"max_concurrent_sessions\":16,\"max_batch_size\":32,\"max_sequence_length\":512,\"load_deadline_ms\":30000}}"},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','sync.auto_init.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"boolean","value":true},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','sync.auto_track_pr_branches.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"boolean","value":false},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','sync.auto_track_pr_poll_secs.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"unsigned","value":300},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','sync.auto_watch.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"boolean","value":false},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','sync.backstop_interval_mins.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"unsigned","value":15},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','sync.branch_gc_days.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"unsigned","value":14},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','sync.full_sync_escalation_files.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"unsigned","value":500},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','sync.max_concurrent_syncs.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"unsigned","value":2},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','sync.orphan_db_gc_days.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"unsigned","value":7},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','sync.read_cooldown_secs.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"unsigned","value":30},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','sync.read_refresh.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"boolean","value":true},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','sync.session_start_stale_threshold_secs.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"unsigned","value":600},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','sync.session_start_sync.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"boolean","value":true},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','sync.watch_debounce_ms.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"unsigned","value":2000},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','sync.watch_max_delay_ms.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"unsigned","value":30000},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','sync.watch_max_projects.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"unsigned","value":32},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','telemetry.timings.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"boolean","value":true},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','user.extraction_timeout_secs.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"unsigned","value":60},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','user.upload_enabled.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"boolean","value":false},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','user.watcher_debounce_ms.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"unsigned","value":2000},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','work.executable_bindings.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"work_executable_bindings","value":[]},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
INSERT INTO configuration_entries VALUES('configuration.revision.root','work.topology_policy.v1','default',NULL,1,'{"schema_version":1,"value":{"kind":"work_topology_policy","value":{"schema_version":1,"placement":{"kind":"existing_worktree_only"},"roots":[],"branch_topology":{"allowed":["no_branches","unbranched","independent_branches"]},"review_topology":{"allowed":["no_review","independent_review","standard_pull_requests"],"github_stacked_prs":"disabled"},"branch_naming":{"prefix":"tracedecay/","components":[{"kind":"task_id_digest_prefix","bytes":10},{"kind":"work_class"},{"kind":"monotonic_collision_ordinal"}],"separator":"slash","maximum_bytes":200,"collision":{"kind":"append_monotonic_ordinal","maximum_attempts":32}},"concurrency":{"maximum_active_per_repository":1,"maximum_parallel_per_task":1,"maximum_global_active":1,"maximum_stack_depth":1},"cross_merge":{"allowed_modes":["disabled"],"default_mode":"disabled","allow_cross_repository":false},"gates":{"cleanliness":"require_clean","tests":[],"review":{"kind":"independent_review_count","count":1},"require_fresh_preflight":true,"maximum_preflight_age_seconds":300},"protected_refs":[{"selector":{"kind":"native_default_branch"},"disposition":"reject"},{"selector":{"kind":"exact","value":"refs/heads/main"},"disposition":"reject"},{"selector":{"kind":"exact","value":"refs/heads/master"},"disposition":"reject"},{"selector":{"kind":"prefix","value":"refs/tags/"},"disposition":"reject"},{"selector":{"kind":"prefix","value":"refs/remotes/"},"disposition":"reject"}],"history_rewrite":"forbid_force_and_rebase","escalation":"reject","retention":{"terminal_retention_seconds":null,"abandoned_retention_seconds":null,"maximum_retained_per_repository":null,"automatic_gc":{"kind":"disabled"}},"notifications":"critical_only"}},"provenance":[{"layer":{"kind":"default"},"revision_id":"configuration.registry.default.v1","disposition":"defaulted","safe_reason":"registry_default"}]}');
CREATE TABLE configuration_topology_policies (
    revision_id TEXT PRIMARY KEY,
    schema_version INTEGER NOT NULL CHECK (schema_version = 1),
    topology_policy_digest TEXT NOT NULL,
    placement_kind TEXT NOT NULL,
    default_cross_merge_mode TEXT NOT NULL,
    allow_cross_repository INTEGER NOT NULL CHECK (allow_cross_repository IN (0, 1)),
    cleanliness_kind TEXT NOT NULL,
    review_kind TEXT NOT NULL,
    require_fresh_preflight INTEGER NOT NULL CHECK (require_fresh_preflight IN (0, 1)),
    maximum_preflight_age_seconds INTEGER NOT NULL,
    history_rewrite_kind TEXT NOT NULL CHECK (history_rewrite_kind = 'forbid_force_and_rebase'),
    escalation_kind TEXT NOT NULL,
    automatic_gc_kind TEXT NOT NULL,
    notification_level TEXT NOT NULL,
    sealed_policy_value BLOB NOT NULL,
    FOREIGN KEY(revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);
INSERT INTO configuration_topology_policies VALUES('configuration.revision.root',1,'sha256:d27b56e55ddd25797af2a8645a2d312424b2a97a16eb52d50b84058af0c0faab','existing_worktree_only','disabled',0,'require_clean','{"allowed":["no_review","independent_review","standard_pull_requests"],"github_stacked_prs":"disabled"}',1,300,'forbid_force_and_rebase','reject','{"kind":"disabled"}','critical_only',X'7b22736368656d615f76657273696f6e223a312c22706c6163656d656e74223a7b226b696e64223a226578697374696e675f776f726b747265655f6f6e6c79227d2c22726f6f7473223a5b5d2c226272616e63685f746f706f6c6f6779223a7b22616c6c6f776564223a5b226e6f5f6272616e63686573222c22756e6272616e63686564222c22696e646570656e64656e745f6272616e63686573225d7d2c227265766965775f746f706f6c6f6779223a7b22616c6c6f776564223a5b226e6f5f726576696577222c22696e646570656e64656e745f726576696577222c227374616e646172645f70756c6c5f7265717565737473225d2c226769746875625f737461636b65645f707273223a2264697361626c6564227d2c226272616e63685f6e616d696e67223a7b22707265666978223a22747261636564656361792f222c22636f6d706f6e656e7473223a5b7b226b696e64223a227461736b5f69645f6469676573745f707265666978222c226279746573223a31307d2c7b226b696e64223a22776f726b5f636c617373227d2c7b226b696e64223a226d6f6e6f746f6e69635f636f6c6c6973696f6e5f6f7264696e616c227d5d2c22736570617261746f72223a22736c617368222c226d6178696d756d5f6279746573223a3230302c22636f6c6c6973696f6e223a7b226b696e64223a22617070656e645f6d6f6e6f746f6e69635f6f7264696e616c222c226d6178696d756d5f617474656d707473223a33327d7d2c22636f6e63757272656e6379223a7b226d6178696d756d5f6163746976655f7065725f7265706f7369746f7279223a312c226d6178696d756d5f706172616c6c656c5f7065725f7461736b223a312c226d6178696d756d5f676c6f62616c5f616374697665223a312c226d6178696d756d5f737461636b5f6465707468223a317d2c2263726f73735f6d65726765223a7b22616c6c6f7765645f6d6f646573223a5b2264697361626c6564225d2c2264656661756c745f6d6f6465223a2264697361626c6564222c22616c6c6f775f63726f73735f7265706f7369746f7279223a66616c73657d2c226761746573223a7b22636c65616e6c696e657373223a22726571756972655f636c65616e222c227465737473223a5b5d2c22726576696577223a7b226b696e64223a22696e646570656e64656e745f7265766965775f636f756e74222c22636f756e74223a317d2c22726571756972655f66726573685f707265666c69676874223a747275652c226d6178696d756d5f707265666c696768745f6167655f7365636f6e6473223a3330307d2c2270726f7465637465645f72656673223a5b7b2273656c6563746f72223a7b226b696e64223a226e61746976655f64656661756c745f6272616e6368227d2c22646973706f736974696f6e223a2272656a656374227d2c7b2273656c6563746f72223a7b226b696e64223a226578616374222c2276616c7565223a22726566732f68656164732f6d61696e227d2c22646973706f736974696f6e223a2272656a656374227d2c7b2273656c6563746f72223a7b226b696e64223a226578616374222c2276616c7565223a22726566732f68656164732f6d6173746572227d2c22646973706f736974696f6e223a2272656a656374227d2c7b2273656c6563746f72223a7b226b696e64223a22707265666978222c2276616c7565223a22726566732f746167732f227d2c22646973706f736974696f6e223a2272656a656374227d2c7b2273656c6563746f72223a7b226b696e64223a22707265666978222c2276616c7565223a22726566732f72656d6f7465732f227d2c22646973706f736974696f6e223a2272656a656374227d5d2c22686973746f72795f72657772697465223a22666f726269645f666f7263655f616e645f726562617365222c22657363616c6174696f6e223a2272656a656374222c22726574656e74696f6e223a7b227465726d696e616c5f726574656e74696f6e5f7365636f6e6473223a6e756c6c2c226162616e646f6e65645f726574656e74696f6e5f7365636f6e6473223a6e756c6c2c226d6178696d756d5f72657461696e65645f7065725f7265706f7369746f7279223a6e756c6c2c226175746f6d617469635f6763223a7b226b696e64223a2264697361626c6564227d7d2c226e6f74696669636174696f6e73223a22637269746963616c5f6f6e6c79227d');
CREATE TABLE configuration_topology_roots (
    revision_id TEXT NOT NULL,
    root_ordinal INTEGER NOT NULL,
    root_id TEXT NOT NULL,
    locator_digest TEXT NOT NULL,
    repository_scope_digest TEXT NOT NULL,
    maximum_active_worktrees INTEGER NOT NULL,
    PRIMARY KEY(revision_id, root_ordinal),
    UNIQUE(revision_id, root_id),
    FOREIGN KEY(revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);
CREATE TABLE configuration_topology_protected_refs (
    revision_id TEXT NOT NULL,
    rule_ordinal INTEGER NOT NULL,
    selector_kind TEXT NOT NULL,
    selector_digest TEXT NOT NULL,
    disposition TEXT NOT NULL,
    PRIMARY KEY(revision_id, rule_ordinal),
    UNIQUE(revision_id, selector_digest),
    FOREIGN KEY(revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);
INSERT INTO configuration_topology_protected_refs VALUES('configuration.revision.root',0,'native_default_branch','sha256:56e81e607fe237229fd1498bc34aaf7d25200483edf221fc5ad9130b046f8ad0','reject');
INSERT INTO configuration_topology_protected_refs VALUES('configuration.revision.root',1,'exact','sha256:bbd38a5d10ad265a2c07c1dfbe6d9891ed111c40fdadf743cede6dfb055b05e7','reject');
INSERT INTO configuration_topology_protected_refs VALUES('configuration.revision.root',2,'exact','sha256:1efd7aa986ff318fd533e92fba9cf42fba371891d03b0d328124046f4d4a7ee8','reject');
INSERT INTO configuration_topology_protected_refs VALUES('configuration.revision.root',3,'prefix','sha256:048b859662196a287ab1796dee38d4284cc8b8bb74661c2979fbf65d40dbf7f0','reject');
INSERT INTO configuration_topology_protected_refs VALUES('configuration.revision.root',4,'prefix','sha256:bc52254d2bcb530eba5139003761a4415bcbed291bde713212fd08c14219a961','reject');
CREATE TABLE configuration_source_bindings (
    revision_id TEXT NOT NULL,
    binding_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    locator_digest TEXT NOT NULL,
    authority_kind TEXT NOT NULL CHECK (authority_kind IN ('project', 'projectless_hermes')),
    project_id TEXT,
    user_profile_id TEXT,
    provenance_digest TEXT NOT NULL,
    PRIMARY KEY(revision_id, binding_id),
    UNIQUE(revision_id, source_kind, locator_digest),
    CHECK (
        (authority_kind = 'project' AND project_id IS NOT NULL AND user_profile_id IS NULL)
        OR
        (authority_kind = 'projectless_hermes' AND project_id IS NULL AND user_profile_id IS NOT NULL)
    ),
    FOREIGN KEY(revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);
CREATE TABLE configuration_access_rules (
    revision_id TEXT NOT NULL,
    rule_id TEXT NOT NULL,
    subject_kind TEXT NOT NULL,
    subject_id TEXT,
    actor_kind TEXT,
    actor_id TEXT,
    operation_kind TEXT,
    source_kind TEXT,
    authority_kind TEXT NOT NULL CHECK (authority_kind IN ('project', 'projectless_hermes')),
    project_id TEXT,
    user_profile_id TEXT,
    capability_encoding TEXT NOT NULL,
    effect TEXT NOT NULL CHECK (effect IN ('allow', 'deny')),
    expires_at INTEGER,
    PRIMARY KEY(revision_id, rule_id),
    CHECK (
        (authority_kind = 'project' AND project_id IS NOT NULL AND user_profile_id IS NULL)
        OR
        (authority_kind = 'projectless_hermes' AND project_id IS NULL AND user_profile_id IS NOT NULL)
    ),
    FOREIGN KEY(revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);
CREATE TABLE configuration_change_plans (
    plan_id TEXT PRIMARY KEY,
    actor_id TEXT NOT NULL,
    base_revision_id TEXT NOT NULL,
    operation_digest TEXT NOT NULL,
    resolved_scope_digest TEXT NOT NULL,
    membership_digest TEXT,
    authorization_policy_digest TEXT NOT NULL,
    policy_epoch INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY(base_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);
CREATE TABLE configuration_change_plan_operations (
    plan_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    payload_schema_revision INTEGER NOT NULL,
    sealed_typed_operation BLOB NOT NULL,
    operation_digest TEXT NOT NULL,
    PRIMARY KEY(plan_id, sequence),
    FOREIGN KEY(plan_id) REFERENCES configuration_change_plans(plan_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);
CREATE TABLE configuration_change_plan_events (
    plan_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    event_kind TEXT NOT NULL,
    safe_reason_code TEXT,
    occurred_at INTEGER NOT NULL,
    PRIMARY KEY(plan_id, sequence),
    FOREIGN KEY(plan_id) REFERENCES configuration_change_plans(plan_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);
CREATE TABLE configuration_mutation_receipts (
    receipt_id TEXT PRIMARY KEY,
    plan_id TEXT,
    actor_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    base_revision_id TEXT NOT NULL,
    result_revision_id TEXT NOT NULL,
    operation_digest TEXT NOT NULL,
    authorization_policy_epoch INTEGER NOT NULL,
    authorization_policy_digest TEXT NOT NULL,
    authority_revalidated_at INTEGER NOT NULL,
    activation_status TEXT NOT NULL,
    receipt_digest TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    effective_deadline_at INTEGER NOT NULL,
    UNIQUE(actor_id, idempotency_key),
    UNIQUE(plan_id, idempotency_key),
    FOREIGN KEY(plan_id) REFERENCES configuration_change_plans(plan_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT,
    FOREIGN KEY(base_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT,
    FOREIGN KEY(result_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);
CREATE TABLE configuration_audit_events (
    event_id TEXT PRIMARY KEY,
    actor_id TEXT NOT NULL,
    idempotency_key TEXT,
    operation_kind TEXT NOT NULL,
    base_revision_id TEXT NOT NULL,
    result_revision_id TEXT,
    sealed_target_reference BLOB,
    event_scoped_target_commitment TEXT NOT NULL,
    receipt_digest TEXT,
    correlation_id TEXT,
    safe_reason_code TEXT,
    occurred_at INTEGER NOT NULL,
    FOREIGN KEY(base_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT,
    FOREIGN KEY(result_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);
CREATE TABLE configuration_audit_redaction_keys (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    key_material BLOB NOT NULL CHECK (length(key_material) = 32),
    created_at INTEGER NOT NULL
);
CREATE TABLE configuration_credential_references (
    reference_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    reference_digest TEXT NOT NULL,
    operation_digest TEXT NOT NULL,
    authorization_policy_epoch INTEGER NOT NULL,
    authorization_policy_digest TEXT NOT NULL,
    authority_revalidated_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    effective_deadline_at INTEGER NOT NULL,
    rotation INTEGER NOT NULL
);
CREATE TABLE configuration_component_activation_events (
    event_id INTEGER PRIMARY KEY AUTOINCREMENT,
    component TEXT NOT NULL,
    desired_revision_id TEXT NOT NULL,
    observed_revision_id TEXT,
    last_working_revision_id TEXT,
    restart_required INTEGER NOT NULL CHECK (restart_required IN (0, 1)),
    activation_error_code TEXT,
    occurred_at INTEGER NOT NULL,
    FOREIGN KEY(desired_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT,
    FOREIGN KEY(observed_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT,
    FOREIGN KEY(last_working_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);
CREATE TABLE configuration_semantic_retrieval_state_v1 (
    project_id TEXT NOT NULL,
    scope_digest TEXT NOT NULL,
    scope_json TEXT NOT NULL,
    epoch INTEGER NOT NULL CHECK (epoch >= 0),
    configuration_revision TEXT NOT NULL,
    transition_digest TEXT,
    activation_receipt_digest TEXT,
    active_vector_generation TEXT,
    rollback_vector_generation TEXT,
    state_json TEXT NOT NULL,
    activation_receipt_json TEXT,
    PRIMARY KEY (project_id, scope_digest, epoch)
);
CREATE TABLE configuration_semantic_retrieval_pending_v1 (
    project_id TEXT NOT NULL,
    scope_digest TEXT NOT NULL,
    scope_json TEXT NOT NULL,
    transition_digest TEXT NOT NULL,
    base_epoch INTEGER NOT NULL CHECK (base_epoch >= 0),
    base_configuration_revision TEXT NOT NULL,
    transition_json TEXT NOT NULL,
    resulting_state_json TEXT NOT NULL,
    staged_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, scope_digest, transition_digest)
);
CREATE TABLE configuration_semantic_retrieval_inventory_v1 (
    project_id TEXT PRIMARY KEY NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0)
);
CREATE TABLE configuration_semantic_accepted_profiles_v1 (
    profile_digest TEXT PRIMARY KEY NOT NULL,
    authority_json TEXT NOT NULL
);
CREATE TABLE configuration_semantic_accepted_profile_receipt_key_v1 (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    key_material BLOB NOT NULL CHECK (length(key_material) = 32)
);
DELETE FROM sqlite_sequence;
CREATE UNIQUE INDEX configuration_semantic_retrieval_transition_v1
    ON configuration_semantic_retrieval_state_v1(project_id, scope_digest, transition_digest)
    WHERE transition_digest IS NOT NULL;
CREATE INDEX configuration_semantic_retrieval_active_vector_v1
    ON configuration_semantic_retrieval_state_v1(
        project_id, active_vector_generation, scope_digest, epoch
    )
    WHERE active_vector_generation IS NOT NULL;
CREATE INDEX configuration_semantic_retrieval_rollback_vector_v1
    ON configuration_semantic_retrieval_state_v1(
        project_id, rollback_vector_generation, scope_digest, epoch
    )
    WHERE rollback_vector_generation IS NOT NULL;
CREATE INDEX idx_configuration_revision_parent
    ON configuration_revisions(parent_revision_id);
CREATE INDEX idx_configuration_entry_key
    ON configuration_entries(key);
CREATE INDEX idx_configuration_topology_root_id
    ON configuration_topology_roots(root_id);
CREATE INDEX idx_configuration_topology_root_locator
    ON configuration_topology_roots(locator_digest);
CREATE INDEX idx_configuration_topology_protected_ref
    ON configuration_topology_protected_refs(selector_digest);
CREATE INDEX idx_configuration_audit_occurred_at
    ON configuration_audit_events(occurred_at, event_id);
CREATE INDEX idx_configuration_component_activation_latest
    ON configuration_component_activation_events(component, event_id DESC);
CREATE TRIGGER configuration_semantic_retrieval_state_scope_insert_v1
BEFORE INSERT ON configuration_semantic_retrieval_state_v1
WHEN json_valid(NEW.scope_json) != 1
  OR json_extract(NEW.scope_json, '$.project_id') IS NULL
  OR json_extract(NEW.scope_json, '$.project_id') != NEW.project_id
  OR json_extract(NEW.scope_json, '$.scope_digest') IS NULL
  OR json_extract(NEW.scope_json, '$.scope_digest') != NEW.scope_digest
BEGIN
    SELECT RAISE(ABORT, 'semantic retrieval state scope binding is invalid');
END;
CREATE TRIGGER configuration_semantic_retrieval_state_scope_update_v1
BEFORE UPDATE ON configuration_semantic_retrieval_state_v1
BEGIN
    SELECT RAISE(ABORT, 'semantic retrieval state is append-only');
END;
CREATE TRIGGER configuration_semantic_retrieval_pending_scope_insert_v1
BEFORE INSERT ON configuration_semantic_retrieval_pending_v1
WHEN json_valid(NEW.scope_json) != 1
  OR json_extract(NEW.scope_json, '$.project_id') IS NULL
  OR json_extract(NEW.scope_json, '$.project_id') != NEW.project_id
  OR json_extract(NEW.scope_json, '$.scope_digest') IS NULL
  OR json_extract(NEW.scope_json, '$.scope_digest') != NEW.scope_digest
BEGIN
    SELECT RAISE(ABORT, 'semantic retrieval pending scope binding is invalid');
END;
CREATE TRIGGER configuration_semantic_retrieval_pending_scope_update_v1
BEFORE UPDATE ON configuration_semantic_retrieval_pending_v1
BEGIN
    SELECT RAISE(ABORT, 'semantic retrieval pending transition is immutable');
END;
CREATE TRIGGER configuration_semantic_retrieval_state_inventory_insert_v1
AFTER INSERT ON configuration_semantic_retrieval_state_v1
BEGIN
    INSERT INTO configuration_semantic_retrieval_inventory_v1(project_id, revision)
    VALUES (NEW.project_id, 1)
    ON CONFLICT(project_id) DO UPDATE SET revision = revision + 1;
END;
CREATE TRIGGER configuration_semantic_retrieval_state_inventory_delete_v1
AFTER DELETE ON configuration_semantic_retrieval_state_v1
BEGIN
    INSERT INTO configuration_semantic_retrieval_inventory_v1(project_id, revision)
    VALUES (OLD.project_id, 1)
    ON CONFLICT(project_id) DO UPDATE SET revision = revision + 1;
END;
CREATE TRIGGER configuration_semantic_retrieval_pending_inventory_insert_v1
AFTER INSERT ON configuration_semantic_retrieval_pending_v1
BEGIN
    INSERT INTO configuration_semantic_retrieval_inventory_v1(project_id, revision)
    VALUES (NEW.project_id, 1)
    ON CONFLICT(project_id) DO UPDATE SET revision = revision + 1;
END;
CREATE TRIGGER configuration_semantic_retrieval_pending_inventory_delete_v1
AFTER DELETE ON configuration_semantic_retrieval_pending_v1
BEGIN
    INSERT INTO configuration_semantic_retrieval_inventory_v1(project_id, revision)
    VALUES (OLD.project_id, 1)
    ON CONFLICT(project_id) DO UPDATE SET revision = revision + 1;
END;
CREATE TRIGGER configuration_revisions_immutable_update
BEFORE UPDATE ON configuration_revisions
BEGIN SELECT RAISE(ABORT, 'configuration revisions are immutable'); END;
CREATE TRIGGER configuration_revisions_immutable_delete
BEFORE DELETE ON configuration_revisions
BEGIN SELECT RAISE(ABORT, 'configuration revisions are immutable'); END;
CREATE TRIGGER configuration_entries_immutable_update
BEFORE UPDATE ON configuration_entries
BEGIN SELECT RAISE(ABORT, 'configuration entries are immutable'); END;
CREATE TRIGGER configuration_entries_immutable_delete
BEFORE DELETE ON configuration_entries
BEGIN SELECT RAISE(ABORT, 'configuration entries are immutable'); END;
CREATE TRIGGER configuration_format_immutable_update
BEFORE UPDATE ON configuration_format
BEGIN SELECT RAISE(ABORT, 'configuration format is immutable'); END;
CREATE TRIGGER configuration_format_immutable_delete
BEFORE DELETE ON configuration_format
BEGIN SELECT RAISE(ABORT, 'configuration format is immutable'); END;
CREATE TRIGGER configuration_topology_policy_immutable_update
BEFORE UPDATE ON configuration_topology_policies
BEGIN SELECT RAISE(ABORT, 'configuration topology policies are immutable'); END;
CREATE TRIGGER configuration_topology_policy_immutable_delete
BEFORE DELETE ON configuration_topology_policies
BEGIN SELECT RAISE(ABORT, 'configuration topology policies are immutable'); END;
CREATE TRIGGER configuration_topology_roots_immutable_update
BEFORE UPDATE ON configuration_topology_roots
BEGIN SELECT RAISE(ABORT, 'configuration topology roots are immutable'); END;
CREATE TRIGGER configuration_topology_roots_immutable_delete
BEFORE DELETE ON configuration_topology_roots
BEGIN SELECT RAISE(ABORT, 'configuration topology roots are immutable'); END;
CREATE TRIGGER configuration_topology_protected_refs_immutable_update
BEFORE UPDATE ON configuration_topology_protected_refs
BEGIN SELECT RAISE(ABORT, 'configuration topology protected refs are immutable'); END;
CREATE TRIGGER configuration_topology_protected_refs_immutable_delete
BEFORE DELETE ON configuration_topology_protected_refs
BEGIN SELECT RAISE(ABORT, 'configuration topology protected refs are immutable'); END;
CREATE TRIGGER configuration_source_bindings_immutable_update
BEFORE UPDATE ON configuration_source_bindings
BEGIN SELECT RAISE(ABORT, 'configuration source bindings are immutable'); END;
CREATE TRIGGER configuration_source_bindings_immutable_delete
BEFORE DELETE ON configuration_source_bindings
BEGIN SELECT RAISE(ABORT, 'configuration source bindings are immutable'); END;
CREATE TRIGGER configuration_access_rules_immutable_update
BEFORE UPDATE ON configuration_access_rules
BEGIN SELECT RAISE(ABORT, 'configuration access rules are immutable'); END;
CREATE TRIGGER configuration_access_rules_immutable_delete
BEFORE DELETE ON configuration_access_rules
BEGIN SELECT RAISE(ABORT, 'configuration access rules are immutable'); END;
CREATE TRIGGER configuration_change_plans_immutable_update
BEFORE UPDATE ON configuration_change_plans
BEGIN SELECT RAISE(ABORT, 'configuration change plans are immutable'); END;
CREATE TRIGGER configuration_change_plans_immutable_delete
BEFORE DELETE ON configuration_change_plans
BEGIN SELECT RAISE(ABORT, 'configuration change plans are immutable'); END;
CREATE TRIGGER configuration_change_plan_operations_immutable_update
BEFORE UPDATE ON configuration_change_plan_operations
BEGIN SELECT RAISE(ABORT, 'configuration change operations are immutable'); END;
CREATE TRIGGER configuration_change_plan_operations_immutable_delete
BEFORE DELETE ON configuration_change_plan_operations
BEGIN SELECT RAISE(ABORT, 'configuration change operations are immutable'); END;
CREATE TRIGGER configuration_change_plan_events_immutable_update
BEFORE UPDATE ON configuration_change_plan_events
BEGIN SELECT RAISE(ABORT, 'configuration change plan events are immutable'); END;
CREATE TRIGGER configuration_change_plan_events_immutable_delete
BEFORE DELETE ON configuration_change_plan_events
BEGIN SELECT RAISE(ABORT, 'configuration change plan events are immutable'); END;
CREATE TRIGGER configuration_mutation_receipts_immutable_update
BEFORE UPDATE ON configuration_mutation_receipts
BEGIN SELECT RAISE(ABORT, 'configuration mutation receipts are immutable'); END;
CREATE TRIGGER configuration_mutation_receipts_immutable_delete
BEFORE DELETE ON configuration_mutation_receipts
BEGIN SELECT RAISE(ABORT, 'configuration mutation receipts are immutable'); END;
CREATE TRIGGER configuration_audit_events_immutable_update
BEFORE UPDATE ON configuration_audit_events
BEGIN SELECT RAISE(ABORT, 'configuration audit events are immutable'); END;
CREATE TRIGGER configuration_audit_events_immutable_delete
BEFORE DELETE ON configuration_audit_events
BEGIN SELECT RAISE(ABORT, 'configuration audit events are immutable'); END;
CREATE TRIGGER configuration_audit_redaction_keys_immutable_update
BEFORE UPDATE ON configuration_audit_redaction_keys
BEGIN SELECT RAISE(ABORT, 'configuration audit redaction keys are immutable'); END;
CREATE TRIGGER configuration_audit_redaction_keys_immutable_delete
BEFORE DELETE ON configuration_audit_redaction_keys
BEGIN SELECT RAISE(ABORT, 'configuration audit redaction keys are immutable'); END;
CREATE TRIGGER configuration_credential_references_immutable_update
BEFORE UPDATE ON configuration_credential_references
BEGIN SELECT RAISE(ABORT, 'configuration credential references are immutable'); END;
CREATE TRIGGER configuration_credential_references_immutable_delete
BEFORE DELETE ON configuration_credential_references
BEGIN SELECT RAISE(ABORT, 'configuration credential references are immutable'); END;
CREATE TRIGGER configuration_component_activation_events_immutable_update
BEFORE UPDATE ON configuration_component_activation_events
BEGIN SELECT RAISE(ABORT, 'configuration component activation events are immutable'); END;
CREATE TRIGGER configuration_component_activation_events_immutable_delete
BEFORE DELETE ON configuration_component_activation_events
BEGIN SELECT RAISE(ABORT, 'configuration component activation events are immutable'); END;
CREATE TRIGGER configuration_semantic_accepted_profile_receipt_key_no_update_v1
BEFORE UPDATE ON configuration_semantic_accepted_profile_receipt_key_v1
BEGIN
    SELECT RAISE(ABORT, 'accepted profile receipt key is immutable');
END;
CREATE TRIGGER configuration_semantic_accepted_profile_receipt_key_no_delete_v1
BEFORE DELETE ON configuration_semantic_accepted_profile_receipt_key_v1
BEGIN
    SELECT RAISE(ABORT, 'accepted profile receipt key is immutable');
END;
