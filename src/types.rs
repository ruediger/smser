use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use strum_macros::{Display, EnumString};

#[derive(
    Clone, Debug, PartialEq, Serialize_repr, Deserialize_repr, Display, ValueEnum, EnumString,
)]
#[strum(serialize_all = "kebab-case")]
#[repr(i32)]
pub enum BoxType {
    LocalInbox = 1,
    LocalSent = 2,
    LocalDraft = 3,
    LocalTrash = 4,
    SimInbox = 5,
    SimSent = 6,
    SimDraft = 7,
    MixInbox = 8,
    MixSent = 9,
    MixDraft = 10,
    Unknown = -1,
}

#[derive(
    Clone, Debug, PartialEq, Serialize_repr, Deserialize_repr, Display, ValueEnum, EnumString,
)]
#[strum(serialize_all = "kebab-case")]
#[repr(i32)]
pub enum SortType {
    Date = 0,
    Phone = 1,
    Index = 2,
    Unknown = -1,
}

#[derive(
    Clone, Debug, PartialEq, Serialize_repr, Deserialize_repr, Display, ValueEnum, EnumString,
)]
#[strum(serialize_all = "kebab-case")]
#[repr(i32)]
pub enum SmsType {
    Single = 1,
    Multipart = 2,
    Unicode = 5,
    DeliveryConfirmationSuccess = 7,
    DeliveryConfirmationFailure = 8,
    Unknown = -1,
}

#[derive(
    Clone, Debug, PartialEq, Serialize_repr, Deserialize_repr, Display, ValueEnum, EnumString,
)]
#[strum(serialize_all = "kebab-case")]
#[repr(i32)]
pub enum Priority {
    Normal = 0,
    Interactive = 1,
    Urgent = 2,
    Emergency = 3,
    Unknown = 4,
}

#[derive(
    Clone, Debug, PartialEq, Serialize_repr, Deserialize_repr, Display, ValueEnum, EnumString,
)]
#[strum(serialize_all = "kebab-case")]
#[repr(i32)]
pub enum SmsStat {
    Unread = 0,
    Read = 1,
    Unknown = -1,
}

/// Represents a single SMS message
#[derive(Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename = "Message")]
pub struct SmsMessage {
    #[serde(rename = "Smstat")]
    pub smstat: SmsStat,
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
    pub priority: Priority,
    #[serde(rename = "SmsType")]
    pub sms_type: SmsType,
}
