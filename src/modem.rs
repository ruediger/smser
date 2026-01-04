use quick_xml::de::from_str;
use quick_xml::se::to_string;
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};

// Re-export types for backwards compatibility
pub use crate::types::{BoxType, Priority, SmsMessage, SmsStat, SmsType, SortType};

#[derive(Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename = "error")]
pub struct ModemErrorResponse {
    pub code: i32,
    pub message: String,
}

#[derive(Debug)]
pub enum Error {
    ReqwestError(reqwest::Error),
    XmlParseError(quick_xml::DeError),
    XmlSerializeError(quick_xml::SeError),
    ModemError { code: i32, message: String },
    SessionError(String),
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::ReqwestError(e) => write!(f, "HTTP request error: {}", e),
            Error::XmlParseError(e) => write!(f, "XML parsing error: {}", e),
            Error::XmlSerializeError(e) => write!(f, "XML serialization error: {}", e),
            Error::ModemError { code, message } => {
                write!(f, "Modem error code {}: {}", code, message)
            }
            Error::SessionError(msg) => write!(f, "Session error: {}", msg),
            Error::Other(msg) => write!(f, "Other error: {}", msg),
        }
    }
}

impl std::error::Error for Error {}

impl From<reqwest::Error> for Error {
    fn from(err: reqwest::Error) -> Self {
        Error::ReqwestError(err)
    }
}

impl From<quick_xml::DeError> for Error {
    fn from(err: quick_xml::DeError) -> Self {
        Error::XmlParseError(err)
    }
}

impl From<quick_xml::SeError> for Error {
    fn from(err: quick_xml::SeError) -> Self {
        Error::XmlSerializeError(err)
    }
}

/// Represents the XML response from /api/webserver/SesTokInfo
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename = "response")]
pub struct SessionInfo {
    #[serde(rename = "SesInfo")]
    pub session_id: String,
    #[serde(rename = "TokInfo")]
    pub token: String,
}

/// Represents the SMS list request XML
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename = "request")]
pub struct SmsListRequest {
    #[serde(rename = "PageIndex")]
    pub page_index: i32,
    #[serde(rename = "ReadCount")]
    pub read_count: u32,
    #[serde(rename = "BoxType")]
    pub box_type: BoxType,
    #[serde(rename = "SortType")]
    pub sort_type: SortType,
    #[serde(rename = "Ascending")]
    pub ascending: i32,
    #[serde(rename = "UnreadPreferred")]
    pub unread_preferred: i32,
}

/// Represents the Messages wrapper in the SMS list response
#[derive(Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename = "Messages")]
pub struct SmsMessages {
    #[serde(rename = "Message", default)]
    pub message: Vec<SmsMessage>,
}

/// Represents the SMS list response XML
#[derive(Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename = "response")]
pub struct SmsListResponse {
    #[serde(rename = "Count")]
    pub count: i32,
    #[serde(rename = "Messages")]
    pub messages: SmsMessages,
}

/// Parameters for fetching the SMS list.
pub struct SmsListParams {
    pub box_type: BoxType,
    pub sort_type: SortType,
    pub read_count: u32,
    pub ascending: bool,
    pub unread_preferred: bool,
}

/// Fetches the SMS list from the modem.
pub async fn get_sms_list(
    modem_url: &str,
    session_id: &str,
    token: &str,
    params: SmsListParams,
) -> Result<SmsListResponse, Error> {
    let client = HttpClient::builder()
        .timeout(std::time::Duration::new(10, 0)) // 10 seconds
        .build()?;
    let url = format!("{}/api/sms/sms-list", modem_url);

    let sms_list_request = SmsListRequest {
        page_index: 1,
        read_count: params.read_count, // Fetching up to 20 messages for now
        box_type: params.box_type,
        sort_type: params.sort_type,
        ascending: if params.ascending { 1 } else { 0 },
        unread_preferred: if params.unread_preferred { 1 } else { 0 },
    };

    let xml_payload = to_string(&sms_list_request)?;

    let cookie = format!("SessionID={}", session_id);

    let response = client
        .post(&url)
        .header("Cookie", cookie)
        .header("X-Requested-With", "XMLHttpRequest")
        .header("__RequestVerificationToken", token)
        .header("Content-Type", "text/xml")
        .body(xml_payload)
        .send()
        .await?;

    let response_text = response.text().await?;

    match from_str::<SmsListResponse>(&response_text) {
        Ok(sms_list_response) => Ok(sms_list_response),
        Err(e) => {
            let error_response: Result<ModemErrorResponse, _> = from_str(&response_text);
            match error_response {
                Ok(err) => Err(Error::ModemError {
                    code: err.code,
                    message: err.message,
                }),
                Err(_) => Err(Error::Other(format!(
                    "Failed to get SMS list: {} Error: {}",
                    response_text, e
                ))),
            }
        }
    }
}

