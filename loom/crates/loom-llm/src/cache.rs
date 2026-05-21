//! `CacheControl` — typed per-content-block prompt-cache markers. The
//! TTL set matches Anthropic's prompt-cache breakpoint API; providers
//! that do not support typed per-block cache markers no-op the marker
//! without error.

/// Cache-control marker attached per content block. `None` is the
/// default; `Ephemeral(CacheTtl)` requests a cache breakpoint with the
/// named lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheControl {
    /// No cache marker on this block.
    #[default]
    None,
    /// Request an ephemeral cache entry with the given lifetime. The
    /// underlying provider treats the boundary as a breakpoint; cache
    /// reads on subsequent matching prefixes return cached bytes.
    Ephemeral(CacheTtl),
}

/// Supported cache lifetimes — matches Anthropic's documented set.
/// Other providers no-op a value they cannot honour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheTtl {
    Minutes5,
    Hours1,
    Hours24,
}
