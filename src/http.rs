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
    #[error("HTTP transport failed: {0}")]
    Transport(String),
    #[error("HTTP response body failed: {0}")]
    Body(String),
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

        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .https_only(true)
            .tls_config(
                TlsConfig::builder()
                    .provider(TlsProvider::Rustls)
                    .root_certs(RootCerts::WebPki)
                    .build(),
            )
            .build();
        let agent = config.new_agent();
        let mut call = agent.get(&request.url);
        for (name, value) in &request.query {
            call = call.query(name, value);
        }
        for (name, value) in &request.headers {
            call = call.header(name, value);
        }

        let mut response = call
            .config()
            .timeout_connect(Some(request.connect_timeout))
            .timeout_recv_response(Some(request.read_timeout))
            .timeout_recv_body(Some(request.read_timeout))
            .build()
            .call()
            .map_err(map_request_error)?;
        let status = response.status().as_u16();
        let body = response.body_mut().read_to_vec().map_err(map_body_error)?;

        Ok(HttpResponse { status, body })
    }
}

fn map_request_error(error: ureq::Error) -> HttpError {
    match error {
        ureq::Error::Timeout(_) => HttpError::Timeout,
        other => HttpError::Transport(other.to_string()),
    }
}

fn map_body_error(error: ureq::Error) -> HttpError {
    match error {
        ureq::Error::Timeout(_) => HttpError::Timeout,
        other => HttpError::Body(other.to_string()),
    }
}
