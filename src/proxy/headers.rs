use http::{HeaderMap, HeaderValue, Request, Uri, header};

use crate::config::BASE_DOMAIN;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicHost {
    pub slug: String,
    pub hostname: String,
}

pub fn extract_public_host<B>(request: &Request<B>) -> Option<PublicHost> {
    let authority = match request.uri().authority() {
        Some(authority) => authority.clone(),
        None => request
            .headers()
            .get(header::HOST)?
            .to_str()
            .ok()?
            .parse::<http::uri::Authority>()
            .ok()?,
    };

    parse_public_host(authority.host())
}

pub fn rewrite_for_upstream<B>(
    request: &mut Request<B>,
    target: std::net::SocketAddr,
    public_hostname: &str,
) -> Result<(), http::Error> {
    let path_and_query = request
        .uri()
        .path_and_query()
        .cloned()
        .unwrap_or_else(|| http::uri::PathAndQuery::from_static("/"));
    let authority = format!("127.0.0.1:{}", target.port());

    *request.uri_mut() = Uri::builder()
        .scheme("http")
        .authority(authority.as_str())
        .path_and_query(path_and_query)
        .build()?;

    let headers = request.headers_mut();
    headers.insert(header::HOST, HeaderValue::from_str(&authority)?);
    headers.insert("x-forwarded-host", HeaderValue::from_str(public_hostname)?);
    headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
    Ok(())
}

pub fn is_upgrade(headers: &HeaderMap) -> bool {
    headers.contains_key(header::UPGRADE)
        && headers
            .get_all(header::CONNECTION)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
}

fn parse_public_host(host: &str) -> Option<PublicHost> {
    let hostname = host.to_ascii_lowercase();
    let suffix = format!(".{BASE_DOMAIN}");
    let slug = hostname.strip_suffix(&suffix)?;

    if !valid_slug(slug) {
        return None;
    }

    Some(PublicHost {
        slug: slug.to_owned(),
        hostname,
    })
}

fn valid_slug(slug: &str) -> bool {
    (1..=63).contains(&slug.len())
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.bytes().all(|byte| byte.is_ascii_digit())
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use http::{Request, header};

    use super::extract_public_host;

    #[test]
    fn extracts_slug_from_host_with_optional_port() {
        let request = Request::builder()
            .header(header::HOST, "Green-Apple.fw.rlzy.me:12345")
            .body(())
            .unwrap();

        let host = extract_public_host(&request).unwrap();
        assert_eq!(host.slug, "green-apple");
        assert_eq!(host.hostname, "green-apple.fw.rlzy.me");
    }

    #[test]
    fn uri_authority_takes_precedence_over_host_header() {
        let request = Request::builder()
            .uri("http://apple-pen.fw.rlzy.me/path")
            .header(header::HOST, "wrong.fw.rlzy.me")
            .body(())
            .unwrap();

        assert_eq!(extract_public_host(&request).unwrap().slug, "apple-pen");
    }

    #[test]
    fn rejects_foreign_nested_and_invalid_hosts() {
        for host in [
            "fw.rlzy.me",
            "a.b.fw.rlzy.me",
            "apple-pen.example.com",
            "-apple.fw.rlzy.me",
            "apple_.fw.rlzy.me",
            "1234.fw.rlzy.me",
        ] {
            let request = Request::builder()
                .header(header::HOST, host)
                .body(())
                .unwrap();
            assert!(extract_public_host(&request).is_none(), "accepted {host}");
        }
    }

    #[test]
    fn rejects_malformed_host_authority() {
        let request = Request::builder()
            .header(header::HOST, "apple pen.fw.rlzy.me")
            .body(())
            .unwrap();
        assert!(extract_public_host(&request).is_none());
    }
}
