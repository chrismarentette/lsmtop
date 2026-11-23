use lsmtop::net::child_ports;
use serde_json::json;

#[test]
fn test_child_ports() {
    let models = serde_json::from_str::<serde_json::Value>(
        include_str!("fixtures/models.json")
    ).unwrap();
    let mut ports = child_ports(&models);
    ports.sort();
    assert_eq!(ports, vec![("gemma-4-e4b-MTP-128k".to_string(), 37347)]);

    let native = json!({
        "data": [
            {
                "id": "foo",
                "object": "model"
            }
        ]
    });
    assert_eq!(child_ports(&native), vec![]);
}
