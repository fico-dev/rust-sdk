//! Polling options.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How to poll a resource until it reaches a terminal state.
///
/// Polling waits `initial_delay`, then checks the resource. After the n-th
/// check that finds it still changing, it waits
/// `min(delay + backoff * 2^n, max_delay)` seconds plus a random amount up
/// to `jitter` seconds, then checks again. It gives up when `max_duration`
/// or `max_tries` runs out.
///
/// Durations are in seconds, and a `None` limit means no limit. Missing
/// fields take their default, so `{}` polls with the defaults. To cancel
/// polling, drop the future or wrap it in `tokio::time::timeout`; there is
/// no stop callback.
///
/// A check that fails ends polling with its error, so a request that gets no
/// response ends it once the client's retries ran out, by default after about
/// 3 seconds. The change keeps going on the platform: wait again to pick it
/// up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PollingOptions {
    /// Exponential backoff factor, in seconds: after the n-th check, the
    /// wait grows by `backoff * 2^n`. Defaults to 0.9.
    pub backoff: f64,
    /// Base wait between checks, in seconds. Defaults to 0.1.
    pub delay: f64,
    /// Wait before the first check, in seconds. Defaults to 1.
    pub initial_delay: f64,
    /// Longest wait between checks, in seconds, before jitter. Defaults to
    /// 20.
    pub max_delay: f64,
    /// How long to keep checking, in seconds, counted from the first check.
    /// Polling gives up after the first check past it. No limit if unset,
    /// the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_duration: Option<f64>,
    /// Most checks to make. No limit if unset, the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tries: Option<u32>,
    /// Upper bound, in seconds, of the random amount added to every wait
    /// between checks. 0 adds none. Defaults to 1.
    pub jitter: f64,
}

impl Default for PollingOptions {
    /// The defaults: a 1 second initial delay, waits from 1.9 up to 20
    /// seconds plus up to 1 second of jitter, and no limits.
    fn default() -> Self {
        Self {
            backoff: 0.9,
            delay: 0.1,
            initial_delay: 1.0,
            max_delay: 20.0,
            max_duration: None,
            max_tries: None,
            jitter: 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Context, Tool, ToolError};

    #[test]
    fn reads_an_empty_object_as_the_defaults() {
        let options: PollingOptions = serde_json::from_value(json!({})).unwrap();
        assert_eq!(options, PollingOptions::default());
        assert_eq!(
            serde_json::to_value(&options).unwrap(),
            json!({
                "backoff": 0.9,
                "delay": 0.1,
                "initial_delay": 1.0,
                "max_delay": 20.0,
                "jitter": 1.0,
            })
        );
    }

    #[test]
    fn reads_every_field() {
        let wire = json!({
            "backoff": 0.5,
            "delay": 2.0,
            "initial_delay": 0.0,
            "max_delay": 30.0,
            "max_duration": 600.0,
            "max_tries": 50,
            "jitter": 0.0,
        });
        let options: PollingOptions = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            options,
            PollingOptions {
                backoff: 0.5,
                delay: 2.0,
                initial_delay: 0.0,
                max_delay: 30.0,
                max_duration: Some(600.0),
                max_tries: Some(50),
                jitter: 0.0,
            }
        );
        assert_eq!(serde_json::to_value(&options).unwrap(), wire);
        let partial: PollingOptions =
            serde_json::from_value(json!({ "max_tries": 3, "max_duration": null })).unwrap();
        assert_eq!(
            partial,
            PollingOptions {
                max_tries: Some(3),
                ..PollingOptions::default()
            }
        );
    }

    /// Parameters of a get operation, as in `RunGetParams`.
    #[derive(Debug, Deserialize, JsonSchema)]
    struct GetParams {
        /// Polls until the resource reaches a terminal state.
        polling: Option<PollingOptions>,
    }

    #[tokio::test]
    async fn serves_polling_options_in_tool_arguments() {
        let tool = Tool::new("get", "Gets a resource.", |_cx, a: GetParams| async move {
            Ok(a.polling)
        });
        let defaults = tool.call(Context::new(), json!({ "polling": {} })).await;
        let defaults: PollingOptions = serde_json::from_value(defaults.unwrap()).unwrap();
        assert_eq!(defaults, PollingOptions::default());
        let err = tool
            .call(Context::new(), json!({ "polling": { "dely": 1 } }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)), "{err}");
        assert!(err.to_string().contains("polling.dely"), "{err}");
    }
}
