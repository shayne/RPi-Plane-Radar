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
    pub verify_tls: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum HttpError {
    #[error("HTTP request timed out")]
    Timeout,
    #[error("HTTP transport failed")]
    Transport,
    #[error("HTTP response body failed")]
    Body,
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

        let agent = build_agent(&request);
        let mut call = agent.get(&request.url);
        for (name, value) in &request.query {
            call = call.query(name, value);
        }
        for (name, value) in &request.headers {
            call = call.header(name, value);
        }

        let mut response = call.call().map_err(map_request_error)?;
        let status = response.status().as_u16();
        let body = response.body_mut().read_to_vec().map_err(map_body_error)?;

        Ok(HttpResponse { status, body })
    }
}

fn build_agent(request: &HttpRequest) -> ureq::Agent {
    let global_timeout = request.connect_timeout.saturating_add(request.read_timeout);
    ureq::Agent::config_builder()
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
        .new_agent()
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
        _ => HttpError::Body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> HttpRequest {
        HttpRequest {
            url: "https://example.invalid/sensitive/location".to_owned(),
            query: Vec::new(),
            headers: Vec::new(),
            connect_timeout: Duration::from_millis(3050),
            read_timeout: Duration::from_secs(10),
            verify_tls: true,
        }
    }

    #[test]
    fn agent_bounds_dns_connect_read_and_end_to_end_time() {
        let agent = build_agent(&request());
        let timeouts = agent.config().timeouts();

        assert_eq!(timeouts.resolve, Some(Duration::from_millis(3050)));
        assert_eq!(timeouts.connect, Some(Duration::from_millis(3050)));
        assert_eq!(timeouts.recv_response, Some(Duration::from_secs(10)));
        assert_eq!(timeouts.recv_body, Some(Duration::from_secs(10)));
        assert_eq!(timeouts.global, Some(Duration::from_millis(13_050)));
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
