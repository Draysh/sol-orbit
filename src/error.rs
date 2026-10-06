use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The JSON body of every error Sol returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    /// Stable machine-readable code, e.g. `unauthorized` or `conflict`.
    pub error: String,
    /// A sentence for people.
    pub message: String,
}
