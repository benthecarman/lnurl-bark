use axum::body::Body;
use axum::extract::{ConnectInfo, State as AxumState};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{http, Json};
use serde_json::json;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    requests_per_minute: u32,
    state: Mutex<RateLimiterState>,
}

struct RateLimiterState {
    clients: HashMap<IpAddr, ClientRateLimit>,
    last_cleanup: Instant,
}

struct ClientRateLimit {
    window_start: Instant,
    requests: u32,
    last_seen: Instant,
}

impl RateLimiter {
    pub fn new(requests_per_minute: u32) -> Self {
        Self {
            requests_per_minute,
            state: Mutex::new(RateLimiterState {
                clients: HashMap::new(),
                last_cleanup: Instant::now(),
            }),
        }
    }

    fn allow(&self, ip: IpAddr) -> bool {
        if self.requests_per_minute == 0 {
            return false;
        }

        let now = Instant::now();
        let window = Duration::from_secs(60);
        let mut state = self.state.lock().expect("rate limiter mutex poisoned");

        if now.duration_since(state.last_cleanup) >= window {
            state
                .clients
                .retain(|_, client| now.duration_since(client.last_seen) < window * 2);
            state.last_cleanup = now;
        }

        let client = state.clients.entry(ip).or_insert(ClientRateLimit {
            window_start: now,
            requests: 0,
            last_seen: now,
        });

        if now.duration_since(client.window_start) >= window {
            client.window_start = now;
            client.requests = 0;
        }

        client.last_seen = now;

        if client.requests >= self.requests_per_minute {
            return false;
        }

        client.requests += 1;
        true
    }
}

pub async fn rate_limit_middleware(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    AxumState(rate_limiter): AxumState<Arc<RateLimiter>>,
    req: http::Request<Body>,
    next: Next<Body>,
) -> Response {
    if req.uri().path() == "/health-check" || rate_limiter.allow(addr.ip()) {
        return next.run(req).await;
    }

    (
        http::StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "status": "ERROR",
            "reason": "Rate limit exceeded",
        })),
    )
        .into_response()
}
