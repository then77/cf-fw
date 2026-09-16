use bytes::Bytes;
use http::{HeaderMap, Response, StatusCode, header};
use http_body_util::Full;
use serde::Serialize;

use super::{FALLBACK_HEADER, FALLBACK_HEADER_VALUE, not_found::accepts_json};

const SERVER_ERROR_HTML: &[u8] = include_bytes!("../../assets/500.html");
const MESSAGE: &str = "The forwarding proxy encountered an internal error.";

#[derive(Serialize)]
struct ServerErrorJson<'a> {
    success: bool,
    code: &'a str,
    message: &'a str,
}

pub fn response(accept_headers: &HeaderMap) -> Response<Full<Bytes>> {
    let (content_type, body) = if accepts_json(accept_headers) {
        let body = serde_json::to_vec(&ServerErrorJson {
            success: false,
            code: "forward_error",
            message: MESSAGE,
        })
        .expect("serializing a fixed JSON response cannot fail");
        ("application/json; charset=utf-8", Bytes::from(body))
    } else {
        (
            "text/html; charset=utf-8",
            Bytes::from_static(SERVER_ERROR_HTML),
        )
    };

    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store")
        .header(FALLBACK_HEADER, FALLBACK_HEADER_VALUE)
        .body(Full::new(body))
        .expect("fixed response headers are valid")
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue, StatusCode, header};
    use http_body_util::BodyExt;

    use super::{FALLBACK_HEADER, FALLBACK_HEADER_VALUE, SERVER_ERROR_HTML, response};

    #[tokio::test]
    async fn defaults_to_the_bundled_html_fallback() {
        let response = response(&HeaderMap::new());

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[FALLBACK_HEADER], FALLBACK_HEADER_VALUE);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            SERVER_ERROR_HTML
        );
    }

    #[tokio::test]
    async fn returns_the_forward_error_json_contract() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));

        let response = response(&headers);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.headers()[FALLBACK_HEADER], FALLBACK_HEADER_VALUE);
        let value: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "success": false,
                "code": "forward_error",
                "message": "The forwarding proxy encountered an internal error."
            })
        );
    }
}
