//! Adapted from colbymchenry/codegraph v1.6.2 (6560052), codegraph-kernel/src/buffers.rs.
//! License: MIT (Copyright (c) 2026 Colby Mchenry).
//!
//! Walkers use per-file row indices for edges and references. Rows stay as Rust
//! values through the SQLite write so no wire encoding or decoding is needed.

#[derive(Default)]
pub(crate) struct BoolFlags(pub [bool; 4]);

impl BoolFlags {
    pub(crate) fn set(&mut self, index: usize, value: bool) {
        self.0[index] = value;
    }
}

pub(crate) const FLAG_IS_EXPORTED: usize = 0;
pub(crate) const FLAG_IS_ASYNC: usize = 1;
pub(crate) const FLAG_IS_STATIC: usize = 2;
pub(crate) const FUNCTION_REF: &str = "function_ref";
pub(crate) const VISIBILITIES: [Option<&str>; 5] = [
    None,
    Some("public"),
    Some("private"),
    Some("protected"),
    Some("internal"),
];

pub(crate) struct NodeRow {
    pub kind: &'static str,
    pub visibility: u8,
    pub flags: BoolFlags,
    pub start_line: u32,
    pub end_line: u32,
    pub start_column: u32,
    pub end_column: u32,
    pub name: String,
    pub qualified_name: String,
    pub id: String,
    pub docstring: Option<String>,
    pub signature: Option<String>,
    pub decorators: Option<String>,
    pub type_parameters: Option<String>,
    pub return_type: Option<String>,
}

pub(crate) struct EdgeRow {
    pub source_idx: u32,
    pub target_idx: u32,
    pub kind: &'static str,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub metadata_json: Option<String>,
}

pub(crate) struct RefRow {
    pub from_idx: u32,
    pub kind: &'static str,
    pub line: u32,
    pub column: u32,
    pub reference_name: String,
    pub candidates: Option<String>,
}

#[derive(Default)]
pub(crate) struct Tables {
    pub nodes: Vec<NodeRow>,
    pub edges: Vec<EdgeRow>,
    pub refs: Vec<RefRow>,
}

impl Tables {
    pub(crate) fn push_node(&mut self, row: NodeRow) -> u32 {
        let index = self.nodes.len() as u32;
        self.nodes.push(row);
        index
    }
}
