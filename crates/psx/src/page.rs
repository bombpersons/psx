//! Shared response pagination; completeness here concerns returned rows, not format coverage.
use serde_json::{json, Value};

pub fn metadata(total: usize, offset: usize, returned: usize, limit: usize) -> Value {
    let next = offset.saturating_add(returned);
    let more = next < total;
    json!({"total":total,"offset":offset,"returned":returned,"limit":limit,
        "truncated":more,"complete":offset==0 && !more,"next_offset":more.then_some(next)})
}

pub fn validate(limit: usize) -> Result<(), String> {
    if (1..=1000).contains(&limit) {
        Ok(())
    } else {
        Err("limit must be 1..=1000".into())
    }
}

pub fn default_limit() -> usize {
    100
}
