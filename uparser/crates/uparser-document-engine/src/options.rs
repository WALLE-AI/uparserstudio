/// Hard budgets for untrusted document input.
#[derive(Debug, Clone)]
pub struct ResourceLimits {
    pub max_input_bytes: u64,
    pub max_entry_bytes: u64,
    pub max_total_uncompressed_bytes: u64,
    pub max_archive_entries: usize,
    pub max_xml_depth: usize,
    /// Nesting depth for binary record trees (OLE-based .doc/.ppt).
    /// Deliberately far tighter than the XML depth: these formats are walked
    /// by real recursion, and no legitimate document nests records anywhere
    /// near this deep.
    pub max_record_depth: usize,
    pub max_xml_nodes: usize,
    pub max_expansion: u64,
    pub max_asset_bytes: usize,
    pub max_text_bytes: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 256 * 1024 * 1024,
            max_entry_bytes: 128 * 1024 * 1024,
            max_total_uncompressed_bytes: 512 * 1024 * 1024,
            max_archive_entries: 100_000,
            max_xml_depth: 256,
            max_record_depth: 64,
            max_xml_nodes: 2_000_000,
            max_expansion: 4_000_000,
            max_asset_bytes: 128 * 1024 * 1024,
            max_text_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParseOptions {
    pub limits: ResourceLimits,
    pub include_assets: bool,
    pub include_notes: bool,
    pub include_headers_footers: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            limits: ResourceLimits::default(),
            include_assets: true,
            include_notes: true,
            include_headers_footers: false,
        }
    }
}
