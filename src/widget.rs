//! A widget on Sol's dashboard.
//!
//! A world's app pushes the latest look of each of its widgets to Sol
//! (`PUT /api/v1/widgets/{id}`), and Sol's dashboard draws it with the shared
//! design components, so a world describes *what* to show and never ships
//! widget code to Sol. Every field is optional; a widget uses whichever parts
//! fit.

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
    /// The box can be ticked from the dashboard. Sol then shows the new state
    /// at once and puts a `widget.toggle` delivery in the world's inbox with
    /// `{ "widget", "item", "done" }`, for the app to apply.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub toggle: bool,
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
