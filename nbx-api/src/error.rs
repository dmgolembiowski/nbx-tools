use std::fmt::Display;

use reqwest::{Error as RequestError, Response};

#[derive(Debug)]
pub enum Error {
    RequestError(RequestError),
    SerdeJsonError {
        error: serde_json::Error,
        raw: String,
    },
    ApiError {
        status_code: u16,
        error: String,
    },
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequestError(error) => {
                write!(f, "Request Error: {}", error)
            }
            Self::SerdeJsonError { error, raw } => {
                write!(f, "Error parsing json: {}\n{}", raw, error)
            }
            Self::ApiError { status_code, error } => {
                write!(f, "Api Error: {} {}", status_code, error)
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<reqwest::Error> for Error {
    fn from(err: reqwest::Error) -> Self {
        Error::RequestError(err)
    }
}

pub(crate) async fn error_for_status(resp: Response) -> Result<Response, Error> {
    let status = resp.status();
    if status.is_client_error() || status.is_server_error() {
        let status_code = status.as_u16();
        let error: String = resp.text().await?;
        Err(Error::ApiError { status_code, error })
    } else {
        Ok(resp)
    }
}
