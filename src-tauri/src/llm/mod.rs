pub mod pi_rpc;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct StreamChunk {
    pub content: String,
    pub done: bool,
}
