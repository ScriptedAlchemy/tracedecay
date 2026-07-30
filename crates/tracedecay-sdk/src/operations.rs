//! Generated typed public operation descriptors. DO NOT EDIT.
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracedecay_api::HttpApplicationOperation;
use tracedecay_tool_catalog::ExecutableUnavailableDispositionV1;
pub trait TypedOperation {
    type Request: Serialize;
    type Result: DeserializeOwned;
    const OPERATION_ID: &'static str;
    const ROUTE: &'static str;
    const BINDING_ID: &'static str;
    const RESULT_SCHEMA_ID: &'static str;
    const RESULT_SCHEMA_REVISION: u32;
}
macro_rules! typed_operation {
    (
        $name:ident, $module:ident, $operation:literal, $route:literal, $binding:literal,
        $schema:literal, $revision:literal
    ) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] pub struct $name; impl
        TypedOperation for $name { type Request = $module ::Request; type Result =
        $module ::Result; const OPERATION_ID : &'static str = $operation; const ROUTE :
        &'static str = $route; const BINDING_ID : &'static str = $binding; const
        RESULT_SCHEMA_ID : &'static str = $schema; const RESULT_SCHEMA_REVISION : u32 =
        $revision; }
    };
}
#[allow(clippy::all)]
pub mod work_accept_proposal {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///`AcceptProposalCommand`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "AcceptProposalCommand",
        ///  "type": "object",
        ///  "required": [
        ///    "review"
        ///  ],
        ///  "properties": {
        ///    "review": {
        ///      "$ref": "#/definitions/ReviewProposalCommand"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct AcceptProposalCommand {
            pub review: ReviewProposalCommand,
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProposalId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProposalId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProposalId(pub ::std::string::String);
        impl ::std::ops::Deref for ProposalId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProposalId> for ::std::string::String {
            fn from(value: ProposalId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProposalId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProposalId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProposalId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`ReviewProposalCommand`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "command_id",
        ///    "expected_version",
        ///    "occurred_at",
        ///    "proposal_digest",
        ///    "proposal_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "command_id": {
        ///      "$ref": "#/definitions/WorkCommandId"
        ///    },
        ///    "expected_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "occurred_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    },
        ///    "proposal_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "proposal_id": {
        ///      "$ref": "#/definitions/ProposalId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct ReviewProposalCommand {
            pub command_id: WorkCommandId,
            pub expected_version: u64,
            pub occurred_at: UtcMicros,
            pub proposal_digest: ManifestDigest,
            pub proposal_id: ProposalId,
            pub task_id: TaskId,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkCommandId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkCommandId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkCommandId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkCommandId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkCommandId> for ::std::string::String {
            fn from(value: WorkCommandId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkCommandId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkCommandId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkCommandId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `ActorId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ActorId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ActorId(pub ::std::string::String);
        impl ::std::ops::Deref for ActorId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ActorId> for ::std::string::String {
            fn from(value: ActorId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ActorId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ActorId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ActorId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProposalId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProposalId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProposalId(pub ::std::string::String);
        impl ::std::ops::Deref for ProposalId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProposalId> for ::std::string::String {
            fn from(value: ProposalId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProposalId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProposalId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProposalId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RepositoryId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RepositoryId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RepositoryId(pub ::std::string::String);
        impl ::std::ops::Deref for RepositoryId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RepositoryId> for ::std::string::String {
            fn from(value: RepositoryId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RepositoryId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RepositoryId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RepositoryId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`RuntimeEvidenceRef`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "evidence_digest",
        ///    "run_id",
        ///    "terminal"
        ///  ],
        ///  "properties": {
        ///    "evidence_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "terminal": {
        ///      "type": "boolean"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct RuntimeEvidenceRef {
            pub evidence_digest: ManifestDigest,
            pub run_id: RunId,
            pub terminal: bool,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkAuthority`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "actor_id",
        ///    "policy_digest",
        ///    "project_id",
        ///    "repository_id",
        ///    "worktree_id"
        ///  ],
        ///  "properties": {
        ///    "actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "policy_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "repository_id": {
        ///      "$ref": "#/definitions/RepositoryId"
        ///    },
        ///    "worktree_id": {
        ///      "$ref": "#/definitions/WorktreeId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAuthority {
            pub actor_id: ActorId,
            pub policy_digest: ManifestDigest,
            pub project_id: ProjectId,
            pub repository_id: RepositoryId,
            pub worktree_id: WorktreeId,
        }
        ///`WorkProjection`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjection",
        ///  "type": "object",
        ///  "required": [
        ///    "authority",
        ///    "dependencies",
        ///    "execution_admitted",
        ///    "history_len",
        ///    "runtime_evidence",
        ///    "task_accepted",
        ///    "task_id",
        ///    "title",
        ///    "version"
        ///  ],
        ///  "properties": {
        ///    "accepted_proposal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/ProposalId"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "authority": {
        ///      "$ref": "#/definitions/WorkAuthority"
        ///    },
        ///    "dependencies": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/TaskId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "execution_admitted": {
        ///      "type": "boolean"
        ///    },
        ///    "history_len": {
        ///      "type": "integer",
        ///      "format": "uint",
        ///      "minimum": 0.0
        ///    },
        ///    "runtime_evidence": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/RuntimeEvidenceRef"
        ///      }
        ///    },
        ///    "task_accepted": {
        ///      "type": "boolean"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    },
        ///    "title": {
        ///      "type": "string"
        ///    },
        ///    "version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProjection {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub accepted_proposal: ::std::option::Option<ProposalId>,
            pub authority: WorkAuthority,
            pub dependencies: Vec<TaskId>,
            pub execution_admitted: bool,
            pub history_len: u32,
            pub runtime_evidence: ::std::vec::Vec<RuntimeEvidenceRef>,
            pub task_accepted: bool,
            pub task_id: TaskId,
            pub title: ::std::string::String,
            pub version: u64,
        }
        ///Strongly typed canonical identity: `WorktreeId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorktreeId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorktreeId(pub ::std::string::String);
        impl ::std::ops::Deref for WorktreeId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorktreeId> for ::std::string::String {
            fn from(value: WorktreeId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorktreeId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorktreeId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorktreeId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub type Request = request::AcceptProposalCommand;
    pub type Result = result::WorkProjection;
}
typed_operation!(
    WorkAcceptProposal, work_accept_proposal, "operation.work.accept_proposal",
    "/application/work/accept-proposal", "binding.http.work.accept_proposal",
    "schema.work.accept_proposal.result", 1
);
#[allow(clippy::all)]
pub mod work_accept_task {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///`AcceptTaskCommand`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "AcceptTaskCommand",
        ///  "type": "object",
        ///  "required": [
        ///    "command_id",
        ///    "expected_version",
        ///    "occurred_at",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "command_id": {
        ///      "$ref": "#/definitions/WorkCommandId"
        ///    },
        ///    "expected_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "occurred_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct AcceptTaskCommand {
            pub command_id: WorkCommandId,
            pub expected_version: u64,
            pub occurred_at: UtcMicros,
            pub task_id: TaskId,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkCommandId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkCommandId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkCommandId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkCommandId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkCommandId> for ::std::string::String {
            fn from(value: WorkCommandId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkCommandId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkCommandId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkCommandId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `ActorId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ActorId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ActorId(pub ::std::string::String);
        impl ::std::ops::Deref for ActorId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ActorId> for ::std::string::String {
            fn from(value: ActorId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ActorId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ActorId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ActorId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProposalId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProposalId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProposalId(pub ::std::string::String);
        impl ::std::ops::Deref for ProposalId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProposalId> for ::std::string::String {
            fn from(value: ProposalId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProposalId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProposalId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProposalId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RepositoryId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RepositoryId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RepositoryId(pub ::std::string::String);
        impl ::std::ops::Deref for RepositoryId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RepositoryId> for ::std::string::String {
            fn from(value: RepositoryId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RepositoryId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RepositoryId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RepositoryId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`RuntimeEvidenceRef`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "evidence_digest",
        ///    "run_id",
        ///    "terminal"
        ///  ],
        ///  "properties": {
        ///    "evidence_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "terminal": {
        ///      "type": "boolean"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct RuntimeEvidenceRef {
            pub evidence_digest: ManifestDigest,
            pub run_id: RunId,
            pub terminal: bool,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkAuthority`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "actor_id",
        ///    "policy_digest",
        ///    "project_id",
        ///    "repository_id",
        ///    "worktree_id"
        ///  ],
        ///  "properties": {
        ///    "actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "policy_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "repository_id": {
        ///      "$ref": "#/definitions/RepositoryId"
        ///    },
        ///    "worktree_id": {
        ///      "$ref": "#/definitions/WorktreeId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAuthority {
            pub actor_id: ActorId,
            pub policy_digest: ManifestDigest,
            pub project_id: ProjectId,
            pub repository_id: RepositoryId,
            pub worktree_id: WorktreeId,
        }
        ///`WorkProjection`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjection",
        ///  "type": "object",
        ///  "required": [
        ///    "authority",
        ///    "dependencies",
        ///    "execution_admitted",
        ///    "history_len",
        ///    "runtime_evidence",
        ///    "task_accepted",
        ///    "task_id",
        ///    "title",
        ///    "version"
        ///  ],
        ///  "properties": {
        ///    "accepted_proposal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/ProposalId"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "authority": {
        ///      "$ref": "#/definitions/WorkAuthority"
        ///    },
        ///    "dependencies": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/TaskId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "execution_admitted": {
        ///      "type": "boolean"
        ///    },
        ///    "history_len": {
        ///      "type": "integer",
        ///      "format": "uint",
        ///      "minimum": 0.0
        ///    },
        ///    "runtime_evidence": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/RuntimeEvidenceRef"
        ///      }
        ///    },
        ///    "task_accepted": {
        ///      "type": "boolean"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    },
        ///    "title": {
        ///      "type": "string"
        ///    },
        ///    "version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProjection {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub accepted_proposal: ::std::option::Option<ProposalId>,
            pub authority: WorkAuthority,
            pub dependencies: Vec<TaskId>,
            pub execution_admitted: bool,
            pub history_len: u32,
            pub runtime_evidence: ::std::vec::Vec<RuntimeEvidenceRef>,
            pub task_accepted: bool,
            pub task_id: TaskId,
            pub title: ::std::string::String,
            pub version: u64,
        }
        ///Strongly typed canonical identity: `WorktreeId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorktreeId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorktreeId(pub ::std::string::String);
        impl ::std::ops::Deref for WorktreeId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorktreeId> for ::std::string::String {
            fn from(value: WorktreeId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorktreeId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorktreeId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorktreeId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub type Request = request::AcceptTaskCommand;
    pub type Result = result::WorkProjection;
}
typed_operation!(
    WorkAcceptTask, work_accept_task, "operation.work.accept_task",
    "/application/work/accept-task", "binding.http.work.accept_task",
    "schema.work.accept_task.result", 1
);
#[allow(clippy::all)]
pub mod work_admit_execution {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///`AdmitExecutionCommand`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "AdmitExecutionCommand",
        ///  "type": "object",
        ///  "required": [
        ///    "command_id",
        ///    "expected_version",
        ///    "occurred_at",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "command_id": {
        ///      "$ref": "#/definitions/WorkCommandId"
        ///    },
        ///    "expected_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "occurred_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct AdmitExecutionCommand {
            pub command_id: WorkCommandId,
            pub expected_version: u64,
            pub occurred_at: UtcMicros,
            pub task_id: TaskId,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkCommandId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkCommandId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkCommandId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkCommandId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkCommandId> for ::std::string::String {
            fn from(value: WorkCommandId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkCommandId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkCommandId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkCommandId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `ActorId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ActorId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ActorId(pub ::std::string::String);
        impl ::std::ops::Deref for ActorId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ActorId> for ::std::string::String {
            fn from(value: ActorId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ActorId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ActorId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ActorId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProposalId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProposalId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProposalId(pub ::std::string::String);
        impl ::std::ops::Deref for ProposalId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProposalId> for ::std::string::String {
            fn from(value: ProposalId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProposalId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProposalId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProposalId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RepositoryId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RepositoryId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RepositoryId(pub ::std::string::String);
        impl ::std::ops::Deref for RepositoryId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RepositoryId> for ::std::string::String {
            fn from(value: RepositoryId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RepositoryId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RepositoryId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RepositoryId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`RuntimeEvidenceRef`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "evidence_digest",
        ///    "run_id",
        ///    "terminal"
        ///  ],
        ///  "properties": {
        ///    "evidence_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "terminal": {
        ///      "type": "boolean"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct RuntimeEvidenceRef {
            pub evidence_digest: ManifestDigest,
            pub run_id: RunId,
            pub terminal: bool,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkAuthority`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "actor_id",
        ///    "policy_digest",
        ///    "project_id",
        ///    "repository_id",
        ///    "worktree_id"
        ///  ],
        ///  "properties": {
        ///    "actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "policy_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "repository_id": {
        ///      "$ref": "#/definitions/RepositoryId"
        ///    },
        ///    "worktree_id": {
        ///      "$ref": "#/definitions/WorktreeId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAuthority {
            pub actor_id: ActorId,
            pub policy_digest: ManifestDigest,
            pub project_id: ProjectId,
            pub repository_id: RepositoryId,
            pub worktree_id: WorktreeId,
        }
        ///`WorkProjection`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjection",
        ///  "type": "object",
        ///  "required": [
        ///    "authority",
        ///    "dependencies",
        ///    "execution_admitted",
        ///    "history_len",
        ///    "runtime_evidence",
        ///    "task_accepted",
        ///    "task_id",
        ///    "title",
        ///    "version"
        ///  ],
        ///  "properties": {
        ///    "accepted_proposal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/ProposalId"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "authority": {
        ///      "$ref": "#/definitions/WorkAuthority"
        ///    },
        ///    "dependencies": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/TaskId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "execution_admitted": {
        ///      "type": "boolean"
        ///    },
        ///    "history_len": {
        ///      "type": "integer",
        ///      "format": "uint",
        ///      "minimum": 0.0
        ///    },
        ///    "runtime_evidence": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/RuntimeEvidenceRef"
        ///      }
        ///    },
        ///    "task_accepted": {
        ///      "type": "boolean"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    },
        ///    "title": {
        ///      "type": "string"
        ///    },
        ///    "version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProjection {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub accepted_proposal: ::std::option::Option<ProposalId>,
            pub authority: WorkAuthority,
            pub dependencies: Vec<TaskId>,
            pub execution_admitted: bool,
            pub history_len: u32,
            pub runtime_evidence: ::std::vec::Vec<RuntimeEvidenceRef>,
            pub task_accepted: bool,
            pub task_id: TaskId,
            pub title: ::std::string::String,
            pub version: u64,
        }
        ///Strongly typed canonical identity: `WorktreeId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorktreeId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorktreeId(pub ::std::string::String);
        impl ::std::ops::Deref for WorktreeId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorktreeId> for ::std::string::String {
            fn from(value: WorktreeId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorktreeId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorktreeId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorktreeId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub type Request = request::AdmitExecutionCommand;
    pub type Result = result::WorkProjection;
}
typed_operation!(
    WorkAdmitExecution, work_admit_execution, "operation.work.admit_execution",
    "/application/work/admit-execution", "binding.http.work.admit_execution",
    "schema.work.admit_execution.result", 1
);
#[allow(clippy::all)]
pub mod work_attach_runtime_evidence {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///`AttachRuntimeEvidenceCommand`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "AttachRuntimeEvidenceCommand",
        ///  "type": "object",
        ///  "required": [
        ///    "command_id",
        ///    "evidence",
        ///    "expected_version",
        ///    "occurred_at",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "command_id": {
        ///      "$ref": "#/definitions/WorkCommandId"
        ///    },
        ///    "evidence": {
        ///      "$ref": "#/definitions/RuntimeEvidenceRef"
        ///    },
        ///    "expected_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "occurred_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct AttachRuntimeEvidenceCommand {
            pub command_id: WorkCommandId,
            pub evidence: RuntimeEvidenceRef,
            pub expected_version: u64,
            pub occurred_at: UtcMicros,
            pub task_id: TaskId,
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`RuntimeEvidenceRef`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "evidence_digest",
        ///    "run_id",
        ///    "terminal"
        ///  ],
        ///  "properties": {
        ///    "evidence_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "terminal": {
        ///      "type": "boolean"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct RuntimeEvidenceRef {
            pub evidence_digest: ManifestDigest,
            pub run_id: RunId,
            pub terminal: bool,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkCommandId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkCommandId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkCommandId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkCommandId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkCommandId> for ::std::string::String {
            fn from(value: WorkCommandId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkCommandId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkCommandId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkCommandId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `ActorId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ActorId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ActorId(pub ::std::string::String);
        impl ::std::ops::Deref for ActorId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ActorId> for ::std::string::String {
            fn from(value: ActorId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ActorId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ActorId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ActorId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProposalId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProposalId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProposalId(pub ::std::string::String);
        impl ::std::ops::Deref for ProposalId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProposalId> for ::std::string::String {
            fn from(value: ProposalId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProposalId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProposalId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProposalId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RepositoryId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RepositoryId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RepositoryId(pub ::std::string::String);
        impl ::std::ops::Deref for RepositoryId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RepositoryId> for ::std::string::String {
            fn from(value: RepositoryId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RepositoryId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RepositoryId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RepositoryId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`RuntimeEvidenceRef`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "evidence_digest",
        ///    "run_id",
        ///    "terminal"
        ///  ],
        ///  "properties": {
        ///    "evidence_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "terminal": {
        ///      "type": "boolean"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct RuntimeEvidenceRef {
            pub evidence_digest: ManifestDigest,
            pub run_id: RunId,
            pub terminal: bool,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkAuthority`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "actor_id",
        ///    "policy_digest",
        ///    "project_id",
        ///    "repository_id",
        ///    "worktree_id"
        ///  ],
        ///  "properties": {
        ///    "actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "policy_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "repository_id": {
        ///      "$ref": "#/definitions/RepositoryId"
        ///    },
        ///    "worktree_id": {
        ///      "$ref": "#/definitions/WorktreeId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAuthority {
            pub actor_id: ActorId,
            pub policy_digest: ManifestDigest,
            pub project_id: ProjectId,
            pub repository_id: RepositoryId,
            pub worktree_id: WorktreeId,
        }
        ///`WorkProjection`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjection",
        ///  "type": "object",
        ///  "required": [
        ///    "authority",
        ///    "dependencies",
        ///    "execution_admitted",
        ///    "history_len",
        ///    "runtime_evidence",
        ///    "task_accepted",
        ///    "task_id",
        ///    "title",
        ///    "version"
        ///  ],
        ///  "properties": {
        ///    "accepted_proposal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/ProposalId"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "authority": {
        ///      "$ref": "#/definitions/WorkAuthority"
        ///    },
        ///    "dependencies": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/TaskId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "execution_admitted": {
        ///      "type": "boolean"
        ///    },
        ///    "history_len": {
        ///      "type": "integer",
        ///      "format": "uint",
        ///      "minimum": 0.0
        ///    },
        ///    "runtime_evidence": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/RuntimeEvidenceRef"
        ///      }
        ///    },
        ///    "task_accepted": {
        ///      "type": "boolean"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    },
        ///    "title": {
        ///      "type": "string"
        ///    },
        ///    "version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProjection {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub accepted_proposal: ::std::option::Option<ProposalId>,
            pub authority: WorkAuthority,
            pub dependencies: Vec<TaskId>,
            pub execution_admitted: bool,
            pub history_len: u32,
            pub runtime_evidence: ::std::vec::Vec<RuntimeEvidenceRef>,
            pub task_accepted: bool,
            pub task_id: TaskId,
            pub title: ::std::string::String,
            pub version: u64,
        }
        ///Strongly typed canonical identity: `WorktreeId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorktreeId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorktreeId(pub ::std::string::String);
        impl ::std::ops::Deref for WorktreeId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorktreeId> for ::std::string::String {
            fn from(value: WorktreeId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorktreeId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorktreeId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorktreeId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub type Request = request::AttachRuntimeEvidenceCommand;
    pub type Result = result::WorkProjection;
}
typed_operation!(
    WorkAttachRuntimeEvidence, work_attach_runtime_evidence,
    "operation.work.attach_runtime_evidence",
    "/application/work/attach-runtime-evidence",
    "binding.http.work.attach_runtime_evidence",
    "schema.work.attach_runtime_evidence.result", 1
);
#[allow(clippy::all)]
pub mod work_attempt_acquire_lease {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `ActorId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ActorId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ActorId(pub ::std::string::String);
        impl ::std::ops::Deref for ActorId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ActorId> for ::std::string::String {
            fn from(value: ActorId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ActorId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ActorId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ActorId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `AttemptId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `AttemptId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct AttemptId(pub ::std::string::String);
        impl ::std::ops::Deref for AttemptId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<AttemptId> for ::std::string::String {
            fn from(value: AttemptId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for AttemptId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for AttemptId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for AttemptId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectionGenerationId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectionGenerationId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectionGenerationId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectionGenerationId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectionGenerationId> for ::std::string::String {
            fn from(value: ProjectionGenerationId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectionGenerationId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectionGenerationId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectionGenerationId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProposalId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProposalId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProposalId(pub ::std::string::String);
        impl ::std::ops::Deref for ProposalId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProposalId> for ::std::string::String {
            fn from(value: ProposalId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProposalId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProposalId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProposalId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProviderId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProviderId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProviderId(pub ::std::string::String);
        impl ::std::ops::Deref for ProviderId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProviderId> for ::std::string::String {
            fn from(value: ProviderId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProviderId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProviderId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProviderId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RepositoryId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RepositoryId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RepositoryId(pub ::std::string::String);
        impl ::std::ops::Deref for RepositoryId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RepositoryId> for ::std::string::String {
            fn from(value: RepositoryId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RepositoryId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RepositoryId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RepositoryId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`RuntimeEvidenceRef`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "evidence_digest",
        ///    "run_id",
        ///    "terminal"
        ///  ],
        ///  "properties": {
        ///    "evidence_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "terminal": {
        ///      "type": "boolean"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct RuntimeEvidenceRef {
            pub evidence_digest: ManifestDigest,
            pub run_id: RunId,
            pub terminal: bool,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkAttemptAcquireLeaseRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkAttemptAcquireLeaseRequestV1",
        ///  "type": "object",
        ///  "required": [
        ///    "identity",
        ///    "lease",
        ///    "projection_binding",
        ///    "requested_route",
        ///    "snapshot"
        ///  ],
        ///  "properties": {
        ///    "identity": {
        ///      "$ref": "#/definitions/WorkAttemptIdentityV1"
        ///    },
        ///    "lease": {
        ///      "$ref": "#/definitions/WorkLeaseFenceV1"
        ///    },
        ///    "projection_binding": {
        ///      "$ref": "#/definitions/WorkAttemptProjectionBindingV1"
        ///    },
        ///    "requested_route": {
        ///      "$ref": "#/definitions/WorkProviderRouteV1"
        ///    },
        ///    "snapshot": {
        ///      "$ref": "#/definitions/WorkProjectionSnapshotV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptAcquireLeaseRequestV1 {
            pub identity: WorkAttemptIdentityV1,
            pub lease: WorkLeaseFenceV1,
            pub projection_binding: WorkAttemptProjectionBindingV1,
            pub requested_route: WorkProviderRouteV1,
            pub snapshot: WorkProjectionSnapshotV1,
        }
        ///`WorkAttemptIdentityV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "attempt_id",
        ///    "run_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "attempt_id": {
        ///      "$ref": "#/definitions/AttemptId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptIdentityV1 {
            pub attempt_id: AttemptId,
            pub run_id: RunId,
            pub task_id: TaskId,
        }
        ///`WorkAttemptProjectionBindingV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "generation_id",
        ///    "sequence",
        ///    "work_version"
        ///  ],
        ///  "properties": {
        ///    "generation_id": {
        ///      "$ref": "#/definitions/ProjectionGenerationId"
        ///    },
        ///    "sequence": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    },
        ///    "work_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProjectionBindingV1 {
            pub generation_id: ProjectionGenerationId,
            pub sequence: WorkProjectionSequenceV1,
            pub work_version: u64,
        }
        ///`WorkAuthority`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "actor_id",
        ///    "policy_digest",
        ///    "project_id",
        ///    "repository_id",
        ///    "worktree_id"
        ///  ],
        ///  "properties": {
        ///    "actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "policy_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "repository_id": {
        ///      "$ref": "#/definitions/RepositoryId"
        ///    },
        ///    "worktree_id": {
        ///      "$ref": "#/definitions/WorktreeId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAuthority {
            pub actor_id: ActorId,
            pub policy_digest: ManifestDigest,
            pub project_id: ProjectId,
            pub repository_id: RepositoryId,
            pub worktree_id: WorktreeId,
        }
        ///`WorkFenceEpochV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkFenceEpochV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkFenceEpochV1(pub u64);
        impl ::std::ops::Deref for WorkFenceEpochV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkFenceEpochV1> for u64 {
            fn from(value: WorkFenceEpochV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkFenceEpochV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkFenceEpochV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkFenceEpochV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkLeaseFenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "epoch",
        ///    "lease_id"
        ///  ],
        ///  "properties": {
        ///    "epoch": {
        ///      "$ref": "#/definitions/WorkFenceEpochV1"
        ///    },
        ///    "lease_id": {
        ///      "$ref": "#/definitions/WorkLeaseId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkLeaseFenceV1 {
            pub epoch: WorkFenceEpochV1,
            pub lease_id: WorkLeaseId,
        }
        ///Strongly typed canonical identity: `WorkLeaseId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkLeaseId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkLeaseId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkLeaseId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkLeaseId> for ::std::string::String {
            fn from(value: WorkLeaseId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkLeaseId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkLeaseId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkLeaseId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProjection`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "authority",
        ///    "dependencies",
        ///    "execution_admitted",
        ///    "history_len",
        ///    "runtime_evidence",
        ///    "task_accepted",
        ///    "task_id",
        ///    "title",
        ///    "version"
        ///  ],
        ///  "properties": {
        ///    "accepted_proposal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/ProposalId"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "authority": {
        ///      "$ref": "#/definitions/WorkAuthority"
        ///    },
        ///    "dependencies": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/TaskId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "execution_admitted": {
        ///      "type": "boolean"
        ///    },
        ///    "history_len": {
        ///      "type": "integer",
        ///      "format": "uint",
        ///      "minimum": 0.0
        ///    },
        ///    "runtime_evidence": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/RuntimeEvidenceRef"
        ///      }
        ///    },
        ///    "task_accepted": {
        ///      "type": "boolean"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    },
        ///    "title": {
        ///      "type": "string"
        ///    },
        ///    "version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProjection {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub accepted_proposal: ::std::option::Option<ProposalId>,
            pub authority: WorkAuthority,
            pub dependencies: Vec<TaskId>,
            pub execution_admitted: bool,
            pub history_len: u32,
            pub runtime_evidence: ::std::vec::Vec<RuntimeEvidenceRef>,
            pub task_accepted: bool,
            pub task_id: TaskId,
            pub title: ::std::string::String,
            pub version: u64,
        }
        ///`WorkProjectionCoverageV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "returned",
        ///        "state",
        ///        "total"
        ///      ],
        ///      "properties": {
        ///        "returned": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "complete"
        ///        },
        ///        "total": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "cursor",
        ///        "range",
        ///        "returned",
        ///        "state",
        ///        "total"
        ///      ],
        ///      "properties": {
        ///        "cursor": {
        ///          "$ref": "#/definitions/WorkProjectionResumeCursorV1"
        ///        },
        ///        "range": {
        ///          "$ref": "#/definitions/WorkProjectionSequenceRangeV1"
        ///        },
        ///        "returned": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "partial"
        ///        },
        ///        "total": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "cap",
        ///        "cursor",
        ///        "range",
        ///        "returned",
        ///        "state",
        ///        "total"
        ///      ],
        ///      "properties": {
        ///        "cap": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        },
        ///        "cursor": {
        ///          "$ref": "#/definitions/WorkProjectionResumeCursorV1"
        ///        },
        ///        "range": {
        ///          "$ref": "#/definitions/WorkProjectionSequenceRangeV1"
        ///        },
        ///        "returned": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "capped"
        ///        },
        ///        "total": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state")]
        pub enum WorkProjectionCoverageV1 {
            #[serde(rename = "complete")]
            Complete { returned: u32, total: u32 },
            #[serde(rename = "partial")]
            Partial {
                cursor: WorkProjectionResumeCursorV1,
                range: WorkProjectionSequenceRangeV1,
                returned: u32,
                total: u32,
            },
            #[serde(rename = "capped")]
            Capped {
                cap: u32,
                cursor: WorkProjectionResumeCursorV1,
                range: WorkProjectionSequenceRangeV1,
                returned: u32,
                total: u32,
            },
        }
        ///`WorkProjectionResumeCursorV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "generation_id",
        ///    "token"
        ///  ],
        ///  "properties": {
        ///    "generation_id": {
        ///      "$ref": "#/definitions/ProjectionGenerationId"
        ///    },
        ///    "token": {
        ///      "type": "string"
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkProjectionResumeCursorV1 {
            pub generation_id: ProjectionGenerationId,
            pub token: ::std::string::String,
        }
        ///`WorkProjectionSequenceRangeV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "end_inclusive",
        ///    "start_exclusive"
        ///  ],
        ///  "properties": {
        ///    "end_inclusive": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    },
        ///    "start_exclusive": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkProjectionSequenceRangeV1 {
            pub end_inclusive: WorkProjectionSequenceV1,
            pub start_exclusive: WorkProjectionSequenceV1,
        }
        ///`WorkProjectionSequenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjectionSequenceV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkProjectionSequenceV1(pub u64);
        impl ::std::ops::Deref for WorkProjectionSequenceV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProjectionSequenceV1> for u64 {
            fn from(value: WorkProjectionSequenceV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkProjectionSequenceV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProjectionSequenceV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkProjectionSequenceV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProjectionSnapshotV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "coverage",
        ///    "generation_id",
        ///    "projections",
        ///    "sequence"
        ///  ],
        ///  "properties": {
        ///    "coverage": {
        ///      "$ref": "#/definitions/WorkProjectionCoverageV1"
        ///    },
        ///    "generation_id": {
        ///      "$ref": "#/definitions/ProjectionGenerationId"
        ///    },
        ///    "projections": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkProjection"
        ///      }
        ///    },
        ///    "sequence": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkProjectionSnapshotV1 {
            pub coverage: WorkProjectionCoverageV1,
            pub generation_id: ProjectionGenerationId,
            pub projections: ::std::vec::Vec<WorkProjection>,
            pub sequence: WorkProjectionSequenceV1,
        }
        ///Strongly typed canonical identity: `WorkProviderRouteId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkProviderRouteId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkProviderRouteId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkProviderRouteId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProviderRouteId> for ::std::string::String {
            fn from(value: WorkProviderRouteId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkProviderRouteId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProviderRouteId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkProviderRouteId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProviderRouteV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "provider_id",
        ///    "route_id"
        ///  ],
        ///  "properties": {
        ///    "provider_id": {
        ///      "$ref": "#/definitions/ProviderId"
        ///    },
        ///    "route_id": {
        ///      "$ref": "#/definitions/WorkProviderRouteId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProviderRouteV1 {
            pub provider_id: ProviderId,
            pub route_id: WorkProviderRouteId,
        }
        ///Strongly typed canonical identity: `WorktreeId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorktreeId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorktreeId(pub ::std::string::String);
        impl ::std::ops::Deref for WorktreeId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorktreeId> for ::std::string::String {
            fn from(value: WorktreeId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorktreeId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorktreeId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorktreeId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `AttemptId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `AttemptId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct AttemptId(pub ::std::string::String);
        impl ::std::ops::Deref for AttemptId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<AttemptId> for ::std::string::String {
            fn from(value: AttemptId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for AttemptId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for AttemptId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for AttemptId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectionGenerationId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectionGenerationId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectionGenerationId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectionGenerationId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectionGenerationId> for ::std::string::String {
            fn from(value: ProjectionGenerationId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectionGenerationId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectionGenerationId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectionGenerationId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProviderId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProviderId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProviderId(pub ::std::string::String);
        impl ::std::ops::Deref for ProviderId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProviderId> for ::std::string::String {
            fn from(value: ProviderId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProviderId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProviderId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProviderId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkArtifactId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkArtifactId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkArtifactId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkArtifactId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkArtifactId> for ::std::string::String {
            fn from(value: WorkArtifactId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkArtifactId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkArtifactId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkArtifactId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkArtifactRefV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifact_id",
        ///    "byte_length",
        ///    "digest"
        ///  ],
        ///  "properties": {
        ///    "artifact_id": {
        ///      "$ref": "#/definitions/WorkArtifactId"
        ///    },
        ///    "byte_length": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkArtifactRefV1 {
            pub artifact_id: WorkArtifactId,
            pub byte_length: u64,
            pub digest: ManifestDigest,
        }
        ///`WorkAttemptIdentityV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "attempt_id",
        ///    "run_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "attempt_id": {
        ///      "$ref": "#/definitions/AttemptId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptIdentityV1 {
            pub attempt_id: AttemptId,
            pub run_id: RunId,
            pub task_id: TaskId,
        }
        ///`WorkAttemptProgressV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "completed",
        ///    "total"
        ///  ],
        ///  "properties": {
        ///    "completed": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "total": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProgressV1 {
            pub completed: u64,
            pub total: u64,
        }
        ///`WorkAttemptProjectionBindingV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "generation_id",
        ///    "sequence",
        ///    "work_version"
        ///  ],
        ///  "properties": {
        ///    "generation_id": {
        ///      "$ref": "#/definitions/ProjectionGenerationId"
        ///    },
        ///    "sequence": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    },
        ///    "work_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProjectionBindingV1 {
            pub generation_id: ProjectionGenerationId,
            pub sequence: WorkProjectionSequenceV1,
            pub work_version: u64,
        }
        ///`WorkAttemptResponseV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkAttemptResponseV1",
        ///  "type": "object",
        ///  "required": [
        ///    "attempt"
        ///  ],
        ///  "properties": {
        ///    "attempt": {
        ///      "$ref": "#/definitions/WorkAttemptV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptResponseV1 {
            pub attempt: WorkAttemptV1,
        }
        ///`WorkAttemptStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "leased",
        ///    "running",
        ///    "cancellation_requested",
        ///    "cancellation_acknowledged",
        ///    "cancellation_escalated",
        ///    "recovery_required",
        ///    "succeeded",
        ///    "failed",
        ///    "cancelled"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkAttemptStateV1 {
            #[serde(rename = "leased")]
            Leased,
            #[serde(rename = "running")]
            Running,
            #[serde(rename = "cancellation_requested")]
            CancellationRequested,
            #[serde(rename = "cancellation_acknowledged")]
            CancellationAcknowledged,
            #[serde(rename = "cancellation_escalated")]
            CancellationEscalated,
            #[serde(rename = "recovery_required")]
            RecoveryRequired,
            #[serde(rename = "succeeded")]
            Succeeded,
            #[serde(rename = "failed")]
            Failed,
            #[serde(rename = "cancelled")]
            Cancelled,
        }
        impl ::std::fmt::Display for WorkAttemptStateV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::Leased => f.write_str("leased"),
                    Self::Running => f.write_str("running"),
                    Self::CancellationRequested => f.write_str("cancellation_requested"),
                    Self::CancellationAcknowledged => {
                        f.write_str("cancellation_acknowledged")
                    }
                    Self::CancellationEscalated => f.write_str("cancellation_escalated"),
                    Self::RecoveryRequired => f.write_str("recovery_required"),
                    Self::Succeeded => f.write_str("succeeded"),
                    Self::Failed => f.write_str("failed"),
                    Self::Cancelled => f.write_str("cancelled"),
                }
            }
        }
        impl ::std::str::FromStr for WorkAttemptStateV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "leased" => Ok(Self::Leased),
                    "running" => Ok(Self::Running),
                    "cancellation_requested" => Ok(Self::CancellationRequested),
                    "cancellation_acknowledged" => Ok(Self::CancellationAcknowledged),
                    "cancellation_escalated" => Ok(Self::CancellationEscalated),
                    "recovery_required" => Ok(Self::RecoveryRequired),
                    "succeeded" => Ok(Self::Succeeded),
                    "failed" => Ok(Self::Failed),
                    "cancelled" => Ok(Self::Cancelled),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        ///`WorkAttemptV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifacts",
        ///    "cancellation",
        ///    "identity",
        ///    "lease",
        ///    "projection_binding",
        ///    "recovery",
        ///    "requested_route",
        ///    "state"
        ///  ],
        ///  "properties": {
        ///    "actual_route": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkProviderRouteV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "artifacts": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkArtifactRefV1"
        ///      }
        ///    },
        ///    "cancellation": {
        ///      "$ref": "#/definitions/WorkCancellationStateV1"
        ///    },
        ///    "identity": {
        ///      "$ref": "#/definitions/WorkAttemptIdentityV1"
        ///    },
        ///    "lease": {
        ///      "$ref": "#/definitions/WorkLeaseFenceV1"
        ///    },
        ///    "progress": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkAttemptProgressV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "projection_binding": {
        ///      "$ref": "#/definitions/WorkAttemptProjectionBindingV1"
        ///    },
        ///    "recovery": {
        ///      "$ref": "#/definitions/WorkRecoveryStateV1"
        ///    },
        ///    "requested_route": {
        ///      "$ref": "#/definitions/WorkProviderRouteV1"
        ///    },
        ///    "state": {
        ///      "$ref": "#/definitions/WorkAttemptStateV1"
        ///    },
        ///    "terminal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkTerminalEvidenceV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkAttemptV1 {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub actual_route: ::std::option::Option<WorkProviderRouteV1>,
            pub artifacts: ::std::vec::Vec<WorkArtifactRefV1>,
            pub cancellation: WorkCancellationStateV1,
            pub identity: WorkAttemptIdentityV1,
            pub lease: WorkLeaseFenceV1,
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub progress: ::std::option::Option<WorkAttemptProgressV1>,
            pub projection_binding: WorkAttemptProjectionBindingV1,
            pub recovery: WorkRecoveryStateV1,
            pub requested_route: WorkProviderRouteV1,
            pub state: WorkAttemptStateV1,
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub terminal: ::std::option::Option<WorkTerminalEvidenceV1>,
        }
        ///`WorkCancellationAcknowledgementV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "acknowledged_at",
        ///    "request"
        ///  ],
        ///  "properties": {
        ///    "acknowledged_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    },
        ///    "request": {
        ///      "$ref": "#/definitions/WorkCancellationRequestV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationAcknowledgementV1 {
            pub acknowledged_at: UtcMicros,
            pub request: WorkCancellationRequestV1,
        }
        ///`WorkCancellationEscalationV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "acknowledgement",
        ///    "escalated_at"
        ///  ],
        ///  "properties": {
        ///    "acknowledgement": {
        ///      "$ref": "#/definitions/WorkCancellationAcknowledgementV1"
        ///    },
        ///    "escalated_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationEscalationV1 {
            pub acknowledgement: WorkCancellationAcknowledgementV1,
            pub escalated_at: UtcMicros,
        }
        ///Strongly typed canonical identity: `WorkCancellationRequestId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkCancellationRequestId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkCancellationRequestId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkCancellationRequestId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkCancellationRequestId> for ::std::string::String {
            fn from(value: WorkCancellationRequestId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkCancellationRequestId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkCancellationRequestId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkCancellationRequestId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkCancellationRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "request_id",
        ///    "requested_at"
        ///  ],
        ///  "properties": {
        ///    "request_id": {
        ///      "$ref": "#/definitions/WorkCancellationRequestId"
        ///    },
        ///    "requested_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationRequestV1 {
            pub request_id: WorkCancellationRequestId,
            pub requested_at: UtcMicros,
        }
        ///`WorkCancellationStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "none"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "requested"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationRequestV1"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "acknowledged"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationAcknowledgementV1"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "escalated"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationEscalationV1"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state", content = "value")]
        pub enum WorkCancellationStateV1 {
            #[serde(rename = "none")]
            None,
            #[serde(rename = "requested")]
            Requested(WorkCancellationRequestV1),
            #[serde(rename = "acknowledged")]
            Acknowledged(WorkCancellationAcknowledgementV1),
            #[serde(rename = "escalated")]
            Escalated(WorkCancellationEscalationV1),
        }
        impl ::std::convert::From<WorkCancellationRequestV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationRequestV1) -> Self {
                Self::Requested(value)
            }
        }
        impl ::std::convert::From<WorkCancellationAcknowledgementV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationAcknowledgementV1) -> Self {
                Self::Acknowledged(value)
            }
        }
        impl ::std::convert::From<WorkCancellationEscalationV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationEscalationV1) -> Self {
                Self::Escalated(value)
            }
        }
        ///`WorkFenceEpochV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkFenceEpochV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkFenceEpochV1(pub u64);
        impl ::std::ops::Deref for WorkFenceEpochV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkFenceEpochV1> for u64 {
            fn from(value: WorkFenceEpochV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkFenceEpochV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkFenceEpochV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkFenceEpochV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkLeaseFenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "epoch",
        ///    "lease_id"
        ///  ],
        ///  "properties": {
        ///    "epoch": {
        ///      "$ref": "#/definitions/WorkFenceEpochV1"
        ///    },
        ///    "lease_id": {
        ///      "$ref": "#/definitions/WorkLeaseId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkLeaseFenceV1 {
            pub epoch: WorkFenceEpochV1,
            pub lease_id: WorkLeaseId,
        }
        ///Strongly typed canonical identity: `WorkLeaseId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkLeaseId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkLeaseId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkLeaseId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkLeaseId> for ::std::string::String {
            fn from(value: WorkLeaseId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkLeaseId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkLeaseId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkLeaseId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProjectionSequenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjectionSequenceV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkProjectionSequenceV1(pub u64);
        impl ::std::ops::Deref for WorkProjectionSequenceV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProjectionSequenceV1> for u64 {
            fn from(value: WorkProjectionSequenceV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkProjectionSequenceV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProjectionSequenceV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkProjectionSequenceV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkProviderRouteId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkProviderRouteId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkProviderRouteId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkProviderRouteId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProviderRouteId> for ::std::string::String {
            fn from(value: WorkProviderRouteId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkProviderRouteId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProviderRouteId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkProviderRouteId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProviderRouteV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "provider_id",
        ///    "route_id"
        ///  ],
        ///  "properties": {
        ///    "provider_id": {
        ///      "$ref": "#/definitions/ProviderId"
        ///    },
        ///    "route_id": {
        ///      "$ref": "#/definitions/WorkProviderRouteId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProviderRouteV1 {
            pub provider_id: ProviderId,
            pub route_id: WorkProviderRouteId,
        }
        ///`WorkRecoveryStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "fresh"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "checkpoint": {
        ///          "anyOf": [
        ///            {
        ///              "$ref": "#/definitions/WorkArtifactRefV1"
        ///            },
        ///            {
        ///              "type": "null"
        ///            }
        ///          ]
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "resumed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "restarted"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "recovery_required"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state")]
        pub enum WorkRecoveryStateV1 {
            #[serde(rename = "fresh")]
            Fresh,
            #[serde(rename = "resumed")]
            Resumed {
                #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
                checkpoint: ::std::option::Option<WorkArtifactRefV1>,
                source_attempt_id: AttemptId,
            },
            #[serde(rename = "restarted")]
            Restarted { reason: WorkRestartReasonV1, source_attempt_id: AttemptId },
            #[serde(rename = "recovery_required")]
            RecoveryRequired {
                reason: WorkRestartReasonV1,
                source_attempt_id: AttemptId,
            },
        }
        ///`WorkRestartReasonV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "lease_lost",
        ///    "provider_unavailable",
        ///    "process_lost",
        ///    "checkpoint_rejected"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkRestartReasonV1 {
            #[serde(rename = "lease_lost")]
            LeaseLost,
            #[serde(rename = "provider_unavailable")]
            ProviderUnavailable,
            #[serde(rename = "process_lost")]
            ProcessLost,
            #[serde(rename = "checkpoint_rejected")]
            CheckpointRejected,
        }
        impl ::std::fmt::Display for WorkRestartReasonV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::LeaseLost => f.write_str("lease_lost"),
                    Self::ProviderUnavailable => f.write_str("provider_unavailable"),
                    Self::ProcessLost => f.write_str("process_lost"),
                    Self::CheckpointRejected => f.write_str("checkpoint_rejected"),
                }
            }
        }
        impl ::std::str::FromStr for WorkRestartReasonV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "lease_lost" => Ok(Self::LeaseLost),
                    "provider_unavailable" => Ok(Self::ProviderUnavailable),
                    "process_lost" => Ok(Self::ProcessLost),
                    "checkpoint_rejected" => Ok(Self::CheckpointRejected),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        ///`WorkTerminalEvidenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "succeeded"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "failed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "cancelled"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "outcome")]
        pub enum WorkTerminalEvidenceV1 {
            #[serde(rename = "succeeded")]
            Succeeded { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "failed")]
            Failed { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "cancelled")]
            Cancelled { evidence_digest: ManifestDigest, observed_at: UtcMicros },
        }
    }
    pub type Request = request::WorkAttemptAcquireLeaseRequestV1;
    pub type Result = result::WorkAttemptResponseV1;
}
typed_operation!(
    WorkAttemptAcquireLease, work_attempt_acquire_lease,
    "operation.work.attempt_acquire_lease", "/application/work/attempt/acquire-lease",
    "binding.http.work.attempt_acquire_lease",
    "schema.work.attempt_acquire_lease.result", 1
);
#[allow(clippy::all)]
pub mod work_attempt_cancel {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `AttemptId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `AttemptId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct AttemptId(pub ::std::string::String);
        impl ::std::ops::Deref for AttemptId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<AttemptId> for ::std::string::String {
            fn from(value: AttemptId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for AttemptId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for AttemptId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for AttemptId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkAttemptCancelRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkAttemptCancelRequestV1",
        ///  "type": "object",
        ///  "required": [
        ///    "identity",
        ///    "lease",
        ///    "request"
        ///  ],
        ///  "properties": {
        ///    "identity": {
        ///      "$ref": "#/definitions/WorkAttemptIdentityV1"
        ///    },
        ///    "lease": {
        ///      "$ref": "#/definitions/WorkLeaseFenceV1"
        ///    },
        ///    "request": {
        ///      "$ref": "#/definitions/WorkCancellationRequestV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptCancelRequestV1 {
            pub identity: WorkAttemptIdentityV1,
            pub lease: WorkLeaseFenceV1,
            pub request: WorkCancellationRequestV1,
        }
        ///`WorkAttemptIdentityV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "attempt_id",
        ///    "run_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "attempt_id": {
        ///      "$ref": "#/definitions/AttemptId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptIdentityV1 {
            pub attempt_id: AttemptId,
            pub run_id: RunId,
            pub task_id: TaskId,
        }
        ///Strongly typed canonical identity: `WorkCancellationRequestId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkCancellationRequestId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkCancellationRequestId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkCancellationRequestId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkCancellationRequestId> for ::std::string::String {
            fn from(value: WorkCancellationRequestId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkCancellationRequestId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkCancellationRequestId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkCancellationRequestId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkCancellationRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "request_id",
        ///    "requested_at"
        ///  ],
        ///  "properties": {
        ///    "request_id": {
        ///      "$ref": "#/definitions/WorkCancellationRequestId"
        ///    },
        ///    "requested_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationRequestV1 {
            pub request_id: WorkCancellationRequestId,
            pub requested_at: UtcMicros,
        }
        ///`WorkFenceEpochV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkFenceEpochV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkFenceEpochV1(pub u64);
        impl ::std::ops::Deref for WorkFenceEpochV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkFenceEpochV1> for u64 {
            fn from(value: WorkFenceEpochV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkFenceEpochV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkFenceEpochV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkFenceEpochV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkLeaseFenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "epoch",
        ///    "lease_id"
        ///  ],
        ///  "properties": {
        ///    "epoch": {
        ///      "$ref": "#/definitions/WorkFenceEpochV1"
        ///    },
        ///    "lease_id": {
        ///      "$ref": "#/definitions/WorkLeaseId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkLeaseFenceV1 {
            pub epoch: WorkFenceEpochV1,
            pub lease_id: WorkLeaseId,
        }
        ///Strongly typed canonical identity: `WorkLeaseId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkLeaseId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkLeaseId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkLeaseId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkLeaseId> for ::std::string::String {
            fn from(value: WorkLeaseId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkLeaseId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkLeaseId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkLeaseId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `AttemptId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `AttemptId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct AttemptId(pub ::std::string::String);
        impl ::std::ops::Deref for AttemptId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<AttemptId> for ::std::string::String {
            fn from(value: AttemptId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for AttemptId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for AttemptId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for AttemptId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectionGenerationId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectionGenerationId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectionGenerationId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectionGenerationId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectionGenerationId> for ::std::string::String {
            fn from(value: ProjectionGenerationId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectionGenerationId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectionGenerationId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectionGenerationId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProviderId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProviderId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProviderId(pub ::std::string::String);
        impl ::std::ops::Deref for ProviderId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProviderId> for ::std::string::String {
            fn from(value: ProviderId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProviderId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProviderId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProviderId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkArtifactId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkArtifactId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkArtifactId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkArtifactId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkArtifactId> for ::std::string::String {
            fn from(value: WorkArtifactId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkArtifactId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkArtifactId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkArtifactId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkArtifactRefV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifact_id",
        ///    "byte_length",
        ///    "digest"
        ///  ],
        ///  "properties": {
        ///    "artifact_id": {
        ///      "$ref": "#/definitions/WorkArtifactId"
        ///    },
        ///    "byte_length": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkArtifactRefV1 {
            pub artifact_id: WorkArtifactId,
            pub byte_length: u64,
            pub digest: ManifestDigest,
        }
        ///`WorkAttemptIdentityV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "attempt_id",
        ///    "run_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "attempt_id": {
        ///      "$ref": "#/definitions/AttemptId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptIdentityV1 {
            pub attempt_id: AttemptId,
            pub run_id: RunId,
            pub task_id: TaskId,
        }
        ///`WorkAttemptProgressV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "completed",
        ///    "total"
        ///  ],
        ///  "properties": {
        ///    "completed": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "total": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProgressV1 {
            pub completed: u64,
            pub total: u64,
        }
        ///`WorkAttemptProjectionBindingV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "generation_id",
        ///    "sequence",
        ///    "work_version"
        ///  ],
        ///  "properties": {
        ///    "generation_id": {
        ///      "$ref": "#/definitions/ProjectionGenerationId"
        ///    },
        ///    "sequence": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    },
        ///    "work_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProjectionBindingV1 {
            pub generation_id: ProjectionGenerationId,
            pub sequence: WorkProjectionSequenceV1,
            pub work_version: u64,
        }
        ///`WorkAttemptResponseV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkAttemptResponseV1",
        ///  "type": "object",
        ///  "required": [
        ///    "attempt"
        ///  ],
        ///  "properties": {
        ///    "attempt": {
        ///      "$ref": "#/definitions/WorkAttemptV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptResponseV1 {
            pub attempt: WorkAttemptV1,
        }
        ///`WorkAttemptStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "leased",
        ///    "running",
        ///    "cancellation_requested",
        ///    "cancellation_acknowledged",
        ///    "cancellation_escalated",
        ///    "recovery_required",
        ///    "succeeded",
        ///    "failed",
        ///    "cancelled"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkAttemptStateV1 {
            #[serde(rename = "leased")]
            Leased,
            #[serde(rename = "running")]
            Running,
            #[serde(rename = "cancellation_requested")]
            CancellationRequested,
            #[serde(rename = "cancellation_acknowledged")]
            CancellationAcknowledged,
            #[serde(rename = "cancellation_escalated")]
            CancellationEscalated,
            #[serde(rename = "recovery_required")]
            RecoveryRequired,
            #[serde(rename = "succeeded")]
            Succeeded,
            #[serde(rename = "failed")]
            Failed,
            #[serde(rename = "cancelled")]
            Cancelled,
        }
        impl ::std::fmt::Display for WorkAttemptStateV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::Leased => f.write_str("leased"),
                    Self::Running => f.write_str("running"),
                    Self::CancellationRequested => f.write_str("cancellation_requested"),
                    Self::CancellationAcknowledged => {
                        f.write_str("cancellation_acknowledged")
                    }
                    Self::CancellationEscalated => f.write_str("cancellation_escalated"),
                    Self::RecoveryRequired => f.write_str("recovery_required"),
                    Self::Succeeded => f.write_str("succeeded"),
                    Self::Failed => f.write_str("failed"),
                    Self::Cancelled => f.write_str("cancelled"),
                }
            }
        }
        impl ::std::str::FromStr for WorkAttemptStateV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "leased" => Ok(Self::Leased),
                    "running" => Ok(Self::Running),
                    "cancellation_requested" => Ok(Self::CancellationRequested),
                    "cancellation_acknowledged" => Ok(Self::CancellationAcknowledged),
                    "cancellation_escalated" => Ok(Self::CancellationEscalated),
                    "recovery_required" => Ok(Self::RecoveryRequired),
                    "succeeded" => Ok(Self::Succeeded),
                    "failed" => Ok(Self::Failed),
                    "cancelled" => Ok(Self::Cancelled),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        ///`WorkAttemptV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifacts",
        ///    "cancellation",
        ///    "identity",
        ///    "lease",
        ///    "projection_binding",
        ///    "recovery",
        ///    "requested_route",
        ///    "state"
        ///  ],
        ///  "properties": {
        ///    "actual_route": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkProviderRouteV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "artifacts": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkArtifactRefV1"
        ///      }
        ///    },
        ///    "cancellation": {
        ///      "$ref": "#/definitions/WorkCancellationStateV1"
        ///    },
        ///    "identity": {
        ///      "$ref": "#/definitions/WorkAttemptIdentityV1"
        ///    },
        ///    "lease": {
        ///      "$ref": "#/definitions/WorkLeaseFenceV1"
        ///    },
        ///    "progress": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkAttemptProgressV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "projection_binding": {
        ///      "$ref": "#/definitions/WorkAttemptProjectionBindingV1"
        ///    },
        ///    "recovery": {
        ///      "$ref": "#/definitions/WorkRecoveryStateV1"
        ///    },
        ///    "requested_route": {
        ///      "$ref": "#/definitions/WorkProviderRouteV1"
        ///    },
        ///    "state": {
        ///      "$ref": "#/definitions/WorkAttemptStateV1"
        ///    },
        ///    "terminal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkTerminalEvidenceV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkAttemptV1 {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub actual_route: ::std::option::Option<WorkProviderRouteV1>,
            pub artifacts: ::std::vec::Vec<WorkArtifactRefV1>,
            pub cancellation: WorkCancellationStateV1,
            pub identity: WorkAttemptIdentityV1,
            pub lease: WorkLeaseFenceV1,
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub progress: ::std::option::Option<WorkAttemptProgressV1>,
            pub projection_binding: WorkAttemptProjectionBindingV1,
            pub recovery: WorkRecoveryStateV1,
            pub requested_route: WorkProviderRouteV1,
            pub state: WorkAttemptStateV1,
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub terminal: ::std::option::Option<WorkTerminalEvidenceV1>,
        }
        ///`WorkCancellationAcknowledgementV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "acknowledged_at",
        ///    "request"
        ///  ],
        ///  "properties": {
        ///    "acknowledged_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    },
        ///    "request": {
        ///      "$ref": "#/definitions/WorkCancellationRequestV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationAcknowledgementV1 {
            pub acknowledged_at: UtcMicros,
            pub request: WorkCancellationRequestV1,
        }
        ///`WorkCancellationEscalationV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "acknowledgement",
        ///    "escalated_at"
        ///  ],
        ///  "properties": {
        ///    "acknowledgement": {
        ///      "$ref": "#/definitions/WorkCancellationAcknowledgementV1"
        ///    },
        ///    "escalated_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationEscalationV1 {
            pub acknowledgement: WorkCancellationAcknowledgementV1,
            pub escalated_at: UtcMicros,
        }
        ///Strongly typed canonical identity: `WorkCancellationRequestId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkCancellationRequestId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkCancellationRequestId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkCancellationRequestId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkCancellationRequestId> for ::std::string::String {
            fn from(value: WorkCancellationRequestId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkCancellationRequestId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkCancellationRequestId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkCancellationRequestId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkCancellationRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "request_id",
        ///    "requested_at"
        ///  ],
        ///  "properties": {
        ///    "request_id": {
        ///      "$ref": "#/definitions/WorkCancellationRequestId"
        ///    },
        ///    "requested_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationRequestV1 {
            pub request_id: WorkCancellationRequestId,
            pub requested_at: UtcMicros,
        }
        ///`WorkCancellationStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "none"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "requested"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationRequestV1"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "acknowledged"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationAcknowledgementV1"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "escalated"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationEscalationV1"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state", content = "value")]
        pub enum WorkCancellationStateV1 {
            #[serde(rename = "none")]
            None,
            #[serde(rename = "requested")]
            Requested(WorkCancellationRequestV1),
            #[serde(rename = "acknowledged")]
            Acknowledged(WorkCancellationAcknowledgementV1),
            #[serde(rename = "escalated")]
            Escalated(WorkCancellationEscalationV1),
        }
        impl ::std::convert::From<WorkCancellationRequestV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationRequestV1) -> Self {
                Self::Requested(value)
            }
        }
        impl ::std::convert::From<WorkCancellationAcknowledgementV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationAcknowledgementV1) -> Self {
                Self::Acknowledged(value)
            }
        }
        impl ::std::convert::From<WorkCancellationEscalationV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationEscalationV1) -> Self {
                Self::Escalated(value)
            }
        }
        ///`WorkFenceEpochV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkFenceEpochV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkFenceEpochV1(pub u64);
        impl ::std::ops::Deref for WorkFenceEpochV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkFenceEpochV1> for u64 {
            fn from(value: WorkFenceEpochV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkFenceEpochV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkFenceEpochV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkFenceEpochV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkLeaseFenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "epoch",
        ///    "lease_id"
        ///  ],
        ///  "properties": {
        ///    "epoch": {
        ///      "$ref": "#/definitions/WorkFenceEpochV1"
        ///    },
        ///    "lease_id": {
        ///      "$ref": "#/definitions/WorkLeaseId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkLeaseFenceV1 {
            pub epoch: WorkFenceEpochV1,
            pub lease_id: WorkLeaseId,
        }
        ///Strongly typed canonical identity: `WorkLeaseId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkLeaseId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkLeaseId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkLeaseId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkLeaseId> for ::std::string::String {
            fn from(value: WorkLeaseId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkLeaseId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkLeaseId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkLeaseId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProjectionSequenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjectionSequenceV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkProjectionSequenceV1(pub u64);
        impl ::std::ops::Deref for WorkProjectionSequenceV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProjectionSequenceV1> for u64 {
            fn from(value: WorkProjectionSequenceV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkProjectionSequenceV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProjectionSequenceV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkProjectionSequenceV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkProviderRouteId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkProviderRouteId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkProviderRouteId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkProviderRouteId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProviderRouteId> for ::std::string::String {
            fn from(value: WorkProviderRouteId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkProviderRouteId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProviderRouteId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkProviderRouteId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProviderRouteV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "provider_id",
        ///    "route_id"
        ///  ],
        ///  "properties": {
        ///    "provider_id": {
        ///      "$ref": "#/definitions/ProviderId"
        ///    },
        ///    "route_id": {
        ///      "$ref": "#/definitions/WorkProviderRouteId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProviderRouteV1 {
            pub provider_id: ProviderId,
            pub route_id: WorkProviderRouteId,
        }
        ///`WorkRecoveryStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "fresh"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "checkpoint": {
        ///          "anyOf": [
        ///            {
        ///              "$ref": "#/definitions/WorkArtifactRefV1"
        ///            },
        ///            {
        ///              "type": "null"
        ///            }
        ///          ]
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "resumed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "restarted"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "recovery_required"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state")]
        pub enum WorkRecoveryStateV1 {
            #[serde(rename = "fresh")]
            Fresh,
            #[serde(rename = "resumed")]
            Resumed {
                #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
                checkpoint: ::std::option::Option<WorkArtifactRefV1>,
                source_attempt_id: AttemptId,
            },
            #[serde(rename = "restarted")]
            Restarted { reason: WorkRestartReasonV1, source_attempt_id: AttemptId },
            #[serde(rename = "recovery_required")]
            RecoveryRequired {
                reason: WorkRestartReasonV1,
                source_attempt_id: AttemptId,
            },
        }
        ///`WorkRestartReasonV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "lease_lost",
        ///    "provider_unavailable",
        ///    "process_lost",
        ///    "checkpoint_rejected"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkRestartReasonV1 {
            #[serde(rename = "lease_lost")]
            LeaseLost,
            #[serde(rename = "provider_unavailable")]
            ProviderUnavailable,
            #[serde(rename = "process_lost")]
            ProcessLost,
            #[serde(rename = "checkpoint_rejected")]
            CheckpointRejected,
        }
        impl ::std::fmt::Display for WorkRestartReasonV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::LeaseLost => f.write_str("lease_lost"),
                    Self::ProviderUnavailable => f.write_str("provider_unavailable"),
                    Self::ProcessLost => f.write_str("process_lost"),
                    Self::CheckpointRejected => f.write_str("checkpoint_rejected"),
                }
            }
        }
        impl ::std::str::FromStr for WorkRestartReasonV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "lease_lost" => Ok(Self::LeaseLost),
                    "provider_unavailable" => Ok(Self::ProviderUnavailable),
                    "process_lost" => Ok(Self::ProcessLost),
                    "checkpoint_rejected" => Ok(Self::CheckpointRejected),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        ///`WorkTerminalEvidenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "succeeded"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "failed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "cancelled"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "outcome")]
        pub enum WorkTerminalEvidenceV1 {
            #[serde(rename = "succeeded")]
            Succeeded { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "failed")]
            Failed { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "cancelled")]
            Cancelled { evidence_digest: ManifestDigest, observed_at: UtcMicros },
        }
    }
    pub type Request = request::WorkAttemptCancelRequestV1;
    pub type Result = result::WorkAttemptResponseV1;
}
typed_operation!(
    WorkAttemptCancel, work_attempt_cancel, "operation.work.attempt_cancel",
    "/application/work/attempt/cancel", "binding.http.work.attempt_cancel",
    "schema.work.attempt_cancel.result", 1
);
#[allow(clippy::all)]
pub mod work_attempt_publish_artifact {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `AttemptId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `AttemptId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct AttemptId(pub ::std::string::String);
        impl ::std::ops::Deref for AttemptId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<AttemptId> for ::std::string::String {
            fn from(value: AttemptId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for AttemptId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for AttemptId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for AttemptId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkArtifactId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkArtifactId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkArtifactId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkArtifactId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkArtifactId> for ::std::string::String {
            fn from(value: WorkArtifactId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkArtifactId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkArtifactId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkArtifactId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkArtifactRefV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifact_id",
        ///    "byte_length",
        ///    "digest"
        ///  ],
        ///  "properties": {
        ///    "artifact_id": {
        ///      "$ref": "#/definitions/WorkArtifactId"
        ///    },
        ///    "byte_length": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkArtifactRefV1 {
            pub artifact_id: WorkArtifactId,
            pub byte_length: u64,
            pub digest: ManifestDigest,
        }
        ///`WorkAttemptIdentityV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "attempt_id",
        ///    "run_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "attempt_id": {
        ///      "$ref": "#/definitions/AttemptId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptIdentityV1 {
            pub attempt_id: AttemptId,
            pub run_id: RunId,
            pub task_id: TaskId,
        }
        ///`WorkAttemptPublishArtifactRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkAttemptPublishArtifactRequestV1",
        ///  "type": "object",
        ///  "required": [
        ///    "artifact",
        ///    "identity",
        ///    "lease"
        ///  ],
        ///  "properties": {
        ///    "artifact": {
        ///      "$ref": "#/definitions/WorkArtifactRefV1"
        ///    },
        ///    "identity": {
        ///      "$ref": "#/definitions/WorkAttemptIdentityV1"
        ///    },
        ///    "lease": {
        ///      "$ref": "#/definitions/WorkLeaseFenceV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptPublishArtifactRequestV1 {
            pub artifact: WorkArtifactRefV1,
            pub identity: WorkAttemptIdentityV1,
            pub lease: WorkLeaseFenceV1,
        }
        ///`WorkFenceEpochV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkFenceEpochV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkFenceEpochV1(pub u64);
        impl ::std::ops::Deref for WorkFenceEpochV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkFenceEpochV1> for u64 {
            fn from(value: WorkFenceEpochV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkFenceEpochV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkFenceEpochV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkFenceEpochV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkLeaseFenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "epoch",
        ///    "lease_id"
        ///  ],
        ///  "properties": {
        ///    "epoch": {
        ///      "$ref": "#/definitions/WorkFenceEpochV1"
        ///    },
        ///    "lease_id": {
        ///      "$ref": "#/definitions/WorkLeaseId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkLeaseFenceV1 {
            pub epoch: WorkFenceEpochV1,
            pub lease_id: WorkLeaseId,
        }
        ///Strongly typed canonical identity: `WorkLeaseId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkLeaseId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkLeaseId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkLeaseId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkLeaseId> for ::std::string::String {
            fn from(value: WorkLeaseId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkLeaseId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkLeaseId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkLeaseId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `AttemptId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `AttemptId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct AttemptId(pub ::std::string::String);
        impl ::std::ops::Deref for AttemptId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<AttemptId> for ::std::string::String {
            fn from(value: AttemptId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for AttemptId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for AttemptId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for AttemptId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectionGenerationId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectionGenerationId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectionGenerationId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectionGenerationId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectionGenerationId> for ::std::string::String {
            fn from(value: ProjectionGenerationId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectionGenerationId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectionGenerationId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectionGenerationId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProviderId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProviderId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProviderId(pub ::std::string::String);
        impl ::std::ops::Deref for ProviderId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProviderId> for ::std::string::String {
            fn from(value: ProviderId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProviderId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProviderId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProviderId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkArtifactId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkArtifactId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkArtifactId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkArtifactId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkArtifactId> for ::std::string::String {
            fn from(value: WorkArtifactId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkArtifactId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkArtifactId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkArtifactId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkArtifactRefV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifact_id",
        ///    "byte_length",
        ///    "digest"
        ///  ],
        ///  "properties": {
        ///    "artifact_id": {
        ///      "$ref": "#/definitions/WorkArtifactId"
        ///    },
        ///    "byte_length": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkArtifactRefV1 {
            pub artifact_id: WorkArtifactId,
            pub byte_length: u64,
            pub digest: ManifestDigest,
        }
        ///`WorkAttemptIdentityV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "attempt_id",
        ///    "run_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "attempt_id": {
        ///      "$ref": "#/definitions/AttemptId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptIdentityV1 {
            pub attempt_id: AttemptId,
            pub run_id: RunId,
            pub task_id: TaskId,
        }
        ///`WorkAttemptProgressV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "completed",
        ///    "total"
        ///  ],
        ///  "properties": {
        ///    "completed": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "total": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProgressV1 {
            pub completed: u64,
            pub total: u64,
        }
        ///`WorkAttemptProjectionBindingV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "generation_id",
        ///    "sequence",
        ///    "work_version"
        ///  ],
        ///  "properties": {
        ///    "generation_id": {
        ///      "$ref": "#/definitions/ProjectionGenerationId"
        ///    },
        ///    "sequence": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    },
        ///    "work_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProjectionBindingV1 {
            pub generation_id: ProjectionGenerationId,
            pub sequence: WorkProjectionSequenceV1,
            pub work_version: u64,
        }
        ///`WorkAttemptResponseV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkAttemptResponseV1",
        ///  "type": "object",
        ///  "required": [
        ///    "attempt"
        ///  ],
        ///  "properties": {
        ///    "attempt": {
        ///      "$ref": "#/definitions/WorkAttemptV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptResponseV1 {
            pub attempt: WorkAttemptV1,
        }
        ///`WorkAttemptStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "leased",
        ///    "running",
        ///    "cancellation_requested",
        ///    "cancellation_acknowledged",
        ///    "cancellation_escalated",
        ///    "recovery_required",
        ///    "succeeded",
        ///    "failed",
        ///    "cancelled"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkAttemptStateV1 {
            #[serde(rename = "leased")]
            Leased,
            #[serde(rename = "running")]
            Running,
            #[serde(rename = "cancellation_requested")]
            CancellationRequested,
            #[serde(rename = "cancellation_acknowledged")]
            CancellationAcknowledged,
            #[serde(rename = "cancellation_escalated")]
            CancellationEscalated,
            #[serde(rename = "recovery_required")]
            RecoveryRequired,
            #[serde(rename = "succeeded")]
            Succeeded,
            #[serde(rename = "failed")]
            Failed,
            #[serde(rename = "cancelled")]
            Cancelled,
        }
        impl ::std::fmt::Display for WorkAttemptStateV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::Leased => f.write_str("leased"),
                    Self::Running => f.write_str("running"),
                    Self::CancellationRequested => f.write_str("cancellation_requested"),
                    Self::CancellationAcknowledged => {
                        f.write_str("cancellation_acknowledged")
                    }
                    Self::CancellationEscalated => f.write_str("cancellation_escalated"),
                    Self::RecoveryRequired => f.write_str("recovery_required"),
                    Self::Succeeded => f.write_str("succeeded"),
                    Self::Failed => f.write_str("failed"),
                    Self::Cancelled => f.write_str("cancelled"),
                }
            }
        }
        impl ::std::str::FromStr for WorkAttemptStateV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "leased" => Ok(Self::Leased),
                    "running" => Ok(Self::Running),
                    "cancellation_requested" => Ok(Self::CancellationRequested),
                    "cancellation_acknowledged" => Ok(Self::CancellationAcknowledged),
                    "cancellation_escalated" => Ok(Self::CancellationEscalated),
                    "recovery_required" => Ok(Self::RecoveryRequired),
                    "succeeded" => Ok(Self::Succeeded),
                    "failed" => Ok(Self::Failed),
                    "cancelled" => Ok(Self::Cancelled),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        ///`WorkAttemptV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifacts",
        ///    "cancellation",
        ///    "identity",
        ///    "lease",
        ///    "projection_binding",
        ///    "recovery",
        ///    "requested_route",
        ///    "state"
        ///  ],
        ///  "properties": {
        ///    "actual_route": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkProviderRouteV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "artifacts": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkArtifactRefV1"
        ///      }
        ///    },
        ///    "cancellation": {
        ///      "$ref": "#/definitions/WorkCancellationStateV1"
        ///    },
        ///    "identity": {
        ///      "$ref": "#/definitions/WorkAttemptIdentityV1"
        ///    },
        ///    "lease": {
        ///      "$ref": "#/definitions/WorkLeaseFenceV1"
        ///    },
        ///    "progress": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkAttemptProgressV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "projection_binding": {
        ///      "$ref": "#/definitions/WorkAttemptProjectionBindingV1"
        ///    },
        ///    "recovery": {
        ///      "$ref": "#/definitions/WorkRecoveryStateV1"
        ///    },
        ///    "requested_route": {
        ///      "$ref": "#/definitions/WorkProviderRouteV1"
        ///    },
        ///    "state": {
        ///      "$ref": "#/definitions/WorkAttemptStateV1"
        ///    },
        ///    "terminal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkTerminalEvidenceV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkAttemptV1 {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub actual_route: ::std::option::Option<WorkProviderRouteV1>,
            pub artifacts: ::std::vec::Vec<WorkArtifactRefV1>,
            pub cancellation: WorkCancellationStateV1,
            pub identity: WorkAttemptIdentityV1,
            pub lease: WorkLeaseFenceV1,
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub progress: ::std::option::Option<WorkAttemptProgressV1>,
            pub projection_binding: WorkAttemptProjectionBindingV1,
            pub recovery: WorkRecoveryStateV1,
            pub requested_route: WorkProviderRouteV1,
            pub state: WorkAttemptStateV1,
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub terminal: ::std::option::Option<WorkTerminalEvidenceV1>,
        }
        ///`WorkCancellationAcknowledgementV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "acknowledged_at",
        ///    "request"
        ///  ],
        ///  "properties": {
        ///    "acknowledged_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    },
        ///    "request": {
        ///      "$ref": "#/definitions/WorkCancellationRequestV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationAcknowledgementV1 {
            pub acknowledged_at: UtcMicros,
            pub request: WorkCancellationRequestV1,
        }
        ///`WorkCancellationEscalationV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "acknowledgement",
        ///    "escalated_at"
        ///  ],
        ///  "properties": {
        ///    "acknowledgement": {
        ///      "$ref": "#/definitions/WorkCancellationAcknowledgementV1"
        ///    },
        ///    "escalated_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationEscalationV1 {
            pub acknowledgement: WorkCancellationAcknowledgementV1,
            pub escalated_at: UtcMicros,
        }
        ///Strongly typed canonical identity: `WorkCancellationRequestId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkCancellationRequestId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkCancellationRequestId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkCancellationRequestId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkCancellationRequestId> for ::std::string::String {
            fn from(value: WorkCancellationRequestId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkCancellationRequestId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkCancellationRequestId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkCancellationRequestId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkCancellationRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "request_id",
        ///    "requested_at"
        ///  ],
        ///  "properties": {
        ///    "request_id": {
        ///      "$ref": "#/definitions/WorkCancellationRequestId"
        ///    },
        ///    "requested_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationRequestV1 {
            pub request_id: WorkCancellationRequestId,
            pub requested_at: UtcMicros,
        }
        ///`WorkCancellationStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "none"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "requested"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationRequestV1"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "acknowledged"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationAcknowledgementV1"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "escalated"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationEscalationV1"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state", content = "value")]
        pub enum WorkCancellationStateV1 {
            #[serde(rename = "none")]
            None,
            #[serde(rename = "requested")]
            Requested(WorkCancellationRequestV1),
            #[serde(rename = "acknowledged")]
            Acknowledged(WorkCancellationAcknowledgementV1),
            #[serde(rename = "escalated")]
            Escalated(WorkCancellationEscalationV1),
        }
        impl ::std::convert::From<WorkCancellationRequestV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationRequestV1) -> Self {
                Self::Requested(value)
            }
        }
        impl ::std::convert::From<WorkCancellationAcknowledgementV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationAcknowledgementV1) -> Self {
                Self::Acknowledged(value)
            }
        }
        impl ::std::convert::From<WorkCancellationEscalationV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationEscalationV1) -> Self {
                Self::Escalated(value)
            }
        }
        ///`WorkFenceEpochV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkFenceEpochV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkFenceEpochV1(pub u64);
        impl ::std::ops::Deref for WorkFenceEpochV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkFenceEpochV1> for u64 {
            fn from(value: WorkFenceEpochV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkFenceEpochV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkFenceEpochV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkFenceEpochV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkLeaseFenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "epoch",
        ///    "lease_id"
        ///  ],
        ///  "properties": {
        ///    "epoch": {
        ///      "$ref": "#/definitions/WorkFenceEpochV1"
        ///    },
        ///    "lease_id": {
        ///      "$ref": "#/definitions/WorkLeaseId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkLeaseFenceV1 {
            pub epoch: WorkFenceEpochV1,
            pub lease_id: WorkLeaseId,
        }
        ///Strongly typed canonical identity: `WorkLeaseId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkLeaseId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkLeaseId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkLeaseId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkLeaseId> for ::std::string::String {
            fn from(value: WorkLeaseId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkLeaseId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkLeaseId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkLeaseId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProjectionSequenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjectionSequenceV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkProjectionSequenceV1(pub u64);
        impl ::std::ops::Deref for WorkProjectionSequenceV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProjectionSequenceV1> for u64 {
            fn from(value: WorkProjectionSequenceV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkProjectionSequenceV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProjectionSequenceV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkProjectionSequenceV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkProviderRouteId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkProviderRouteId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkProviderRouteId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkProviderRouteId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProviderRouteId> for ::std::string::String {
            fn from(value: WorkProviderRouteId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkProviderRouteId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProviderRouteId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkProviderRouteId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProviderRouteV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "provider_id",
        ///    "route_id"
        ///  ],
        ///  "properties": {
        ///    "provider_id": {
        ///      "$ref": "#/definitions/ProviderId"
        ///    },
        ///    "route_id": {
        ///      "$ref": "#/definitions/WorkProviderRouteId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProviderRouteV1 {
            pub provider_id: ProviderId,
            pub route_id: WorkProviderRouteId,
        }
        ///`WorkRecoveryStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "fresh"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "checkpoint": {
        ///          "anyOf": [
        ///            {
        ///              "$ref": "#/definitions/WorkArtifactRefV1"
        ///            },
        ///            {
        ///              "type": "null"
        ///            }
        ///          ]
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "resumed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "restarted"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "recovery_required"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state")]
        pub enum WorkRecoveryStateV1 {
            #[serde(rename = "fresh")]
            Fresh,
            #[serde(rename = "resumed")]
            Resumed {
                #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
                checkpoint: ::std::option::Option<WorkArtifactRefV1>,
                source_attempt_id: AttemptId,
            },
            #[serde(rename = "restarted")]
            Restarted { reason: WorkRestartReasonV1, source_attempt_id: AttemptId },
            #[serde(rename = "recovery_required")]
            RecoveryRequired {
                reason: WorkRestartReasonV1,
                source_attempt_id: AttemptId,
            },
        }
        ///`WorkRestartReasonV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "lease_lost",
        ///    "provider_unavailable",
        ///    "process_lost",
        ///    "checkpoint_rejected"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkRestartReasonV1 {
            #[serde(rename = "lease_lost")]
            LeaseLost,
            #[serde(rename = "provider_unavailable")]
            ProviderUnavailable,
            #[serde(rename = "process_lost")]
            ProcessLost,
            #[serde(rename = "checkpoint_rejected")]
            CheckpointRejected,
        }
        impl ::std::fmt::Display for WorkRestartReasonV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::LeaseLost => f.write_str("lease_lost"),
                    Self::ProviderUnavailable => f.write_str("provider_unavailable"),
                    Self::ProcessLost => f.write_str("process_lost"),
                    Self::CheckpointRejected => f.write_str("checkpoint_rejected"),
                }
            }
        }
        impl ::std::str::FromStr for WorkRestartReasonV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "lease_lost" => Ok(Self::LeaseLost),
                    "provider_unavailable" => Ok(Self::ProviderUnavailable),
                    "process_lost" => Ok(Self::ProcessLost),
                    "checkpoint_rejected" => Ok(Self::CheckpointRejected),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        ///`WorkTerminalEvidenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "succeeded"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "failed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "cancelled"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "outcome")]
        pub enum WorkTerminalEvidenceV1 {
            #[serde(rename = "succeeded")]
            Succeeded { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "failed")]
            Failed { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "cancelled")]
            Cancelled { evidence_digest: ManifestDigest, observed_at: UtcMicros },
        }
    }
    pub type Request = request::WorkAttemptPublishArtifactRequestV1;
    pub type Result = result::WorkAttemptResponseV1;
}
typed_operation!(
    WorkAttemptPublishArtifact, work_attempt_publish_artifact,
    "operation.work.attempt_publish_artifact",
    "/application/work/attempt/publish-artifact",
    "binding.http.work.attempt_publish_artifact",
    "schema.work.attempt_publish_artifact.result", 1
);
#[allow(clippy::all)]
pub mod work_attempt_publish_progress {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `AttemptId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `AttemptId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct AttemptId(pub ::std::string::String);
        impl ::std::ops::Deref for AttemptId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<AttemptId> for ::std::string::String {
            fn from(value: AttemptId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for AttemptId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for AttemptId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for AttemptId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkAttemptIdentityV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "attempt_id",
        ///    "run_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "attempt_id": {
        ///      "$ref": "#/definitions/AttemptId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptIdentityV1 {
            pub attempt_id: AttemptId,
            pub run_id: RunId,
            pub task_id: TaskId,
        }
        ///`WorkAttemptProgressV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "completed",
        ///    "total"
        ///  ],
        ///  "properties": {
        ///    "completed": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "total": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProgressV1 {
            pub completed: u64,
            pub total: u64,
        }
        ///`WorkAttemptPublishProgressRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkAttemptPublishProgressRequestV1",
        ///  "type": "object",
        ///  "required": [
        ///    "identity",
        ///    "lease",
        ///    "progress"
        ///  ],
        ///  "properties": {
        ///    "identity": {
        ///      "$ref": "#/definitions/WorkAttemptIdentityV1"
        ///    },
        ///    "lease": {
        ///      "$ref": "#/definitions/WorkLeaseFenceV1"
        ///    },
        ///    "progress": {
        ///      "$ref": "#/definitions/WorkAttemptProgressV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptPublishProgressRequestV1 {
            pub identity: WorkAttemptIdentityV1,
            pub lease: WorkLeaseFenceV1,
            pub progress: WorkAttemptProgressV1,
        }
        ///`WorkFenceEpochV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkFenceEpochV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkFenceEpochV1(pub u64);
        impl ::std::ops::Deref for WorkFenceEpochV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkFenceEpochV1> for u64 {
            fn from(value: WorkFenceEpochV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkFenceEpochV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkFenceEpochV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkFenceEpochV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkLeaseFenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "epoch",
        ///    "lease_id"
        ///  ],
        ///  "properties": {
        ///    "epoch": {
        ///      "$ref": "#/definitions/WorkFenceEpochV1"
        ///    },
        ///    "lease_id": {
        ///      "$ref": "#/definitions/WorkLeaseId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkLeaseFenceV1 {
            pub epoch: WorkFenceEpochV1,
            pub lease_id: WorkLeaseId,
        }
        ///Strongly typed canonical identity: `WorkLeaseId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkLeaseId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkLeaseId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkLeaseId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkLeaseId> for ::std::string::String {
            fn from(value: WorkLeaseId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkLeaseId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkLeaseId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkLeaseId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `AttemptId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `AttemptId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct AttemptId(pub ::std::string::String);
        impl ::std::ops::Deref for AttemptId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<AttemptId> for ::std::string::String {
            fn from(value: AttemptId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for AttemptId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for AttemptId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for AttemptId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectionGenerationId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectionGenerationId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectionGenerationId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectionGenerationId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectionGenerationId> for ::std::string::String {
            fn from(value: ProjectionGenerationId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectionGenerationId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectionGenerationId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectionGenerationId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProviderId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProviderId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProviderId(pub ::std::string::String);
        impl ::std::ops::Deref for ProviderId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProviderId> for ::std::string::String {
            fn from(value: ProviderId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProviderId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProviderId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProviderId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkArtifactId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkArtifactId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkArtifactId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkArtifactId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkArtifactId> for ::std::string::String {
            fn from(value: WorkArtifactId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkArtifactId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkArtifactId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkArtifactId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkArtifactRefV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifact_id",
        ///    "byte_length",
        ///    "digest"
        ///  ],
        ///  "properties": {
        ///    "artifact_id": {
        ///      "$ref": "#/definitions/WorkArtifactId"
        ///    },
        ///    "byte_length": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkArtifactRefV1 {
            pub artifact_id: WorkArtifactId,
            pub byte_length: u64,
            pub digest: ManifestDigest,
        }
        ///`WorkAttemptIdentityV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "attempt_id",
        ///    "run_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "attempt_id": {
        ///      "$ref": "#/definitions/AttemptId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptIdentityV1 {
            pub attempt_id: AttemptId,
            pub run_id: RunId,
            pub task_id: TaskId,
        }
        ///`WorkAttemptProgressV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "completed",
        ///    "total"
        ///  ],
        ///  "properties": {
        ///    "completed": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "total": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProgressV1 {
            pub completed: u64,
            pub total: u64,
        }
        ///`WorkAttemptProjectionBindingV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "generation_id",
        ///    "sequence",
        ///    "work_version"
        ///  ],
        ///  "properties": {
        ///    "generation_id": {
        ///      "$ref": "#/definitions/ProjectionGenerationId"
        ///    },
        ///    "sequence": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    },
        ///    "work_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProjectionBindingV1 {
            pub generation_id: ProjectionGenerationId,
            pub sequence: WorkProjectionSequenceV1,
            pub work_version: u64,
        }
        ///`WorkAttemptResponseV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkAttemptResponseV1",
        ///  "type": "object",
        ///  "required": [
        ///    "attempt"
        ///  ],
        ///  "properties": {
        ///    "attempt": {
        ///      "$ref": "#/definitions/WorkAttemptV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptResponseV1 {
            pub attempt: WorkAttemptV1,
        }
        ///`WorkAttemptStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "leased",
        ///    "running",
        ///    "cancellation_requested",
        ///    "cancellation_acknowledged",
        ///    "cancellation_escalated",
        ///    "recovery_required",
        ///    "succeeded",
        ///    "failed",
        ///    "cancelled"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkAttemptStateV1 {
            #[serde(rename = "leased")]
            Leased,
            #[serde(rename = "running")]
            Running,
            #[serde(rename = "cancellation_requested")]
            CancellationRequested,
            #[serde(rename = "cancellation_acknowledged")]
            CancellationAcknowledged,
            #[serde(rename = "cancellation_escalated")]
            CancellationEscalated,
            #[serde(rename = "recovery_required")]
            RecoveryRequired,
            #[serde(rename = "succeeded")]
            Succeeded,
            #[serde(rename = "failed")]
            Failed,
            #[serde(rename = "cancelled")]
            Cancelled,
        }
        impl ::std::fmt::Display for WorkAttemptStateV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::Leased => f.write_str("leased"),
                    Self::Running => f.write_str("running"),
                    Self::CancellationRequested => f.write_str("cancellation_requested"),
                    Self::CancellationAcknowledged => {
                        f.write_str("cancellation_acknowledged")
                    }
                    Self::CancellationEscalated => f.write_str("cancellation_escalated"),
                    Self::RecoveryRequired => f.write_str("recovery_required"),
                    Self::Succeeded => f.write_str("succeeded"),
                    Self::Failed => f.write_str("failed"),
                    Self::Cancelled => f.write_str("cancelled"),
                }
            }
        }
        impl ::std::str::FromStr for WorkAttemptStateV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "leased" => Ok(Self::Leased),
                    "running" => Ok(Self::Running),
                    "cancellation_requested" => Ok(Self::CancellationRequested),
                    "cancellation_acknowledged" => Ok(Self::CancellationAcknowledged),
                    "cancellation_escalated" => Ok(Self::CancellationEscalated),
                    "recovery_required" => Ok(Self::RecoveryRequired),
                    "succeeded" => Ok(Self::Succeeded),
                    "failed" => Ok(Self::Failed),
                    "cancelled" => Ok(Self::Cancelled),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        ///`WorkAttemptV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifacts",
        ///    "cancellation",
        ///    "identity",
        ///    "lease",
        ///    "projection_binding",
        ///    "recovery",
        ///    "requested_route",
        ///    "state"
        ///  ],
        ///  "properties": {
        ///    "actual_route": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkProviderRouteV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "artifacts": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkArtifactRefV1"
        ///      }
        ///    },
        ///    "cancellation": {
        ///      "$ref": "#/definitions/WorkCancellationStateV1"
        ///    },
        ///    "identity": {
        ///      "$ref": "#/definitions/WorkAttemptIdentityV1"
        ///    },
        ///    "lease": {
        ///      "$ref": "#/definitions/WorkLeaseFenceV1"
        ///    },
        ///    "progress": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkAttemptProgressV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "projection_binding": {
        ///      "$ref": "#/definitions/WorkAttemptProjectionBindingV1"
        ///    },
        ///    "recovery": {
        ///      "$ref": "#/definitions/WorkRecoveryStateV1"
        ///    },
        ///    "requested_route": {
        ///      "$ref": "#/definitions/WorkProviderRouteV1"
        ///    },
        ///    "state": {
        ///      "$ref": "#/definitions/WorkAttemptStateV1"
        ///    },
        ///    "terminal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkTerminalEvidenceV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkAttemptV1 {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub actual_route: ::std::option::Option<WorkProviderRouteV1>,
            pub artifacts: ::std::vec::Vec<WorkArtifactRefV1>,
            pub cancellation: WorkCancellationStateV1,
            pub identity: WorkAttemptIdentityV1,
            pub lease: WorkLeaseFenceV1,
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub progress: ::std::option::Option<WorkAttemptProgressV1>,
            pub projection_binding: WorkAttemptProjectionBindingV1,
            pub recovery: WorkRecoveryStateV1,
            pub requested_route: WorkProviderRouteV1,
            pub state: WorkAttemptStateV1,
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub terminal: ::std::option::Option<WorkTerminalEvidenceV1>,
        }
        ///`WorkCancellationAcknowledgementV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "acknowledged_at",
        ///    "request"
        ///  ],
        ///  "properties": {
        ///    "acknowledged_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    },
        ///    "request": {
        ///      "$ref": "#/definitions/WorkCancellationRequestV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationAcknowledgementV1 {
            pub acknowledged_at: UtcMicros,
            pub request: WorkCancellationRequestV1,
        }
        ///`WorkCancellationEscalationV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "acknowledgement",
        ///    "escalated_at"
        ///  ],
        ///  "properties": {
        ///    "acknowledgement": {
        ///      "$ref": "#/definitions/WorkCancellationAcknowledgementV1"
        ///    },
        ///    "escalated_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationEscalationV1 {
            pub acknowledgement: WorkCancellationAcknowledgementV1,
            pub escalated_at: UtcMicros,
        }
        ///Strongly typed canonical identity: `WorkCancellationRequestId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkCancellationRequestId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkCancellationRequestId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkCancellationRequestId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkCancellationRequestId> for ::std::string::String {
            fn from(value: WorkCancellationRequestId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkCancellationRequestId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkCancellationRequestId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkCancellationRequestId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkCancellationRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "request_id",
        ///    "requested_at"
        ///  ],
        ///  "properties": {
        ///    "request_id": {
        ///      "$ref": "#/definitions/WorkCancellationRequestId"
        ///    },
        ///    "requested_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationRequestV1 {
            pub request_id: WorkCancellationRequestId,
            pub requested_at: UtcMicros,
        }
        ///`WorkCancellationStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "none"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "requested"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationRequestV1"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "acknowledged"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationAcknowledgementV1"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "escalated"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationEscalationV1"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state", content = "value")]
        pub enum WorkCancellationStateV1 {
            #[serde(rename = "none")]
            None,
            #[serde(rename = "requested")]
            Requested(WorkCancellationRequestV1),
            #[serde(rename = "acknowledged")]
            Acknowledged(WorkCancellationAcknowledgementV1),
            #[serde(rename = "escalated")]
            Escalated(WorkCancellationEscalationV1),
        }
        impl ::std::convert::From<WorkCancellationRequestV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationRequestV1) -> Self {
                Self::Requested(value)
            }
        }
        impl ::std::convert::From<WorkCancellationAcknowledgementV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationAcknowledgementV1) -> Self {
                Self::Acknowledged(value)
            }
        }
        impl ::std::convert::From<WorkCancellationEscalationV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationEscalationV1) -> Self {
                Self::Escalated(value)
            }
        }
        ///`WorkFenceEpochV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkFenceEpochV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkFenceEpochV1(pub u64);
        impl ::std::ops::Deref for WorkFenceEpochV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkFenceEpochV1> for u64 {
            fn from(value: WorkFenceEpochV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkFenceEpochV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkFenceEpochV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkFenceEpochV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkLeaseFenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "epoch",
        ///    "lease_id"
        ///  ],
        ///  "properties": {
        ///    "epoch": {
        ///      "$ref": "#/definitions/WorkFenceEpochV1"
        ///    },
        ///    "lease_id": {
        ///      "$ref": "#/definitions/WorkLeaseId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkLeaseFenceV1 {
            pub epoch: WorkFenceEpochV1,
            pub lease_id: WorkLeaseId,
        }
        ///Strongly typed canonical identity: `WorkLeaseId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkLeaseId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkLeaseId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkLeaseId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkLeaseId> for ::std::string::String {
            fn from(value: WorkLeaseId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkLeaseId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkLeaseId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkLeaseId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProjectionSequenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjectionSequenceV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkProjectionSequenceV1(pub u64);
        impl ::std::ops::Deref for WorkProjectionSequenceV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProjectionSequenceV1> for u64 {
            fn from(value: WorkProjectionSequenceV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkProjectionSequenceV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProjectionSequenceV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkProjectionSequenceV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkProviderRouteId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkProviderRouteId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkProviderRouteId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkProviderRouteId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProviderRouteId> for ::std::string::String {
            fn from(value: WorkProviderRouteId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkProviderRouteId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProviderRouteId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkProviderRouteId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProviderRouteV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "provider_id",
        ///    "route_id"
        ///  ],
        ///  "properties": {
        ///    "provider_id": {
        ///      "$ref": "#/definitions/ProviderId"
        ///    },
        ///    "route_id": {
        ///      "$ref": "#/definitions/WorkProviderRouteId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProviderRouteV1 {
            pub provider_id: ProviderId,
            pub route_id: WorkProviderRouteId,
        }
        ///`WorkRecoveryStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "fresh"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "checkpoint": {
        ///          "anyOf": [
        ///            {
        ///              "$ref": "#/definitions/WorkArtifactRefV1"
        ///            },
        ///            {
        ///              "type": "null"
        ///            }
        ///          ]
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "resumed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "restarted"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "recovery_required"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state")]
        pub enum WorkRecoveryStateV1 {
            #[serde(rename = "fresh")]
            Fresh,
            #[serde(rename = "resumed")]
            Resumed {
                #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
                checkpoint: ::std::option::Option<WorkArtifactRefV1>,
                source_attempt_id: AttemptId,
            },
            #[serde(rename = "restarted")]
            Restarted { reason: WorkRestartReasonV1, source_attempt_id: AttemptId },
            #[serde(rename = "recovery_required")]
            RecoveryRequired {
                reason: WorkRestartReasonV1,
                source_attempt_id: AttemptId,
            },
        }
        ///`WorkRestartReasonV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "lease_lost",
        ///    "provider_unavailable",
        ///    "process_lost",
        ///    "checkpoint_rejected"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkRestartReasonV1 {
            #[serde(rename = "lease_lost")]
            LeaseLost,
            #[serde(rename = "provider_unavailable")]
            ProviderUnavailable,
            #[serde(rename = "process_lost")]
            ProcessLost,
            #[serde(rename = "checkpoint_rejected")]
            CheckpointRejected,
        }
        impl ::std::fmt::Display for WorkRestartReasonV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::LeaseLost => f.write_str("lease_lost"),
                    Self::ProviderUnavailable => f.write_str("provider_unavailable"),
                    Self::ProcessLost => f.write_str("process_lost"),
                    Self::CheckpointRejected => f.write_str("checkpoint_rejected"),
                }
            }
        }
        impl ::std::str::FromStr for WorkRestartReasonV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "lease_lost" => Ok(Self::LeaseLost),
                    "provider_unavailable" => Ok(Self::ProviderUnavailable),
                    "process_lost" => Ok(Self::ProcessLost),
                    "checkpoint_rejected" => Ok(Self::CheckpointRejected),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        ///`WorkTerminalEvidenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "succeeded"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "failed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "cancelled"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "outcome")]
        pub enum WorkTerminalEvidenceV1 {
            #[serde(rename = "succeeded")]
            Succeeded { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "failed")]
            Failed { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "cancelled")]
            Cancelled { evidence_digest: ManifestDigest, observed_at: UtcMicros },
        }
    }
    pub type Request = request::WorkAttemptPublishProgressRequestV1;
    pub type Result = result::WorkAttemptResponseV1;
}
typed_operation!(
    WorkAttemptPublishProgress, work_attempt_publish_progress,
    "operation.work.attempt_publish_progress",
    "/application/work/attempt/publish-progress",
    "binding.http.work.attempt_publish_progress",
    "schema.work.attempt_publish_progress.result", 1
);
#[allow(clippy::all)]
pub mod work_attempt_recover {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `AttemptId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `AttemptId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct AttemptId(pub ::std::string::String);
        impl ::std::ops::Deref for AttemptId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<AttemptId> for ::std::string::String {
            fn from(value: AttemptId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for AttemptId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for AttemptId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for AttemptId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkAttemptIdentityV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "attempt_id",
        ///    "run_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "attempt_id": {
        ///      "$ref": "#/definitions/AttemptId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptIdentityV1 {
            pub attempt_id: AttemptId,
            pub run_id: RunId,
            pub task_id: TaskId,
        }
        ///`WorkAttemptRecoverRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkAttemptRecoverRequestV1",
        ///  "type": "object",
        ///  "required": [
        ///    "identity",
        ///    "lease",
        ///    "reason"
        ///  ],
        ///  "properties": {
        ///    "identity": {
        ///      "$ref": "#/definitions/WorkAttemptIdentityV1"
        ///    },
        ///    "lease": {
        ///      "$ref": "#/definitions/WorkLeaseFenceV1"
        ///    },
        ///    "reason": {
        ///      "$ref": "#/definitions/WorkRestartReasonV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptRecoverRequestV1 {
            pub identity: WorkAttemptIdentityV1,
            pub lease: WorkLeaseFenceV1,
            pub reason: WorkRestartReasonV1,
        }
        ///`WorkFenceEpochV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkFenceEpochV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkFenceEpochV1(pub u64);
        impl ::std::ops::Deref for WorkFenceEpochV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkFenceEpochV1> for u64 {
            fn from(value: WorkFenceEpochV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkFenceEpochV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkFenceEpochV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkFenceEpochV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkLeaseFenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "epoch",
        ///    "lease_id"
        ///  ],
        ///  "properties": {
        ///    "epoch": {
        ///      "$ref": "#/definitions/WorkFenceEpochV1"
        ///    },
        ///    "lease_id": {
        ///      "$ref": "#/definitions/WorkLeaseId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkLeaseFenceV1 {
            pub epoch: WorkFenceEpochV1,
            pub lease_id: WorkLeaseId,
        }
        ///Strongly typed canonical identity: `WorkLeaseId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkLeaseId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkLeaseId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkLeaseId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkLeaseId> for ::std::string::String {
            fn from(value: WorkLeaseId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkLeaseId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkLeaseId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkLeaseId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkRestartReasonV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "lease_lost",
        ///    "provider_unavailable",
        ///    "process_lost",
        ///    "checkpoint_rejected"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkRestartReasonV1 {
            #[serde(rename = "lease_lost")]
            LeaseLost,
            #[serde(rename = "provider_unavailable")]
            ProviderUnavailable,
            #[serde(rename = "process_lost")]
            ProcessLost,
            #[serde(rename = "checkpoint_rejected")]
            CheckpointRejected,
        }
        impl ::std::fmt::Display for WorkRestartReasonV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::LeaseLost => f.write_str("lease_lost"),
                    Self::ProviderUnavailable => f.write_str("provider_unavailable"),
                    Self::ProcessLost => f.write_str("process_lost"),
                    Self::CheckpointRejected => f.write_str("checkpoint_rejected"),
                }
            }
        }
        impl ::std::str::FromStr for WorkRestartReasonV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "lease_lost" => Ok(Self::LeaseLost),
                    "provider_unavailable" => Ok(Self::ProviderUnavailable),
                    "process_lost" => Ok(Self::ProcessLost),
                    "checkpoint_rejected" => Ok(Self::CheckpointRejected),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `AttemptId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `AttemptId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct AttemptId(pub ::std::string::String);
        impl ::std::ops::Deref for AttemptId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<AttemptId> for ::std::string::String {
            fn from(value: AttemptId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for AttemptId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for AttemptId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for AttemptId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectionGenerationId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectionGenerationId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectionGenerationId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectionGenerationId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectionGenerationId> for ::std::string::String {
            fn from(value: ProjectionGenerationId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectionGenerationId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectionGenerationId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectionGenerationId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProviderId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProviderId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProviderId(pub ::std::string::String);
        impl ::std::ops::Deref for ProviderId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProviderId> for ::std::string::String {
            fn from(value: ProviderId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProviderId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProviderId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProviderId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkArtifactId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkArtifactId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkArtifactId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkArtifactId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkArtifactId> for ::std::string::String {
            fn from(value: WorkArtifactId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkArtifactId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkArtifactId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkArtifactId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkArtifactRefV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifact_id",
        ///    "byte_length",
        ///    "digest"
        ///  ],
        ///  "properties": {
        ///    "artifact_id": {
        ///      "$ref": "#/definitions/WorkArtifactId"
        ///    },
        ///    "byte_length": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkArtifactRefV1 {
            pub artifact_id: WorkArtifactId,
            pub byte_length: u64,
            pub digest: ManifestDigest,
        }
        ///`WorkAttemptIdentityV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "attempt_id",
        ///    "run_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "attempt_id": {
        ///      "$ref": "#/definitions/AttemptId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptIdentityV1 {
            pub attempt_id: AttemptId,
            pub run_id: RunId,
            pub task_id: TaskId,
        }
        ///`WorkAttemptProgressV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "completed",
        ///    "total"
        ///  ],
        ///  "properties": {
        ///    "completed": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "total": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProgressV1 {
            pub completed: u64,
            pub total: u64,
        }
        ///`WorkAttemptProjectionBindingV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "generation_id",
        ///    "sequence",
        ///    "work_version"
        ///  ],
        ///  "properties": {
        ///    "generation_id": {
        ///      "$ref": "#/definitions/ProjectionGenerationId"
        ///    },
        ///    "sequence": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    },
        ///    "work_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProjectionBindingV1 {
            pub generation_id: ProjectionGenerationId,
            pub sequence: WorkProjectionSequenceV1,
            pub work_version: u64,
        }
        ///`WorkAttemptResponseV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkAttemptResponseV1",
        ///  "type": "object",
        ///  "required": [
        ///    "attempt"
        ///  ],
        ///  "properties": {
        ///    "attempt": {
        ///      "$ref": "#/definitions/WorkAttemptV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptResponseV1 {
            pub attempt: WorkAttemptV1,
        }
        ///`WorkAttemptStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "leased",
        ///    "running",
        ///    "cancellation_requested",
        ///    "cancellation_acknowledged",
        ///    "cancellation_escalated",
        ///    "recovery_required",
        ///    "succeeded",
        ///    "failed",
        ///    "cancelled"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkAttemptStateV1 {
            #[serde(rename = "leased")]
            Leased,
            #[serde(rename = "running")]
            Running,
            #[serde(rename = "cancellation_requested")]
            CancellationRequested,
            #[serde(rename = "cancellation_acknowledged")]
            CancellationAcknowledged,
            #[serde(rename = "cancellation_escalated")]
            CancellationEscalated,
            #[serde(rename = "recovery_required")]
            RecoveryRequired,
            #[serde(rename = "succeeded")]
            Succeeded,
            #[serde(rename = "failed")]
            Failed,
            #[serde(rename = "cancelled")]
            Cancelled,
        }
        impl ::std::fmt::Display for WorkAttemptStateV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::Leased => f.write_str("leased"),
                    Self::Running => f.write_str("running"),
                    Self::CancellationRequested => f.write_str("cancellation_requested"),
                    Self::CancellationAcknowledged => {
                        f.write_str("cancellation_acknowledged")
                    }
                    Self::CancellationEscalated => f.write_str("cancellation_escalated"),
                    Self::RecoveryRequired => f.write_str("recovery_required"),
                    Self::Succeeded => f.write_str("succeeded"),
                    Self::Failed => f.write_str("failed"),
                    Self::Cancelled => f.write_str("cancelled"),
                }
            }
        }
        impl ::std::str::FromStr for WorkAttemptStateV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "leased" => Ok(Self::Leased),
                    "running" => Ok(Self::Running),
                    "cancellation_requested" => Ok(Self::CancellationRequested),
                    "cancellation_acknowledged" => Ok(Self::CancellationAcknowledged),
                    "cancellation_escalated" => Ok(Self::CancellationEscalated),
                    "recovery_required" => Ok(Self::RecoveryRequired),
                    "succeeded" => Ok(Self::Succeeded),
                    "failed" => Ok(Self::Failed),
                    "cancelled" => Ok(Self::Cancelled),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        ///`WorkAttemptV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifacts",
        ///    "cancellation",
        ///    "identity",
        ///    "lease",
        ///    "projection_binding",
        ///    "recovery",
        ///    "requested_route",
        ///    "state"
        ///  ],
        ///  "properties": {
        ///    "actual_route": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkProviderRouteV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "artifacts": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkArtifactRefV1"
        ///      }
        ///    },
        ///    "cancellation": {
        ///      "$ref": "#/definitions/WorkCancellationStateV1"
        ///    },
        ///    "identity": {
        ///      "$ref": "#/definitions/WorkAttemptIdentityV1"
        ///    },
        ///    "lease": {
        ///      "$ref": "#/definitions/WorkLeaseFenceV1"
        ///    },
        ///    "progress": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkAttemptProgressV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "projection_binding": {
        ///      "$ref": "#/definitions/WorkAttemptProjectionBindingV1"
        ///    },
        ///    "recovery": {
        ///      "$ref": "#/definitions/WorkRecoveryStateV1"
        ///    },
        ///    "requested_route": {
        ///      "$ref": "#/definitions/WorkProviderRouteV1"
        ///    },
        ///    "state": {
        ///      "$ref": "#/definitions/WorkAttemptStateV1"
        ///    },
        ///    "terminal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkTerminalEvidenceV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkAttemptV1 {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub actual_route: ::std::option::Option<WorkProviderRouteV1>,
            pub artifacts: ::std::vec::Vec<WorkArtifactRefV1>,
            pub cancellation: WorkCancellationStateV1,
            pub identity: WorkAttemptIdentityV1,
            pub lease: WorkLeaseFenceV1,
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub progress: ::std::option::Option<WorkAttemptProgressV1>,
            pub projection_binding: WorkAttemptProjectionBindingV1,
            pub recovery: WorkRecoveryStateV1,
            pub requested_route: WorkProviderRouteV1,
            pub state: WorkAttemptStateV1,
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub terminal: ::std::option::Option<WorkTerminalEvidenceV1>,
        }
        ///`WorkCancellationAcknowledgementV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "acknowledged_at",
        ///    "request"
        ///  ],
        ///  "properties": {
        ///    "acknowledged_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    },
        ///    "request": {
        ///      "$ref": "#/definitions/WorkCancellationRequestV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationAcknowledgementV1 {
            pub acknowledged_at: UtcMicros,
            pub request: WorkCancellationRequestV1,
        }
        ///`WorkCancellationEscalationV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "acknowledgement",
        ///    "escalated_at"
        ///  ],
        ///  "properties": {
        ///    "acknowledgement": {
        ///      "$ref": "#/definitions/WorkCancellationAcknowledgementV1"
        ///    },
        ///    "escalated_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationEscalationV1 {
            pub acknowledgement: WorkCancellationAcknowledgementV1,
            pub escalated_at: UtcMicros,
        }
        ///Strongly typed canonical identity: `WorkCancellationRequestId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkCancellationRequestId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkCancellationRequestId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkCancellationRequestId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkCancellationRequestId> for ::std::string::String {
            fn from(value: WorkCancellationRequestId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkCancellationRequestId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkCancellationRequestId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkCancellationRequestId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkCancellationRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "request_id",
        ///    "requested_at"
        ///  ],
        ///  "properties": {
        ///    "request_id": {
        ///      "$ref": "#/definitions/WorkCancellationRequestId"
        ///    },
        ///    "requested_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationRequestV1 {
            pub request_id: WorkCancellationRequestId,
            pub requested_at: UtcMicros,
        }
        ///`WorkCancellationStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "none"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "requested"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationRequestV1"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "acknowledged"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationAcknowledgementV1"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "escalated"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationEscalationV1"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state", content = "value")]
        pub enum WorkCancellationStateV1 {
            #[serde(rename = "none")]
            None,
            #[serde(rename = "requested")]
            Requested(WorkCancellationRequestV1),
            #[serde(rename = "acknowledged")]
            Acknowledged(WorkCancellationAcknowledgementV1),
            #[serde(rename = "escalated")]
            Escalated(WorkCancellationEscalationV1),
        }
        impl ::std::convert::From<WorkCancellationRequestV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationRequestV1) -> Self {
                Self::Requested(value)
            }
        }
        impl ::std::convert::From<WorkCancellationAcknowledgementV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationAcknowledgementV1) -> Self {
                Self::Acknowledged(value)
            }
        }
        impl ::std::convert::From<WorkCancellationEscalationV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationEscalationV1) -> Self {
                Self::Escalated(value)
            }
        }
        ///`WorkFenceEpochV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkFenceEpochV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkFenceEpochV1(pub u64);
        impl ::std::ops::Deref for WorkFenceEpochV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkFenceEpochV1> for u64 {
            fn from(value: WorkFenceEpochV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkFenceEpochV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkFenceEpochV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkFenceEpochV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkLeaseFenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "epoch",
        ///    "lease_id"
        ///  ],
        ///  "properties": {
        ///    "epoch": {
        ///      "$ref": "#/definitions/WorkFenceEpochV1"
        ///    },
        ///    "lease_id": {
        ///      "$ref": "#/definitions/WorkLeaseId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkLeaseFenceV1 {
            pub epoch: WorkFenceEpochV1,
            pub lease_id: WorkLeaseId,
        }
        ///Strongly typed canonical identity: `WorkLeaseId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkLeaseId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkLeaseId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkLeaseId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkLeaseId> for ::std::string::String {
            fn from(value: WorkLeaseId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkLeaseId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkLeaseId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkLeaseId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProjectionSequenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjectionSequenceV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkProjectionSequenceV1(pub u64);
        impl ::std::ops::Deref for WorkProjectionSequenceV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProjectionSequenceV1> for u64 {
            fn from(value: WorkProjectionSequenceV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkProjectionSequenceV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProjectionSequenceV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkProjectionSequenceV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkProviderRouteId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkProviderRouteId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkProviderRouteId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkProviderRouteId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProviderRouteId> for ::std::string::String {
            fn from(value: WorkProviderRouteId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkProviderRouteId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProviderRouteId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkProviderRouteId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProviderRouteV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "provider_id",
        ///    "route_id"
        ///  ],
        ///  "properties": {
        ///    "provider_id": {
        ///      "$ref": "#/definitions/ProviderId"
        ///    },
        ///    "route_id": {
        ///      "$ref": "#/definitions/WorkProviderRouteId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProviderRouteV1 {
            pub provider_id: ProviderId,
            pub route_id: WorkProviderRouteId,
        }
        ///`WorkRecoveryStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "fresh"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "checkpoint": {
        ///          "anyOf": [
        ///            {
        ///              "$ref": "#/definitions/WorkArtifactRefV1"
        ///            },
        ///            {
        ///              "type": "null"
        ///            }
        ///          ]
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "resumed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "restarted"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "recovery_required"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state")]
        pub enum WorkRecoveryStateV1 {
            #[serde(rename = "fresh")]
            Fresh,
            #[serde(rename = "resumed")]
            Resumed {
                #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
                checkpoint: ::std::option::Option<WorkArtifactRefV1>,
                source_attempt_id: AttemptId,
            },
            #[serde(rename = "restarted")]
            Restarted { reason: WorkRestartReasonV1, source_attempt_id: AttemptId },
            #[serde(rename = "recovery_required")]
            RecoveryRequired {
                reason: WorkRestartReasonV1,
                source_attempt_id: AttemptId,
            },
        }
        ///`WorkRestartReasonV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "lease_lost",
        ///    "provider_unavailable",
        ///    "process_lost",
        ///    "checkpoint_rejected"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkRestartReasonV1 {
            #[serde(rename = "lease_lost")]
            LeaseLost,
            #[serde(rename = "provider_unavailable")]
            ProviderUnavailable,
            #[serde(rename = "process_lost")]
            ProcessLost,
            #[serde(rename = "checkpoint_rejected")]
            CheckpointRejected,
        }
        impl ::std::fmt::Display for WorkRestartReasonV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::LeaseLost => f.write_str("lease_lost"),
                    Self::ProviderUnavailable => f.write_str("provider_unavailable"),
                    Self::ProcessLost => f.write_str("process_lost"),
                    Self::CheckpointRejected => f.write_str("checkpoint_rejected"),
                }
            }
        }
        impl ::std::str::FromStr for WorkRestartReasonV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "lease_lost" => Ok(Self::LeaseLost),
                    "provider_unavailable" => Ok(Self::ProviderUnavailable),
                    "process_lost" => Ok(Self::ProcessLost),
                    "checkpoint_rejected" => Ok(Self::CheckpointRejected),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        ///`WorkTerminalEvidenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "succeeded"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "failed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "cancelled"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "outcome")]
        pub enum WorkTerminalEvidenceV1 {
            #[serde(rename = "succeeded")]
            Succeeded { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "failed")]
            Failed { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "cancelled")]
            Cancelled { evidence_digest: ManifestDigest, observed_at: UtcMicros },
        }
    }
    pub type Request = request::WorkAttemptRecoverRequestV1;
    pub type Result = result::WorkAttemptResponseV1;
}
typed_operation!(
    WorkAttemptRecover, work_attempt_recover, "operation.work.attempt_recover",
    "/application/work/attempt/recover", "binding.http.work.attempt_recover",
    "schema.work.attempt_recover.result", 1
);
#[allow(clippy::all)]
pub mod work_attempt_renew_lease {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `AttemptId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `AttemptId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct AttemptId(pub ::std::string::String);
        impl ::std::ops::Deref for AttemptId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<AttemptId> for ::std::string::String {
            fn from(value: AttemptId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for AttemptId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for AttemptId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for AttemptId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkAttemptIdentityV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "attempt_id",
        ///    "run_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "attempt_id": {
        ///      "$ref": "#/definitions/AttemptId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptIdentityV1 {
            pub attempt_id: AttemptId,
            pub run_id: RunId,
            pub task_id: TaskId,
        }
        ///`WorkAttemptRenewLeaseRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkAttemptRenewLeaseRequestV1",
        ///  "type": "object",
        ///  "required": [
        ///    "expected",
        ///    "identity",
        ///    "replacement"
        ///  ],
        ///  "properties": {
        ///    "expected": {
        ///      "$ref": "#/definitions/WorkLeaseFenceV1"
        ///    },
        ///    "identity": {
        ///      "$ref": "#/definitions/WorkAttemptIdentityV1"
        ///    },
        ///    "replacement": {
        ///      "$ref": "#/definitions/WorkLeaseFenceV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptRenewLeaseRequestV1 {
            pub expected: WorkLeaseFenceV1,
            pub identity: WorkAttemptIdentityV1,
            pub replacement: WorkLeaseFenceV1,
        }
        ///`WorkFenceEpochV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkFenceEpochV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkFenceEpochV1(pub u64);
        impl ::std::ops::Deref for WorkFenceEpochV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkFenceEpochV1> for u64 {
            fn from(value: WorkFenceEpochV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkFenceEpochV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkFenceEpochV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkFenceEpochV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkLeaseFenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "epoch",
        ///    "lease_id"
        ///  ],
        ///  "properties": {
        ///    "epoch": {
        ///      "$ref": "#/definitions/WorkFenceEpochV1"
        ///    },
        ///    "lease_id": {
        ///      "$ref": "#/definitions/WorkLeaseId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkLeaseFenceV1 {
            pub epoch: WorkFenceEpochV1,
            pub lease_id: WorkLeaseId,
        }
        ///Strongly typed canonical identity: `WorkLeaseId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkLeaseId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkLeaseId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkLeaseId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkLeaseId> for ::std::string::String {
            fn from(value: WorkLeaseId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkLeaseId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkLeaseId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkLeaseId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `AttemptId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `AttemptId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct AttemptId(pub ::std::string::String);
        impl ::std::ops::Deref for AttemptId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<AttemptId> for ::std::string::String {
            fn from(value: AttemptId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for AttemptId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for AttemptId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for AttemptId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectionGenerationId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectionGenerationId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectionGenerationId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectionGenerationId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectionGenerationId> for ::std::string::String {
            fn from(value: ProjectionGenerationId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectionGenerationId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectionGenerationId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectionGenerationId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProviderId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProviderId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProviderId(pub ::std::string::String);
        impl ::std::ops::Deref for ProviderId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProviderId> for ::std::string::String {
            fn from(value: ProviderId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProviderId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProviderId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProviderId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkArtifactId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkArtifactId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkArtifactId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkArtifactId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkArtifactId> for ::std::string::String {
            fn from(value: WorkArtifactId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkArtifactId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkArtifactId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkArtifactId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkArtifactRefV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifact_id",
        ///    "byte_length",
        ///    "digest"
        ///  ],
        ///  "properties": {
        ///    "artifact_id": {
        ///      "$ref": "#/definitions/WorkArtifactId"
        ///    },
        ///    "byte_length": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkArtifactRefV1 {
            pub artifact_id: WorkArtifactId,
            pub byte_length: u64,
            pub digest: ManifestDigest,
        }
        ///`WorkAttemptIdentityV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "attempt_id",
        ///    "run_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "attempt_id": {
        ///      "$ref": "#/definitions/AttemptId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptIdentityV1 {
            pub attempt_id: AttemptId,
            pub run_id: RunId,
            pub task_id: TaskId,
        }
        ///`WorkAttemptProgressV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "completed",
        ///    "total"
        ///  ],
        ///  "properties": {
        ///    "completed": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "total": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProgressV1 {
            pub completed: u64,
            pub total: u64,
        }
        ///`WorkAttemptProjectionBindingV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "generation_id",
        ///    "sequence",
        ///    "work_version"
        ///  ],
        ///  "properties": {
        ///    "generation_id": {
        ///      "$ref": "#/definitions/ProjectionGenerationId"
        ///    },
        ///    "sequence": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    },
        ///    "work_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProjectionBindingV1 {
            pub generation_id: ProjectionGenerationId,
            pub sequence: WorkProjectionSequenceV1,
            pub work_version: u64,
        }
        ///`WorkAttemptResponseV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkAttemptResponseV1",
        ///  "type": "object",
        ///  "required": [
        ///    "attempt"
        ///  ],
        ///  "properties": {
        ///    "attempt": {
        ///      "$ref": "#/definitions/WorkAttemptV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptResponseV1 {
            pub attempt: WorkAttemptV1,
        }
        ///`WorkAttemptStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "leased",
        ///    "running",
        ///    "cancellation_requested",
        ///    "cancellation_acknowledged",
        ///    "cancellation_escalated",
        ///    "recovery_required",
        ///    "succeeded",
        ///    "failed",
        ///    "cancelled"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkAttemptStateV1 {
            #[serde(rename = "leased")]
            Leased,
            #[serde(rename = "running")]
            Running,
            #[serde(rename = "cancellation_requested")]
            CancellationRequested,
            #[serde(rename = "cancellation_acknowledged")]
            CancellationAcknowledged,
            #[serde(rename = "cancellation_escalated")]
            CancellationEscalated,
            #[serde(rename = "recovery_required")]
            RecoveryRequired,
            #[serde(rename = "succeeded")]
            Succeeded,
            #[serde(rename = "failed")]
            Failed,
            #[serde(rename = "cancelled")]
            Cancelled,
        }
        impl ::std::fmt::Display for WorkAttemptStateV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::Leased => f.write_str("leased"),
                    Self::Running => f.write_str("running"),
                    Self::CancellationRequested => f.write_str("cancellation_requested"),
                    Self::CancellationAcknowledged => {
                        f.write_str("cancellation_acknowledged")
                    }
                    Self::CancellationEscalated => f.write_str("cancellation_escalated"),
                    Self::RecoveryRequired => f.write_str("recovery_required"),
                    Self::Succeeded => f.write_str("succeeded"),
                    Self::Failed => f.write_str("failed"),
                    Self::Cancelled => f.write_str("cancelled"),
                }
            }
        }
        impl ::std::str::FromStr for WorkAttemptStateV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "leased" => Ok(Self::Leased),
                    "running" => Ok(Self::Running),
                    "cancellation_requested" => Ok(Self::CancellationRequested),
                    "cancellation_acknowledged" => Ok(Self::CancellationAcknowledged),
                    "cancellation_escalated" => Ok(Self::CancellationEscalated),
                    "recovery_required" => Ok(Self::RecoveryRequired),
                    "succeeded" => Ok(Self::Succeeded),
                    "failed" => Ok(Self::Failed),
                    "cancelled" => Ok(Self::Cancelled),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        ///`WorkAttemptV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifacts",
        ///    "cancellation",
        ///    "identity",
        ///    "lease",
        ///    "projection_binding",
        ///    "recovery",
        ///    "requested_route",
        ///    "state"
        ///  ],
        ///  "properties": {
        ///    "actual_route": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkProviderRouteV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "artifacts": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkArtifactRefV1"
        ///      }
        ///    },
        ///    "cancellation": {
        ///      "$ref": "#/definitions/WorkCancellationStateV1"
        ///    },
        ///    "identity": {
        ///      "$ref": "#/definitions/WorkAttemptIdentityV1"
        ///    },
        ///    "lease": {
        ///      "$ref": "#/definitions/WorkLeaseFenceV1"
        ///    },
        ///    "progress": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkAttemptProgressV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "projection_binding": {
        ///      "$ref": "#/definitions/WorkAttemptProjectionBindingV1"
        ///    },
        ///    "recovery": {
        ///      "$ref": "#/definitions/WorkRecoveryStateV1"
        ///    },
        ///    "requested_route": {
        ///      "$ref": "#/definitions/WorkProviderRouteV1"
        ///    },
        ///    "state": {
        ///      "$ref": "#/definitions/WorkAttemptStateV1"
        ///    },
        ///    "terminal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkTerminalEvidenceV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkAttemptV1 {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub actual_route: ::std::option::Option<WorkProviderRouteV1>,
            pub artifacts: ::std::vec::Vec<WorkArtifactRefV1>,
            pub cancellation: WorkCancellationStateV1,
            pub identity: WorkAttemptIdentityV1,
            pub lease: WorkLeaseFenceV1,
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub progress: ::std::option::Option<WorkAttemptProgressV1>,
            pub projection_binding: WorkAttemptProjectionBindingV1,
            pub recovery: WorkRecoveryStateV1,
            pub requested_route: WorkProviderRouteV1,
            pub state: WorkAttemptStateV1,
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub terminal: ::std::option::Option<WorkTerminalEvidenceV1>,
        }
        ///`WorkCancellationAcknowledgementV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "acknowledged_at",
        ///    "request"
        ///  ],
        ///  "properties": {
        ///    "acknowledged_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    },
        ///    "request": {
        ///      "$ref": "#/definitions/WorkCancellationRequestV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationAcknowledgementV1 {
            pub acknowledged_at: UtcMicros,
            pub request: WorkCancellationRequestV1,
        }
        ///`WorkCancellationEscalationV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "acknowledgement",
        ///    "escalated_at"
        ///  ],
        ///  "properties": {
        ///    "acknowledgement": {
        ///      "$ref": "#/definitions/WorkCancellationAcknowledgementV1"
        ///    },
        ///    "escalated_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationEscalationV1 {
            pub acknowledgement: WorkCancellationAcknowledgementV1,
            pub escalated_at: UtcMicros,
        }
        ///Strongly typed canonical identity: `WorkCancellationRequestId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkCancellationRequestId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkCancellationRequestId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkCancellationRequestId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkCancellationRequestId> for ::std::string::String {
            fn from(value: WorkCancellationRequestId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkCancellationRequestId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkCancellationRequestId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkCancellationRequestId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkCancellationRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "request_id",
        ///    "requested_at"
        ///  ],
        ///  "properties": {
        ///    "request_id": {
        ///      "$ref": "#/definitions/WorkCancellationRequestId"
        ///    },
        ///    "requested_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationRequestV1 {
            pub request_id: WorkCancellationRequestId,
            pub requested_at: UtcMicros,
        }
        ///`WorkCancellationStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "none"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "requested"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationRequestV1"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "acknowledged"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationAcknowledgementV1"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "escalated"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationEscalationV1"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state", content = "value")]
        pub enum WorkCancellationStateV1 {
            #[serde(rename = "none")]
            None,
            #[serde(rename = "requested")]
            Requested(WorkCancellationRequestV1),
            #[serde(rename = "acknowledged")]
            Acknowledged(WorkCancellationAcknowledgementV1),
            #[serde(rename = "escalated")]
            Escalated(WorkCancellationEscalationV1),
        }
        impl ::std::convert::From<WorkCancellationRequestV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationRequestV1) -> Self {
                Self::Requested(value)
            }
        }
        impl ::std::convert::From<WorkCancellationAcknowledgementV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationAcknowledgementV1) -> Self {
                Self::Acknowledged(value)
            }
        }
        impl ::std::convert::From<WorkCancellationEscalationV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationEscalationV1) -> Self {
                Self::Escalated(value)
            }
        }
        ///`WorkFenceEpochV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkFenceEpochV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkFenceEpochV1(pub u64);
        impl ::std::ops::Deref for WorkFenceEpochV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkFenceEpochV1> for u64 {
            fn from(value: WorkFenceEpochV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkFenceEpochV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkFenceEpochV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkFenceEpochV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkLeaseFenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "epoch",
        ///    "lease_id"
        ///  ],
        ///  "properties": {
        ///    "epoch": {
        ///      "$ref": "#/definitions/WorkFenceEpochV1"
        ///    },
        ///    "lease_id": {
        ///      "$ref": "#/definitions/WorkLeaseId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkLeaseFenceV1 {
            pub epoch: WorkFenceEpochV1,
            pub lease_id: WorkLeaseId,
        }
        ///Strongly typed canonical identity: `WorkLeaseId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkLeaseId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkLeaseId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkLeaseId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkLeaseId> for ::std::string::String {
            fn from(value: WorkLeaseId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkLeaseId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkLeaseId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkLeaseId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProjectionSequenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjectionSequenceV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkProjectionSequenceV1(pub u64);
        impl ::std::ops::Deref for WorkProjectionSequenceV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProjectionSequenceV1> for u64 {
            fn from(value: WorkProjectionSequenceV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkProjectionSequenceV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProjectionSequenceV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkProjectionSequenceV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkProviderRouteId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkProviderRouteId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkProviderRouteId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkProviderRouteId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProviderRouteId> for ::std::string::String {
            fn from(value: WorkProviderRouteId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkProviderRouteId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProviderRouteId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkProviderRouteId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProviderRouteV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "provider_id",
        ///    "route_id"
        ///  ],
        ///  "properties": {
        ///    "provider_id": {
        ///      "$ref": "#/definitions/ProviderId"
        ///    },
        ///    "route_id": {
        ///      "$ref": "#/definitions/WorkProviderRouteId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProviderRouteV1 {
            pub provider_id: ProviderId,
            pub route_id: WorkProviderRouteId,
        }
        ///`WorkRecoveryStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "fresh"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "checkpoint": {
        ///          "anyOf": [
        ///            {
        ///              "$ref": "#/definitions/WorkArtifactRefV1"
        ///            },
        ///            {
        ///              "type": "null"
        ///            }
        ///          ]
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "resumed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "restarted"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "recovery_required"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state")]
        pub enum WorkRecoveryStateV1 {
            #[serde(rename = "fresh")]
            Fresh,
            #[serde(rename = "resumed")]
            Resumed {
                #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
                checkpoint: ::std::option::Option<WorkArtifactRefV1>,
                source_attempt_id: AttemptId,
            },
            #[serde(rename = "restarted")]
            Restarted { reason: WorkRestartReasonV1, source_attempt_id: AttemptId },
            #[serde(rename = "recovery_required")]
            RecoveryRequired {
                reason: WorkRestartReasonV1,
                source_attempt_id: AttemptId,
            },
        }
        ///`WorkRestartReasonV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "lease_lost",
        ///    "provider_unavailable",
        ///    "process_lost",
        ///    "checkpoint_rejected"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkRestartReasonV1 {
            #[serde(rename = "lease_lost")]
            LeaseLost,
            #[serde(rename = "provider_unavailable")]
            ProviderUnavailable,
            #[serde(rename = "process_lost")]
            ProcessLost,
            #[serde(rename = "checkpoint_rejected")]
            CheckpointRejected,
        }
        impl ::std::fmt::Display for WorkRestartReasonV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::LeaseLost => f.write_str("lease_lost"),
                    Self::ProviderUnavailable => f.write_str("provider_unavailable"),
                    Self::ProcessLost => f.write_str("process_lost"),
                    Self::CheckpointRejected => f.write_str("checkpoint_rejected"),
                }
            }
        }
        impl ::std::str::FromStr for WorkRestartReasonV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "lease_lost" => Ok(Self::LeaseLost),
                    "provider_unavailable" => Ok(Self::ProviderUnavailable),
                    "process_lost" => Ok(Self::ProcessLost),
                    "checkpoint_rejected" => Ok(Self::CheckpointRejected),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        ///`WorkTerminalEvidenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "succeeded"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "failed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "cancelled"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "outcome")]
        pub enum WorkTerminalEvidenceV1 {
            #[serde(rename = "succeeded")]
            Succeeded { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "failed")]
            Failed { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "cancelled")]
            Cancelled { evidence_digest: ManifestDigest, observed_at: UtcMicros },
        }
    }
    pub type Request = request::WorkAttemptRenewLeaseRequestV1;
    pub type Result = result::WorkAttemptResponseV1;
}
typed_operation!(
    WorkAttemptRenewLease, work_attempt_renew_lease,
    "operation.work.attempt_renew_lease", "/application/work/attempt/renew-lease",
    "binding.http.work.attempt_renew_lease", "schema.work.attempt_renew_lease.result", 1
);
#[allow(clippy::all)]
pub mod work_attempt_start {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `AttemptId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `AttemptId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct AttemptId(pub ::std::string::String);
        impl ::std::ops::Deref for AttemptId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<AttemptId> for ::std::string::String {
            fn from(value: AttemptId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for AttemptId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for AttemptId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for AttemptId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkArtifactId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkArtifactId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkArtifactId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkArtifactId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkArtifactId> for ::std::string::String {
            fn from(value: WorkArtifactId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkArtifactId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkArtifactId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkArtifactId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkArtifactRefV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifact_id",
        ///    "byte_length",
        ///    "digest"
        ///  ],
        ///  "properties": {
        ///    "artifact_id": {
        ///      "$ref": "#/definitions/WorkArtifactId"
        ///    },
        ///    "byte_length": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkArtifactRefV1 {
            pub artifact_id: WorkArtifactId,
            pub byte_length: u64,
            pub digest: ManifestDigest,
        }
        ///`WorkAttemptIdentityV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "attempt_id",
        ///    "run_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "attempt_id": {
        ///      "$ref": "#/definitions/AttemptId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptIdentityV1 {
            pub attempt_id: AttemptId,
            pub run_id: RunId,
            pub task_id: TaskId,
        }
        ///`WorkAttemptStartRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkAttemptStartRequestV1",
        ///  "type": "object",
        ///  "required": [
        ///    "identity",
        ///    "lease",
        ///    "recovery"
        ///  ],
        ///  "properties": {
        ///    "identity": {
        ///      "$ref": "#/definitions/WorkAttemptIdentityV1"
        ///    },
        ///    "lease": {
        ///      "$ref": "#/definitions/WorkLeaseFenceV1"
        ///    },
        ///    "recovery": {
        ///      "$ref": "#/definitions/WorkRecoveryStateV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptStartRequestV1 {
            pub identity: WorkAttemptIdentityV1,
            pub lease: WorkLeaseFenceV1,
            pub recovery: WorkRecoveryStateV1,
        }
        ///`WorkFenceEpochV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkFenceEpochV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkFenceEpochV1(pub u64);
        impl ::std::ops::Deref for WorkFenceEpochV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkFenceEpochV1> for u64 {
            fn from(value: WorkFenceEpochV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkFenceEpochV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkFenceEpochV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkFenceEpochV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkLeaseFenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "epoch",
        ///    "lease_id"
        ///  ],
        ///  "properties": {
        ///    "epoch": {
        ///      "$ref": "#/definitions/WorkFenceEpochV1"
        ///    },
        ///    "lease_id": {
        ///      "$ref": "#/definitions/WorkLeaseId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkLeaseFenceV1 {
            pub epoch: WorkFenceEpochV1,
            pub lease_id: WorkLeaseId,
        }
        ///Strongly typed canonical identity: `WorkLeaseId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkLeaseId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkLeaseId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkLeaseId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkLeaseId> for ::std::string::String {
            fn from(value: WorkLeaseId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkLeaseId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkLeaseId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkLeaseId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkRecoveryStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "fresh"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "checkpoint": {
        ///          "anyOf": [
        ///            {
        ///              "$ref": "#/definitions/WorkArtifactRefV1"
        ///            },
        ///            {
        ///              "type": "null"
        ///            }
        ///          ]
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "resumed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "restarted"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "recovery_required"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state")]
        pub enum WorkRecoveryStateV1 {
            #[serde(rename = "fresh")]
            Fresh,
            #[serde(rename = "resumed")]
            Resumed {
                #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
                checkpoint: ::std::option::Option<WorkArtifactRefV1>,
                source_attempt_id: AttemptId,
            },
            #[serde(rename = "restarted")]
            Restarted { reason: WorkRestartReasonV1, source_attempt_id: AttemptId },
            #[serde(rename = "recovery_required")]
            RecoveryRequired {
                reason: WorkRestartReasonV1,
                source_attempt_id: AttemptId,
            },
        }
        ///`WorkRestartReasonV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "lease_lost",
        ///    "provider_unavailable",
        ///    "process_lost",
        ///    "checkpoint_rejected"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkRestartReasonV1 {
            #[serde(rename = "lease_lost")]
            LeaseLost,
            #[serde(rename = "provider_unavailable")]
            ProviderUnavailable,
            #[serde(rename = "process_lost")]
            ProcessLost,
            #[serde(rename = "checkpoint_rejected")]
            CheckpointRejected,
        }
        impl ::std::fmt::Display for WorkRestartReasonV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::LeaseLost => f.write_str("lease_lost"),
                    Self::ProviderUnavailable => f.write_str("provider_unavailable"),
                    Self::ProcessLost => f.write_str("process_lost"),
                    Self::CheckpointRejected => f.write_str("checkpoint_rejected"),
                }
            }
        }
        impl ::std::str::FromStr for WorkRestartReasonV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "lease_lost" => Ok(Self::LeaseLost),
                    "provider_unavailable" => Ok(Self::ProviderUnavailable),
                    "process_lost" => Ok(Self::ProcessLost),
                    "checkpoint_rejected" => Ok(Self::CheckpointRejected),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `AttemptId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `AttemptId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct AttemptId(pub ::std::string::String);
        impl ::std::ops::Deref for AttemptId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<AttemptId> for ::std::string::String {
            fn from(value: AttemptId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for AttemptId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for AttemptId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for AttemptId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectionGenerationId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectionGenerationId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectionGenerationId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectionGenerationId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectionGenerationId> for ::std::string::String {
            fn from(value: ProjectionGenerationId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectionGenerationId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectionGenerationId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectionGenerationId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProviderId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProviderId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProviderId(pub ::std::string::String);
        impl ::std::ops::Deref for ProviderId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProviderId> for ::std::string::String {
            fn from(value: ProviderId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProviderId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProviderId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProviderId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkArtifactId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkArtifactId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkArtifactId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkArtifactId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkArtifactId> for ::std::string::String {
            fn from(value: WorkArtifactId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkArtifactId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkArtifactId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkArtifactId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkArtifactRefV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifact_id",
        ///    "byte_length",
        ///    "digest"
        ///  ],
        ///  "properties": {
        ///    "artifact_id": {
        ///      "$ref": "#/definitions/WorkArtifactId"
        ///    },
        ///    "byte_length": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkArtifactRefV1 {
            pub artifact_id: WorkArtifactId,
            pub byte_length: u64,
            pub digest: ManifestDigest,
        }
        ///`WorkAttemptIdentityV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "attempt_id",
        ///    "run_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "attempt_id": {
        ///      "$ref": "#/definitions/AttemptId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptIdentityV1 {
            pub attempt_id: AttemptId,
            pub run_id: RunId,
            pub task_id: TaskId,
        }
        ///`WorkAttemptProgressV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "completed",
        ///    "total"
        ///  ],
        ///  "properties": {
        ///    "completed": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "total": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProgressV1 {
            pub completed: u64,
            pub total: u64,
        }
        ///`WorkAttemptProjectionBindingV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "generation_id",
        ///    "sequence",
        ///    "work_version"
        ///  ],
        ///  "properties": {
        ///    "generation_id": {
        ///      "$ref": "#/definitions/ProjectionGenerationId"
        ///    },
        ///    "sequence": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    },
        ///    "work_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProjectionBindingV1 {
            pub generation_id: ProjectionGenerationId,
            pub sequence: WorkProjectionSequenceV1,
            pub work_version: u64,
        }
        ///`WorkAttemptResponseV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkAttemptResponseV1",
        ///  "type": "object",
        ///  "required": [
        ///    "attempt"
        ///  ],
        ///  "properties": {
        ///    "attempt": {
        ///      "$ref": "#/definitions/WorkAttemptV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptResponseV1 {
            pub attempt: WorkAttemptV1,
        }
        ///`WorkAttemptStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "leased",
        ///    "running",
        ///    "cancellation_requested",
        ///    "cancellation_acknowledged",
        ///    "cancellation_escalated",
        ///    "recovery_required",
        ///    "succeeded",
        ///    "failed",
        ///    "cancelled"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkAttemptStateV1 {
            #[serde(rename = "leased")]
            Leased,
            #[serde(rename = "running")]
            Running,
            #[serde(rename = "cancellation_requested")]
            CancellationRequested,
            #[serde(rename = "cancellation_acknowledged")]
            CancellationAcknowledged,
            #[serde(rename = "cancellation_escalated")]
            CancellationEscalated,
            #[serde(rename = "recovery_required")]
            RecoveryRequired,
            #[serde(rename = "succeeded")]
            Succeeded,
            #[serde(rename = "failed")]
            Failed,
            #[serde(rename = "cancelled")]
            Cancelled,
        }
        impl ::std::fmt::Display for WorkAttemptStateV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::Leased => f.write_str("leased"),
                    Self::Running => f.write_str("running"),
                    Self::CancellationRequested => f.write_str("cancellation_requested"),
                    Self::CancellationAcknowledged => {
                        f.write_str("cancellation_acknowledged")
                    }
                    Self::CancellationEscalated => f.write_str("cancellation_escalated"),
                    Self::RecoveryRequired => f.write_str("recovery_required"),
                    Self::Succeeded => f.write_str("succeeded"),
                    Self::Failed => f.write_str("failed"),
                    Self::Cancelled => f.write_str("cancelled"),
                }
            }
        }
        impl ::std::str::FromStr for WorkAttemptStateV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "leased" => Ok(Self::Leased),
                    "running" => Ok(Self::Running),
                    "cancellation_requested" => Ok(Self::CancellationRequested),
                    "cancellation_acknowledged" => Ok(Self::CancellationAcknowledged),
                    "cancellation_escalated" => Ok(Self::CancellationEscalated),
                    "recovery_required" => Ok(Self::RecoveryRequired),
                    "succeeded" => Ok(Self::Succeeded),
                    "failed" => Ok(Self::Failed),
                    "cancelled" => Ok(Self::Cancelled),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        ///`WorkAttemptV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifacts",
        ///    "cancellation",
        ///    "identity",
        ///    "lease",
        ///    "projection_binding",
        ///    "recovery",
        ///    "requested_route",
        ///    "state"
        ///  ],
        ///  "properties": {
        ///    "actual_route": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkProviderRouteV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "artifacts": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkArtifactRefV1"
        ///      }
        ///    },
        ///    "cancellation": {
        ///      "$ref": "#/definitions/WorkCancellationStateV1"
        ///    },
        ///    "identity": {
        ///      "$ref": "#/definitions/WorkAttemptIdentityV1"
        ///    },
        ///    "lease": {
        ///      "$ref": "#/definitions/WorkLeaseFenceV1"
        ///    },
        ///    "progress": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkAttemptProgressV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "projection_binding": {
        ///      "$ref": "#/definitions/WorkAttemptProjectionBindingV1"
        ///    },
        ///    "recovery": {
        ///      "$ref": "#/definitions/WorkRecoveryStateV1"
        ///    },
        ///    "requested_route": {
        ///      "$ref": "#/definitions/WorkProviderRouteV1"
        ///    },
        ///    "state": {
        ///      "$ref": "#/definitions/WorkAttemptStateV1"
        ///    },
        ///    "terminal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkTerminalEvidenceV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkAttemptV1 {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub actual_route: ::std::option::Option<WorkProviderRouteV1>,
            pub artifacts: ::std::vec::Vec<WorkArtifactRefV1>,
            pub cancellation: WorkCancellationStateV1,
            pub identity: WorkAttemptIdentityV1,
            pub lease: WorkLeaseFenceV1,
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub progress: ::std::option::Option<WorkAttemptProgressV1>,
            pub projection_binding: WorkAttemptProjectionBindingV1,
            pub recovery: WorkRecoveryStateV1,
            pub requested_route: WorkProviderRouteV1,
            pub state: WorkAttemptStateV1,
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub terminal: ::std::option::Option<WorkTerminalEvidenceV1>,
        }
        ///`WorkCancellationAcknowledgementV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "acknowledged_at",
        ///    "request"
        ///  ],
        ///  "properties": {
        ///    "acknowledged_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    },
        ///    "request": {
        ///      "$ref": "#/definitions/WorkCancellationRequestV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationAcknowledgementV1 {
            pub acknowledged_at: UtcMicros,
            pub request: WorkCancellationRequestV1,
        }
        ///`WorkCancellationEscalationV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "acknowledgement",
        ///    "escalated_at"
        ///  ],
        ///  "properties": {
        ///    "acknowledgement": {
        ///      "$ref": "#/definitions/WorkCancellationAcknowledgementV1"
        ///    },
        ///    "escalated_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationEscalationV1 {
            pub acknowledgement: WorkCancellationAcknowledgementV1,
            pub escalated_at: UtcMicros,
        }
        ///Strongly typed canonical identity: `WorkCancellationRequestId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkCancellationRequestId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkCancellationRequestId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkCancellationRequestId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkCancellationRequestId> for ::std::string::String {
            fn from(value: WorkCancellationRequestId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkCancellationRequestId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkCancellationRequestId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkCancellationRequestId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkCancellationRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "request_id",
        ///    "requested_at"
        ///  ],
        ///  "properties": {
        ///    "request_id": {
        ///      "$ref": "#/definitions/WorkCancellationRequestId"
        ///    },
        ///    "requested_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationRequestV1 {
            pub request_id: WorkCancellationRequestId,
            pub requested_at: UtcMicros,
        }
        ///`WorkCancellationStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "none"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "requested"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationRequestV1"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "acknowledged"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationAcknowledgementV1"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "escalated"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationEscalationV1"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state", content = "value")]
        pub enum WorkCancellationStateV1 {
            #[serde(rename = "none")]
            None,
            #[serde(rename = "requested")]
            Requested(WorkCancellationRequestV1),
            #[serde(rename = "acknowledged")]
            Acknowledged(WorkCancellationAcknowledgementV1),
            #[serde(rename = "escalated")]
            Escalated(WorkCancellationEscalationV1),
        }
        impl ::std::convert::From<WorkCancellationRequestV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationRequestV1) -> Self {
                Self::Requested(value)
            }
        }
        impl ::std::convert::From<WorkCancellationAcknowledgementV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationAcknowledgementV1) -> Self {
                Self::Acknowledged(value)
            }
        }
        impl ::std::convert::From<WorkCancellationEscalationV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationEscalationV1) -> Self {
                Self::Escalated(value)
            }
        }
        ///`WorkFenceEpochV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkFenceEpochV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkFenceEpochV1(pub u64);
        impl ::std::ops::Deref for WorkFenceEpochV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkFenceEpochV1> for u64 {
            fn from(value: WorkFenceEpochV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkFenceEpochV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkFenceEpochV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkFenceEpochV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkLeaseFenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "epoch",
        ///    "lease_id"
        ///  ],
        ///  "properties": {
        ///    "epoch": {
        ///      "$ref": "#/definitions/WorkFenceEpochV1"
        ///    },
        ///    "lease_id": {
        ///      "$ref": "#/definitions/WorkLeaseId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkLeaseFenceV1 {
            pub epoch: WorkFenceEpochV1,
            pub lease_id: WorkLeaseId,
        }
        ///Strongly typed canonical identity: `WorkLeaseId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkLeaseId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkLeaseId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkLeaseId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkLeaseId> for ::std::string::String {
            fn from(value: WorkLeaseId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkLeaseId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkLeaseId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkLeaseId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProjectionSequenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjectionSequenceV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkProjectionSequenceV1(pub u64);
        impl ::std::ops::Deref for WorkProjectionSequenceV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProjectionSequenceV1> for u64 {
            fn from(value: WorkProjectionSequenceV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkProjectionSequenceV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProjectionSequenceV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkProjectionSequenceV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkProviderRouteId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkProviderRouteId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkProviderRouteId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkProviderRouteId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProviderRouteId> for ::std::string::String {
            fn from(value: WorkProviderRouteId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkProviderRouteId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProviderRouteId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkProviderRouteId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProviderRouteV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "provider_id",
        ///    "route_id"
        ///  ],
        ///  "properties": {
        ///    "provider_id": {
        ///      "$ref": "#/definitions/ProviderId"
        ///    },
        ///    "route_id": {
        ///      "$ref": "#/definitions/WorkProviderRouteId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProviderRouteV1 {
            pub provider_id: ProviderId,
            pub route_id: WorkProviderRouteId,
        }
        ///`WorkRecoveryStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "fresh"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "checkpoint": {
        ///          "anyOf": [
        ///            {
        ///              "$ref": "#/definitions/WorkArtifactRefV1"
        ///            },
        ///            {
        ///              "type": "null"
        ///            }
        ///          ]
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "resumed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "restarted"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "recovery_required"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state")]
        pub enum WorkRecoveryStateV1 {
            #[serde(rename = "fresh")]
            Fresh,
            #[serde(rename = "resumed")]
            Resumed {
                #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
                checkpoint: ::std::option::Option<WorkArtifactRefV1>,
                source_attempt_id: AttemptId,
            },
            #[serde(rename = "restarted")]
            Restarted { reason: WorkRestartReasonV1, source_attempt_id: AttemptId },
            #[serde(rename = "recovery_required")]
            RecoveryRequired {
                reason: WorkRestartReasonV1,
                source_attempt_id: AttemptId,
            },
        }
        ///`WorkRestartReasonV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "lease_lost",
        ///    "provider_unavailable",
        ///    "process_lost",
        ///    "checkpoint_rejected"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkRestartReasonV1 {
            #[serde(rename = "lease_lost")]
            LeaseLost,
            #[serde(rename = "provider_unavailable")]
            ProviderUnavailable,
            #[serde(rename = "process_lost")]
            ProcessLost,
            #[serde(rename = "checkpoint_rejected")]
            CheckpointRejected,
        }
        impl ::std::fmt::Display for WorkRestartReasonV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::LeaseLost => f.write_str("lease_lost"),
                    Self::ProviderUnavailable => f.write_str("provider_unavailable"),
                    Self::ProcessLost => f.write_str("process_lost"),
                    Self::CheckpointRejected => f.write_str("checkpoint_rejected"),
                }
            }
        }
        impl ::std::str::FromStr for WorkRestartReasonV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "lease_lost" => Ok(Self::LeaseLost),
                    "provider_unavailable" => Ok(Self::ProviderUnavailable),
                    "process_lost" => Ok(Self::ProcessLost),
                    "checkpoint_rejected" => Ok(Self::CheckpointRejected),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        ///`WorkTerminalEvidenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "succeeded"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "failed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "cancelled"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "outcome")]
        pub enum WorkTerminalEvidenceV1 {
            #[serde(rename = "succeeded")]
            Succeeded { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "failed")]
            Failed { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "cancelled")]
            Cancelled { evidence_digest: ManifestDigest, observed_at: UtcMicros },
        }
    }
    pub type Request = request::WorkAttemptStartRequestV1;
    pub type Result = result::WorkAttemptResponseV1;
}
typed_operation!(
    WorkAttemptStart, work_attempt_start, "operation.work.attempt_start",
    "/application/work/attempt/start", "binding.http.work.attempt_start",
    "schema.work.attempt_start.result", 1
);
#[allow(clippy::all)]
pub mod work_attempt_terminalize {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `AttemptId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `AttemptId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct AttemptId(pub ::std::string::String);
        impl ::std::ops::Deref for AttemptId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<AttemptId> for ::std::string::String {
            fn from(value: AttemptId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for AttemptId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for AttemptId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for AttemptId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkAttemptIdentityV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "attempt_id",
        ///    "run_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "attempt_id": {
        ///      "$ref": "#/definitions/AttemptId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptIdentityV1 {
            pub attempt_id: AttemptId,
            pub run_id: RunId,
            pub task_id: TaskId,
        }
        ///`WorkAttemptTerminalizeRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkAttemptTerminalizeRequestV1",
        ///  "type": "object",
        ///  "required": [
        ///    "identity",
        ///    "lease",
        ///    "terminal"
        ///  ],
        ///  "properties": {
        ///    "identity": {
        ///      "$ref": "#/definitions/WorkAttemptIdentityV1"
        ///    },
        ///    "lease": {
        ///      "$ref": "#/definitions/WorkLeaseFenceV1"
        ///    },
        ///    "terminal": {
        ///      "$ref": "#/definitions/WorkTerminalEvidenceV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptTerminalizeRequestV1 {
            pub identity: WorkAttemptIdentityV1,
            pub lease: WorkLeaseFenceV1,
            pub terminal: WorkTerminalEvidenceV1,
        }
        ///`WorkFenceEpochV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkFenceEpochV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkFenceEpochV1(pub u64);
        impl ::std::ops::Deref for WorkFenceEpochV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkFenceEpochV1> for u64 {
            fn from(value: WorkFenceEpochV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkFenceEpochV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkFenceEpochV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkFenceEpochV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkLeaseFenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "epoch",
        ///    "lease_id"
        ///  ],
        ///  "properties": {
        ///    "epoch": {
        ///      "$ref": "#/definitions/WorkFenceEpochV1"
        ///    },
        ///    "lease_id": {
        ///      "$ref": "#/definitions/WorkLeaseId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkLeaseFenceV1 {
            pub epoch: WorkFenceEpochV1,
            pub lease_id: WorkLeaseId,
        }
        ///Strongly typed canonical identity: `WorkLeaseId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkLeaseId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkLeaseId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkLeaseId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkLeaseId> for ::std::string::String {
            fn from(value: WorkLeaseId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkLeaseId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkLeaseId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkLeaseId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkTerminalEvidenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "succeeded"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "failed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "cancelled"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "outcome")]
        pub enum WorkTerminalEvidenceV1 {
            #[serde(rename = "succeeded")]
            Succeeded { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "failed")]
            Failed { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "cancelled")]
            Cancelled { evidence_digest: ManifestDigest, observed_at: UtcMicros },
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `AttemptId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `AttemptId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct AttemptId(pub ::std::string::String);
        impl ::std::ops::Deref for AttemptId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<AttemptId> for ::std::string::String {
            fn from(value: AttemptId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for AttemptId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for AttemptId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for AttemptId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectionGenerationId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectionGenerationId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectionGenerationId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectionGenerationId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectionGenerationId> for ::std::string::String {
            fn from(value: ProjectionGenerationId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectionGenerationId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectionGenerationId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectionGenerationId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProviderId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProviderId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProviderId(pub ::std::string::String);
        impl ::std::ops::Deref for ProviderId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProviderId> for ::std::string::String {
            fn from(value: ProviderId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProviderId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProviderId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProviderId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkArtifactId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkArtifactId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkArtifactId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkArtifactId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkArtifactId> for ::std::string::String {
            fn from(value: WorkArtifactId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkArtifactId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkArtifactId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkArtifactId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkArtifactRefV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifact_id",
        ///    "byte_length",
        ///    "digest"
        ///  ],
        ///  "properties": {
        ///    "artifact_id": {
        ///      "$ref": "#/definitions/WorkArtifactId"
        ///    },
        ///    "byte_length": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkArtifactRefV1 {
            pub artifact_id: WorkArtifactId,
            pub byte_length: u64,
            pub digest: ManifestDigest,
        }
        ///`WorkAttemptIdentityV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "attempt_id",
        ///    "run_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "attempt_id": {
        ///      "$ref": "#/definitions/AttemptId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptIdentityV1 {
            pub attempt_id: AttemptId,
            pub run_id: RunId,
            pub task_id: TaskId,
        }
        ///`WorkAttemptProgressV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "completed",
        ///    "total"
        ///  ],
        ///  "properties": {
        ///    "completed": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "total": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProgressV1 {
            pub completed: u64,
            pub total: u64,
        }
        ///`WorkAttemptProjectionBindingV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "generation_id",
        ///    "sequence",
        ///    "work_version"
        ///  ],
        ///  "properties": {
        ///    "generation_id": {
        ///      "$ref": "#/definitions/ProjectionGenerationId"
        ///    },
        ///    "sequence": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    },
        ///    "work_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptProjectionBindingV1 {
            pub generation_id: ProjectionGenerationId,
            pub sequence: WorkProjectionSequenceV1,
            pub work_version: u64,
        }
        ///`WorkAttemptResponseV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkAttemptResponseV1",
        ///  "type": "object",
        ///  "required": [
        ///    "attempt"
        ///  ],
        ///  "properties": {
        ///    "attempt": {
        ///      "$ref": "#/definitions/WorkAttemptV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAttemptResponseV1 {
            pub attempt: WorkAttemptV1,
        }
        ///`WorkAttemptStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "leased",
        ///    "running",
        ///    "cancellation_requested",
        ///    "cancellation_acknowledged",
        ///    "cancellation_escalated",
        ///    "recovery_required",
        ///    "succeeded",
        ///    "failed",
        ///    "cancelled"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkAttemptStateV1 {
            #[serde(rename = "leased")]
            Leased,
            #[serde(rename = "running")]
            Running,
            #[serde(rename = "cancellation_requested")]
            CancellationRequested,
            #[serde(rename = "cancellation_acknowledged")]
            CancellationAcknowledged,
            #[serde(rename = "cancellation_escalated")]
            CancellationEscalated,
            #[serde(rename = "recovery_required")]
            RecoveryRequired,
            #[serde(rename = "succeeded")]
            Succeeded,
            #[serde(rename = "failed")]
            Failed,
            #[serde(rename = "cancelled")]
            Cancelled,
        }
        impl ::std::fmt::Display for WorkAttemptStateV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::Leased => f.write_str("leased"),
                    Self::Running => f.write_str("running"),
                    Self::CancellationRequested => f.write_str("cancellation_requested"),
                    Self::CancellationAcknowledged => {
                        f.write_str("cancellation_acknowledged")
                    }
                    Self::CancellationEscalated => f.write_str("cancellation_escalated"),
                    Self::RecoveryRequired => f.write_str("recovery_required"),
                    Self::Succeeded => f.write_str("succeeded"),
                    Self::Failed => f.write_str("failed"),
                    Self::Cancelled => f.write_str("cancelled"),
                }
            }
        }
        impl ::std::str::FromStr for WorkAttemptStateV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "leased" => Ok(Self::Leased),
                    "running" => Ok(Self::Running),
                    "cancellation_requested" => Ok(Self::CancellationRequested),
                    "cancellation_acknowledged" => Ok(Self::CancellationAcknowledged),
                    "cancellation_escalated" => Ok(Self::CancellationEscalated),
                    "recovery_required" => Ok(Self::RecoveryRequired),
                    "succeeded" => Ok(Self::Succeeded),
                    "failed" => Ok(Self::Failed),
                    "cancelled" => Ok(Self::Cancelled),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkAttemptStateV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        ///`WorkAttemptV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "artifacts",
        ///    "cancellation",
        ///    "identity",
        ///    "lease",
        ///    "projection_binding",
        ///    "recovery",
        ///    "requested_route",
        ///    "state"
        ///  ],
        ///  "properties": {
        ///    "actual_route": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkProviderRouteV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "artifacts": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkArtifactRefV1"
        ///      }
        ///    },
        ///    "cancellation": {
        ///      "$ref": "#/definitions/WorkCancellationStateV1"
        ///    },
        ///    "identity": {
        ///      "$ref": "#/definitions/WorkAttemptIdentityV1"
        ///    },
        ///    "lease": {
        ///      "$ref": "#/definitions/WorkLeaseFenceV1"
        ///    },
        ///    "progress": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkAttemptProgressV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "projection_binding": {
        ///      "$ref": "#/definitions/WorkAttemptProjectionBindingV1"
        ///    },
        ///    "recovery": {
        ///      "$ref": "#/definitions/WorkRecoveryStateV1"
        ///    },
        ///    "requested_route": {
        ///      "$ref": "#/definitions/WorkProviderRouteV1"
        ///    },
        ///    "state": {
        ///      "$ref": "#/definitions/WorkAttemptStateV1"
        ///    },
        ///    "terminal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkTerminalEvidenceV1"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkAttemptV1 {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub actual_route: ::std::option::Option<WorkProviderRouteV1>,
            pub artifacts: ::std::vec::Vec<WorkArtifactRefV1>,
            pub cancellation: WorkCancellationStateV1,
            pub identity: WorkAttemptIdentityV1,
            pub lease: WorkLeaseFenceV1,
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub progress: ::std::option::Option<WorkAttemptProgressV1>,
            pub projection_binding: WorkAttemptProjectionBindingV1,
            pub recovery: WorkRecoveryStateV1,
            pub requested_route: WorkProviderRouteV1,
            pub state: WorkAttemptStateV1,
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub terminal: ::std::option::Option<WorkTerminalEvidenceV1>,
        }
        ///`WorkCancellationAcknowledgementV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "acknowledged_at",
        ///    "request"
        ///  ],
        ///  "properties": {
        ///    "acknowledged_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    },
        ///    "request": {
        ///      "$ref": "#/definitions/WorkCancellationRequestV1"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationAcknowledgementV1 {
            pub acknowledged_at: UtcMicros,
            pub request: WorkCancellationRequestV1,
        }
        ///`WorkCancellationEscalationV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "acknowledgement",
        ///    "escalated_at"
        ///  ],
        ///  "properties": {
        ///    "acknowledgement": {
        ///      "$ref": "#/definitions/WorkCancellationAcknowledgementV1"
        ///    },
        ///    "escalated_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationEscalationV1 {
            pub acknowledgement: WorkCancellationAcknowledgementV1,
            pub escalated_at: UtcMicros,
        }
        ///Strongly typed canonical identity: `WorkCancellationRequestId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkCancellationRequestId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkCancellationRequestId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkCancellationRequestId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkCancellationRequestId> for ::std::string::String {
            fn from(value: WorkCancellationRequestId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkCancellationRequestId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkCancellationRequestId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkCancellationRequestId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkCancellationRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "request_id",
        ///    "requested_at"
        ///  ],
        ///  "properties": {
        ///    "request_id": {
        ///      "$ref": "#/definitions/WorkCancellationRequestId"
        ///    },
        ///    "requested_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkCancellationRequestV1 {
            pub request_id: WorkCancellationRequestId,
            pub requested_at: UtcMicros,
        }
        ///`WorkCancellationStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "none"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "requested"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationRequestV1"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "acknowledged"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationAcknowledgementV1"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state",
        ///        "value"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "escalated"
        ///        },
        ///        "value": {
        ///          "$ref": "#/definitions/WorkCancellationEscalationV1"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state", content = "value")]
        pub enum WorkCancellationStateV1 {
            #[serde(rename = "none")]
            None,
            #[serde(rename = "requested")]
            Requested(WorkCancellationRequestV1),
            #[serde(rename = "acknowledged")]
            Acknowledged(WorkCancellationAcknowledgementV1),
            #[serde(rename = "escalated")]
            Escalated(WorkCancellationEscalationV1),
        }
        impl ::std::convert::From<WorkCancellationRequestV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationRequestV1) -> Self {
                Self::Requested(value)
            }
        }
        impl ::std::convert::From<WorkCancellationAcknowledgementV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationAcknowledgementV1) -> Self {
                Self::Acknowledged(value)
            }
        }
        impl ::std::convert::From<WorkCancellationEscalationV1>
        for WorkCancellationStateV1 {
            fn from(value: WorkCancellationEscalationV1) -> Self {
                Self::Escalated(value)
            }
        }
        ///`WorkFenceEpochV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkFenceEpochV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkFenceEpochV1(pub u64);
        impl ::std::ops::Deref for WorkFenceEpochV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkFenceEpochV1> for u64 {
            fn from(value: WorkFenceEpochV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkFenceEpochV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkFenceEpochV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkFenceEpochV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkFenceEpochV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkLeaseFenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "epoch",
        ///    "lease_id"
        ///  ],
        ///  "properties": {
        ///    "epoch": {
        ///      "$ref": "#/definitions/WorkFenceEpochV1"
        ///    },
        ///    "lease_id": {
        ///      "$ref": "#/definitions/WorkLeaseId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkLeaseFenceV1 {
            pub epoch: WorkFenceEpochV1,
            pub lease_id: WorkLeaseId,
        }
        ///Strongly typed canonical identity: `WorkLeaseId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkLeaseId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkLeaseId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkLeaseId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkLeaseId> for ::std::string::String {
            fn from(value: WorkLeaseId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkLeaseId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkLeaseId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkLeaseId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProjectionSequenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjectionSequenceV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkProjectionSequenceV1(pub u64);
        impl ::std::ops::Deref for WorkProjectionSequenceV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProjectionSequenceV1> for u64 {
            fn from(value: WorkProjectionSequenceV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkProjectionSequenceV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProjectionSequenceV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkProjectionSequenceV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkProviderRouteId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkProviderRouteId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkProviderRouteId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkProviderRouteId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProviderRouteId> for ::std::string::String {
            fn from(value: WorkProviderRouteId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkProviderRouteId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProviderRouteId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkProviderRouteId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProviderRouteV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "provider_id",
        ///    "route_id"
        ///  ],
        ///  "properties": {
        ///    "provider_id": {
        ///      "$ref": "#/definitions/ProviderId"
        ///    },
        ///    "route_id": {
        ///      "$ref": "#/definitions/WorkProviderRouteId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProviderRouteV1 {
            pub provider_id: ProviderId,
            pub route_id: WorkProviderRouteId,
        }
        ///`WorkRecoveryStateV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "state": {
        ///          "type": "string",
        ///          "const": "fresh"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "checkpoint": {
        ///          "anyOf": [
        ///            {
        ///              "$ref": "#/definitions/WorkArtifactRefV1"
        ///            },
        ///            {
        ///              "type": "null"
        ///            }
        ///          ]
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "resumed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "restarted"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "reason",
        ///        "source_attempt_id",
        ///        "state"
        ///      ],
        ///      "properties": {
        ///        "reason": {
        ///          "$ref": "#/definitions/WorkRestartReasonV1"
        ///        },
        ///        "source_attempt_id": {
        ///          "$ref": "#/definitions/AttemptId"
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "recovery_required"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state")]
        pub enum WorkRecoveryStateV1 {
            #[serde(rename = "fresh")]
            Fresh,
            #[serde(rename = "resumed")]
            Resumed {
                #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
                checkpoint: ::std::option::Option<WorkArtifactRefV1>,
                source_attempt_id: AttemptId,
            },
            #[serde(rename = "restarted")]
            Restarted { reason: WorkRestartReasonV1, source_attempt_id: AttemptId },
            #[serde(rename = "recovery_required")]
            RecoveryRequired {
                reason: WorkRestartReasonV1,
                source_attempt_id: AttemptId,
            },
        }
        ///`WorkRestartReasonV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "string",
        ///  "enum": [
        ///    "lease_lost",
        ///    "provider_unavailable",
        ///    "process_lost",
        ///    "checkpoint_rejected"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum WorkRestartReasonV1 {
            #[serde(rename = "lease_lost")]
            LeaseLost,
            #[serde(rename = "provider_unavailable")]
            ProviderUnavailable,
            #[serde(rename = "process_lost")]
            ProcessLost,
            #[serde(rename = "checkpoint_rejected")]
            CheckpointRejected,
        }
        impl ::std::fmt::Display for WorkRestartReasonV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::LeaseLost => f.write_str("lease_lost"),
                    Self::ProviderUnavailable => f.write_str("provider_unavailable"),
                    Self::ProcessLost => f.write_str("process_lost"),
                    Self::CheckpointRejected => f.write_str("checkpoint_rejected"),
                }
            }
        }
        impl ::std::str::FromStr for WorkRestartReasonV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "lease_lost" => Ok(Self::LeaseLost),
                    "provider_unavailable" => Ok(Self::ProviderUnavailable),
                    "process_lost" => Ok(Self::ProcessLost),
                    "checkpoint_rejected" => Ok(Self::CheckpointRejected),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String> for WorkRestartReasonV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        ///`WorkTerminalEvidenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "succeeded"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "failed"
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "evidence_digest",
        ///        "observed_at",
        ///        "outcome"
        ///      ],
        ///      "properties": {
        ///        "evidence_digest": {
        ///          "$ref": "#/definitions/ManifestDigest"
        ///        },
        ///        "observed_at": {
        ///          "$ref": "#/definitions/UtcMicros"
        ///        },
        ///        "outcome": {
        ///          "type": "string",
        ///          "const": "cancelled"
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "outcome")]
        pub enum WorkTerminalEvidenceV1 {
            #[serde(rename = "succeeded")]
            Succeeded { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "failed")]
            Failed { evidence_digest: ManifestDigest, observed_at: UtcMicros },
            #[serde(rename = "cancelled")]
            Cancelled { evidence_digest: ManifestDigest, observed_at: UtcMicros },
        }
    }
    pub type Request = request::WorkAttemptTerminalizeRequestV1;
    pub type Result = result::WorkAttemptResponseV1;
}
typed_operation!(
    WorkAttemptTerminalize, work_attempt_terminalize,
    "operation.work.attempt_terminalize", "/application/work/attempt/terminalize",
    "binding.http.work.attempt_terminalize", "schema.work.attempt_terminalize.result", 1
);
#[allow(clippy::all)]
pub mod work_create {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///`CreateWorkCommand`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "CreateWorkCommand",
        ///  "type": "object",
        ///  "required": [
        ///    "command_id",
        ///    "occurred_at",
        ///    "task_id",
        ///    "title"
        ///  ],
        ///  "properties": {
        ///    "command_id": {
        ///      "$ref": "#/definitions/WorkCommandId"
        ///    },
        ///    "dependencies": {
        ///      "default": [],
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/TaskId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "occurred_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    },
        ///    "title": {
        ///      "type": "string"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct CreateWorkCommand {
            pub command_id: WorkCommandId,
            #[serde(default = "defaults::create_work_command_dependencies")]
            pub dependencies: Vec<TaskId>,
            pub occurred_at: UtcMicros,
            pub task_id: TaskId,
            pub title: ::std::string::String,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkCommandId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkCommandId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkCommandId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkCommandId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkCommandId> for ::std::string::String {
            fn from(value: WorkCommandId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkCommandId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkCommandId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkCommandId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        /// Generation of default values for serde.
        pub mod defaults {
            pub(super) fn create_work_command_dependencies() -> Vec<super::TaskId> {
                vec![]
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `ActorId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ActorId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ActorId(pub ::std::string::String);
        impl ::std::ops::Deref for ActorId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ActorId> for ::std::string::String {
            fn from(value: ActorId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ActorId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ActorId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ActorId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProposalId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProposalId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProposalId(pub ::std::string::String);
        impl ::std::ops::Deref for ProposalId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProposalId> for ::std::string::String {
            fn from(value: ProposalId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProposalId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProposalId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProposalId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RepositoryId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RepositoryId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RepositoryId(pub ::std::string::String);
        impl ::std::ops::Deref for RepositoryId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RepositoryId> for ::std::string::String {
            fn from(value: RepositoryId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RepositoryId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RepositoryId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RepositoryId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`RuntimeEvidenceRef`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "evidence_digest",
        ///    "run_id",
        ///    "terminal"
        ///  ],
        ///  "properties": {
        ///    "evidence_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "terminal": {
        ///      "type": "boolean"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct RuntimeEvidenceRef {
            pub evidence_digest: ManifestDigest,
            pub run_id: RunId,
            pub terminal: bool,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkAuthority`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "actor_id",
        ///    "policy_digest",
        ///    "project_id",
        ///    "repository_id",
        ///    "worktree_id"
        ///  ],
        ///  "properties": {
        ///    "actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "policy_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "repository_id": {
        ///      "$ref": "#/definitions/RepositoryId"
        ///    },
        ///    "worktree_id": {
        ///      "$ref": "#/definitions/WorktreeId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAuthority {
            pub actor_id: ActorId,
            pub policy_digest: ManifestDigest,
            pub project_id: ProjectId,
            pub repository_id: RepositoryId,
            pub worktree_id: WorktreeId,
        }
        ///`WorkProjection`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjection",
        ///  "type": "object",
        ///  "required": [
        ///    "authority",
        ///    "dependencies",
        ///    "execution_admitted",
        ///    "history_len",
        ///    "runtime_evidence",
        ///    "task_accepted",
        ///    "task_id",
        ///    "title",
        ///    "version"
        ///  ],
        ///  "properties": {
        ///    "accepted_proposal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/ProposalId"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "authority": {
        ///      "$ref": "#/definitions/WorkAuthority"
        ///    },
        ///    "dependencies": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/TaskId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "execution_admitted": {
        ///      "type": "boolean"
        ///    },
        ///    "history_len": {
        ///      "type": "integer",
        ///      "format": "uint",
        ///      "minimum": 0.0
        ///    },
        ///    "runtime_evidence": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/RuntimeEvidenceRef"
        ///      }
        ///    },
        ///    "task_accepted": {
        ///      "type": "boolean"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    },
        ///    "title": {
        ///      "type": "string"
        ///    },
        ///    "version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProjection {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub accepted_proposal: ::std::option::Option<ProposalId>,
            pub authority: WorkAuthority,
            pub dependencies: Vec<TaskId>,
            pub execution_admitted: bool,
            pub history_len: u32,
            pub runtime_evidence: ::std::vec::Vec<RuntimeEvidenceRef>,
            pub task_accepted: bool,
            pub task_id: TaskId,
            pub title: ::std::string::String,
            pub version: u64,
        }
        ///Strongly typed canonical identity: `WorktreeId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorktreeId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorktreeId(pub ::std::string::String);
        impl ::std::ops::Deref for WorktreeId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorktreeId> for ::std::string::String {
            fn from(value: WorktreeId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorktreeId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorktreeId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorktreeId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub type Request = request::CreateWorkCommand;
    pub type Result = result::WorkProjection;
}
typed_operation!(
    WorkCreate, work_create, "operation.work.create", "/application/work/create",
    "binding.http.work.create", "schema.work.create.result", 1
);
#[allow(clippy::all)]
pub mod work_delta {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `ProjectionGenerationId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectionGenerationId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectionGenerationId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectionGenerationId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectionGenerationId> for ::std::string::String {
            fn from(value: ProjectionGenerationId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectionGenerationId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectionGenerationId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectionGenerationId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProjectionDeltaRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjectionDeltaRequestV1",
        ///  "type": "object",
        ///  "required": [
        ///    "cursor",
        ///    "page_size"
        ///  ],
        ///  "properties": {
        ///    "cursor": {
        ///      "$ref": "#/definitions/WorkProjectionResumeCursorV1"
        ///    },
        ///    "page_size": {
        ///      "type": "integer",
        ///      "format": "uint32",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProjectionDeltaRequestV1 {
            pub cursor: WorkProjectionResumeCursorV1,
            pub page_size: u32,
        }
        ///`WorkProjectionResumeCursorV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "generation_id",
        ///    "token"
        ///  ],
        ///  "properties": {
        ///    "generation_id": {
        ///      "$ref": "#/definitions/ProjectionGenerationId"
        ///    },
        ///    "token": {
        ///      "type": "string"
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkProjectionResumeCursorV1 {
            pub generation_id: ProjectionGenerationId,
            pub token: ::std::string::String,
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `ActorId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ActorId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ActorId(pub ::std::string::String);
        impl ::std::ops::Deref for ActorId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ActorId> for ::std::string::String {
            fn from(value: ActorId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ActorId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ActorId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ActorId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectionGenerationId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectionGenerationId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectionGenerationId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectionGenerationId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectionGenerationId> for ::std::string::String {
            fn from(value: ProjectionGenerationId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectionGenerationId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectionGenerationId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectionGenerationId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProposalId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProposalId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProposalId(pub ::std::string::String);
        impl ::std::ops::Deref for ProposalId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProposalId> for ::std::string::String {
            fn from(value: ProposalId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProposalId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProposalId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProposalId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RepositoryId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RepositoryId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RepositoryId(pub ::std::string::String);
        impl ::std::ops::Deref for RepositoryId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RepositoryId> for ::std::string::String {
            fn from(value: RepositoryId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RepositoryId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RepositoryId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RepositoryId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`RuntimeEvidenceRef`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "evidence_digest",
        ///    "run_id",
        ///    "terminal"
        ///  ],
        ///  "properties": {
        ///    "evidence_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "terminal": {
        ///      "type": "boolean"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct RuntimeEvidenceRef {
            pub evidence_digest: ManifestDigest,
            pub run_id: RunId,
            pub terminal: bool,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkAuthority`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "actor_id",
        ///    "policy_digest",
        ///    "project_id",
        ///    "repository_id",
        ///    "worktree_id"
        ///  ],
        ///  "properties": {
        ///    "actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "policy_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "repository_id": {
        ///      "$ref": "#/definitions/RepositoryId"
        ///    },
        ///    "worktree_id": {
        ///      "$ref": "#/definitions/WorktreeId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAuthority {
            pub actor_id: ActorId,
            pub policy_digest: ManifestDigest,
            pub project_id: ProjectId,
            pub repository_id: RepositoryId,
            pub worktree_id: WorktreeId,
        }
        ///`WorkProjection`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "authority",
        ///    "dependencies",
        ///    "execution_admitted",
        ///    "history_len",
        ///    "runtime_evidence",
        ///    "task_accepted",
        ///    "task_id",
        ///    "title",
        ///    "version"
        ///  ],
        ///  "properties": {
        ///    "accepted_proposal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/ProposalId"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "authority": {
        ///      "$ref": "#/definitions/WorkAuthority"
        ///    },
        ///    "dependencies": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/TaskId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "execution_admitted": {
        ///      "type": "boolean"
        ///    },
        ///    "history_len": {
        ///      "type": "integer",
        ///      "format": "uint",
        ///      "minimum": 0.0
        ///    },
        ///    "runtime_evidence": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/RuntimeEvidenceRef"
        ///      }
        ///    },
        ///    "task_accepted": {
        ///      "type": "boolean"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    },
        ///    "title": {
        ///      "type": "string"
        ///    },
        ///    "version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProjection {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub accepted_proposal: ::std::option::Option<ProposalId>,
            pub authority: WorkAuthority,
            pub dependencies: Vec<TaskId>,
            pub execution_admitted: bool,
            pub history_len: u32,
            pub runtime_evidence: ::std::vec::Vec<RuntimeEvidenceRef>,
            pub task_accepted: bool,
            pub task_id: TaskId,
            pub title: ::std::string::String,
            pub version: u64,
        }
        ///`WorkProjectionCoverageV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "returned",
        ///        "state",
        ///        "total"
        ///      ],
        ///      "properties": {
        ///        "returned": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "complete"
        ///        },
        ///        "total": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "cursor",
        ///        "range",
        ///        "returned",
        ///        "state",
        ///        "total"
        ///      ],
        ///      "properties": {
        ///        "cursor": {
        ///          "$ref": "#/definitions/WorkProjectionResumeCursorV1"
        ///        },
        ///        "range": {
        ///          "$ref": "#/definitions/WorkProjectionSequenceRangeV1"
        ///        },
        ///        "returned": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "partial"
        ///        },
        ///        "total": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "cap",
        ///        "cursor",
        ///        "range",
        ///        "returned",
        ///        "state",
        ///        "total"
        ///      ],
        ///      "properties": {
        ///        "cap": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        },
        ///        "cursor": {
        ///          "$ref": "#/definitions/WorkProjectionResumeCursorV1"
        ///        },
        ///        "range": {
        ///          "$ref": "#/definitions/WorkProjectionSequenceRangeV1"
        ///        },
        ///        "returned": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "capped"
        ///        },
        ///        "total": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state")]
        pub enum WorkProjectionCoverageV1 {
            #[serde(rename = "complete")]
            Complete { returned: u32, total: u32 },
            #[serde(rename = "partial")]
            Partial {
                cursor: WorkProjectionResumeCursorV1,
                range: WorkProjectionSequenceRangeV1,
                returned: u32,
                total: u32,
            },
            #[serde(rename = "capped")]
            Capped {
                cap: u32,
                cursor: WorkProjectionResumeCursorV1,
                range: WorkProjectionSequenceRangeV1,
                returned: u32,
                total: u32,
            },
        }
        ///`WorkProjectionDeltaV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjectionDeltaV1",
        ///  "type": "object",
        ///  "required": [
        ///    "changed",
        ///    "coverage",
        ///    "from_sequence",
        ///    "generation_id",
        ///    "removed",
        ///    "to_sequence"
        ///  ],
        ///  "properties": {
        ///    "changed": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkProjection"
        ///      }
        ///    },
        ///    "coverage": {
        ///      "$ref": "#/definitions/WorkProjectionCoverageV1"
        ///    },
        ///    "from_sequence": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    },
        ///    "generation_id": {
        ///      "$ref": "#/definitions/ProjectionGenerationId"
        ///    },
        ///    "removed": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/TaskId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "to_sequence": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkProjectionDeltaV1 {
            pub changed: ::std::vec::Vec<WorkProjection>,
            pub coverage: WorkProjectionCoverageV1,
            pub from_sequence: WorkProjectionSequenceV1,
            pub generation_id: ProjectionGenerationId,
            pub removed: Vec<TaskId>,
            pub to_sequence: WorkProjectionSequenceV1,
        }
        ///`WorkProjectionResumeCursorV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "generation_id",
        ///    "token"
        ///  ],
        ///  "properties": {
        ///    "generation_id": {
        ///      "$ref": "#/definitions/ProjectionGenerationId"
        ///    },
        ///    "token": {
        ///      "type": "string"
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkProjectionResumeCursorV1 {
            pub generation_id: ProjectionGenerationId,
            pub token: ::std::string::String,
        }
        ///`WorkProjectionSequenceRangeV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "end_inclusive",
        ///    "start_exclusive"
        ///  ],
        ///  "properties": {
        ///    "end_inclusive": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    },
        ///    "start_exclusive": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkProjectionSequenceRangeV1 {
            pub end_inclusive: WorkProjectionSequenceV1,
            pub start_exclusive: WorkProjectionSequenceV1,
        }
        ///`WorkProjectionSequenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjectionSequenceV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkProjectionSequenceV1(pub u64);
        impl ::std::ops::Deref for WorkProjectionSequenceV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProjectionSequenceV1> for u64 {
            fn from(value: WorkProjectionSequenceV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkProjectionSequenceV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProjectionSequenceV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkProjectionSequenceV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorktreeId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorktreeId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorktreeId(pub ::std::string::String);
        impl ::std::ops::Deref for WorktreeId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorktreeId> for ::std::string::String {
            fn from(value: WorktreeId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorktreeId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorktreeId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorktreeId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub type Request = request::WorkProjectionDeltaRequestV1;
    pub type Result = result::WorkProjectionDeltaV1;
}
typed_operation!(
    WorkDelta, work_delta, "operation.work.delta", "/application/work/delta",
    "binding.http.work.delta", "schema.work.delta.result", 1
);
#[allow(clippy::all)]
pub mod work_replan_dependencies {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///`ReplanDependenciesCommand`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "ReplanDependenciesCommand",
        ///  "type": "object",
        ///  "required": [
        ///    "command_id",
        ///    "expected_version",
        ///    "occurred_at",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "command_id": {
        ///      "$ref": "#/definitions/WorkCommandId"
        ///    },
        ///    "dependencies": {
        ///      "default": [],
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/TaskId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "expected_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "occurred_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct ReplanDependenciesCommand {
            pub command_id: WorkCommandId,
            #[serde(default = "defaults::replan_dependencies_command_dependencies")]
            pub dependencies: Vec<TaskId>,
            pub expected_version: u64,
            pub occurred_at: UtcMicros,
            pub task_id: TaskId,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkCommandId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkCommandId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkCommandId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkCommandId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkCommandId> for ::std::string::String {
            fn from(value: WorkCommandId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkCommandId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkCommandId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkCommandId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        /// Generation of default values for serde.
        pub mod defaults {
            pub(super) fn replan_dependencies_command_dependencies() -> Vec<
                super::TaskId,
            > {
                vec![]
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `ActorId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ActorId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ActorId(pub ::std::string::String);
        impl ::std::ops::Deref for ActorId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ActorId> for ::std::string::String {
            fn from(value: ActorId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ActorId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ActorId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ActorId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProposalId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProposalId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProposalId(pub ::std::string::String);
        impl ::std::ops::Deref for ProposalId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProposalId> for ::std::string::String {
            fn from(value: ProposalId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProposalId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProposalId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProposalId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RepositoryId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RepositoryId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RepositoryId(pub ::std::string::String);
        impl ::std::ops::Deref for RepositoryId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RepositoryId> for ::std::string::String {
            fn from(value: RepositoryId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RepositoryId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RepositoryId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RepositoryId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`RuntimeEvidenceRef`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "evidence_digest",
        ///    "run_id",
        ///    "terminal"
        ///  ],
        ///  "properties": {
        ///    "evidence_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "terminal": {
        ///      "type": "boolean"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct RuntimeEvidenceRef {
            pub evidence_digest: ManifestDigest,
            pub run_id: RunId,
            pub terminal: bool,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkAuthority`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "actor_id",
        ///    "policy_digest",
        ///    "project_id",
        ///    "repository_id",
        ///    "worktree_id"
        ///  ],
        ///  "properties": {
        ///    "actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "policy_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "repository_id": {
        ///      "$ref": "#/definitions/RepositoryId"
        ///    },
        ///    "worktree_id": {
        ///      "$ref": "#/definitions/WorktreeId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAuthority {
            pub actor_id: ActorId,
            pub policy_digest: ManifestDigest,
            pub project_id: ProjectId,
            pub repository_id: RepositoryId,
            pub worktree_id: WorktreeId,
        }
        ///`WorkProjection`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjection",
        ///  "type": "object",
        ///  "required": [
        ///    "authority",
        ///    "dependencies",
        ///    "execution_admitted",
        ///    "history_len",
        ///    "runtime_evidence",
        ///    "task_accepted",
        ///    "task_id",
        ///    "title",
        ///    "version"
        ///  ],
        ///  "properties": {
        ///    "accepted_proposal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/ProposalId"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "authority": {
        ///      "$ref": "#/definitions/WorkAuthority"
        ///    },
        ///    "dependencies": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/TaskId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "execution_admitted": {
        ///      "type": "boolean"
        ///    },
        ///    "history_len": {
        ///      "type": "integer",
        ///      "format": "uint",
        ///      "minimum": 0.0
        ///    },
        ///    "runtime_evidence": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/RuntimeEvidenceRef"
        ///      }
        ///    },
        ///    "task_accepted": {
        ///      "type": "boolean"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    },
        ///    "title": {
        ///      "type": "string"
        ///    },
        ///    "version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProjection {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub accepted_proposal: ::std::option::Option<ProposalId>,
            pub authority: WorkAuthority,
            pub dependencies: Vec<TaskId>,
            pub execution_admitted: bool,
            pub history_len: u32,
            pub runtime_evidence: ::std::vec::Vec<RuntimeEvidenceRef>,
            pub task_accepted: bool,
            pub task_id: TaskId,
            pub title: ::std::string::String,
            pub version: u64,
        }
        ///Strongly typed canonical identity: `WorktreeId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorktreeId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorktreeId(pub ::std::string::String);
        impl ::std::ops::Deref for WorktreeId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorktreeId> for ::std::string::String {
            fn from(value: WorktreeId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorktreeId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorktreeId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorktreeId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub type Request = request::ReplanDependenciesCommand;
    pub type Result = result::WorkProjection;
}
typed_operation!(
    WorkReplanDependencies, work_replan_dependencies,
    "operation.work.replan_dependencies", "/application/work/replan-dependencies",
    "binding.http.work.replan_dependencies", "schema.work.replan_dependencies.result", 1
);
#[allow(clippy::all)]
pub mod work_review_proposal {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProposalId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProposalId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProposalId(pub ::std::string::String);
        impl ::std::ops::Deref for ProposalId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProposalId> for ::std::string::String {
            fn from(value: ProposalId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProposalId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProposalId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProposalId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`ReviewProposalCommand`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "command_id",
        ///    "expected_version",
        ///    "occurred_at",
        ///    "proposal_digest",
        ///    "proposal_id",
        ///    "task_id"
        ///  ],
        ///  "properties": {
        ///    "command_id": {
        ///      "$ref": "#/definitions/WorkCommandId"
        ///    },
        ///    "expected_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "occurred_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    },
        ///    "proposal_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "proposal_id": {
        ///      "$ref": "#/definitions/ProposalId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct ReviewProposalCommand {
            pub command_id: WorkCommandId,
            pub expected_version: u64,
            pub occurred_at: UtcMicros,
            pub proposal_digest: ManifestDigest,
            pub proposal_id: ProposalId,
            pub task_id: TaskId,
        }
        /**A proposal review records a non-accepting disposition. Acceptance remains a
separate command so callers cannot accidentally collapse review into
approval.*/
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "A proposal review records a non-accepting disposition. Acceptance remains a\nseparate command so callers cannot accidentally collapse review into\napproval.",
        ///  "type": "string",
        ///  "enum": [
        ///    "rejected",
        ///    "superseded"
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        pub enum ReviewProposalDispositionV1 {
            #[serde(rename = "rejected")]
            Rejected,
            #[serde(rename = "superseded")]
            Superseded,
        }
        impl ::std::fmt::Display for ReviewProposalDispositionV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match *self {
                    Self::Rejected => f.write_str("rejected"),
                    Self::Superseded => f.write_str("superseded"),
                }
            }
        }
        impl ::std::str::FromStr for ReviewProposalDispositionV1 {
            type Err = self::error::ConversionError;
            fn from_str(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                match value {
                    "rejected" => Ok(Self::Rejected),
                    "superseded" => Ok(Self::Superseded),
                    _ => Err("invalid value".into()),
                }
            }
        }
        impl ::std::convert::TryFrom<&str> for ReviewProposalDispositionV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &str,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<&::std::string::String>
        for ReviewProposalDispositionV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: &::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<::std::string::String>
        for ReviewProposalDispositionV1 {
            type Error = self::error::ConversionError;
            fn try_from(
                value: ::std::string::String,
            ) -> ::std::result::Result<Self, self::error::ConversionError> {
                value.parse()
            }
        }
        ///`ReviewProposalRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "ReviewProposalRequestV1",
        ///  "type": "object",
        ///  "required": [
        ///    "disposition",
        ///    "review"
        ///  ],
        ///  "properties": {
        ///    "disposition": {
        ///      "$ref": "#/definitions/ReviewProposalDispositionV1"
        ///    },
        ///    "review": {
        ///      "$ref": "#/definitions/ReviewProposalCommand"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct ReviewProposalRequestV1 {
            pub disposition: ReviewProposalDispositionV1,
            pub review: ReviewProposalCommand,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkCommandId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkCommandId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorkCommandId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkCommandId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkCommandId> for ::std::string::String {
            fn from(value: WorkCommandId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkCommandId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkCommandId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkCommandId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `ActorId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ActorId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ActorId(pub ::std::string::String);
        impl ::std::ops::Deref for ActorId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ActorId> for ::std::string::String {
            fn from(value: ActorId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ActorId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ActorId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ActorId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProposalId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProposalId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProposalId(pub ::std::string::String);
        impl ::std::ops::Deref for ProposalId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProposalId> for ::std::string::String {
            fn from(value: ProposalId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProposalId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProposalId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProposalId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RepositoryId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RepositoryId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RepositoryId(pub ::std::string::String);
        impl ::std::ops::Deref for RepositoryId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RepositoryId> for ::std::string::String {
            fn from(value: RepositoryId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RepositoryId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RepositoryId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RepositoryId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`RuntimeEvidenceRef`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "evidence_digest",
        ///    "run_id",
        ///    "terminal"
        ///  ],
        ///  "properties": {
        ///    "evidence_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "terminal": {
        ///      "type": "boolean"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct RuntimeEvidenceRef {
            pub evidence_digest: ManifestDigest,
            pub run_id: RunId,
            pub terminal: bool,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkAuthority`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "actor_id",
        ///    "policy_digest",
        ///    "project_id",
        ///    "repository_id",
        ///    "worktree_id"
        ///  ],
        ///  "properties": {
        ///    "actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "policy_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "repository_id": {
        ///      "$ref": "#/definitions/RepositoryId"
        ///    },
        ///    "worktree_id": {
        ///      "$ref": "#/definitions/WorktreeId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAuthority {
            pub actor_id: ActorId,
            pub policy_digest: ManifestDigest,
            pub project_id: ProjectId,
            pub repository_id: RepositoryId,
            pub worktree_id: WorktreeId,
        }
        ///`WorkProjection`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjection",
        ///  "type": "object",
        ///  "required": [
        ///    "authority",
        ///    "dependencies",
        ///    "execution_admitted",
        ///    "history_len",
        ///    "runtime_evidence",
        ///    "task_accepted",
        ///    "task_id",
        ///    "title",
        ///    "version"
        ///  ],
        ///  "properties": {
        ///    "accepted_proposal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/ProposalId"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "authority": {
        ///      "$ref": "#/definitions/WorkAuthority"
        ///    },
        ///    "dependencies": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/TaskId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "execution_admitted": {
        ///      "type": "boolean"
        ///    },
        ///    "history_len": {
        ///      "type": "integer",
        ///      "format": "uint",
        ///      "minimum": 0.0
        ///    },
        ///    "runtime_evidence": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/RuntimeEvidenceRef"
        ///      }
        ///    },
        ///    "task_accepted": {
        ///      "type": "boolean"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    },
        ///    "title": {
        ///      "type": "string"
        ///    },
        ///    "version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProjection {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub accepted_proposal: ::std::option::Option<ProposalId>,
            pub authority: WorkAuthority,
            pub dependencies: Vec<TaskId>,
            pub execution_admitted: bool,
            pub history_len: u32,
            pub runtime_evidence: ::std::vec::Vec<RuntimeEvidenceRef>,
            pub task_accepted: bool,
            pub task_id: TaskId,
            pub title: ::std::string::String,
            pub version: u64,
        }
        ///Strongly typed canonical identity: `WorktreeId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorktreeId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorktreeId(pub ::std::string::String);
        impl ::std::ops::Deref for WorktreeId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorktreeId> for ::std::string::String {
            fn from(value: WorktreeId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorktreeId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorktreeId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorktreeId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub type Request = request::ReviewProposalRequestV1;
    pub type Result = result::WorkProjection;
}
typed_operation!(
    WorkReviewProposal, work_review_proposal, "operation.work.review_proposal",
    "/application/work/review-proposal", "binding.http.work.review_proposal",
    "schema.work.review_proposal.result", 1
);
#[allow(clippy::all)]
pub mod work_snapshot {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///`WorkProjectionSnapshotRequestV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjectionSnapshotRequestV1",
        ///  "type": "object",
        ///  "required": [
        ///    "page_size"
        ///  ],
        ///  "properties": {
        ///    "page_size": {
        ///      "type": "integer",
        ///      "format": "uint32",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProjectionSnapshotRequestV1 {
            pub page_size: u32,
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(
                    &self,
                    f: &mut ::std::fmt::Formatter<'_>,
                ) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `ActorId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ActorId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ActorId(pub ::std::string::String);
        impl ::std::ops::Deref for ActorId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ActorId> for ::std::string::String {
            fn from(value: ActorId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ActorId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ActorId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ActorId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectionGenerationId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectionGenerationId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProjectionGenerationId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectionGenerationId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectionGenerationId> for ::std::string::String {
            fn from(value: ProjectionGenerationId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectionGenerationId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectionGenerationId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectionGenerationId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProposalId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProposalId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct ProposalId(pub ::std::string::String);
        impl ::std::ops::Deref for ProposalId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProposalId> for ::std::string::String {
            fn from(value: ProposalId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProposalId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProposalId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProposalId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RepositoryId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RepositoryId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RepositoryId(pub ::std::string::String);
        impl ::std::ops::Deref for RepositoryId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RepositoryId> for ::std::string::String {
            fn from(value: RepositoryId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RepositoryId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RepositoryId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RepositoryId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`RuntimeEvidenceRef`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "evidence_digest",
        ///    "run_id",
        ///    "terminal"
        ///  ],
        ///  "properties": {
        ///    "evidence_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "terminal": {
        ///      "type": "boolean"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct RuntimeEvidenceRef {
            pub evidence_digest: ManifestDigest,
            pub run_id: RunId,
            pub terminal: bool,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkAuthority`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "actor_id",
        ///    "policy_digest",
        ///    "project_id",
        ///    "repository_id",
        ///    "worktree_id"
        ///  ],
        ///  "properties": {
        ///    "actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "policy_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "repository_id": {
        ///      "$ref": "#/definitions/RepositoryId"
        ///    },
        ///    "worktree_id": {
        ///      "$ref": "#/definitions/WorktreeId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkAuthority {
            pub actor_id: ActorId,
            pub policy_digest: ManifestDigest,
            pub project_id: ProjectId,
            pub repository_id: RepositoryId,
            pub worktree_id: WorktreeId,
        }
        ///`WorkProjection`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "authority",
        ///    "dependencies",
        ///    "execution_admitted",
        ///    "history_len",
        ///    "runtime_evidence",
        ///    "task_accepted",
        ///    "task_id",
        ///    "title",
        ///    "version"
        ///  ],
        ///  "properties": {
        ///    "accepted_proposal": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/ProposalId"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "authority": {
        ///      "$ref": "#/definitions/WorkAuthority"
        ///    },
        ///    "dependencies": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/TaskId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "execution_admitted": {
        ///      "type": "boolean"
        ///    },
        ///    "history_len": {
        ///      "type": "integer",
        ///      "format": "uint",
        ///      "minimum": 0.0
        ///    },
        ///    "runtime_evidence": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/RuntimeEvidenceRef"
        ///      }
        ///    },
        ///    "task_accepted": {
        ///      "type": "boolean"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    },
        ///    "title": {
        ///      "type": "string"
        ///    },
        ///    "version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkProjection {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub accepted_proposal: ::std::option::Option<ProposalId>,
            pub authority: WorkAuthority,
            pub dependencies: Vec<TaskId>,
            pub execution_admitted: bool,
            pub history_len: u32,
            pub runtime_evidence: ::std::vec::Vec<RuntimeEvidenceRef>,
            pub task_accepted: bool,
            pub task_id: TaskId,
            pub title: ::std::string::String,
            pub version: u64,
        }
        ///`WorkProjectionCoverageV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "oneOf": [
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "returned",
        ///        "state",
        ///        "total"
        ///      ],
        ///      "properties": {
        ///        "returned": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "complete"
        ///        },
        ///        "total": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "cursor",
        ///        "range",
        ///        "returned",
        ///        "state",
        ///        "total"
        ///      ],
        ///      "properties": {
        ///        "cursor": {
        ///          "$ref": "#/definitions/WorkProjectionResumeCursorV1"
        ///        },
        ///        "range": {
        ///          "$ref": "#/definitions/WorkProjectionSequenceRangeV1"
        ///        },
        ///        "returned": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "partial"
        ///        },
        ///        "total": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        }
        ///      }
        ///    },
        ///    {
        ///      "type": "object",
        ///      "required": [
        ///        "cap",
        ///        "cursor",
        ///        "range",
        ///        "returned",
        ///        "state",
        ///        "total"
        ///      ],
        ///      "properties": {
        ///        "cap": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        },
        ///        "cursor": {
        ///          "$ref": "#/definitions/WorkProjectionResumeCursorV1"
        ///        },
        ///        "range": {
        ///          "$ref": "#/definitions/WorkProjectionSequenceRangeV1"
        ///        },
        ///        "returned": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        },
        ///        "state": {
        ///          "type": "string",
        ///          "const": "capped"
        ///        },
        ///        "total": {
        ///          "type": "integer",
        ///          "format": "uint32",
        ///          "minimum": 0.0
        ///        }
        ///      }
        ///    }
        ///  ]
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(tag = "state")]
        pub enum WorkProjectionCoverageV1 {
            #[serde(rename = "complete")]
            Complete { returned: u32, total: u32 },
            #[serde(rename = "partial")]
            Partial {
                cursor: WorkProjectionResumeCursorV1,
                range: WorkProjectionSequenceRangeV1,
                returned: u32,
                total: u32,
            },
            #[serde(rename = "capped")]
            Capped {
                cap: u32,
                cursor: WorkProjectionResumeCursorV1,
                range: WorkProjectionSequenceRangeV1,
                returned: u32,
                total: u32,
            },
        }
        ///`WorkProjectionResumeCursorV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "generation_id",
        ///    "token"
        ///  ],
        ///  "properties": {
        ///    "generation_id": {
        ///      "$ref": "#/definitions/ProjectionGenerationId"
        ///    },
        ///    "token": {
        ///      "type": "string"
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkProjectionResumeCursorV1 {
            pub generation_id: ProjectionGenerationId,
            pub token: ::std::string::String,
        }
        ///`WorkProjectionSequenceRangeV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "end_inclusive",
        ///    "start_exclusive"
        ///  ],
        ///  "properties": {
        ///    "end_inclusive": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    },
        ///    "start_exclusive": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkProjectionSequenceRangeV1 {
            pub end_inclusive: WorkProjectionSequenceV1,
            pub start_exclusive: WorkProjectionSequenceV1,
        }
        ///`WorkProjectionSequenceV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjectionSequenceV1",
        ///  "type": "integer",
        ///  "format": "uint64",
        ///  "minimum": 0.0
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct WorkProjectionSequenceV1(pub u64);
        impl ::std::ops::Deref for WorkProjectionSequenceV1 {
            type Target = u64;
            fn deref(&self) -> &u64 {
                &self.0
            }
        }
        impl ::std::convert::From<WorkProjectionSequenceV1> for u64 {
            fn from(value: WorkProjectionSequenceV1) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<u64> for WorkProjectionSequenceV1 {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkProjectionSequenceV1 {
            type Err = <u64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for WorkProjectionSequenceV1 {
            type Error = <u64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for WorkProjectionSequenceV1 {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkProjectionSnapshotV1`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkProjectionSnapshotV1",
        ///  "type": "object",
        ///  "required": [
        ///    "coverage",
        ///    "generation_id",
        ///    "projections",
        ///    "sequence"
        ///  ],
        ///  "properties": {
        ///    "coverage": {
        ///      "$ref": "#/definitions/WorkProjectionCoverageV1"
        ///    },
        ///    "generation_id": {
        ///      "$ref": "#/definitions/ProjectionGenerationId"
        ///    },
        ///    "projections": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkProjection"
        ///      }
        ///    },
        ///    "sequence": {
        ///      "$ref": "#/definitions/WorkProjectionSequenceV1"
        ///    }
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        pub struct WorkProjectionSnapshotV1 {
            pub coverage: WorkProjectionCoverageV1,
            pub generation_id: ProjectionGenerationId,
            pub projections: ::std::vec::Vec<WorkProjection>,
            pub sequence: WorkProjectionSequenceV1,
        }
        ///Strongly typed canonical identity: `WorktreeId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorktreeId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd
        )]
        #[serde(transparent)]
        pub struct WorktreeId(pub ::std::string::String);
        impl ::std::ops::Deref for WorktreeId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorktreeId> for ::std::string::String {
            fn from(value: WorktreeId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorktreeId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorktreeId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorktreeId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub type Request = request::WorkProjectionSnapshotRequestV1;
    pub type Result = result::WorkProjectionSnapshotV1;
}
typed_operation!(
    WorkSnapshot, work_snapshot, "operation.work.snapshot", "/application/work/snapshot",
    "binding.http.work.snapshot", "schema.work.snapshot.result", 1
);
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaseOperationCapability {
    pub operation: HttpApplicationOperation,
    pub route: String,
    pub disposition: ExecutableUnavailableDispositionV1,
}
pub fn base_operation_capabilities() -> impl ExactSizeIterator<
    Item = BaseOperationCapability,
> {
    HttpApplicationOperation::ALL
        .iter()
        .copied()
        .map(|operation| BaseOperationCapability {
            route: format!("/application{}", operation.route_path()),
            operation,
            disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
        })
}