/// Represents a phone number in the SMS request XML
#[derive(Debug, Serialize, PartialEq)]
pub struct Phones {
    #[serde(rename = "Phone")]
    pub phone: Vec<String>,
}

/// Represents the SMS sending request XML
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename = "request")]
pub struct SmsRequest {
    #[serde(rename = "Index")]
    pub index: i32,
    #[serde(rename = "Phones")]
    pub phones: Phones,
    #[serde(rename = "Sca")]
    pub sca: String,
    #[serde(rename = "Content")]
    pub content: String,
    #[serde(rename = "Length")]
    pub length: i32,
    #[serde(rename = "Reserved")]
    pub reserved: i32,
    #[serde(rename = "Date")]
    pub date: i32,
}

/// Fetches the session ID and token from the modem.
pub async fn get_session_info(modem_url: &str) -> Result<(String, String), Error> {
    let client = HttpClient::builder()
        .timeout(std::time::Duration::new(10, 0)) // 10 seconds
        .build()?;
    let url = format!("{}/api/webserver/SesTokInfo", modem_url);
    let response = client.get(&url).send().await?;
    let response_text = response.text().await?;

    let session_info: Result<SessionInfo, _> = from_str(&response_text);

    match session_info {
        Ok(info) => {
            let session_id = info
                .session_id
                .strip_prefix("SessionID=")
                .ok_or_else(|| {
                    Error::SessionError("SesInfo did not start with 'SessionID='".to_string())
                })?
                .to_string();
            Ok((session_id, info.token))
        }
        Err(_) => {
            let error_response: Result<ModemErrorResponse, _> = from_str(&response_text);
            match error_response {
                Ok(err) => Err(Error::ModemError {
                    code: err.code,
                    message: err.message,
                }),
                Err(_) => Err(Error::Other(format!(
                    "Failed to get session info: {}",
                    response_text
                ))),
            }
        }
    }
}

/// Sends an SMS message via the modem.
pub async fn send_sms(
    modem_url: &str,
    session_id: &str,
    token: &str,
    to: &str,
    message: &str,
    dry_run: bool,
) -> Result<(), Error> {
    let client = HttpClient::builder()
        .timeout(std::time::Duration::new(10, 0)) // 10 seconds
        .build()?;
    let url = format!("{}/api/sms/send-sms", modem_url);

    let to_clean: String = to.chars().filter(|c| !c.is_whitespace()).collect();

    let sms_request = SmsRequest {
        index: -1,
        phones: Phones {
            phone: vec![to_clean],
        },
        sca: "".to_string(),
        content: message.to_string(),
        length: message.len() as i32,
        reserved: -1,
        date: -1,
    };

    let xml_payload = to_string(&sms_request)?;

    let cookie = format!("SessionID={}", session_id);

    if dry_run {
        println!("DRY RUN: Not sending message.");
        Ok(())
    } else {
        let response = client
            .post(&url)
            .header("Cookie", cookie)
            .header("X-Requested-With", "XMLHttpRequest")
            .header("__RequestVerificationToken", token)
            .header("Content-Type", "text/xml")
            .body(xml_payload)
            .send()
            .await?;

        let response_text = response.text().await?;

        if response_text.contains("<response>OK</response>") {
            println!("SMS sent successfully!");
            Ok(())
        } else {
            let error_response: Result<ModemErrorResponse, _> = from_str(&response_text);
            match error_response {
                Ok(err) => Err(Error::ModemError {
                    code: err.code,
                    message: err.message,
                }),
                Err(_) => Err(Error::Other(format!(
                    "Failed to send SMS: {}",
                    response_text
                ))),
            }
        }
    }
}
