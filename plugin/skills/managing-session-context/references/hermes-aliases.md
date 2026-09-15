# Hermes LCM aliases

Hermes exposes native `lcm_grep`, `lcm_load_session`, `lcm_describe`,
`lcm_expand`, and `lcm_expand_query` aliases. Their schemas use host-specific
fields such as `session_scope` and `max_content_chars`. Read the live alias
schema; do not mix its fields with canonical TraceDecay commands or assume the
aliases exist on another host.
