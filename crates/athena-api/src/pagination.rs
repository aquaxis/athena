//! Cursor pagination (P1-39; R-API-06, ATH-API-005).
//!
//! List endpoints answer `?limit=&cursor=...` and carry `next_cursor` in the response.
//! The cursor is the *last served* item's ordinal in the concrete page serialization —
//! opaque to the client (no semantics they can rely on beyond "stability ordering"),
//! stable while the underlying collection is. Adding an item earlier in the fold may
//! shift cursors — that is what cursor pagination is *for*: the response always tells
//! what to ask for next.
//!
//! `Page<T>` rides in `athena-persistence::repository` (the store's row list is the
//! authority); this module is the HTTP-side projection: parse the query, slice, encode.

use athena_core::Page;
use serde_json::{Value, json};
use std::collections::HashMap;

/// The default page size when the caller omits `limit` (spec §22.3's baseline row).
pub const DEFAULT_LIMIT: u64 = 100;

/// The parsed query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Query {
    /// How many items to serve.
    pub limit: u64,
    /// The opaque cursor from the previous response, when any.
    pub cursor: Option<u64>,
}

impl Query {
    /// `?limit=&cursor=` — both optional; `limit=0` is refused by the caller's own shape
    /// check (R-API-06 allows only positive page sizes).
    #[must_use]
    pub fn parse(query: &str) -> Self {
        let pairs: HashMap<&str, &str> = query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .collect();
        let limit = pairs
            .get("limit")
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|limit| *limit > 0)
            .unwrap_or(DEFAULT_LIMIT);
        let cursor = pairs.get("cursor").and_then(|raw| raw.parse::<u64>().ok());
        Self { limit, cursor }
    }
}

/// Slice one full page, anchoring on the cursor (the first served item is the one the
/// previous answer addressed as `next_cursor`).
///
/// The set shape must behave like a row count the *store* returned (stable within one
/// pagination walk); `None` cursor = from the start.
#[must_use]
pub fn slice<T: Clone>(total: &[T], query: &Query) -> Page<T> {
    let start = query.cursor.unwrap_or(0) as usize;
    let end = start.saturating_add(query.limit as usize);
    let items: Vec<T> = total
        .iter()
        .skip(start)
        .take(query.limit as usize)
        .cloned()
        .collect();
    let next_cursor = if end < total.len() {
        Some(end.to_string())
    } else {
        None
    };
    Page { items, next_cursor }
}

/// The response body for one page.
#[must_use]
pub fn body<T: serde::Serialize>(page: &Page<T>) -> Value {
    let next = page.next_cursor.as_ref().map(|cursor| json!(cursor));
    json!({
        "items": page.items,
        "next_cursor": next,
    })
}

#[cfg(test)]
#[path = "pagination_tests.rs"]
mod tests;
