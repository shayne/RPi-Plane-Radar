use std::io::Read;
use std::time::Duration;

use thiserror::Error;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

pub trait HttpClient: Send + Sync + 'static {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpRequest {
    pub url: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub max_response_bytes: usize,
    pub verify_tls: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum HttpError {
    #[error("HTTP timeout values exceed the supported duration")]
    InvalidTimeout,
    #[error("HTTP response body limit must be greater than zero")]
    InvalidBodyLimit,
    #[error("HTTP request timed out")]
    Timeout,
    #[error("HTTP transport failed")]
    Transport,
    #[error("HTTP response body failed")]
    Body,
    #[error("HTTP response body exceeded its limit")]
    BodyTooLarge,
    #[error("TLS certificate verification is required")]
    TlsVerificationRequired,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UreqHttpClient;

impl HttpClient for UreqHttpClient {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        if !request.verify_tls {
            return Err(HttpError::TlsVerificationRequired);
        }

        let agent = build_agent(&request)?;
        let mut call = agent.get(&request.url);
        for (name, value) in &request.query {
            call = call.query(name, value);
        }
        for (name, value) in &request.headers {
            call = call.header(name, value);
        }

        let mut response = call.call().map_err(map_request_error)?;
        let status = response.status().as_u16();
        let body = read_response_body(response.body_mut(), request.max_response_bytes)?;

        Ok(HttpResponse { status, body })
    }
}

fn build_agent(request: &HttpRequest) -> Result<ureq::Agent, HttpError> {
    if request.max_response_bytes == 0 {
        return Err(HttpError::InvalidBodyLimit);
    }
    let global_timeout = request
        .connect_timeout
        .checked_add(request.read_timeout)
        .ok_or(HttpError::InvalidTimeout)?;
    Ok(ureq::Agent::config_builder()
        .http_status_as_error(false)
        .https_only(true)
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::Rustls)
                .root_certs(RootCerts::WebPki)
                .build(),
        )
        .timeout_global(Some(global_timeout))
        .timeout_resolve(Some(request.connect_timeout))
        .timeout_connect(Some(request.connect_timeout))
        .timeout_recv_response(Some(request.read_timeout))
        .timeout_recv_body(Some(request.read_timeout))
        .build()
        .new_agent())
}

fn read_response_body(
    body: &mut ureq::Body,
    max_response_bytes: usize,
) -> Result<Vec<u8>, HttpError> {
    let mut reader = body.with_config().limit(max_response_bytes as u64).reader();
    let mut bytes = Vec::with_capacity(max_response_bytes.min(8192));
    let mut buffer = [0_u8; 8192];

    loop {
        let remaining = max_response_bytes.saturating_sub(bytes.len());
        if remaining == 0 {
            return match reader.read(&mut buffer[..1]) {
                Ok(0) => Ok(bytes),
                Ok(_) => Err(HttpError::BodyTooLarge),
                Err(error) => Err(map_body_error(error.into())),
            };
        }

        let read_limit = remaining.min(buffer.len());
        match reader.read(&mut buffer[..read_limit]) {
            Ok(0) => return Ok(bytes),
            Ok(read) => bytes.extend_from_slice(&buffer[..read]),
            Err(error) => return Err(map_body_error(error.into())),
        }
    }
}

fn map_request_error(error: ureq::Error) -> HttpError {
    match error {
        ureq::Error::Timeout(_) => HttpError::Timeout,
        _ => HttpError::Transport,
    }
}

