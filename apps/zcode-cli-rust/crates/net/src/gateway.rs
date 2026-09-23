//! Official Coding Plan endpoints are sent through the ZCode platform gateway
//! (Node `adapters/src/model/official-coding-plan-gateway.ts`). Only exact https
//! endpoint matches are rewritten; the query survives, the fragment does not.
use url::Url;

const ROUTES: [(&str, &str); 2] = [
    (
        "https://open.bigmodel.cn:443/api/anthropic/v1/messages",
        "/api/v1/ultra/anthropic/v1/messages",
    ),
    (
        "https://api.z.ai:443/api/anthropic/v1/messages",
        "/api/v1/ultra-zai/anthropic/v1/messages",
    ),
];

/// Node `endpointKey`.
fn endpoint_key(url: &Url) -> String {
    let path = match url.path() {
        "/" => "/",
        path => match path.trim_end_matches('/') {
            "" => "/",
            trimmed => trimmed,
        },
    };
    format!(
        "https://{}:{}{}",
        url.host_str().unwrap_or("").to_lowercase(),
        url.port().unwrap_or(443),
        path
    )
}

/// The gateway URL for an official endpoint, `None` for every other URL.
pub fn rewrite(request_url: &str, origin: &str) -> Option<String> {
    let url = Url::parse(request_url)
        .ok()
        .filter(|u| u.scheme() == "https")?;
    let key = endpoint_key(&url);
    let (_, path) = ROUTES.iter().find(|(endpoint, _)| *endpoint == key)?;
    let mut gateway = Url::parse(origin).ok()?.join(path).ok()?;
    gateway.set_query(url.query().filter(|q| !q.is_empty()));
    Some(gateway.into())
}
