//! Every data type of the decision capability, defined once and shared by
//! [`Platform`](super::platform::Platform) and [`Local`](super::local::Local).
//!
//! One file per noun. `decision.rs` glob re-exports every file, so users
//! write `fico_sdk::decision::Run`, and type names must be unique across
//! the files.
//!
//! Every enum is `#[non_exhaustive]`, since the platform adds values. Callers
//! can still build its variants.
//!
//! Enums that responses carry read values this SDK does not know as
//! `Unknown`:
//! - Response-only enums do it with `#[serde(other)]`.
//! - Enums that requests use too implement [`OrUnknown`]. They deserialize
//!   strictly, and `Unknown` stays out of the tool schemas, so an LLM never
//!   picks it. Every field of one that a response can carry reads with
//!   `#[serde(deserialize_with = "super::or_unknown")]`, including the
//!   fields of types that requests share, such as
//!   [`Secret`](secrets::Secret). Fields that only requests carry need no
//!   attribute.
//!
//! `or_unknown` reads new values as `Unknown`, except in tool arguments:
//! [`Tool::call`](crate::tool::Tool::call) reads them strictly, so a typo
//! fails at any depth instead of reaching the platform as `"unknown"`.
//!
//! A decoded response never goes back in a request, since `Unknown`
//! serializes as `"unknown"` and fields this SDK does not model are lost.
//! Updates that GET and PUT a resource overwrite only the changed keys of
//! the fetched JSON object, so the server's values survive.

use serde::Deserialize;
use serde::de::value::Error as ValueError;
use serde::de::{DeserializeOwned, Deserializer, IntoDeserializer};

use crate::tool::reading_arguments;

pub(super) mod acceptance;
pub(super) mod account;
pub(super) mod build;
pub(super) mod ensemble;
pub(super) mod environment;
pub(super) mod experiment;
pub(super) mod input_set;
pub(super) mod integration;
pub(super) mod local_workspace;
pub(super) mod manifest;
pub(super) mod polling;
pub(super) mod run;
pub(super) mod scenario;
pub(super) mod secrets;
pub(super) mod shadow;
pub(super) mod sso;
pub(super) mod switchback;
pub(super) mod upload;
pub(super) mod workspace;

/// An enum that requests and responses both use. It deserializes strictly,
/// and [`or_unknown`] reads the fields of it that responses carry.
pub(crate) trait OrUnknown: DeserializeOwned {
    /// The variant for values this SDK does not know.
    const UNKNOWN: Self;

    /// `raw` as a variant, or [`UNKNOWN`](Self::UNKNOWN).
    fn from_raw(raw: String) -> Self {
        let raw = IntoDeserializer::<ValueError>::into_deserializer(raw);
        Self::deserialize(raw).unwrap_or(Self::UNKNOWN)
    }
}

/// The type of a field that [`or_unknown`] reads: an [`OrUnknown`] enum,
/// or an `Option`, `Vec` or `Option<Vec>` of one.
pub(crate) trait OrUnknownField: DeserializeOwned {
    /// Reads the field, with values this SDK does not know as `Unknown`.
    fn deserialize_or_unknown<'de, D: Deserializer<'de>>(d: D) -> Result<Self, D::Error>;
}

impl<T: OrUnknown> OrUnknownField for T {
    fn deserialize_or_unknown<'de, D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(T::from_raw)
    }
}

impl<T: OrUnknown> OrUnknownField for Option<T> {
    fn deserialize_or_unknown<'de, D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Option::<String>::deserialize(d).map(|raw| raw.map(T::from_raw))
    }
}

impl<T: OrUnknown> OrUnknownField for Vec<T> {
    fn deserialize_or_unknown<'de, D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Vec::<String>::deserialize(d).map(|raw| raw.into_iter().map(T::from_raw).collect())
    }
}

impl<T: OrUnknown> OrUnknownField for Option<Vec<T>> {
    fn deserialize_or_unknown<'de, D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Option::<Vec<String>>::deserialize(d)
            .map(|raw| raw.map(|raw| raw.into_iter().map(T::from_raw).collect()))
    }
}

