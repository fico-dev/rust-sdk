//! Account types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::OrUnknown;

/// An account (an organization in the API), including its members.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Account {
    /// ID of the account (API: `id`).
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Name of the account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Members of the account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<AccountMember>>,
}

/// A member of an account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct AccountMember {
    /// Email of the member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Role of the member in the account.
    #[serde(
        default,
        deserialize_with = "super::or_unknown",
        skip_serializing_if = "Option::is_none"
    )]
    pub role: Option<AccountMemberRole>,
    /// Whether the member has yet to accept their invitation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_invite: Option<bool>,
}

/// Role of an account member.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum AccountMemberRole {
    /// The account's root user.
    Root,
    /// An administrator.
    Admin,
    /// A developer.
    Developer,
    /// An operator.
    Operator,
    /// A viewer.
    #[default]
    Viewer,
    /// A value this version of the SDK does not know, read from a response
    /// field. Requests should not use it: tool arguments reject it, and tool
    /// schemas leave it out.
    #[serde(skip_deserializing)]
    #[schemars(skip)]
    Unknown,
}

impl OrUnknown for AccountMemberRole {
    const UNKNOWN: Self = AccountMemberRole::Unknown;
}
