# Serving production report

## Outcome

Authenticated Unix socket serving now accepts the registered shutdown RPC before profile mounting, starts lifecycle draining, and makes the foreground accept loop enter its bounded teardown path. Reserved control capacity rejects bulk requests before profile mounting and returns the existing typed `bulk_capacity_reached` response.

## Tests

- Red: `authenticated_socket_shutdown_acks_and_begins_draining` initially failed because Unix serving routed shutdown after profile binding and never drained.
- Red: `authenticated_socket_reserved_lane_rejects_bulk_with_typed_backpressure` initially failed because bulk rejection mounted a profile before responding.
- Green: both direct authenticated-socket tests pass, as does unauthenticated-handshake rejection.

## Scope

- No query admission or `git_watch` changes were required.
- Removed client-side staged saturation/backpressure variants that no live wire path constructed. The daemon remains the authority for typed saturation through `ApplicationProblem::Saturated` and JSON-RPC response data.
