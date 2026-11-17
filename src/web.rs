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
        .route("/check", post(check))
        .route("/include-approved", post(set_include_approved))
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
    let board = render_board(&app, false)?;
    let controls = render_controls(&app, false)?;
    Ok(html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "PR roundup" }
                link rel="stylesheet" href="/assets/style.css";
                script src="/assets/htmx.min.js" {}
                script src="/assets/htmx-ext-sse.js" {}
                script src="/assets/app.js" defer {}
            }
            body hx-ext="sse" sse-connect="/events" {
                header {
                    h1 { "Open pull requests" }
                    (controls)
                }
                (board)
            }
        }
    })
}

async fn board_fragment(State(app): State<Arc<App>>) -> WebResult<Markup> {
    Ok(render_board(&app, true)?)
}

/// The header's buttons and when GitHub was last polled. A board fragment
/// carries it out of band, since the header itself is never swapped.
fn render_controls(app: &App, oob: bool) -> color_eyre::Result<Markup> {
    let fetched_at = app
        .latest
        .read()
        .map_err(|_| color_eyre::eyre::eyre!("lock poisoned"))?
        .fetched_at;
    let include_approved = app.store()?.include_approved()?;
    Ok(html! {
        div #controls hx-swap-oob=[oob.then_some("true")] {
            div.buttons {
                label.include-approved {
                    input #include-approved type="checkbox" checked[include_approved];
                    "Include approved"
                }
                button.refresh hx-post="/refresh" hx-swap="none" { "Refresh now" }
            }
            p.updated {
                @match fetched_at {
                    // assets/app.js writes how long ago this was.
                    Some(at) => { "Updated " time data-epoch=(at) { (at) } }
                    None => { "Waiting for the first poll of GitHub…" }
                }
            }
        }
    })
}

/// The board, and with `oob` the header's controls to swap alongside it.
fn render_board(app: &App, oob: bool) -> color_eyre::Result<Markup> {
    let board = app.board()?;
    let live_tags = serde_json::to_string(&board.live_tags())?;
    let error = app
        .latest
        .read()
        .map_err(|_| color_eyre::eyre::eyre!("lock poisoned"))?
        .error
        .clone();
    let controls = oob.then(|| render_controls(app, true)).transpose()?;
    Ok(html! {
        @if let Some(controls) = controls { (controls) }
        div #board data-tags=(live_tags) hx-get="/board" hx-trigger="sse:update, reload" hx-swap="outerHTML" {
            @if let Some(error) = error {
                pre.error { (error) }
            }
            @if board.sections.is_empty() {
                p.empty { "No open pull requests." }
            }
            @for section in &board.sections {
                @let plain = board::copy(section, &app.config.emoji, false);
                @let with_repos = board::copy(section, &app.config.emoji, true);
                section.tag-section data-tag=(section.order_key())
                    data-text=(plain.text) data-html=(plain.html)
                    data-text-repos=(with_repos.text) data-html-repos=(with_repos.html) {
                    div.section-head {
                        h2 {
                            @match &section.tag {
                                Some(tag) => { (tag) }
                                None => { span.untagged { "Untagged" } }
                            }
                        }
                        div.copy-buttons {
                            button.copy type="button" data-mode="plain" data-label="Copy" data-label-selected="Copy {n}" { "Copy" }
                            button.copy type="button" data-mode="repos" data-label="Copy with repo headings"
                                data-label-selected="Copy {n} with repo headings" {
                                "Copy with repo headings"
                            }
                            label.select title="Select to copy with other groups" {
                                input.select-section type="checkbox";
                            }
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
            @for mark in pr.marks() {
                span.icon title=(mark.label()) { (mark.icon()) }
            }
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
    Ok(render_board(&app, true)?.into_response())
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

#[derive(Deserialize)]
struct IncludeApproved {
    include: bool,
}

async fn set_include_approved(
    State(app): State<Arc<App>>,
    Json(body): Json<IncludeApproved>,
) -> WebResult<StatusCode> {
    app.store()?.set_include_approved(body.include)?;
    app.changed();
    Ok(StatusCode::NO_CONTENT)
}

/// "Refresh now": polls GitHub regardless of when it was last polled.
async fn refresh(State(app): State<Arc<App>>) -> WebResult<StatusCode> {
    app.request_refresh(true)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Sent when the page is opened or focused: polls GitHub unless that
/// happened recently.
async fn check(State(app): State<Arc<App>>) -> WebResult<StatusCode> {
    app.request_refresh(false)?;
    Ok(StatusCode::NO_CONTENT)
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
