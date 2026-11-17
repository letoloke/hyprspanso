#[derive(Debug, Clone)]
pub enum Event {
    Key { code: u16, value: i32 },
    Layout(String),
    #[allow(dead_code)]
    Window(String, String),
    Reload,
    Select { trigger: String, target: Option<String> },
}
