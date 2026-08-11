use anyhow::{Context, bail};
use axum::{
    Router,
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::Response,
    routing::any,
};
use clap::Parser;
use reqwest::Client;
use std::{collections::BTreeMap, fs, sync::Arc, time::Duration};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};
use url::Url;

const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Parser)]
struct Cli {
    #[arg(
        long,
        env = "AURSMITH_AGENT_GATEWAY_BIND",
        default_value = "0.0.0.0:8091"
    )]
    bind: String,
}

#[derive(Clone)]
struct RouteConfig {
    base_url: Url,
    api_key: Arc<str>,
    auth_style: AuthStyle,
}

#[derive(Clone, Copy)]
enum AuthStyle {
    Bearer,
    ApiKey,
}

#[derive(Clone)]
struct AppState {
    client: Client,
    routes: Arc<BTreeMap<String, RouteConfig>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "aursmith=info".into()))
        .with(tracing_subscriber::fmt::layer().json())
        .init();
    let cli = Cli::parse();
    let routes = [
        ("low-1", load_route("LOW_AGENT_1")?),
        ("low-2", load_route("LOW_AGENT_2")?),
        ("low-3", load_route("LOW_AGENT_3")?),
        ("high", load_route("HIGH_AGENT")?),
    ]
    .into_iter()
    .map(|(name, route)| (name.to_owned(), route))
    .collect();
    let state = AppState {
        client: Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(240))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        routes: Arc::new(routes),
    };
    let app = Router::new()
        .route("/{tier}/{*path}", any(proxy))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&cli.bind).await?;
    axum::serve(listener, app)
        .await
        .context("Agent 凭据网关异常退出")
}

fn load_route(route: &str) -> anyhow::Result<RouteConfig> {
    let base_name = format!("AURSMITH_{route}_PROVIDER_BASE_URL");
    let base = std::env::var(&base_name).with_context(|| format!("缺少 {base_name}"))?;
    let mut base_url = Url::parse(&base).with_context(|| format!("{base_name} 不是有效 URL"))?;
    if base_url.scheme() != "https" {
        bail!("{base_name} 必须使用 HTTPS");
    }
    if base_url.host_str().is_none() || base_url.query().is_some() || base_url.fragment().is_some()
    {
        bail!("{base_name} 必须是无查询参数和片段的绝对 HTTPS URL");
    }
    if !base_url.path().ends_with('/') {
        base_url.set_path(&format!("{}/", base_url.path()));
    }
    let key_file_name = format!("AURSMITH_{route}_API_KEY_FILE");
    let key_file =
        std::env::var(&key_file_name).with_context(|| format!("缺少 {key_file_name}"))?;
    let metadata = fs::symlink_metadata(&key_file)
        .with_context(|| format!("无法检查 Agent API key secret {key_file}"))?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > 64 * 1024 {
        bail!("Agent API key secret 类型或大小无效");
    }
    let api_key = fs::read_to_string(&key_file)
        .with_context(|| format!("无法读取 Agent API key secret {key_file}"))?
        .trim()
        .to_owned();
    if api_key.is_empty() || api_key.contains(['\r', '\n']) {
        bail!("Agent API key secret 内容无效");
    }
    let style_name = format!("AURSMITH_{route}_AUTH_STYLE");
    let auth_style = match std::env::var(&style_name)
        .unwrap_or_else(|_| "bearer".into())
        .as_str()
    {
        "bearer" => AuthStyle::Bearer,
        "x-api-key" => AuthStyle::ApiKey,
        _ => bail!("{style_name} 只能是 bearer 或 x-api-key"),
    };
    Ok(RouteConfig {
        base_url,
        api_key: api_key.into(),
        auth_style,
    })
}

async fn proxy(
    State(state): State<AppState>,
    Path((tier, path)): Path<(String, String)>,
    request: Request,
) -> Result<Response, (StatusCode, String)> {
    let route = state
        .routes
        .get(&tier)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "未知 Agent 路由".into()))?;
    let query = request
        .uri()
        .query()
        .map(|value| format!("?{value}"))
        .unwrap_or_default();
    let target = route
        .base_url
        .join(&format!("{path}{query}"))
        .map_err(internal)?;
    if target.origin() != route.base_url.origin() {
        return Err((StatusCode::BAD_REQUEST, "目标地址越界".into()));
    }
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, MAX_REQUEST_BYTES)
        .await
        .map_err(internal)?;
    let mut outbound = state.client.request(parts.method, target).body(bytes);
    for (name, value) in filtered_request_headers(&parts.headers) {
        outbound = outbound.header(name, value);
    }
    outbound = match route.auth_style {
        AuthStyle::Bearer => outbound.bearer_auth(route.api_key.as_ref()),
        AuthStyle::ApiKey => outbound.header("x-api-key", route.api_key.as_ref()),
    };
    let upstream = outbound.send().await.map_err(internal)?;
    let status = upstream.status();
    let headers = filtered_response_headers(upstream.headers());
    let mut response = Response::builder().status(status);
    for (name, value) in headers {
        response = response.header(name, value);
    }
    response
        .body(Body::from_stream(upstream.bytes_stream()))
        .map_err(internal)
}

fn filtered_request_headers(headers: &HeaderMap) -> Vec<(HeaderName, HeaderValue)> {
    headers
        .iter()
        .filter(|(name, _)| {
            !matches!(
                name.as_str(),
                "authorization"
                    | "x-api-key"
                    | "host"
                    | "content-length"
                    | "connection"
                    | "proxy-authorization"
            )
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

fn filtered_response_headers(headers: &HeaderMap) -> Vec<(HeaderName, HeaderValue)> {
    headers
        .iter()
        .filter(|(name, _)| !matches!(name.as_str(), "content-length" | "connection"))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

fn internal(error: impl std::fmt::Display) -> (StatusCode, String) {
    tracing::warn!(error = %error, "Agent 凭据网关请求失败");
    (StatusCode::BAD_GATEWAY, "Agent provider 请求失败".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::to_bytes, http::Request, routing::post};
    use tower::ServiceExt;

    #[test]
    fn removes_caller_credentials_and_hop_by_hop_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer leaked"));
        headers.insert("x-api-key", HeaderValue::from_static("leaked"));
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        let filtered = filtered_request_headers(&headers);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].0, "content-type");
    }

    #[tokio::test]
    async fn proxy_replaces_caller_key_with_gateway_secret() {
        let upstream = Router::new().route(
            "/api/messages",
            post(|headers: HeaderMap| async move {
                headers
                    .get("x-api-key")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("missing")
                    .to_owned()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let route = RouteConfig {
            base_url: Url::parse(&format!("http://{address}/api/")).unwrap(),
            api_key: Arc::from("gateway-secret"),
            auth_style: AuthStyle::ApiKey,
        };
        let app = Router::new()
            .route("/{tier}/{*path}", any(proxy))
            .with_state(AppState {
                client: Client::new(),
                routes: Arc::new(BTreeMap::from([("low-1".into(), route)])),
            });
        let response = app
            .oneshot(
                Request::post("/low-1/messages")
                    .header("x-api-key", "caller-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(body, "gateway-secret");
    }
}
