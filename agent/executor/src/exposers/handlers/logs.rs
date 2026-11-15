use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use futures::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::exposers::state::*;

#[derive(serde::Deserialize)]
pub(crate) struct LogQuery {
    follow: Option<bool>,
    #[serde(rename = "tailLines")]
    tail_lines: Option<i64>,
    #[serde(rename = "sinceSeconds")]
    since_seconds: Option<i64>,
    #[serde(rename = "sinceTime")]
    since_time: Option<String>,
    timestamps: Option<bool>,
    /// Dashboards send this for crash-looping containers.
    previous: Option<bool>,
    /// Accepted and ignored (we do not truncate mid-line).
    #[serde(rename = "limitBytes")]
    limit_bytes: Option<i64>,
}

pub(crate) async fn container_logs(
    State(state): State<KubeletState>,
    Path((namespace, pod_id, container_name)): Path<(String, String, String)>,
    Query(q): Query<LogQuery>,
) -> Response {
    let key = format!("{}/{}/{}", namespace, pod_id, container_name);
    let tail = q.tail_lines.unwrap_or(200).clamp(0, 10_000) as usize;
    let timestamps = q.timestamps.unwrap_or(false);
    let _ = q.limit_bytes;

    // `--previous` now reads the rotated file (the container instance that ran
    // before the current one).
    if q.previous.unwrap_or(false) {
        let prev = state.logs.tail_previous(&key, tail).await;
        if prev.is_empty() {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header("Content-Type", "text/plain")
                .body(Body::from(
                    "previous terminated container \"\".log not found: \
                     no earlier instance of this container was recorded\n",
                ))
                .unwrap();
        }
        let body = prev
            .iter()
            .map(|l| l.render(timestamps) + "\n")
            .collect::<String>();
        return Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/plain")
            .body(Body::from(body))
            .unwrap();
    }

    // sinceSeconds / sinceTime are honoured so dashboards that ask for "last 5
    // minutes" do not get the whole buffer.
    let now = chrono::Utc::now();
    let since = if let Some(secs) = q.since_seconds {
        Some(now - chrono::Duration::seconds(secs.max(0)))
    } else if let Some(ref ts) = q.since_time {
        chrono::DateTime::parse_from_rfc3339(ts)
            .ok()
            .map(|t| t.with_timezone(&chrono::Utc))
    } else {
        None
    };

    let initial = state.logs.tail(&key, tail, since).await;
    // After an agent restart the in-memory buffer is empty but the file still
    // has the output - read it so `kubectl logs` is not blank.
    let initial = if initial.is_empty() {
        let from_file = state.logs.tail_file(&key, tail).await;
        if since.is_none() {
            from_file
        } else {
            from_file
                .into_iter()
                .filter(|l| since.is_some_and(|s| l.at >= s))
                .collect()
        }
    } else {
        initial
    };

    if q.follow.unwrap_or(false) {
        let rx = state.logs.subscribe(&key).await;
        let head = initial
            .iter()
            .map(|l| l.render(timestamps) + "\n")
            .collect::<String>();
        let head_stream =
            futures::stream::once(async move { Ok::<_, std::io::Error>(bytes::Bytes::from(head)) });
        // Ends the stream on EOF so dashboards stop "tailing forever".
        let live = BroadcastStream::new(rx)
            .filter_map(move |item| async move {
                match item {
                    Ok(line) => Some((line.eof, line.render(timestamps))),
                    // Lagged: the follower missed lines, keep the connection open.
                    Err(_) => None,
                }
            })
            .take_while(|(eof, _)| futures::future::ready(!*eof))
            .map(|(_, text)| Ok::<_, std::io::Error>(bytes::Bytes::from(text + "\n")));
        return Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/plain")
            .body(Body::from_stream(head_stream.chain(live)))
            .unwrap();
    }

    let output = initial
        .iter()
        .map(|l| l.render(timestamps) + "\n")
        .collect::<String>();
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/plain")
        .body(Body::from(output))
        .unwrap()
}
