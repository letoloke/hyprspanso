#[derive(Debug, Clone)]
pub enum Event {
    Key {
        code: u16,
        value: i32,
        device: String,
    },
    Layout { keyboard: String, layout: String },
    /// Focus event. `address` comes from `activewindowv2` (empty when the
    /// compositor only sent `activewindow`); `class` from `activewindow`.
    Focus { address: String, class: String },
    /// A keyboard node started/stopped being read. Modifier state is reset on
    /// both, because a release can be lost when a device goes away.
    DeviceChanged(String),
    Reload,
    /// A trigger was chosen (search window, or disambiguation picker).
    /// `index` selects among entries sharing the same trigger.
    Choose {
        trigger: String,
        index: usize,
        target: Option<String>,
    },
    /// A value was picked for a `type: choice` variable.
    PickVar {
        trigger: String,
        var: String,
        index: usize,
        target: Option<String>,
    },
    FormValues {
        trigger: String,
        values: std::collections::HashMap<String, String>,
        target: Option<String>,
    },
}
