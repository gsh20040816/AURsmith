use crate::{
    auth::{self, AuthenticatedSession},
    config::Config,
    error::ApiError,
    packages,
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{delete, get, post},
};
use serde::Deserialize;
use sqlx::SqlitePool;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};

const REQUEST_BODY_LIMIT_BYTES: usize = 4 * 1024;
const PASSWORD_VERIFICATION_CONCURRENCY: usize = 2;

#[derive(Clone)]
pub struct AppState {
    pub database: SqlitePool,
    pub config: Arc<Config>,
    pub(crate) login_throttle: Arc<Mutex<auth::LoginThrottle>>,
    pub(crate) password_verification_permits: Arc<Semaphore>,
}

impl AppState {
    pub fn new(database: SqlitePool, config: Config) -> Self {
        Self {
            database,
            config: Arc::new(config),
            login_throttle: Arc::new(Mutex::new(auth::LoginThrottle::default())),
            password_verification_permits: Arc::new(Semaphore::new(
                PASSWORD_VERIFICATION_CONCURRENCY,
            )),
        }
    }
}

pub fn router(state: AppState) -> Router {
    let management = Router::new()
        .route("/manage", get(manage_page))
        .route("/manage/logout", post(auth::logout))
        .route("/manage/packages", post(add_package))
        .route("/manage/packages/{pkgbase}/pause", post(pause_package))
        .route("/manage/packages/{pkgbase}/resume", post(resume_package))
        .route("/manage/packages/{pkgbase}", delete(delete_package))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authorize_management_request,
        ));

    Router::new()
        .route("/", get(|| async { Redirect::to("/manage") }))
        .route("/login", get(login_page))
        .route("/auth/login", post(auth::login))
        .route("/assets/aursmith.css", get(stylesheet))
        .route("/assets/aursmith.js", get(javascript))
        .route("/healthz", get(health))
        .merge(management)
        .fallback(StatusCode::NOT_FOUND)
        .with_state(state)
        .layer(DefaultBodyLimit::max(REQUEST_BODY_LIMIT_BYTES))
        .layer(middleware::from_fn(security_headers))
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

async fn login_page() -> Html<&'static str> {
    Html(LOGIN_HTML)
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLESHEET,
    )
}

async fn javascript() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        JAVASCRIPT,
    )
}