fn map_body_error(error: ureq::Error) -> HttpError {
    match error {
        ureq::Error::Timeout(_) => HttpError::Timeout,
        ureq::Error::BodyExceedsLimit(_) => HttpError::BodyTooLarge,
        _ => HttpError::Body,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::thread;

    use flate2::Compression;
    use flate2::write::GzEncoder;

    use super::*;

    fn request() -> HttpRequest {
        HttpRequest {
            url: "https://example.invalid/sensitive/location".to_owned(),
            query: Vec::new(),
            headers: Vec::new(),
            connect_timeout: Duration::from_millis(3050),
            read_timeout: Duration::from_secs(10),
            max_response_bytes: 256 * 1024,
            verify_tls: true,
        }
    }

    #[test]
    fn agent_bounds_dns_connect_read_and_end_to_end_time() {
        let agent = build_agent(&request()).expect("agent");
        let timeouts = agent.config().timeouts();

        assert_eq!(timeouts.resolve, Some(Duration::from_millis(3050)));
        assert_eq!(timeouts.connect, Some(Duration::from_millis(3050)));
        assert_eq!(timeouts.recv_response, Some(Duration::from_secs(10)));
        assert_eq!(timeouts.recv_body, Some(Duration::from_secs(10)));
        assert_eq!(timeouts.global, Some(Duration::from_millis(13_050)));
    }

    #[test]
    fn timeout_sum_overflow_is_rejected() {
        let mut request = request();
        request.connect_timeout = Duration::MAX;
        request.read_timeout = Duration::from_nanos(1);
        assert!(matches!(
            build_agent(&request),
            Err(HttpError::InvalidTimeout)
        ));
    }

    #[test]
    fn body_limits_are_explicit_and_bounded() {
        let mut request = request();
        request.max_response_bytes = 0;
        assert!(matches!(
            build_agent(&request),
            Err(HttpError::InvalidBodyLimit)
        ));

        assert_eq!(
            map_body_error(ureq::Error::BodyExceedsLimit(64)),
            HttpError::BodyTooLarge
        );
        assert_eq!(
            map_body_error(ureq::Error::RequireHttpsOnly("not a body limit".to_owned())),
            HttpError::Body
        );
    }

    #[test]
    fn gzip_expansion_cannot_exceed_the_decoded_response_limit() {
        let decoded = vec![b'x'; 4096];
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&decoded).expect("gzip input");
        let compressed = encoder.finish().expect("gzip output");
        assert!(compressed.len() < 128, "fixture must fit the wire limit");

        let server = tiny_http::Server::http("127.0.0.1:0").expect("HTTP server");
        let address = server.server_addr();
        let server_thread = thread::spawn(move || {
            let request = server.recv().expect("request");
            let response = tiny_http::Response::from_data(compressed).with_header(
                tiny_http::Header::from_bytes("Content-Encoding", "gzip").expect("gzip header"),
            );
            request.respond(response).expect("response");
        });

        let agent = ureq::Agent::config_builder()
            .https_only(false)
            .build()
            .new_agent();
        let mut response = agent
            .get(format!("http://{address}"))
            .call()
            .expect("response");

        assert_eq!(
            read_response_body(response.body_mut(), 128),
            Err(HttpError::BodyTooLarge)
        );
        server_thread.join().expect("server thread");
    }

    #[test]
    fn mapped_ureq_errors_do_not_retain_location_bearing_text() {
        let sensitive = "40.712800/lon/-74.006000/dist/7.2";
        let request_error = map_request_error(ureq::Error::BadUri(sensitive.to_owned()));
        let body_error = map_body_error(ureq::Error::RequireHttpsOnly(sensitive.to_owned()));

        assert_eq!(request_error, HttpError::Transport);
        assert_eq!(body_error, HttpError::Body);
        assert!(!request_error.to_string().contains(sensitive));
        assert!(!body_error.to_string().contains(sensitive));
    }

    #[test]
    fn resolver_and_global_timeouts_keep_the_timeout_category() {
        assert_eq!(
            map_request_error(ureq::Error::Timeout(ureq::Timeout::Resolve)),
            HttpError::Timeout
        );
        assert_eq!(
            map_request_error(ureq::Error::Timeout(ureq::Timeout::Global)),
            HttpError::Timeout
        );
        assert_eq!(
            map_body_error(ureq::Error::Timeout(ureq::Timeout::RecvBody)),
            HttpError::Timeout
        );
    }
}
