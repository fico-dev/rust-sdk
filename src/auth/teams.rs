//! The teams (organizations) of the user who logged in, one of which a
//! PKCE profile acts for.

use serde::{Deserialize, Serialize};

use crate::{Client, Error, Result};

/// A team that the user who logged in belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Team {
    /// The team's ID, which a PKCE profile stores as its `team_id` and
    /// sends in [`ACCOUNT_HEADER`](crate::config::ACCOUNT_HEADER).
    #[serde(rename = "id")]
    pub team_id: String,
    /// The team's name, to show the user.
    pub name: String,
    /// The user's role in the team.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Whether the user's invitation to the team is still pending.
    #[serde(default)]
    pub pending_invite: bool,
}

/// The teams that the user of `client` belongs to, `GET
/// v1/internal/me/organization`. Build `client` with a session's token,
/// e.g. from
/// [`Resolved::client_builder`](crate::config::Resolved::client_builder) or
/// [`ensure_token`](super::ensure_token) and
/// [`Client::with_bearer_token`].
///
/// # Errors
///
/// Returns the transport's errors, and [`Error::NotLoggedIn`] from the
/// client's token source.
pub async fn list_teams(client: &Client) -> Result<Vec<Team>> {
    client.get("/v1/internal/me/organization").send_json().await
}

/// The ID of the team named `name` in `teams`, ignoring case, as `fico
/// configuration create --team` takes it.
///
/// # Errors
///
/// Returns [`Error::Invalid`] if no team has that name. Its message lists
/// the names of `teams`, so the user can pick one.
pub fn resolve_team<'a>(teams: &'a [Team], name: &str) -> Result<&'a Team> {
    let name = name.trim();
    let lower = name.to_lowercase();
    teams
        .iter()
        .find(|team| team.name.to_lowercase() == lower)
        .ok_or_else(|| {
            let mut names: Vec<&str> = teams.iter().map(|team| team.name.as_str()).collect();
            names.sort_unstable();
            let names = match names.is_empty() {
                true => "none".to_owned(),
                false => names
                    .iter()
                    .map(|name| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
            };
            Error::Invalid {
                reason: format!("no team is named `{name}`; the teams are: {names}"),
            }
        })
}
