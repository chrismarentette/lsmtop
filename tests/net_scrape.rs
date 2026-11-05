use lsmtop::net::Scraper;

#[test]
fn test_scrape_server_unreachable() {
    let scraper = Scraper::new();
    let res = scraper.scrape_server("127.0.0.1", 1);
    assert!(res.is_none());
}
