//! Upload URL types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A presigned URL to upload a large input or a file to, and the upload's ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct UploadUrl {
    /// ID of the upload, which a run or a managed input takes once the data
    /// is uploaded.
    pub upload_id: String,
    /// Presigned URL to upload the data to. It needs no authorization and
    /// expires 10 minutes after the platform issues it.
    pub upload_url: String,
}
