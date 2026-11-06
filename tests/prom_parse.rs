use lsmtop::net::prom::parse_prom;

#[test]
fn test_parse_prom() {
    let text = include_str!("fixtures/metrics.prom");
    let metrics = parse_prom(text);
    assert!(metrics.contains_key("llamacpp:prompt_tokens_total"));
    assert!(metrics.contains_key("llamacpp:tokens_predicted_total"));
    assert!(metrics.contains_key("llamacpp:requests_processing"));
    assert_eq!(metrics.get("llamacpp:prompt_tokens_total").copied(), Some(121315.0));

    // Test cases
    let cases = [
        "# comment",
        "foo 1.0",
        "bar{label=\"a b\"} 2.0 12345",
        "baz NaN",
        "qux 3.0",
        "qux 4.0", // last wins
    ].join("\n");
    let m = parse_prom(&cases);
    assert_eq!(m.get("foo").copied(), Some(1.0));
    assert_eq!(m.get("bar").copied(), Some(2.0));
    assert_eq!(m.get("baz"), None); // NaN rejected
    assert_eq!(m.get("qux").copied(), Some(4.0)); // last wins
}
