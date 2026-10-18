# Session refresh and LCM health

Recall does not ingest or refresh. A `refresh_required` result needs authorized
lifecycle intent before `tracedecay_session_refresh_begin`. Preserve the
returned project or profile scope in the request's `scope` selector and carry
opaque handles unchanged through `tracedecay_session_refresh_status` or
`tracedecay_session_refresh_cancel`. Only receipt-backed success proves durable
cancellation. A profile refresh cannot be routed through an arbitrary active
project or reconstructed from chat text.

Compression admission and session boundaries are authenticated daemon-owned
host operations, not agent-generated summaries or recall operations.
`tracedecay_lcm_status` and `tracedecay_lcm_doctor` provide bounded read-only
diagnosis and expose no repair or cleanup control.