async fn manage_page(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Html<String>, ApiError> {
    let tracked_packages = packages::list(&state.database).await?;
    Ok(Html(render_management_page(
        &session.username,
        &tracked_packages,
    )))
}

#[derive(Deserialize)]
struct AddPackageRequest {
    pkgbase: String,
}

async fn add_package(
    State(state): State<AppState>,
    Json(request): Json<AddPackageRequest>,
) -> Result<StatusCode, ApiError> {
    packages::add(&state.database, &request.pkgbase).await?;
    Ok(StatusCode::CREATED)
}

async fn pause_package(
    State(state): State<AppState>,
    Path(pkgbase): Path<String>,
) -> Result<StatusCode, ApiError> {
    packages::set_state(&state.database, &pkgbase, "paused").await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn resume_package(
    State(state): State<AppState>,
    Path(pkgbase): Path<String>,
) -> Result<StatusCode, ApiError> {
    packages::set_state(&state.database, &pkgbase, "active").await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_package(
    State(state): State<AppState>,
    Path(pkgbase): Path<String>,
) -> Result<StatusCode, ApiError> {
    packages::delete(&state.database, &pkgbase).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn render_management_page(username: &str, tracked_packages: &[packages::TrackedPackage]) -> String {
    let package_content = if tracked_packages.is_empty() {
        "<section class=\"empty-state\"><strong>还没有跟踪包</strong><p>在上方输入一个准确的 AUR pkgbase，然后选择“添加 pkgbase”。</p></section>".to_owned()
    } else {
        tracked_packages
            .iter()
            .map(render_package)
            .collect::<Vec<_>>()
            .join("")
    };
    format!(
        r#"<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>包目录 · AURsmith</title>
  <link rel="stylesheet" href="/assets/aursmith.css">
  <script src="/assets/aursmith.js" defer></script>
</head>
<body>
  <main class="shell">
    <header class="masthead">
      <div><p class="eyebrow">AURsmith / 明确包目录</p><h1>只锻造你明确加入的包</h1><p class="lede">当前核心只管理 pkgbase、批准 baseline 与最后错误，不伪装尚未实现的构建或发布状态。</p></div>
      <div class="operator"><span>{}</span><button type="button" class="quiet" data-action="logout">退出</button></div>
    </header>
    <section class="add-panel" aria-labelledby="add-title">
      <div><h2 id="add-title">添加 pkgbase</h2><p>输入 AUR 的准确 pkgbase；不会搜索、猜测或自动加入依赖。</p></div>
      <form id="add-package"><label for="pkgbase">pkgbase</label><div class="input-row"><input id="pkgbase" name="pkgbase" required maxlength="128" autocomplete="off" spellcheck="false" placeholder="例如 paru"><button type="submit">添加 pkgbase</button></div></form>
    </section>
    <div id="page-error" class="error" role="alert" hidden></div>
    <section class="package-list" aria-label="显式跟踪的 pkgbase">{}</section>
  </main>
</body>
</html>"#,
        escape_html(username),
        package_content
    )
}

fn render_package(package: &packages::TrackedPackage) -> String {
    let pkgbase = escape_html(&package.pkgbase);
    let state = escape_html(&package.state);
    let baseline = match (
        package.approved_aur_commit.as_deref(),
        package.approved_tree_sha256.as_deref(),
        package.approved_at,
    ) {
        (Some(commit), Some(tree), Some(approved_at)) => format!(
            "<code title=\"commit {} / tree {}\">{}… / {}…</code><small>{}</small>",
            escape_html(commit),
            escape_html(tree),
            escape_html(&commit[..12]),
            escape_html(&tree[..12]),
            escape_html(&approved_at.to_rfc3339())
        ),
        _ => "<span class=\"muted\">尚无批准 baseline</span>".to_owned(),
    };
    let last_error = package
        .last_error
        .as_deref()
        .map(|error| format!("<p class=\"last-error\">{}</p>", escape_html(error)))
        .unwrap_or_else(|| "<p class=\"muted\">没有记录错误</p>".to_owned());
    let state_action = if package.state == "active" {
        "<button type=\"button\" class=\"quiet\" data-action=\"pause\">暂停</button>"
    } else {
        "<button type=\"button\" class=\"quiet\" data-action=\"resume\">恢复</button>"
    };
    format!(
        r#"<article class="package" data-pkgbase="{}">
  <div class="forge-tag {}" aria-hidden="true"><span>PKG</span></div>
  <div class="package-main"><div class="package-title"><h2>{}</h2><span class="state {}">{}</span></div><dl><div><dt>批准 baseline</dt><dd>{}</dd></div><div><dt>最后错误</dt><dd>{}</dd></div></dl></div>
  <div class="actions">{}<button type="button" class="danger" data-action="delete">物理删除</button></div>
</article>"#,
        pkgbase, state, pkgbase, state, state, baseline, last_error, state_action
    )
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

const LOGIN_HTML: &str = r#"<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>登录 · AURsmith</title>
  <link rel="stylesheet" href="/assets/aursmith.css">
  <script src="/assets/aursmith.js" defer></script>
</head>
<body class="login-body">
  <main class="login-panel">
    <p class="eyebrow">AURsmith / 本地管理员</p>
    <h1>进入明确包目录</h1>
    <p>管理员只能在服务器本地创建或重置；浏览器仅用于登录和管理 pkgbase。</p>
    <form id="login-form" method="post" action="/auth/login"><label for="username">用户名</label><input id="username" name="username" required autocomplete="username"><label for="password">密码</label><input id="password" name="password" type="password" required autocomplete="current-password"><button type="submit">登录</button></form>
    <div id="page-error" class="error" role="alert" hidden></div>
  </main>
</body>
</html>"#;

const JAVASCRIPT: &str = r#"(() => {
  const errorBox = document.querySelector('#page-error');
  const showError = (message) => {
    if (!errorBox) return;
    errorBox.textContent = message;
    errorBox.hidden = false;
    errorBox.focus?.();
  };
  const responseError = async (response, fallback) => {
    const body = await response.json().catch(() => null);
    return body?.message || fallback;
  };
  const write = async (path, method, body) => {
    const response = await fetch(path, {
      method,
      credentials: 'same-origin',
      headers: {'Content-Type': 'application/json', 'X-AURsmith-CSRF': '1'},
      body: body === undefined ? undefined : JSON.stringify(body)
    });
    if (response.status === 401) {
      window.location.replace('/login');
      return false;
    }
    if (!response.ok) throw new Error(await responseError(response, `操作失败：HTTP ${response.status}`));
    return true;
  };

  document.querySelector('#login-form')?.addEventListener('submit', async (event) => {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    try {
      const response = await fetch('/auth/login', {
        method: 'POST',
        credentials: 'same-origin',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({username: form.get('username'), password: form.get('password')})
      });
      if (!response.ok) throw new Error(await responseError(response, `登录失败：HTTP ${response.status}`));
      window.location.replace('/manage');
    } catch (error) {
      showError(error instanceof Error ? error.message : '登录失败，请检查连接后重试');
    }
  });

  document.querySelector('#add-package')?.addEventListener('submit', async (event) => {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    try {
      if (await write('/manage/packages', 'POST', {pkgbase: form.get('pkgbase')})) window.location.reload();
    } catch (error) {
      showError(error instanceof Error ? error.message : '添加失败，请检查 pkgbase 后重试');
    }
  });

  document.addEventListener('click', async (event) => {
    const button = event.target.closest?.('[data-action]');
    if (!button) return;
    const action = button.dataset.action;
    try {
      if (action === 'logout') {
        if (await write('/manage/logout', 'POST')) window.location.replace('/login');
        return;
      }
      const row = button.closest('[data-pkgbase]');
      const pkgbase = row?.dataset.pkgbase;
      if (!pkgbase) return;
      if (action === 'delete' && !window.confirm(`物理删除 ${pkgbase}？批准 baseline 和最后错误也会删除。`)) return;
      const suffix = action === 'pause' || action === 'resume' ? `/${action}` : '';
      const method = action === 'delete' ? 'DELETE' : 'POST';
      if (await write(`/manage/packages/${encodeURIComponent(pkgbase)}${suffix}`, method)) window.location.reload();
    } catch (error) {
      showError(error instanceof Error ? error.message : '操作失败，请稍后重试');
    }
  });
})();"#;

const STYLESHEET: &str = r#":root {
  --canvas: #E7EDF0;
  --surface: #F9FBFC;
  --ink: #17242D;
  --muted: #5D6B73;
  --temper: #23677A;
  --ember: #B74424;
  color: var(--ink);
  background: var(--canvas);
  font-family: ui-sans-serif, system-ui, -apple-system, "Noto Sans CJK SC", "Microsoft YaHei", sans-serif;
}
* { box-sizing: border-box; }
body { margin: 0; min-height: 100vh; background: var(--canvas); }
button, input { font: inherit; }
button, input { border-radius: .35rem; }
button { border: 1px solid var(--temper); background: var(--temper); color: var(--surface); padding: .72rem 1rem; font-weight: 700; cursor: pointer; }
button.quiet { background: transparent; color: var(--temper); }
button.danger { border-color: var(--ember); background: transparent; color: var(--ember); }
button:focus-visible, input:focus-visible { outline: 3px solid var(--temper); outline-offset: 3px; }
input { width: 100%; border: 1px solid #9AA8AE; background: var(--surface); color: var(--ink); padding: .72rem .8rem; }
h1, h2 { font-family: "Arial Narrow", "Roboto Condensed", "Noto Sans CJK SC", "Microsoft YaHei", sans-serif; margin: 0; letter-spacing: .01em; }
code, .state, .eyebrow { font-family: ui-monospace, "Cascadia Mono", "Noto Sans Mono CJK SC", Consolas, monospace; }
.shell { width: min(72rem, calc(100% - 2rem)); margin: 0 auto; padding: 3rem 0; }
.masthead { display: flex; justify-content: space-between; gap: 2rem; align-items: flex-start; margin-bottom: 2rem; }
.eyebrow { margin: 0 0 .65rem; color: var(--temper); font-size: .78rem; font-weight: 800; letter-spacing: .1em; text-transform: uppercase; }
.lede { max-width: 46rem; color: var(--muted); line-height: 1.65; }
.operator { display: flex; align-items: center; gap: .75rem; white-space: nowrap; }
.add-panel, .package, .login-panel { background: var(--surface); border: 1px solid #C1CDD2; box-shadow: 0 8px 24px rgba(23,36,45,.07); }
.add-panel { display: grid; grid-template-columns: minmax(15rem, .8fr) minmax(20rem, 1.2fr); gap: 2rem; padding: 1.5rem; margin-bottom: 1rem; }
.add-panel p, .login-panel p { color: var(--muted); line-height: 1.55; }
label { display: block; margin-bottom: .4rem; font-weight: 700; }
.input-row { display: flex; gap: .6rem; }
.input-row button { white-space: nowrap; }
.error { margin: 1rem 0; border-left: .4rem solid var(--ember); background: var(--surface); color: var(--ember); padding: 1rem; font-weight: 700; }
.package-list { display: grid; gap: .75rem; }
.package { display: grid; grid-template-columns: 4.5rem 1fr auto; min-height: 9rem; overflow: hidden; }
.forge-tag { display: grid; place-items: center; background: var(--temper); color: var(--surface); clip-path: polygon(0 0, 100% 0, 84% 50%, 100% 100%, 0 100%); }
.forge-tag.paused { background: var(--muted); }
.forge-tag span { writing-mode: vertical-rl; letter-spacing: .16em; font: 800 .72rem ui-monospace, monospace; }
.package-main { padding: 1.25rem 1.5rem; min-width: 0; }
.package-title { display: flex; align-items: center; gap: .75rem; }
.package-title h2 { overflow-wrap: anywhere; }
.state { border: 1px solid currentColor; padding: .16rem .45rem; color: var(--temper); font-size: .72rem; }
.state.paused { color: var(--muted); }
dl { display: grid; gap: .6rem; margin: 1rem 0 0; }
dl div { display: grid; grid-template-columns: 9rem 1fr; gap: .75rem; }
dt { color: var(--muted); }
dd { margin: 0; min-width: 0; overflow-wrap: anywhere; }
dd small { display: block; color: var(--muted); margin-top: .25rem; }
.last-error { color: var(--ember); margin: 0; white-space: pre-wrap; }
.muted { color: var(--muted); }
.actions { display: flex; flex-direction: column; justify-content: center; gap: .55rem; padding: 1rem; border-left: 1px solid #D5DEE2; }
.empty-state { border: 1px dashed #9AA8AE; padding: 2rem; text-align: center; color: var(--muted); }
.empty-state strong { color: var(--ink); }
.login-body { display: grid; place-items: center; padding: 1rem; }
.login-panel { width: min(28rem, 100%); padding: 2rem; }
.login-panel form { display: grid; gap: .7rem; margin-top: 1.5rem; }
@media (max-width: 700px) {
  .shell { width: min(100% - 1rem, 42rem); padding: 1.25rem 0; }
  .masthead, .add-panel { display: grid; grid-template-columns: 1fr; gap: 1rem; }
  .package { grid-template-columns: 2.75rem 1fr; }
  .actions { grid-column: 1 / -1; flex-direction: row; border-left: 0; border-top: 1px solid #D5DEE2; }
  dl div { grid-template-columns: 1fr; gap: .2rem; }
}
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after { scroll-behavior: auto !important; transition: none !important; animation: none !important; }
}"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{admin, auth};
    use axum::{
        body::{Body, to_bytes},
        http::{Request, header::SET_COOKIE},
    };
    use serde_json::json;
    use tower::ServiceExt;

    const PASSWORD: &str = "足够长的测试密码-123456";

    async fn test_app(username: &str) -> (Router, SqlitePool, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let database = crate::db::open_or_create(&directory.path().join("aursmith.db"))
            .await
            .unwrap();
        admin::initialize(&database, username, PASSWORD)
            .await
            .unwrap();
        let config = Config::new(
            "127.0.0.1:0".parse().unwrap(),
            directory.path().join("aursmith.db"),
            "https://aursmith.test",
            30,
            1,
        )
        .unwrap();
        (
            router(AppState::new(database.clone(), config)),
            database,
            directory,
        )
    }

    async fn login(app: &Router, username: &str, source: &str) -> String {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/login")
                    .header("origin", "https://aursmith.test")
                    .header(auth::TRUSTED_CLIENT_IP_HEADER_NAME, source)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"username": username, "password": PASSWORD}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        response.headers()[SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    async fn body_text(response: Response) -> String {
        String::from_utf8(
            to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap()
    }

    #[test]
    fn html_escape_covers_text_and_attribute_delimiters() {
        assert_eq!(escape_html("<&>\"'"), "&lt;&amp;&gt;&quot;&#39;");
    }

    #[tokio::test]
    async fn root_and_expired_pages_redirect_while_management_api_returns_401() {
        let (app, _, _directory) = test_app("admin").await;
        let root = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(root.status(), StatusCode::SEE_OTHER);
        assert_eq!(root.headers()[header::LOCATION], "/manage");

        let page = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/manage")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(page.status(), StatusCode::SEE_OTHER);
        assert_eq!(page.headers()[header::LOCATION], "/login");

        let api = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/manage/packages")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"pkgbase": "paru"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(api.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn management_page_does_not_hide_database_failures_as_login_redirects() {
        let (app, database, _directory) = test_app("admin").await;
        let cookie = login(&app, "admin", "192.0.2.250").await;
        database.close().await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/manage")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_ne!(
            response.headers().get(header::LOCATION),
            Some(&HeaderValue::from_static("/login"))
        );
    }

    #[tokio::test]
    async fn server_html_is_authenticated_escaped_and_uses_native_assets() {
        let username = "<admin>\"'";
        let (app, _, _directory) = test_app(username).await;
        let login_page = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/login")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login_page.status(), StatusCode::OK);
        assert_eq!(
            login_page.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        assert!(
            login_page
                .headers()
                .contains_key(header::CONTENT_SECURITY_POLICY)
        );

        let cookie = login(&app, username, "192.0.2.1").await;
        let page = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/manage")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_text(page).await;
        assert!(body.contains("&lt;admin&gt;&quot;&#39;"));
        assert!(body.contains("还没有跟踪包"));
        assert!(body.contains("/assets/aursmith.js"));
        for removed in ["React", "Vite", "SSE", "Worker", "Profile", "Release"] {
            assert!(!body.contains(removed), "{removed}");
        }

        let script = app
            .oneshot(
                Request::builder()
                    .uri("/assets/aursmith.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let script = body_text(script).await;
        assert!(script.contains("'X-AURsmith-CSRF': '1'"));
        assert!(script.contains("textContent = message"));
        assert!(!script.contains("innerHTML"));
    }

    #[tokio::test]
    async fn login_form_fails_closed_without_javascript_and_oversized_bodies_are_rejected() {
        let (app, _, _directory) = test_app("admin").await;
        let page = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/login")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let page = body_text(page).await;
        assert!(page.contains("<form id=\"login-form\" method=\"post\" action=\"/auth/login\">"));

        let form_fallback = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/login")
                    .header("origin", "https://aursmith.test")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("username=admin&password=secret"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(form_fallback.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        let oversized = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/login")
                    .header("origin", "https://aursmith.test")
                    .header("content-type", "application/json")
                    .body(Body::from("x".repeat(REQUEST_BODY_LIMIT_BYTES + 1)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn authenticated_crud_requires_origin_and_csrf_and_get_is_read_only() {
        let (app, database, _directory) = test_app("admin").await;
        let cookie = login(&app, "admin", "192.0.2.2").await;
        for (origin, csrf) in [
            (None, None),
            (Some("https://other.test"), Some(auth::CSRF_HEADER_VALUE)),
            (Some("https://aursmith.test"), None),
        ] {
            let mut request = Request::builder()
                .method("POST")
                .uri("/manage/packages")
                .header("cookie", &cookie)
                .header("content-type", "application/json");
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            if let Some(csrf) = csrf {
                request = request.header(auth::CSRF_HEADER_NAME, csrf);
            }
            let response = app
                .clone()
                .oneshot(
                    request
                        .body(Body::from(json!({"pkgbase": "paru"}).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }

        let add = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/manage/packages")
                    .header("cookie", &cookie)
                    .header("origin", "https://aursmith.test")
                    .header(auth::CSRF_HEADER_NAME, auth::CSRF_HEADER_VALUE)
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"pkgbase": "paru"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(add.status(), StatusCode::CREATED);

        let before: String = sqlx::query_scalar("SELECT last_seen_at FROM sessions")
            .fetch_one(&database)
            .await
            .unwrap();
        let page = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/manage")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(body_text(page).await.contains("paru"));
        let after: String = sqlx::query_scalar("SELECT last_seen_at FROM sessions")
            .fetch_one(&database)
            .await
            .unwrap();
        assert_eq!(after, before, "GET 不得刷新 session 活动时间");

        for (suffix, expected_state) in [("pause", "paused"), ("resume", "active")] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/manage/packages/paru/{suffix}"))
                        .header("cookie", &cookie)
                        .header("origin", "https://aursmith.test")
                        .header(auth::CSRF_HEADER_NAME, auth::CSRF_HEADER_VALUE)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            let state: String =
                sqlx::query_scalar("SELECT state FROM tracked_packages WHERE pkgbase = 'paru'")
                    .fetch_one(&database)
                    .await
                    .unwrap();
            assert_eq!(state, expected_state);
        }

        let deleted = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/manage/packages/paru")
                    .header("cookie", &cookie)
                    .header("origin", "https://aursmith.test")
                    .header(auth::CSRF_HEADER_NAME, auth::CSRF_HEADER_VALUE)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM tracked_packages")
                .fetch_one(&database)
                .await
                .unwrap(),
            0
        );
        let old_route = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/workers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(old_route.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn idle_absolute_and_logout_invalidate_management_pages() {
        let (app, database, _directory) = test_app("admin").await;
        let idle_cookie = login(&app, "admin", "192.0.2.3").await;
        sqlx::query("UPDATE sessions SET last_seen_at = ?")
            .bind(chrono::Utc::now() - chrono::Duration::minutes(31))
            .execute(&database)
            .await
            .unwrap();
        let idle = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/manage")
                    .header("cookie", idle_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(idle.status(), StatusCode::SEE_OTHER);
        assert_eq!(idle.headers()[header::LOCATION], "/login");

        sqlx::query("DELETE FROM sessions")
            .execute(&database)
            .await
            .unwrap();
        let absolute_cookie = login(&app, "admin", "192.0.2.4").await;
        sqlx::query("UPDATE sessions SET expires_at = ?")
            .bind(chrono::Utc::now() - chrono::Duration::seconds(1))
            .execute(&database)
            .await
            .unwrap();
        let absolute = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/manage")
                    .header("cookie", absolute_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(absolute.status(), StatusCode::SEE_OTHER);

        sqlx::query("DELETE FROM sessions")
            .execute(&database)
            .await
            .unwrap();
        let logout_cookie = login(&app, "admin", "192.0.2.5").await;
        let rejected = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/manage/logout")
                    .header("cookie", &logout_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
        let logout = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/manage/logout")
                    .header("cookie", &logout_cookie)
                    .header("origin", "https://aursmith.test")
                    .header(auth::CSRF_HEADER_NAME, auth::CSRF_HEADER_VALUE)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(logout.status(), StatusCode::NO_CONTENT);
        assert!(
            logout.headers()[SET_COOKIE]
                .to_str()
                .unwrap()
                .contains("Max-Age=0")
        );
        let after_logout = app
            .oneshot(
                Request::builder()
                    .uri("/manage")
                    .header("cookie", logout_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(after_logout.status(), StatusCode::SEE_OTHER);
    }

    #[tokio::test]
    async fn login_requires_origin_and_rate_limits_each_source_with_retry_after() {
        let (app, _, _directory) = test_app("admin").await;
        for origin in [None, Some("https://other.test")] {
            let mut request = Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json");
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            let response = app
                .clone()
                .oneshot(
                    request
                        .body(Body::from(
                            json!({"username": "admin", "password": "错误但足够长的密码-123456"})
                                .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }

        let bad_body = || {
            Body::from(
                json!({"username": "admin", "password": "错误但足够长的密码-123456"}).to_string(),
            )
        };
        for _ in 0..auth::LoginThrottle::SOURCE_LIMIT {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/auth/login")
                        .header("origin", "https://aursmith.test")
                        .header(auth::TRUSTED_CLIENT_IP_HEADER_NAME, "192.0.2.10")
                        .header("content-type", "application/json")
                        .body(bad_body())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let limited = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/login")
                    .header("origin", "https://aursmith.test")
                    .header(auth::TRUSTED_CLIENT_IP_HEADER_NAME, "192.0.2.10")
                    .header("content-type", "application/json")
                    .body(bad_body())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(limited.headers()[header::RETRY_AFTER], "60");

        let other = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/login")
                    .header("origin", "https://aursmith.test")
                    .header(auth::TRUSTED_CLIENT_IP_HEADER_NAME, "192.0.2.11")
                    .header("content-type", "application/json")
                    .body(bad_body())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(other.status(), StatusCode::UNAUTHORIZED);

        for _ in 0..auth::LoginThrottle::SOURCE_LIMIT + 2 {
            login(&app, "admin", "192.0.2.12").await;
        }
        let failed_after_success = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/login")
                    .header("origin", "https://aursmith.test")
                    .header(auth::TRUSTED_CLIENT_IP_HEADER_NAME, "192.0.2.12")
                    .header("content-type", "application/json")
                    .body(bad_body())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(failed_after_success.status(), StatusCode::UNAUTHORIZED);
    }
}
