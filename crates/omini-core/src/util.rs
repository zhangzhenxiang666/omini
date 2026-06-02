use std::sync::OnceLock;

/// 全局唯一的 reqwest::Client。
///
/// reqwest 官方推荐全局单例以复用连接池和 DNS 缓存。
/// 所有 HTTP 请求都应通过此 client 发出。
///
/// 自动读取代理环境变量（按优先级）：
/// `HTTPS_PROXY` / `https_proxy` → https 请求
/// `HTTP_PROXY` / `http_proxy`   → http 请求
/// `ALL_PROXY` / `all_proxy`     → 兜底
pub fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let mut builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(300));

        if let Some(proxy_url) = proxy_from_env() {
            builder = builder.proxy(reqwest::Proxy::all(&proxy_url).expect("Invalid proxy URL"));
        }

        builder
            .build()
            .expect("Failed to create global reqwest::Client")
    })
}

fn proxy_from_env() -> Option<String> {
    std::env::var("HTTPS_PROXY")
        .or_else(|_| std::env::var("https_proxy"))
        .or_else(|_| std::env::var("HTTP_PROXY"))
        .or_else(|_| std::env::var("http_proxy"))
        .or_else(|_| std::env::var("ALL_PROXY"))
        .or_else(|_| std::env::var("all_proxy"))
        .ok()
        .filter(|s| !s.is_empty())
}
