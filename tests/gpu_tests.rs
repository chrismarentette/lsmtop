use lsmtop::gpu::GpuCache;
use std::time::{Duration, Instant};

#[test]
fn test_gpu_cache_transitions() {
    let mut cache = GpuCache::new(Duration::from_millis(900), Duration::from_secs(60));
    let t0 = Instant::now();
    
    // First call, fails -> 60s cooldown
    let mut called = false;
    let res = cache.get(t0, || { called = true; None });
    assert!(called);
    assert_eq!(res, None);
    
    // Second call within 60s -> cached fail (fetch not called)
    let t1 = t0 + Duration::from_secs(10);
    let mut called = false;
    let res = cache.get(t1, || { called = true; Some(50.0) });
    assert!(!called);
    assert_eq!(res, None);
    
    // Call after 60s cooldown expires -> fetch succeeds
    let t2 = t0 + Duration::from_secs(61);
    let mut called = false;
    let res = cache.get(t2, || { called = true; Some(50.0) });
    assert!(called);
    assert_eq!(res, Some(50.0));
    
    // Call within 900ms ok ttl -> cached value
    let t3 = t2 + Duration::from_millis(100);
    let mut called = false;
    let res = cache.get(t3, || { called = true; Some(99.0) });
    assert!(!called);
    assert_eq!(res, Some(50.0));
    
    // Call after ok ttl -> fetch again
    let t4 = t2 + Duration::from_secs(1);
    let mut called = false;
    let res = cache.get(t4, || { called = true; Some(99.0) });
    assert!(called);
    assert_eq!(res, Some(99.0));
}
