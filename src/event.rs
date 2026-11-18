#[derive(Debug, Clone)]
pub enum Event {
    Key { code: u16, value: i32 },
    Layout(String),
    #[allow(dead_code)]
    Window(String, String),
    /// A keyboard node started/stopped being read. Modifier state is reset on
    /// both, because a release can be lost when a device goes away.
    DeviceChanged(String),
    Reload,
    Select { trigger: String, target: Option<String> },
}
