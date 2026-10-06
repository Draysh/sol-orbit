//! What a dashboard widget route returns.
//!
//! Sol's dashboard draws every app's widgets with the shared design
//! components, so an app describes *what* to show and never ships widget code
//! to Sol. Every field is optional; a widget uses whichever parts fit.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct WidgetView {
    /// The headline figure, e.g. `2 / 3`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub figure: Option<String>,
    /// What the figure counts, e.g. `habits done today`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    /// Draws a progress line under the figure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    /// `Label: value` rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<Row>,
    /// A list, or a checklist when items have `done`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<Item>,
    /// Shown instead when the widget has nothing else to say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Progress {
    pub value: f64,
    pub max: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Row {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Item {
    pub id: String,
    pub label: String,
    /// A short note on the right, e.g. a streak or a time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<String>,
    /// Draws a checkbox when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<bool>,
    /// What ticking the checkbox calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toggle: Option<Toggle>,
}

/// Ticking a checklist item sends `{ "<field>": <new state> }` with `method`
/// to `path` under the app's API, e.g. `PUT /api/terra/habits/1/today`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Toggle {
    pub method: String,
    pub path: String,
    pub field: String,
}

impl WidgetView {
    pub fn row(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.rows.push(Row {
            label: label.into(),
            value: value.into(),
        });
        self
    }
}

impl Toggle {
    pub fn put(path: impl Into<String>, field: &str) -> Self {
        Self {
            method: "PUT".into(),
            path: path.into(),
            field: field.into(),
        }
    }
}
