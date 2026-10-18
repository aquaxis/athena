use super::*;

#[test]
fn the_defaults_and_parsing_hold() {
    let _ = None::<String>;
    // R-API-06: `?limit=&cursor=`.
    let none = Query::parse("");
    assert_eq!(none.limit, DEFAULT_LIMIT);
    assert_eq!(none.cursor, None);

    let both = Query::parse("limit=5&cursor=13");
    assert_eq!(both.limit, 5);
    assert_eq!(both.cursor, Some(13));

    // `limit=0` is refused as a shape (positive page sizes only) and falls to default.
    assert_eq!(Query::parse("limit=0").limit, DEFAULT_LIMIT);
    // Malformed values fall through too.
    assert_eq!(Query::parse("limit=abc&cursor=abc").cursor, None);
}

#[test]
fn the_first_and_later_pages_walk_the_row_order() {
    let total: Vec<u32> = (0..250).collect();
    let first = slice(&total, &Query::parse("limit=100"));
    assert_eq!(first.items.len(), 100);
    assert_eq!(first.next_cursor, Some(100.to_string()));
    let later = slice(&total, &Query::parse("limit=100&cursor=100"));
    assert_eq!(later.items[0], 100);
    assert_eq!(later.next_cursor, Some(200.to_string()));
    let tail = slice(&total, &Query::parse("limit=100&cursor=200"));
    assert_eq!(tail.items.len(), 50);
    assert_eq!(tail.next_cursor, None, "the end has no cursor");
    // The same walk reproduces the whole set: first page + middle page + tail page.
    let middle = slice(&total, &Query::parse("limit=100&cursor=100"));
    let mut collected = first.items.clone();
    collected.extend(middle.items.clone());
    collected.extend(tail.items.clone());
    collected.sort();
    assert_eq!(collected, total);
}

#[test]
fn the_walk_beyond_the_end_has_no_cursor() {
    let total: Vec<u32> = (0..5).collect();
    let beyond = slice(&total, &Query::parse("limit=10&cursor=20"));
    assert!(beyond.items.is_empty());
    assert_eq!(beyond.next_cursor, None);
}

#[test]
fn the_body_shape_names_items_and_next_cursor() {
    // ATH-API-005: cursor pagination, `next_cursor` in the response.
    let page = Page {
        items: vec!["a", "b"],
        next_cursor: Some("2".to_string()),
    };
    let encoded = super::body(&page);
    assert!(encoded.get("items").is_some());
    assert_eq!(encoded["next_cursor"], serde_json::json!("2"));
    let last = super::body(&Page {
        items: vec!["a"],
        next_cursor: None,
    });
    assert_eq!(last["next_cursor"], serde_json::Value::Null);
}