/// Reads an optional response field with the empty string as `None`, which
/// the API sends for a field it has no value for:
/// `#[serde(default, deserialize_with = "super::empty_as_none")]`.
pub(crate) fn empty_as_none<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    match Option::<String>::deserialize(d)? {
        Some(raw) if !raw.is_empty() => {
            T::deserialize(IntoDeserializer::<D::Error>::into_deserializer(raw)).map(Some)
        }
        _ => Ok(None),
    }
}

/// Reads a field of an [`OrUnknown`] enum, or an `Option` or `Vec` of one,
/// that a response can carry, with values this SDK does not know as
/// `Unknown`: `#[serde(deserialize_with = "super::or_unknown")]`. In tool
/// arguments, it is as strict as plain deserialization.
///
/// `deserialize_with` makes a field required, so optional fields also need
/// `#[serde(default)]`.
pub(crate) fn or_unknown<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: OrUnknownField,
{
    if reading_arguments() {
        T::deserialize(d)
    } else {
        T::deserialize_or_unknown(d)
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use schemars::JsonSchema;
    use serde::de::DeserializeOwned;
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};

    use super::{OrUnknown, or_unknown};
    use crate::decision::*;
    use crate::tool::{Context, Tool, ToolError};

    fn read<T: DeserializeOwned>(wire: &str) -> T {
        serde_json::from_value(json!(wire)).unwrap()
    }

    /// Every string value in `schema`: its `enum` lists and `const`s.
    fn schema_values(schema: &Value, values: &mut Vec<String>) {
        match schema {
            Value::Object(object) => {
                for (key, value) in object {
                    match (key.as_str(), value) {
                        ("const", Value::String(value)) => values.push(value.clone()),
                        ("enum", Value::Array(items)) => {
                            values.extend(items.iter().filter_map(Value::as_str).map(str::to_owned))
                        }
                        _ => schema_values(value, values),
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| schema_values(item, values)),
            _ => {}
        }
    }

    fn assert_schema_hides_unknown<T: JsonSchema>() {
        let schema = schemars::schema_for!(T);
        let mut values = Vec::new();
        schema_values(schema.as_value(), &mut values);
        assert!(!values.is_empty(), "{}", T::schema_name());
        assert!(
            !values.iter().any(|v| v == "unknown"),
            "{}",
            T::schema_name()
        );
    }

    fn assert_wire<T: Serialize + DeserializeOwned + PartialEq + Debug>(pairs: &[(T, &str)]) {
        for (value, wire) in pairs {
            assert_eq!(
                serde_json::to_value(value).unwrap(),
                json!(wire),
                "{value:?}"
            );
            assert_eq!(&read::<T>(wire), value, "{wire}");
        }
    }

    /// `Unknown` is written as `"unknown"`, and only [`or_unknown`] reads it,
    /// or a value it does not know: plain deserialization rejects both.
    fn assert_or_unknown<T: OrUnknown + Serialize + PartialEq + Debug>() {
        let name = std::any::type_name::<T>();
        assert_eq!(
            serde_json::to_value(T::UNKNOWN).unwrap(),
            json!("unknown"),
            "{name}"
        );
        for wire in ["not-a-value", "unknown"] {
            let value: T = or_unknown(json!(wire)).unwrap();
            assert_eq!(value, T::UNKNOWN, "{name}: {wire}");
            assert!(
                serde_json::from_value::<T>(json!(wire)).is_err(),
                "{name}: {wire}"
            );
        }
    }

    /// The expected values are the API's.
    #[test]
    fn enums_use_the_api_values() {
        assert_wire(&[
            (RunStatus::Canceled, "canceled"),
            (RunStatus::Failed, "failed"),
            (RunStatus::None, "none"),
            (RunStatus::Queued, "queued"),
            (RunStatus::Running, "running"),
            (RunStatus::Succeeded, "succeeded"),
        ]);
        assert_wire(&[
            (ContentFormat::Json, "json"),
            (ContentFormat::MultiFile, "multi-file"),
        ]);
        assert_wire(&[
            (RunType::Standard, "standard"),
            (RunType::External, "external"),
            (RunType::Ensemble, "ensemble"),
            (RunType::EnsembleChild, "ensemble-child"),
        ]);
        assert_wire(&[
            (TrackedRunStatus::Succeeded, "succeeded"),
            (TrackedRunStatus::Failed, "failed"),
        ]);
        assert_wire(&[
            (WorkspaceType::Custom, "custom"),
            (WorkspaceType::Subscription, "subscription"),
            (WorkspaceType::Pipeline, "pipeline"),
            (WorkspaceType::Unknown, "unknown"),
        ]);
        assert_wire(&[(MetricType::DirectComparison, "direct-comparison")]);
        assert_wire(&[
            (StatisticType::Min, "min"),
            (StatisticType::Max, "max"),
            (StatisticType::Mean, "mean"),
            (StatisticType::Std, "std"),
            (
                StatisticType::ShiftedGeometricMean,
                "shifted_geometric_mean",
            ),
            (StatisticType::P01, "p01"),
            (StatisticType::P05, "p05"),
            (StatisticType::P10, "p10"),
            (StatisticType::P25, "p25"),
            (StatisticType::P50, "p50"),
            (StatisticType::P75, "p75"),
            (StatisticType::P90, "p90"),
            (StatisticType::P95, "p95"),
            (StatisticType::P99, "p99"),
        ]);
        assert_wire(&[
            (Comparison::EqualTo, "eq"),
            (Comparison::GreaterThan, "gt"),
            (Comparison::GreaterThanOrEqualTo, "ge"),
            (Comparison::LessThan, "lt"),
            (Comparison::LessThanOrEqualTo, "le"),
            (Comparison::NotEqualTo, "ne"),
        ]);
        assert_wire(&[
            (MetricToleranceType::Undefined, ""),
            (MetricToleranceType::Absolute, "absolute"),
            (MetricToleranceType::Relative, "relative"),
        ]);
        assert_wire(&[
            (ExperimentStatus::Started, "started"),
            (ExperimentStatus::Completed, "completed"),
            (ExperimentStatus::Failed, "failed"),
            (ExperimentStatus::Draft, "draft"),
            (ExperimentStatus::Canceled, "canceled"),
            (ExperimentStatus::Stopping, "stopping"),
            (ExperimentStatus::Deleting, "deleting"),
            (ExperimentStatus::DeleteFailed, "delete-failed"),
            (ExperimentStatus::Unknown, "unknown"),
        ]);
        assert_wire(&[
            (ScenarioInputType::InputSet, "input_set"),
            (ScenarioInputType::Input, "input"),
            (ScenarioInputType::New, "new"),
        ]);
        assert_wire(&[
            (StopIntent::Complete, "complete"),
            (StopIntent::Cancel, "cancel"),
        ]);
        assert_wire(&[
            (RuleObjective::Maximize, "maximize"),
            (RuleObjective::Minimize, "minimize"),
        ]);
        assert_wire(&[
            (RuleToleranceType::Absolute, "absolute"),
            (RuleToleranceType::Relative, "relative"),
        ]);
        assert_wire(&[(SecretType::Env, "env"), (SecretType::File, "file")]);
        assert_wire(&[
            (AccountMemberRole::Root, "root"),
            (AccountMemberRole::Admin, "admin"),
            (AccountMemberRole::Developer, "developer"),
            (AccountMemberRole::Operator, "operator"),
            (AccountMemberRole::Viewer, "viewer"),
        ]);
        assert_wire(&[
            (ManifestType::Python, "python"),
            (ManifestType::Go, "go"),
            (ManifestType::Java, "java"),
            (ManifestType::Binary, "binary"),
        ]);
        assert_wire(&[
            (
                ManifestRuntime::Default,
                "ghcr.io/nextmv-io/runtime/default:latest",
            ),
            (
                ManifestRuntime::Python,
                "ghcr.io/nextmv-io/runtime/python:3.11",
            ),
            (
                ManifestRuntime::Java,
                "ghcr.io/nextmv-io/runtime/java:latest",
            ),
            (
                ManifestRuntime::Pyomo,
                "ghcr.io/nextmv-io/runtime/pyomo:latest",
            ),
            (
                ManifestRuntime::Cuopt,
                "ghcr.io/nextmv-io/runtime/cuopt:latest",
            ),
            (
                ManifestRuntime::Gamspy,
                "ghcr.io/nextmv-io/runtime/gamspy:latest",
            ),
        ]);
        assert_wire(&[
            (ManifestPythonArch::Arm64, "arm64"),
            (ManifestPythonArch::Amd64, "amd64"),
        ]);
        assert_wire(&[
            (ControlType::Input, "input"),
            (ControlType::Select, "select"),
            (ControlType::Multiselect, "multiselect"),
            (ControlType::Slider, "slider"),
            (ControlType::Toggle, "toggle"),
        ]);
        assert_wire(&[
            (ManifestOptionType::String, "string"),
            (ManifestOptionType::Bool, "bool"),
            (ManifestOptionType::Int, "int"),
            (ManifestOptionType::Float, "float"),
        ]);
    }

    #[test]
    fn reads_new_values_from_responses_as_unknown() {
        assert_eq!(read::<WorkspaceType>("not-a-value"), WorkspaceType::Unknown);
        assert_eq!(
            read::<ExperimentStatus>("not-a-value"),
            ExperimentStatus::Unknown
        );
        assert_or_unknown::<RunStatus>();
        assert_or_unknown::<ContentFormat>();
        assert_or_unknown::<RunType>();
        assert_or_unknown::<MetricType>();
        assert_or_unknown::<StatisticType>();
        assert_or_unknown::<Comparison>();
        assert_or_unknown::<MetricToleranceType>();
        assert_or_unknown::<RuleObjective>();
        assert_or_unknown::<RuleToleranceType>();
        assert_or_unknown::<SecretType>();
        assert_or_unknown::<AccountMemberRole>();
        assert_or_unknown::<ControlType>();
        assert_or_unknown::<ManifestOptionType>();
        assert_or_unknown::<IntegrationType>();
        assert_or_unknown::<IntegrationProvider>();
        assert_or_unknown::<ExecutableType>();
    }

    /// A response with every field shape that [`or_unknown`] reads.
    #[derive(Debug, Deserialize)]
    struct Response {
        #[serde(deserialize_with = "or_unknown")]
        status: RunStatus,
        #[serde(default, deserialize_with = "or_unknown")]
        format: Option<ContentFormat>,
        #[serde(default, deserialize_with = "or_unknown")]
        roles: Vec<AccountMemberRole>,
    }

    #[test]
    fn reads_unknown_values_in_every_field_shape() {
        let response: Response = serde_json::from_value(json!({
            "status": "deleting",
            "format": "zip",
            "roles": ["admin", "owner"],
        }))
        .unwrap();
        assert_eq!(response.status, RunStatus::Unknown);
        assert_eq!(response.format, Some(ContentFormat::Unknown));
        assert_eq!(
            response.roles,
            [AccountMemberRole::Admin, AccountMemberRole::Unknown]
        );
        let response: Response =
            serde_json::from_value(json!({ "status": "queued", "format": null })).unwrap();
        assert_eq!(response.status, RunStatus::Queued);
        assert_eq!(response.format, None);
        assert!(response.roles.is_empty());
        let wrong_type = serde_json::from_value::<Response>(json!({ "status": 1 }));
        assert!(wrong_type.is_err());
    }

    /// Arguments of a test tool that takes an `OrUnknown` enum.
    #[derive(Debug, Deserialize, JsonSchema)]
    struct Filter {
        /// Status of the runs to list.
        status: Option<RunStatus>,
    }

    #[tokio::test]
    async fn rejects_unknown_values_in_tool_arguments() {
        let tool = Tool::new("filter", "Filters runs.", |_cx, a: Filter| async move {
            Ok(a.status)
        });
        let ok = tool.call(Context::new(), json!({ "status": "succeeded" }));
        assert_eq!(ok.await.unwrap(), json!("succeeded"));
        for wire in ["succeded", "unknown"] {
            let err = tool
                .call(Context::new(), json!({ "status": wire }))
                .await
                .unwrap_err();
            assert!(matches!(err, ToolError::InvalidArguments(_)), "{err}");
            assert_eq!(
                err.to_string(),
                format!(
                    "invalid arguments: unknown variant `{wire}`, expected one of \
                     `canceled`, `failed`, `none`, `queued`, `running`, `succeeded`"
                )
            );
        }
    }

    /// A type that requests and responses share, such as `Secret`, with every
    /// field shape that [`or_unknown`] reads.
    #[derive(Debug, Deserialize, JsonSchema)]
    struct Shared {
        /// How a run receives the secret.
        #[serde(rename = "type", deserialize_with = "or_unknown")]
        secret_type: SecretType,
        /// Format of the secret.
        #[serde(default, deserialize_with = "or_unknown")]
        format: Option<ContentFormat>,
        /// Roles that may read the secret.
        #[serde(default, deserialize_with = "or_unknown")]
        roles: Vec<AccountMemberRole>,
    }

    /// Arguments of a test tool that takes [`Shared`], nested.
    #[derive(Debug, Deserialize, JsonSchema)]
    struct Create {
        /// The secrets to create.
        secrets: Vec<Shared>,
    }

    #[tokio::test]
    async fn rejects_unknown_values_in_nested_tool_arguments() {
        let tool = Tool::new("create", "Creates secrets.", |_cx, a: Create| async move {
            Ok(a.secrets.len())
        });
        let secret = json!({ "type": "env", "format": "json", "roles": ["admin"] });
        let ok = tool.call(Context::new(), json!({ "secrets": [secret] }));
        assert_eq!(ok.await.unwrap(), json!(1));
        let cases = [
            (
                json!({ "type": "enviroment" }),
                "unknown variant `enviroment`, expected `env` or `file`",
            ),
            (
                json!({ "type": "unknown" }),
                "unknown variant `unknown`, expected `env` or `file`",
            ),
            (
                json!({ "type": "env", "format": "zip" }),
                "unknown variant `zip`, expected `json` or `multi-file`",
            ),
            (
                json!({ "type": "env", "roles": ["admin", "owner"] }),
                "unknown variant `owner`, expected one of \
                 `root`, `admin`, `developer`, `operator`, `viewer`",
            ),
        ];
        for (secret, message) in cases {
            let err = tool
                .call(Context::new(), json!({ "secrets": [secret] }))
                .await
                .unwrap_err();
            assert!(matches!(err, ToolError::InvalidArguments(_)), "{err}");
            assert_eq!(err.to_string(), format!("invalid arguments: {message}"));
        }
        // The same type in a response reads new values as `Unknown`. The
        // test runtime runs the calls on this thread, so this also checks
        // that they leave strict mode behind.
        let response: Shared = serde_json::from_value(json!({
            "type": "vault",
            "format": "zip",
            "roles": ["admin", "owner"],
        }))
        .unwrap();
        assert_eq!(response.secret_type, SecretType::Unknown);
        assert_eq!(response.format, Some(ContentFormat::Unknown));
        assert_eq!(
            response.roles,
            [AccountMemberRole::Admin, AccountMemberRole::Unknown]
        );
    }

    /// Enums that requests use too keep `Unknown` out of tool schemas, so an
    /// LLM never picks it.
    #[test]
    fn hides_unknown_from_request_schemas() {
        assert_schema_hides_unknown::<RunStatus>();
        assert_schema_hides_unknown::<ContentFormat>();
        assert_schema_hides_unknown::<RunType>();
        assert_schema_hides_unknown::<MetricType>();
        assert_schema_hides_unknown::<StatisticType>();
        assert_schema_hides_unknown::<Comparison>();
        assert_schema_hides_unknown::<MetricToleranceType>();
        assert_schema_hides_unknown::<RuleObjective>();
        assert_schema_hides_unknown::<RuleToleranceType>();
        assert_schema_hides_unknown::<SecretType>();
        assert_schema_hides_unknown::<AccountMemberRole>();
        assert_schema_hides_unknown::<ControlType>();
        assert_schema_hides_unknown::<ManifestOptionType>();
        assert_schema_hides_unknown::<IntegrationType>();
        assert_schema_hides_unknown::<IntegrationProvider>();
        assert_schema_hides_unknown::<ExecutableType>();
    }
}
