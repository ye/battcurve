//! Local web server exposing the analysis curves as JSON + a uPlot frontend.

use crate::core::analysis::{self, SessionKind};
use crate::core::sample::Sample;
use crate::core::storage::{self, Backend};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Html,
    routing::get,
    Json, Router,
};
use serde::Serialize;

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

fn load(backend: Backend) -> Result<Vec<Sample>, (StatusCode, String)> {
    let mut samples = storage::open(backend)
        .and_then(|s| s.read_all())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    analysis::fill_derived_power(&mut samples);
    Ok(samples)
}

pub async fn run(port: u16, backend: Backend) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/api/samples", get(samples))
        .route("/api/sessions", get(sessions))
        .route("/api/session/:id", get(session_detail))
        .route("/api/health", get(health))
        .with_state(backend);

    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("battcurve: serving http://{addr}  (Ctrl-C to stop)");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

async fn samples(State(backend): State<Backend>) -> ApiResult<Vec<Sample>> {
    Ok(Json(load(backend)?))
}

#[derive(Serialize)]
struct SessionMeta {
    id: usize,
    kind: SessionKind,
    start_ts: i64,
    end_ts: i64,
    start_soc: f64,
    end_soc: f64,
    duration_secs: i64,
    sample_count: usize,
}

async fn sessions(State(backend): State<Backend>) -> ApiResult<Vec<SessionMeta>> {
    let all = load(backend)?;
    let metas = analysis::segment_sessions(&all)
        .into_iter()
        .map(|s| SessionMeta {
            id: s.id,
            kind: s.kind,
            start_ts: s.start_ts,
            end_ts: s.end_ts,
            start_soc: s.start_soc,
            end_soc: s.end_soc,
            duration_secs: s.duration_secs(),
            sample_count: s.samples.len(),
        })
        .collect();
    Ok(Json(metas))
}

#[derive(Serialize)]
struct SessionDetail {
    meta: SessionMeta,
    samples: Vec<Sample>,
    dq_dv: Vec<analysis::DqDvPoint>,
    cc_cv: Option<analysis::CcCv>,
}

async fn session_detail(
    State(backend): State<Backend>,
    Path(id): Path<usize>,
) -> ApiResult<SessionDetail> {
    let all = load(backend)?;
    let sess = analysis::segment_sessions(&all)
        .into_iter()
        .find(|s| s.id == id)
        .ok_or((StatusCode::NOT_FOUND, format!("no session {id}")))?;
    let cc_cv = (sess.kind == SessionKind::Charge).then(|| analysis::detect_cc_cv(&sess));
    Ok(Json(SessionDetail {
        meta: SessionMeta {
            id: sess.id,
            kind: sess.kind,
            start_ts: sess.start_ts,
            end_ts: sess.end_ts,
            start_soc: sess.start_soc,
            end_soc: sess.end_soc,
            duration_secs: sess.duration_secs(),
            sample_count: sess.samples.len(),
        },
        dq_dv: analysis::dq_dv(&sess, 5),
        cc_cv,
        samples: sess.samples,
    }))
}

async fn health(State(backend): State<Backend>) -> ApiResult<Option<analysis::HealthSummary>> {
    Ok(Json(analysis::health_summary(&load(backend)?)))
}
