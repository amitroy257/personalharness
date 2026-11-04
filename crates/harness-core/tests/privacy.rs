//! Privacy guarantees around public ranking data:
//! * "public data off" means no outbound requests at all;
//! * optional keys are sent only to the service they belong to;
//! * the anonymous sources receive no credentials or identifiers.

use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Router;
use harness_core::settings::RankingSettings;
use harness_core::signals::{refresh, SignalUrls, Signals};

type Log = Arc<Mutex<Vec<(String, Vec<(String, String)>)>>>;

async fn record(State(log): State<Log>, req: Request) -> impl IntoResponse {
    let headers = req.headers().iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string())).collect();
    log.lock().unwrap().push((req.uri().to_string(), headers));
    (StatusCode::NOT_FOUND, "nope")
}

async fn mock() -> (SignalUrls, Log) {
    let log: Log = Arc::default();
    let app = Router::new().fallback(record).with_state(log.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let urls = SignalUrls {
        arena_base: format!("{base}/arena"),
        hf_api: format!("{base}/hf/api/models"),
        or_models: format!("{base}/or/api/v1/models"),
        or_rankings: format!("{base}/or/api/v1/datasets/rankings-daily"),
        aa_models: format!("{base}/aa/api/v2/data/llms/models"),
    };
    (urls, log)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

#[tokio::test]
async fn public_data_off_makes_zero_requests() {
    let (urls, log) = mock().await;
    let cfg = RankingSettings {
        public_data: false,
        openrouter_key: "or-secret".into(),
        artificial_analysis_key: "aa-secret".into(),
        ..Default::default()
    };
    let s = refresh(&client(), &cfg, &Signals::default(), &urls).await;
    assert!(log.lock().unwrap().is_empty(), "requests were made: {:?}", log.lock().unwrap());
    assert!(!s.arena_code.is_empty(), "bundled snapshot still used offline");
}

#[tokio::test]
async fn keys_only_reach_their_own_service() {
    let (urls, log) = mock().await;
    let cfg = RankingSettings {
        public_data: true,
        openrouter_key: "or-secret".into(),
        artificial_analysis_key: "aa-secret".into(),
        ..Default::default()
    };
    let s = refresh(&client(), &cfg, &Signals::default(), &urls).await;
    let log = log.lock().unwrap();
    assert!(!log.is_empty());
    for (uri, headers) in log.iter() {
        let all = headers.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(";");
        let has_or = all.contains("or-secret");
        let has_aa = all.contains("aa-secret");
        if uri.starts_with("/or/api/v1/datasets/rankings-daily") {
            assert!(has_or && !has_aa, "{uri}: {all}");
        } else if uri.starts_with("/aa/") {
            assert!(has_aa && !has_or, "{uri}: {all}");
        } else {
            assert!(!has_or && !has_aa, "anonymous source got a key: {uri}: {all}");
            assert!(!headers.iter().any(|(k, _)| k == "authorization" || k == "cookie" || k == "x-api-key"), "{uri}: {all}");
        }
        // only a generic product User-Agent, nothing identifying the user
        let ua = headers.iter().find(|(k, _)| k == "user-agent").map(|(_, v)| v.as_str()).unwrap_or("");
        assert!(ua.starts_with("PersonalHarness/"), "{uri}: user-agent {ua}");
    }
    let paths: Vec<&str> = log.iter().map(|(u, _)| u.as_str()).collect();
    assert!(paths.iter().any(|p| p.starts_with("/arena/latest.json")));
    assert!(paths.iter().any(|p| p.starts_with("/hf/")));
    assert!(paths.iter().any(|p| p.starts_with("/or/api/v1/models")));
    // failing sources are reported, and the bundled leaderboard data is kept
    assert!(s.sources.iter().any(|x| x.id == "arena" && !x.ok));
    assert!(!s.arena_code.is_empty());
}
