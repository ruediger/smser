use clap::Parser;
use quick_xml::de::from_str;
use quick_xml::se::to_string;
use serde::Deserialize;
use serde::Serialize;

const MODEM_BASE_URL: &str = "http://192.168.8.1";

/// Represents the XML response from /api/webserver/SesTokInfo
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename = "response")]
struct SessionInfo {
    #[serde(rename = "SesInfo")]
    session_id: String,
    #[serde(rename = "TokInfo")]
    token: String,
}

/// Represents the SMS list request XML
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename = "request")]
struct SmsListRequest {
    #[serde(rename = "PageIndex")]
    page_index: i32,
    #[serde(rename = "ReadCount")]
    read_count: i32,
    #[serde(rename = "BoxType")]
    box_type: i32,
    #[serde(rename = "SortType")]
    sort_type: i32,
    #[serde(rename = "Ascending")]
    ascending: i32,
    #[serde(rename = "UnreadPreferred")]
    unread_preferred: i32,
}

/// Represents a single SMS message in the response
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename = "Message")]
struct SmsMessage {
    #[serde(rename = "Smstat")]
    smstat: i32,
    #[serde(rename = "Index")]
    index: i32,
    #[serde(rename = "Phone")]
    phone: String,
    #[serde(rename = "Content")]
    content: String,
    #[serde(rename = "Date")]
    date: String,
    #[serde(rename = "Sca")]
    sca: String,
    #[serde(rename = "SaveType")]
    save_type: i32,
    #[serde(rename = "Priority")]
    priority: i32,
    #[serde(rename = "SmsType")]
    sms_type: i32,
}

/// Represents the Messages wrapper in the SMS list response
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename = "Messages")]
struct SmsMessages {
    #[serde(rename = "Message", default)]
    message: Vec<SmsMessage>,
}

/// Represents the SMS list response XML
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename = "response")]
struct SmsListResponse {
    #[serde(rename = "Count")]
    count: i32,
    #[serde(rename = "Messages")]
    messages: SmsMessages,
}

/// Fetches the SMS list from the modem.
fn get_sms_list(session_id: &str, token: &str) -> Result<SmsListResponse, Box<dyn std::error::Error>> {
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
struct Phones {
    #[serde(rename = "Phone")]
    phone: Vec<String>,
}

/// Represents the SMS sending request XML
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename = "request")]
struct SmsRequest {
    #[serde(rename = "Index")]
    index: i32,
    #[serde(rename = "Phones")]
    phones: Phones,
    #[serde(rename = "Sca")]
    sca: String,
    #[serde(rename = "Content")]
    content: String,
    #[serde(rename = "Length")]
    length: i32,
    #[serde(rename = "Reserved")]
    reserved: i32,
    #[serde(rename = "Date")]
    date: i32,
}

/// Fetches the session ID and token from the modem.
fn get_session_info() -> Result<(String, String), Box<dyn std::error::Error>> {
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
fn send_sms(session_id: &str, token: &str, to: &str, message: &str) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::blocking::Client::new();
    let url = format!("{}/api/sms/send-sms", MODEM_BASE_URL);

    let sms_request = SmsRequest {
        index: -1,
        phones: Phones { phone: vec![to.to_string()] },
        sca: "".to_string(),
        content: message.to_string(),
        length: message.len() as i32,
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

    if response.contains("<response>OK</response>") {
        println!("SMS sent successfully!");
        Ok(())
    } else {
        Err(format!("Failed to send SMS: {}", response).into())
    }
}

/// Simple program to send SMS via a Huawei E3372 modem
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: SmsCommand,
}

#[derive(clap::Subcommand, Debug, PartialEq)]
enum SmsCommand {
    /// Send an SMS message
    Send {
        /// The destination phone number
        #[arg(short, long)]
        to: String,

        /// The message to send
        #[arg(short, long)]
        message: String,
    },
    /// Receive SMS messages
    Receive,
}

fn main() {
    let args = Args::parse();

    match get_session_info() {
        Ok((session_id, token)) => {
            println!("Session ID: {}", session_id);
            println!("Token: {}", token);

            match args.command {
                SmsCommand::Send { to, message } => {
                    match send_sms(&session_id, &token, &to, &message) {
                        Ok(_) => {},
                        Err(e) => eprintln!("Error sending SMS: {}", e),
                    }
                },
                SmsCommand::Receive => {
                    match get_sms_list(&session_id, &token) {
                        Ok(response) => {
                            println!("Received {} SMS messages:", response.count);
                            for msg in response.messages.message {
                                println!("  From: {}", msg.phone);
                                println!("  Content: {}", msg.content);
                                println!("  Date: {}", msg.date);
                                println!("  --------------------");
                            }
                        },
                        Err(e) => eprintln!("Error receiving SMS: {}", e),
                    }
                },
            }
        }
        Err(e) => {
            eprintln!("Error getting session info: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_args_parsing_send_short_flags() {
        let args = Args::try_parse_from(&["smser", "send", "-t", "1234567890", "-m", "Hello, world!"])
            .expect("Failed to parse arguments");
        match args.command {
            SmsCommand::Send { to, message } => {
                assert_eq!(to, "1234567890");
                assert_eq!(message, "Hello, world!");
            },
            _ => panic!("Expected Send command"),
        }
    }

    #[test]
    fn test_args_parsing_send_long_flags() {
        let args = Args::try_parse_from(&["smser", "send", "--to", "1234567890", "--message", "Hello, world!"])
            .expect("Failed to parse arguments");
        match args.command {
            SmsCommand::Send { to, message } => {
                assert_eq!(to, "1234567890");
                assert_eq!(message, "Hello, world!");
            },
            _ => panic!("Expected Send command"),
        }
    }

    #[test]
    fn test_args_parsing_receive() {
        let args = Args::try_parse_from(&["smser", "receive"])
            .expect("Failed to parse arguments");
        assert_eq!(args.command, SmsCommand::Receive);
    }

    #[test]
    fn test_get_session_info_error() {
        // This test expects an error since the modem is not likely to be available
        let result = get_session_info();
        assert!(result.is_err());
    }

    #[test]
    fn test_send_sms_error() {
        // This test expects an error since the modem is not likely to be available
        let result = send_sms("dummy_session_id", "dummy_token", "+1234567890", "Test message");
        assert!(result.is_err());
    }

    #[test]
    fn test_get_sms_list_error() {
        // This test expects an error since the modem is not likely to be available
        let result = get_sms_list("dummy_session_id", "dummy_token");
        assert!(result.is_err());
    }
}
