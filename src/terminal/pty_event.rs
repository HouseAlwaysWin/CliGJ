#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawPtyMode {
    Shell,
    InteractiveAi,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RawPtyEvent {
    Bytes(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    RenderMode { mode: RawPtyMode },
}

impl RawPtyEvent {
    pub fn byte_len(&self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes.len(),
            Self::Resize { .. } | Self::RenderMode { .. } => 0,
        }
    }
}
