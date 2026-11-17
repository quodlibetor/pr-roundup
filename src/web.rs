//! The page, its fragments and the endpoints that change tags and order.

use std::{convert::Infallible, sync::Arc};

use axum::{
    Form, Json, Router,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use maud::{DOCTYPE, Markup, html};
use serde::Deserialize;
use tokio_stream::{Stream, StreamExt, wrappers::BroadcastStream};

use crate::{app::App, board, model::Pr};

const HTMX: &str = include_str!("../assets/htmx.min.js");
const SSE: &str = include_str!("../assets/htmx-ext-sse.js");
const APP_JS: &str = include_str!("../assets/app.js");
const STYLE: &str = include_str!("../assets/style.css");

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/board", get(board_fragment))
        .route("/events", get(events))
        .route("/tag", post(set_tag))
        .route("/order", post(set_order))
        .route("/refresh", post(refresh))
        .route("/assets/{file}", get(asset))
        .with_state(app)
}

struct WebError(color_eyre::Report);

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        tracing::warn!("request failed: {:?}", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, format!("{:#}", self.0)).into_response()
    }
}

impl<E: Into<color_eyre::Report>> From<E> for WebError {
    fn from(error: E) -> Self {
        WebError(error.into())
    }
}

type WebResult<T> = Result<T, WebError>;

async fn index(State(app): State<Arc<App>>) -> WebResult<Markup> {
    let board = render_board(&app)?;
    Ok(html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "PR links" }
                link rel="stylesheet" href="/assets/style.css";
                script src="/assets/htmx.min.js" {}
                script src="/assets/htmx-ext-sse.js" {}
                script src="/assets/app.js" defer {}
            }
            body hx-ext="sse" sse-connect="/events" {
                header {
                    h1 { "Open pull requests" }
                    button.refresh hx-post="/refresh" hx-swap="none" { "Refresh now" }
                }
                (board)
            }
        }
    })
}

async fn board_fragment(State(app): State<Arc<App>>) -> WebResult<Markup> {
    Ok(render_board(&app)?)
}

fn render_board(app: &App) -> color_eyre::Result<Markup> {
    let board = app.board()?;
    let live_tags = serde_json::to_string(&board.live_tags())?;
    let (fetched_at, error) = {
        let latest = app
            .latest
            .read()
            .map_err(|_| color_eyre::eyre::eyre!("lock poisoned"))?;
        (latest.fetched_at, latest.error.clone())
    };
    Ok(html! {
        div #board data-tags=(live_tags) hx-get="/board" hx-trigger="sse:update, reload" hx-swap="outerHTML" {
            p.status {
                @match fetched_at {
                    Some(at) => { "Updated " time data-epoch=(at) { (at) } }
                    None => { "Waiting for the first poll of GitHub…" }
                }
            }
            @if let Some(error) = error {
                pre.error { (error) }
            }
            @if board.sections.is_empty() {
                p.empty { "No open pull requests." }
            }
            @for section in &board.sections {
                @let copied = board::copy(section, &app.config.emoji);
                section.tag-section data-tag=(section.order_key()) {
                    div.section-head {
                        h2 {
                            @match &section.tag {
                                Some(tag) => { (tag) }
                                None => { span.untagged { "Untagged" } }
                            }
                        }
                        button.copy type="button" data-text=(copied.text) data-html=(copied.html) {
                            "Copy for Slack"
                        }
                    }
                    div.repos {
                        @for group in &section.repos {
                            div.repo data-repo=(group.repo) {
                                h3.repo-head draggable="true" title="Drag to reorder" {
                                    span.grip { "⠿" } (group.repo)
                                }
                                @for stack in &group.stacks {
                                    ul.stack .multi[stack.len() > 1] {
                                        @for pr in stack {
                                            (pr_row(pr, section.tag.as_deref()))
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

fn pr_row(pr: &Pr, tag: Option<&str>) -> Markup {
    let untag = serde_json::json!({ "id": pr.id, "tag": "" }).to_string();
    html! {
        li.pr.(pr.status.css_class()) {
            span.icon title=(pr.status.label()) { (pr.status.icon()) }
            a.title href=(pr.url) target="_blank" rel="noopener" { (pr.title) }
            span.num { "#" (pr.number) }
            @if pr.draft { span.badge { "draft" } }
            @if pr.is_finished() { span.badge.(pr.status.css_class()) { (pr.status.label()) } }
            form.tag hx-post="/tag" hx-trigger="change, submit" hx-target="#board" hx-swap="outerHTML" {
                input type="hidden" name="id" value=(pr.id);
                // assets/app.js turns this into a combobox offering the
                // tags in #board's data-tags.
                input type="text" name="tag" autocomplete="off" role="combobox"
                    aria-autocomplete="list" aria-expanded="false"
                    placeholder="+ tag" value=[tag];
            }
            @if tag.is_some() {
                button.untag type="button" title="Remove tag" hx-post="/tag" hx-vals=(untag)
                    hx-target="#board" hx-swap="outerHTML" { "×" }
            }
        }
    }
}

async fn events(State(app): State<Arc<App>>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // A lagged receiver missed some notices, which still means "update".
    // The data is never read, but EventSource drops an event whose data is
    // empty, so there has to be some.
    let stream = BroadcastStream::new(app.updates.subscribe())
        .map(|_| Ok(Event::default().event("update").data("changed")));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
struct TagForm {
    id: String,
    tag: String,
}

async fn set_tag(State(app): State<Arc<App>>, Form(form): Form<TagForm>) -> WebResult<Response> {
    let tag = form.tag.trim();
    if tag.is_empty() {
        app.store()?.clear_tag(&form.id)?;
    } else {
        let Some(pr) = app.find(&form.id)? else {
            return Ok((StatusCode::NOT_FOUND, "no such pull request").into_response());
        };
        app.store()?.set_tag(&pr, tag)?;
    }
    app.changed();
    Ok(render_board(&app)?.into_response())
}

#[derive(Deserialize)]
struct OrderBody {
    tag: String,
    repos: Vec<String>,
}

async fn set_order(
    State(app): State<Arc<App>>,
    Json(body): Json<OrderBody>,
) -> WebResult<StatusCode> {
    app.store()?.set_repo_order(&body.tag, &body.repos)?;
    app.changed();
    Ok(StatusCode::NO_CONTENT)
}

async fn refresh(State(app): State<Arc<App>>) -> StatusCode {
    app.refresh.notify_one();
    StatusCode::NO_CONTENT
}

async fn asset(Path(file): Path<String>) -> Response {
    let (body, kind) = match file.as_str() {
        "htmx.min.js" => (HTMX, "text/javascript"),
        "htmx-ext-sse.js" => (SSE, "text/javascript"),
        "app.js" => (APP_JS, "text/javascript"),
        "style.css" => (STYLE, "text/css"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, kind)], body).into_response()
}
