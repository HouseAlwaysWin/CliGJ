use std::io::{Read, Write};

pub trait PtyReader: Read + Send {}
pub trait PtyWriter: Write + Send {}

pub trait PtyProcess: Send + Sync {
    fn resize(&self, cols: u16, rows: u16) -> Result<(), String>;
}

pub struct PtyPair {
    pub process: Box<dyn PtyProcess>,
    pub reader: Box<dyn PtyReader>,
    pub writer: Box<dyn PtyWriter>,
}
