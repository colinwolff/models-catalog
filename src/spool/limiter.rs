use std::time::Duration;

use chrono::Utc;
use tracing::{debug, info, warn};

use crate::config::{RateLimitConfig, SpoolConfig};
use crate::error::CatalogError;
use crate::spool::disk::SourceSpoolMeta;

/// Result of checking or fetching a single upstream source.
#[derive(Debug, Clone)]
pub enum SourceFetchOutcome {
    /// Upstream returned `200 OK` with a new JSON body.
    Live { body: String, meta: SourceSpoolMeta },
    /// Upstream returned `304 Not Modified` matching our `ETag` / `Last-Modified`.
    NotModified304 { meta: SourceSpoolMeta },
    /// Skipped outbound network call because disk TTL or `min_refresh_cooldown` is still active.
    CooldownOrFresh { meta: SourceSpoolMeta },
}

/// Checks cooldown/TTL and performs a conditional HTTP `GET` (`If-None-Match` / `If-Modified-Since`)
/// with exponential backoff + jitter on `429` and `5xx`.
#[cfg(feature = "reqwest-client")]
pub async fn fetch_upstream_with_policy(
    client: &reqwest::Client,
    source_name: &str,
    url: &str,
    prev_meta: &SourceSpoolMeta,
    has_cached_payload: bool,
    config: &SpoolConfig,
    force_refresh: bool,
) -> Result<SourceFetchOutcome, CatalogError> {
    let now = Utc::now();

    if !force_refresh && has_cached_payload {
        // 1. Check hard rate-limit cooldown (`min_refresh_cooldown`) first
        if let Some(last_attempt) = prev_meta.last_attempt_at.or(prev_meta.last_verified_at) {
            if let Ok(elapsed) = (now - last_attempt).to_std() {
                if elapsed < config.rate_limit.min_refresh_cooldown {
                    debug!(
                        source = source_name,
                        ?elapsed,
                        cooldown = ?config.rate_limit.min_refresh_cooldown,
                        "skipping network fetch due to min_refresh_cooldown"
                    );
                    return Ok(SourceFetchOutcome::CooldownOrFresh {
                        meta: prev_meta.clone(),
                    });
                }
            }
        }

        // 2. Check disk TTL freshness
        if let Some(last_verified) = prev_meta.last_verified_at.or(prev_meta.last_fetched_at) {
            if let Ok(elapsed) = (now - last_verified).to_std() {
                if elapsed < config.disk_ttl {
                    debug!(
                        source = source_name,
                        ?elapsed,
                        disk_ttl = ?config.disk_ttl,
                        "using fresh disk-spooled payload without network check"
                    );
                    return Ok(SourceFetchOutcome::CooldownOrFresh {
                        meta: prev_meta.clone(),
                    });
                }
            }
        }
    } else if !force_refresh && !has_cached_payload {
        // Even if no cached payload exists, respect min_refresh_cooldown if we just attempted and failed
        if let Some(last_attempt) = prev_meta.last_attempt_at {
            if let Ok(elapsed) = (now - last_attempt).to_std() {
                if elapsed < config.rate_limit.min_refresh_cooldown {
                    let retry_after = last_attempt
                        + chrono::Duration::from_std(config.rate_limit.min_refresh_cooldown)
                            .unwrap_or_else(|_| chrono::Duration::minutes(5));
                    return Err(CatalogError::RateLimited {
                        source_name: source_name.to_string(),
                        retry_after,
                    });
                }
            }
        }
    }

    let mut attempt: u32 = 0;
    loop {
        let mut req = client.get(url).timeout(config.rate_limit.request_timeout);

        if has_cached_payload {
            if let Some(etag) = prev_meta.etag.as_deref().filter(|s| !s.is_empty()) {
                req = req.header(reqwest::header::IF_NONE_MATCH, etag);
            }
            if let Some(lm) = prev_meta.last_modified.as_deref().filter(|s| !s.is_empty()) {
                req = req.header(reqwest::header::IF_MODIFIED_SINCE, lm);
            }
        }

        let attempt_time = Utc::now();
        let response_res = req.send().await;

        match response_res {
            Ok(resp) => {
                let status = resp.status();
                if status == reqwest::StatusCode::NOT_MODIFIED {
                    info!(source = source_name, "upstream returned 304 Not Modified");
                    let mut updated = prev_meta.clone();
                    updated.last_verified_at = Some(attempt_time);
                    updated.last_attempt_at = Some(attempt_time);
                    return Ok(SourceFetchOutcome::NotModified304 { meta: updated });
                }

                if status.is_success() {
                    let etag = resp
                        .headers()
                        .get(reqwest::header::ETAG)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string)
                        .or_else(|| prev_meta.etag.clone());

                    let last_modified = resp
                        .headers()
                        .get(reqwest::header::LAST_MODIFIED)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string)
                        .or_else(|| prev_meta.last_modified.clone());

                    let body = resp.text().await.map_err(|source| CatalogError::Http {
                        source_name: source_name.to_string(),
                        source,
                    })?;

                    let updated = SourceSpoolMeta {
                        etag,
                        last_modified,
                        last_fetched_at: Some(attempt_time),
                        last_verified_at: Some(attempt_time),
                        last_attempt_at: Some(attempt_time),
                    };

                    info!(
                        source = source_name,
                        bytes = body.len(),
                        "fetched live upstream catalog payload"
                    );
                    return Ok(SourceFetchOutcome::Live {
                        body,
                        meta: updated,
                    });
                }

                let is_retryable =
                    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error();

                if is_retryable && attempt < config.rate_limit.max_retries {
                    let delay = compute_backoff_delay(&config.rate_limit, attempt);
                    warn!(
                        source = source_name,
                        status = status.as_u16(),
                        attempt = attempt + 1,
                        max_retries = config.rate_limit.max_retries,
                        ?delay,
                        "retryable upstream HTTP status; backing off"
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                    continue;
                }

                return Err(CatalogError::HttpStatus {
                    source_name: source_name.to_string(),
                    status: status.as_u16(),
                });
            }
            Err(err) => {
                if attempt < config.rate_limit.max_retries {
                    let delay = compute_backoff_delay(&config.rate_limit, attempt);
                    warn!(
                        source = source_name,
                        error = %err,
                        attempt = attempt + 1,
                        max_retries = config.rate_limit.max_retries,
                        ?delay,
                        "network error fetching upstream; backing off"
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                    continue;
                }

                return Err(CatalogError::Http {
                    source_name: source_name.to_string(),
                    source: err,
                });
            }
        }
    }
}

#[cfg(feature = "reqwest-client")]
fn compute_backoff_delay(cfg: &RateLimitConfig, attempt: u32) -> Duration {
    let shift = attempt.min(10);
    let factor = 1u32 << shift;
    let base = cfg.initial_backoff.saturating_mul(factor);
    let capped = std::cmp::min(base, cfg.max_backoff);

    let jitter_ms = if capped.as_millis() > 0 {
        let max_jitter = (capped.as_millis() as u64 / 4).max(1);
        rand::random_range(0..=max_jitter)
    } else {
        0
    };

    std::cmp::min(
        capped.saturating_add(Duration::from_millis(jitter_ms)),
        cfg.max_backoff,
    )
}
