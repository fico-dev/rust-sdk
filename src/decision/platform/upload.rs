//! Upload URLs, on `v1/applications/{workspace_id}/runs/uploadurl`.

use super::Platform;
use crate::Result;
use crate::decision::UploadUrl;
use crate::decision::path::api_path;
use crate::tool::{InWorkspace, NoParams, Tool};

impl Platform {
    /// Creates a presigned URL to upload a large input or a file to.
    ///
    /// Upload the data with [`Platform::upload_data`], then pass the
    /// `upload_id` to a run or a managed input. The URL expires 10 minutes
    /// after the platform issues it.
    ///
    /// # Errors
    ///
    /// Returns the [errors of every method](Platform#errors).
    pub async fn create_upload(&self, workspace_id: &str) -> Result<UploadUrl> {
        let path = api_path(&["v1", "applications", workspace_id, "runs", "uploadurl"])?;
        self.client.post(path).send_json().await
    }
}

pub(crate) fn tools() -> Vec<Tool> {
    vec![Tool::new(
        "decision_platform_upload_create",
        "Creates a presigned URL to upload a large input or a file to.",
        |cx, a: InWorkspace<NoParams>| async move {
            cx.platform()?.create_upload(&a.workspace_id).await
        },
    )]
}
