#[derive(Clone)]
pub(super) struct OwnerKey {
    pub(super) kind: &'static str,
    pub(super) project_id: String,
    pub(super) json: String,
}
