use borgchart::github::api;
use borgchart::github::model;
#[test]
fn pagination_rejects_foreign_routes_and_changed_queries() {
    let aliases = std::collections::BTreeMap::new();
    let p = "/notifications?all=true&page=1";
    assert_eq!(
        api::next_page(
            "<https://api.github.com/notifications?all=true&page=2>; rel=\"next\"",
            p,
            &aliases
        )
        .unwrap(),
        Some("/notifications?all=true&page=2".into())
    );
    for s in [
        "<https://evil.example/notifications?all=true&page=2>; rel=\"next\"",
        "<https://api.github.com/notifications?all=false&page=2>; rel=\"next\"",
        "<https://api.github.com/notifications?all=true&page=3>; rel=\"next\"",
    ] {
        assert!(api::next_page(s, p, &aliases).is_err());
    }
}
#[test]
fn header_parser_and_dates_are_strict() {
    assert!(api::parse(b"HTTP/2 200 OK\r\nDate: a\r\nDate: b\r\n\r\n{}").is_err());
    assert_eq!(
        api::http_date("Mon, 09 Nov 2026 22:00:00 GMT").unwrap() * 1000,
        model::stamp("2026-11-09T22:00:00Z").unwrap()
    );
    assert!(api::http_date("nonsense").is_err());
}
#[test]
fn repeated_vary_is_accepted() {
    assert!(
        api::parse(b"HTTP/2 200 OK\r\nVary: Accept\r\nVary: Accept-Encoding\r\n\r\n{}").is_ok()
    );
}
