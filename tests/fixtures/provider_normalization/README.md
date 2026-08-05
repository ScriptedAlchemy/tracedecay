# Provider normalization evidence

`manifest.json` separates recorded provider-machine bytes from generated
adapter-behavior samples and negative non-inference cases. Only entries under
`native_acceptance.fixtures` confer normalization acceptance. Generated inputs
may exercise parser behavior, but they are not provider or schema evidence.

An unavailable entry records the exact product surface and the missing
provider-release or schema evidence. It remains unavailable until a sanitized
native capture is checked in with its producer release, artifact schema, and
payload digest.

An envelope `version` is TraceDecay's canonical-envelope version, not evidence
that the provider wire format is versioned. `UnknownVersion` coverage belongs
here only when a checked-in provider input proves a genuine unsupported native
schema version.
