use clap::Parser;
use crate::modem::{self, BoxType, SortType};
use serde_json;

/// Simple program to send SMS via a Huawei E3372 modem
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// The URL of the modem (e.g., "http://192.168.8.1")
    #[arg(long, default_value = "http://192.168.8.1")]
    pub modem_url: String,

    #[command(subcommand)]
    pub command: SmsCommand,
}

#[derive(clap::Subcommand, Debug, PartialEq)]
pub enum SmsCommand {
    /// Send an SMS message
    Send {
        /// The destination phone number
        #[arg(short, long)]
        to: String,

        /// The message to send
        #[arg(short, long)]
        message: String,

        /// Do not actually send a message
        #[arg(long)]
        dry_run: bool,
    },
    /// Receive SMS messages
    Receive {
        /// How many messages to read.
        #[arg(long,default_value_t=20)]
        count: u32,

        /// Sort in ascending order?
        #[arg(long)]
        ascending: bool,

        /// Prefer unread messages?
        #[arg(long)]
        unread_preferred: bool,

        /// Type of message box to read from (e.g., LocalInbox, LocalSent).
        #[arg(long, default_value_t = BoxType::LocalInbox)]
        box_type: BoxType,

        /// Sort messages by (e.g., Date, Phone, Index).
        #[arg(long, default_value_t = SortType::Date)]
        sort_by: SortType,

        /// Output messages in JSON format.
        #[arg(long)]
        json: bool,
    },
}

pub fn run() {
    let args = Args::parse();

    match modem::get_session_info(&args.modem_url) {
        Ok((session_id, token)) => {
            println!("Session ID: {}", session_id);
            println!("Token: {}", token);

            match args.command {
                SmsCommand::Send { to, message, dry_run } => {
                    if let Err(e) = modem::send_sms(&args.modem_url, &session_id, &token, &to, &message, dry_run) {
                        eprintln!("Error sending SMS: {}", e);
                    }
                },
                SmsCommand::Receive { count, ascending, unread_preferred, box_type, sort_by, json } => {
                    match modem::get_sms_list(&args.modem_url, &session_id, &token, box_type, sort_by, count, ascending, unread_preferred) {
                        Ok(response) => {
                            if json {
                                match serde_json::to_string_pretty(&response.messages.message) {
                                    Ok(json_output) => println!("{}", json_output),
                                    Err(e) => eprintln!("Error serializing to JSON: {}", e),
                                }
                            } else {
                                println!("Received {} SMS messages:", response.count);
                                for msg in response.messages.message {
                                    println!("  From: {}", msg.phone);
                                    println!("  Content: {}", msg.content);
                                    println!("  Date: {}", msg.date);
                                    println!("  Priority: {}", msg.priority);
                                    println!("  SmsType: {}", msg.sms_type);
                                    println!("  Smstat: {}", msg.smstat);
                                    println!("  SaveType: {}", msg.save_type);
                                    println!("  --------------------");
                                }
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
    use crate::modem;

    #[test]
    fn test_args_parsing_send_short_flags() {
        let args = Args::try_parse_from(&["smser", "--modem-url", "http://test.com", "send", "-t", "1234567890", "-m", "Hello, world!"])
            .expect("Failed to parse arguments");
        assert_eq!(args.modem_url, "http://test.com");
        match args.command {
            SmsCommand::Send { to, message, dry_run } => {
                assert_eq!(to, "1234567890");
                assert_eq!(message, "Hello, world!");
                assert_eq!(dry_run, false);
            },
            _ => panic!("Expected Send command"),
        }
    }

    #[test]
    fn test_args_parsing_send_long_flags() {
        let args = Args::try_parse_from(&["smser", "--modem-url", "http://test.com", "send", "--to", "1234567890", "--message", "Hello, world!", "--dry-run"])
            .expect("Failed to parse arguments");
        assert_eq!(args.modem_url, "http://test.com");
        match args.command {
            SmsCommand::Send { to, message, dry_run } => {
                assert_eq!(to, "1234567890");
                assert_eq!(message, "Hello, world!");
                assert_eq!(dry_run, true);
            },
            _ => panic!("Expected Send command"),
        }
    }

    #[test]
    fn test_args_parsing_receive() {
        let args = Args::try_parse_from(&["smser", "--modem-url", "http://test.com", "receive", "--count", "50", "--ascending", "--unread-preferred", "--box-type", "local-sent", "--sort-by", "phone", "--json"])
            .expect("Failed to parse arguments");
        assert_eq!(args.modem_url, "http://test.com");
        match args.command {
            SmsCommand::Receive { count, ascending, unread_preferred, box_type, sort_by, json } => {
                assert_eq!(count, 50);
                assert_eq!(ascending, true);
                assert_eq!(unread_preferred, true);
                assert_eq!(box_type, BoxType::LocalSent);
                assert_eq!(sort_by, SortType::Phone);
                assert_eq!(json, true);
            },
            _ => panic!("Expected Receive command"),
        }
    }

    // These tests rely on the modem being unavailable, which is typically true during CI/CD or local development without a modem.
    // They verify that the error handling paths are correctly triggered.

    #[test]
    fn test_get_session_info_error() {
        let result = modem::get_session_info("http://nonexistent.com");
        assert!(result.is_err());
    }

    #[test]
    fn test_send_sms_dry_run() {
        let result = modem::send_sms("http://nonexistent.com", "dummy_session_id", "dummy_token", "+1234567890", "Test message", true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_send_sms_error() {
        let result = modem::send_sms("http://nonexistent.com", "dummy_session_id", "dummy_token", "+1234567890", "Test message", false);
        assert!(result.is_err());
    }

    #[test]
    fn test_get_sms_list_error() {
        let result = modem::get_sms_list("http://nonexistent.com", "dummy_session_id", "dummy_token", BoxType::LocalInbox, SortType::Date, 20, false, false);
        assert!(result.is_err());
    }
}
