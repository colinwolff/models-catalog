#![cfg(all(feature = "reqwest-client", feature = "disk-spool"))]

use std::sync::Arc;
use std::time::Duration;

use models_catalog::{CatalogManager, DataOrigin, RateLimitConfig};
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SAMPLE_MODELS_DEV: &str = include_str!("fixtures/models-dev.sample.json");
const SAMPLE_CLINE_FEED: &str = include_str!("fixtures/cline-recommended.sample.json");

#[tokio::test]
async fn twenty_concurrent_get_catalog_calls_coalesce_to_single_http_request() {
    let server = MockServer::start().await;
    let temp_dir = TempDir::new().expect("temp dir");

    Mock::given(method("GET"))
        .and(path("/models-dev.json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(SAMPLE_MODELS_DEV)
                .insert_header("ETag", "\"md-etag-1\"")
                .set_delay(Duration::from_millis(60)),
        )
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/recommended-models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(SAMPLE_CLINE_FEED)
                .insert_header("ETag", "\"cf-etag-1\"")
                .set_delay(Duration::from_millis(60)),
        )
        .expect(1)
        .mount(&server)
        .await;

    let manager = CatalogManager::builder()
        .with_spool_dir(Some(temp_dir.path().to_path_buf()))
        .with_models_dev_url(format!("{}/models-dev.json", server.uri()))
        .with_cline_recommended_url(format!("{}/recommended-models", server.uri()))
        .with_bundled_fallback(false)
        .build()
        .expect("build manager");

    let mut handles = Vec::new();
    for _ in 0..20 {
        let mgr = manager.clone();
        handles.push(tokio::spawn(async move { mgr.get_catalog().await }));
    }

    for handle in handles {
        let catalog = handle
            .await
            .expect("task join")
            .expect("get_catalog succeeds");
        assert!(catalog.get_model("anthropic", "claude-sonnet-5").is_some());
        assert!(
            catalog
                .get_model("cline-pass", "cline-pass/minimax/minimax-m2.5")
                .is_some()
        );
    }

    // Wiremock's `.expect(1)` on both mocks verifies on drop that exactly 1 request hit each endpoint.
}

#[tokio::test]
async fn cooldown_prevents_network_io_and_304_updates_meta_without_reparse() {
    let server = MockServer::start().await;
    let temp_dir = TempDir::new().expect("temp dir");

    // Initial 200 OK responses with ETags
    Mock::given(method("GET"))
        .and(path("/models-dev.json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(SAMPLE_MODELS_DEV)
                .insert_header("ETag", "\"md-v1\""),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/recommended-models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(SAMPLE_CLINE_FEED)
                .insert_header("ETag", "\"cf-v1\""),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Subsequent conditional requests with If-None-Match return 304 Not Modified (with empty body!)
    Mock::given(method("GET"))
        .and(path("/models-dev.json"))
        .and(header("if-none-match", "\"md-v1\""))
        .respond_with(ResponseTemplate::new(304))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/recommended-models"))
        .and(header("if-none-match", "\"cf-v1\""))
        .respond_with(ResponseTemplate::new(304))
        .expect(1)
        .mount(&server)
        .await;

    // Set memory_ttl = 0 and stale_while_revalidate = false so every get_catalog() enters the spool/cooldown check
    let manager = CatalogManager::builder()
        .with_spool_dir(Some(temp_dir.path().to_path_buf()))
        .with_memory_ttl(Duration::ZERO)
        .with_disk_ttl(Duration::ZERO)
        .with_min_refresh_cooldown(Duration::from_secs(300))
        .with_stale_while_revalidate(false)
        .with_models_dev_url(format!("{}/models-dev.json", server.uri()))
        .with_cline_recommended_url(format!("{}/recommended-models", server.uri()))
        .build()
        .expect("build manager");

    // 1. Cold start fetches live 200 OK
    let snap1 = manager.get_catalog().await.expect("initial fetch");
    assert_eq!(snap1.sources["models_dev"].origin, DataOrigin::Live);
    assert_eq!(
        snap1.sources["models_dev"].etag.as_deref(),
        Some("\"md-v1\"")
    );

    // 2. Call within min_refresh_cooldown does NOT trigger network I/O
    let snap2 = manager.get_catalog().await.expect("cooldown fetch");
    assert_eq!(snap2.sources["models_dev"].origin, DataOrigin::DiskSpool);

    let requests_so_far = server.received_requests().await.expect("requests").len();
    assert_eq!(
        requests_so_far, 2,
        "only the initial 2 requests should have been sent"
    );

    // 3. Explicit `refresh(true)` bypasses cooldown, sends If-None-Match, and handles 304 Not Modified
    // without re-downloading or re-parsing!
    let snap3 = manager.refresh(true).await.expect("forced 304 refresh");
    assert_eq!(
        snap3.sources["models_dev"].origin,
        DataOrigin::NotModified304
    );
    assert_eq!(
        snap3.sources["cline_recommended"].origin,
        DataOrigin::NotModified304
    );
    assert!(
        snap3.sources["models_dev"].last_verified_at
            >= snap1.sources["models_dev"].last_verified_at
    );
}

#[tokio::test]
async fn retries_on_429_with_backoff_and_succeeds() {
    let server = MockServer::start().await;
    let temp_dir = TempDir::new().expect("temp dir");

    // First 2 calls to models-dev return 429, 3rd returns 200 OK
    Mock::given(method("GET"))
        .and(path("/models-dev.json"))
        .respond_with(ResponseTemplate::new(429))
        .up_to_n_times(2)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/models-dev.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_MODELS_DEV))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/recommended-models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_CLINE_FEED))
        .expect(1)
        .mount(&server)
        .await;

    let manager = CatalogManager::builder()
        .with_spool_dir(Some(temp_dir.path().to_path_buf()))
        .with_models_dev_url(format!("{}/models-dev.json", server.uri()))
        .with_cline_recommended_url(format!("{}/recommended-models", server.uri()))
        .with_rate_limit(RateLimitConfig {
            min_refresh_cooldown: Duration::from_secs(60),
            max_retries: 3,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
            request_timeout: Duration::from_secs(5),
        })
        .build()
        .expect("build manager");

    let snap = manager
        .get_catalog()
        .await
        .expect("should recover after 429 retries");
    assert!(!snap.degraded);
    assert!(snap.get_model("anthropic", "claude-sonnet-5").is_some());
}

#[tokio::test]
async fn degrades_gracefully_when_undocumented_cline_feed_is_unavailable() {
    let server = MockServer::start().await;
    let temp_dir = TempDir::new().expect("temp dir");

    Mock::given(method("GET"))
        .and(path("/models-dev.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_MODELS_DEV))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/recommended-models"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let manager = CatalogManager::builder()
        .with_spool_dir(Some(temp_dir.path().to_path_buf()))
        .with_models_dev_url(format!("{}/models-dev.json", server.uri()))
        .with_cline_recommended_url(format!("{}/recommended-models", server.uri()))
        .with_rate_limit(RateLimitConfig {
            min_refresh_cooldown: Duration::from_secs(60),
            max_retries: 1,
            initial_backoff: Duration::from_millis(5),
            max_backoff: Duration::from_millis(10),
            request_timeout: Duration::from_secs(5),
        })
        .build()
        .expect("build manager");

    let snap: Arc<_> = manager
        .get_catalog()
        .await
        .expect("should degrade gracefully when cline feed returns 503");
    assert!(snap.degraded);
    assert!(snap.get_model("anthropic", "claude-sonnet-5").is_some());
    assert!(
        snap.sources["cline_recommended"].warning.is_some(),
        "cline_recommended source status should record warning"
    );
}
