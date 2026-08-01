//! Small serde helpers shared across serialized store schemas.

/// `skip_serializing_if` predicate that drops a field when it equals its type's
/// [`Default`] (e.g. a `0` counter or timestamp), keeping serialized store rows
/// compact and stable.
///
/// serde's `skip_serializing_if` requires the `fn(&T) -> bool` shape, so this
/// takes `&T`; the `trivially_copy_pass_by_ref` lint is expected for `Copy`
/// scalars and allowed here once for every caller.
#[allow(clippy::trivially_copy_pass_by_ref)]
pub fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}
