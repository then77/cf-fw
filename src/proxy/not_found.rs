use bytes::Bytes;
use http::{HeaderMap, Response, StatusCode, header};
use http_body_util::Full;
use serde::Serialize;

use super::{FALLBACK_HEADER, FALLBACK_HEADER_VALUE};

const NOT_FOUND_HTML: &[u8] = include_bytes!("../../assets/404.html");
const MESSAGE: &str = "This subdomain forwarding can't be found/unavailable.";

#[derive(Serialize)]
struct NotFoundJson<'a> {
    success: bool,
    code: &'a str,
    message: &'a str,
}

pub fn response(accept_headers: &HeaderMap) -> Response<Full<Bytes>> {
    let wants_json = accepts_json(accept_headers);
    let (content_type, body) = if wants_json {
        let body = serde_json::to_vec(&NotFoundJson {
            success: false,
            code: "forward_not_found",
            message: MESSAGE,
        })
        .expect("serializing a fixed JSON response cannot fail");
        ("application/json; charset=utf-8", Bytes::from(body))
    } else {
        (
            "text/html; charset=utf-8",
            Bytes::from_static(NOT_FOUND_HTML),
        )
    };

    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store")
        .header(FALLBACK_HEADER, FALLBACK_HEADER_VALUE)
        .body(Full::new(body))
        .expect("fixed response headers are valid")
}

pub(super) fn accepts_json(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|range| range.split(';').next())
        .any(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue, StatusCode, header};
    use http_body_util::BodyExt;

    use super::{FALLBACK_HEADER, FALLBACK_HEADER_VALUE, NOT_FOUND_HTML, response};

    #[tokio::test]
    async fn missing_accept_uses_exact_bundled_html() {
        let response = response(&HeaderMap::new());
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[FALLBACK_HEADER], FALLBACK_HEADER_VALUE);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            NOT_FOUND_HTML
        );
    }

    #[tokio::test]
    async fn exact_json_media_range_wins_case_insensitively() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/html, Application/JSON; q=0, */*"),
        );

        let response = response(&headers);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/json; charset=utf-8"
        );
        let value: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "success": false,
                "code": "forward_not_found",
                "message": "This subdomain forwarding can't be found/unavailable."
            })
        );
    }

    #[tokio::test]
    async fn wildcards_and_json_suffixes_use_html() {
        for accept in ["*/*", "text/html", "application/problem+json"] {
            let mut headers = HeaderMap::new();
            headers.insert(header::ACCEPT, HeaderValue::from_str(accept).unwrap());
            assert_eq!(
                response(&headers).headers()[header::CONTENT_TYPE],
                "text/html; charset=utf-8"
            );
        }
    }

    #[tokio::test]
    async fn invalid_accept_bytes_use_html() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_bytes(b"application/json,\xff").unwrap(),
        );
        assert_eq!(
            response(&headers).headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
    }
}
