use quick_xml::de::from_str;
use quick_xml::se::to_string;
use serde::Deserialize;
use serde::Serialize;

const MODEM_BASE_URL: &str = "http://192.168.8.1";

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
    pub read_count: i32,
    #[serde(rename = "BoxType")]
    pub box_type: i32,
    #[serde(rename = "SortType")]
    pub sort_type: i32,
    #[serde(rename = "Ascending")]
    pub ascending: i32,
    #[serde(rename = "UnreadPreferred")]
    pub unread_preferred: i32,
}

/// Represents a single SMS message in the response
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename = "Message")]
pub struct SmsMessage {
    #[serde(rename = "Smstat")]
    pub smstat: i32,
    #[serde(rename = "Index")]
    pub index: i32,
    #[serde(rename = "Phone")]
    pub phone: String,
    #[serde(rename = "Content")]
    pub content: String,
    #[serde(rename = "Date")]
    pub date: String,
    #[serde(rename = "Sca")]
    pub sca: String,
    #[serde(rename = "SaveType")]
    pub save_type: i32,
    #[serde(rename = "Priority")]
    pub priority: i32,
    #[serde(rename = "SmsType")]
    pub sms_type: i32,
}

/// Represents the Messages wrapper in the SMS list response
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename = "Messages")]
pub struct SmsMessages {
    #[serde(rename = "Message", default)]
    pub message: Vec<SmsMessage>,
}

/// Represents the SMS list response XML
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename = "response")]
pub struct SmsListResponse {
    #[serde(rename = "Count")]
    pub count: i32,
    #[serde(rename = "Messages")]
    pub messages: SmsMessages,
}

/// Fetches the SMS list from the modem.
pub fn get_sms_list(session_id: &str, token: &str) -> Result<SmsListResponse, Box<dyn std::error::Error>> {
    let client = reqwest::blocking::Client::new();
    let url = format!("{}/api/sms/sms-list", MODEM_BASE_URL);

    let sms_list_request = SmsListRequest {
        page_index: 1,
        read_count: 20, // Fetching up to 20 messages for now
        box_type: 1, // Inbox
        sort_type: 0,
        ascending: 0,
        unread_preferred: 0,
    };

    let xml_payload = to_string(&sms_list_request)?;

    let cookie = format!("SessionID={}", session_id);

    let response = client.post(&url)
        .header("Cookie", cookie)
        .header("X-Requested-With", "XMLHttpRequest")
        .header("__RequestVerificationToken", token)
        .header("Content-Type", "text/xml")
        .body(xml_payload)
        .send()?
        .text()?;

    let sms_list_response: SmsListResponse = from_str(&response)?;

    Ok(sms_list_response)
}

/// Represents a phone number in the SMS request XML
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename = "Phone")]
pub struct SmsPhone {
    #[serde(rename = "$value")]
    pub number: String,
}

/// Represents the SMS sending request XML
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename = "request")]
pub struct SmsRequest {
    #[serde(rename = "Index")]
    pub index: i32,
    #[serde(rename = "Phones")]
    pub phones: Vec<SmsPhone>,
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
pub fn get_session_info() -> Result<(String, String), Box<dyn std::error::Error>> {
    let client = reqwest::blocking::Client::new();
    let url = format!("{}/api/webserver/SesTokInfo", MODEM_BASE_URL);
    let response = client.get(&url).send()?.text()?;

    let session_info: SessionInfo = from_str(&response)?;

    let session_id = session_info.session_id
        .strip_prefix("SessionID=")
        .ok_or("SesInfo did not start with 'SessionID='")?
        .to_string();

    Ok((session_id, session_info.token))
}

/// Sends an SMS message via the modem.
pub fn send_sms(session_id: &str, token: &str, to: &str, message: &str) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::blocking::Client::new();
    let url = format!("{}/api/sms/send-sms", MODEM_BASE_URL);

    let sms_request = SmsRequest {
        index: -1,
        phones: vec![SmsPhone { number: to.to_string() }],
        sca: "".to_string(),
        content: message.to_string(),
        length: -1,
        reserved: -1,
        date: -1,
    };

    let xml_payload = to_string(&sms_request)?;

    let cookie = format!("SessionID={}", session_id);

    let response = client.post(&url)
        .header("Cookie", cookie)
        .header("X-Requested-With", "XMLHttpRequest")
        .header("__RequestVerificationToken", token)
        .header("Content-Type", "text/xml")
        .body(xml_payload)
        .send()?
        .text()?;

    // The modem typically responds with <response>OK</response> on success
    if response.contains("<response>OK</response>") {
        println!("SMS sent successfully!");
        Ok(())
    } else {
        Err(format!("Failed to send SMS: {}", response).into())
    }
}
