// ============================================================================
// models.rs — Data structures for votes, bills, and XML/JSON deserialization
// ============================================================================
//
// This file defines all the data types used throughout the scraper. It's
// split into three categories:
//
//   1. **Insert params** — structs that map to database table rows. These are
//      what we pass to `db.rs` functions for SQL INSERT/UPDATE operations.
//
//   2. **Parsed intermediaries** — structs that hold a fully-parsed entity
//      (bill or vote) along with all its related data (actions, members, etc.).
//      These are assembled during parsing and then persisted in a transaction.
//
//   3. **Deserialization structs** — structs that mirror the shape of XML/JSON
//      source files so `serde` can automatically parse them. Think of these
//      like Pydantic models or Zod schemas — they define the expected shape
//      and `serde` handles the parsing.
//
// ============================================================================
// KEY RUST CONCEPT: Derive Macros & Serde
// ============================================================================
//
// `#[derive(...)]` auto-generates code. The most common derives here:
//
//   - `Debug`: Allows printing with `{:?}` (like Python's `__repr__`).
//   - `Clone`: Allows deep-copying with `.clone()`.
//   - `Default`: Creates a zero/empty instance with `Type::default()`.
//   - `Deserialize`: Lets `serde` parse JSON/XML/bincode into this struct.
//   - `Serialize`: Lets `serde` convert this struct to JSON/XML/bincode.
//
// `#[serde(rename = "xmlFieldName")]` maps a Rust field name to the actual
// key in the JSON/XML. Like Pydantic's `Field(alias="...")` or Jackson's
// `@JsonProperty("...")`.
//
// `#[serde(default)]` means "if this field is missing from the input, use
// its Default value" (empty string, 0, empty vec, etc.). Like setting a
// default value on a Pydantic field.
//
// ============================================================================
// KEY RUST CONCEPT: Option<T>
// ============================================================================
//
// Throughout this file, `Option<String>`, `Option<NaiveDate>`, etc. mean
// "this field might not have a value." It's Rust's null-safety mechanism:
//
//   - `Some("hello".to_string())` — has a value
//   - `None` — no value (like `null`/`None` in JS/Python)
//
// Unlike JS/Python, you CANNOT accidentally use a None value — the compiler
// forces you to check for it first (via `match`, `if let`, `.unwrap()`, etc.).
// This eliminates null pointer exceptions at compile time.
//
// ============================================================================

use chrono::NaiveDate;
use serde::{Deserialize, Deserializer};

fn null_or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    let value = Option::<T>::deserialize(deserializer)?;
    Ok(value.unwrap_or_default())
}
use serde_json::Value;

// ============================================================================
// Database INSERT parameter structs
// ============================================================================
// These structs hold the data needed to insert/update a row in the database.
// Each field maps to a column. `Option<T>` fields become NULL in the database
// when set to `None`.
// ============================================================================

/// Parameters for inserting/upserting a bill into the `bills` table.
#[derive(Debug, Clone)]
pub struct InsertBillParams {
    pub billid: Option<String>,       // e.g., "118-HR-42"
    pub billnumber: i32,               // e.g., 42
    pub billtype: String,              // e.g., "hr", "s", "hres"
    pub introducedat: Option<NaiveDate>,
    pub congress: i32,                 // e.g., 118
    pub summary_date: Option<String>,
    pub summary_text: Option<String>,
    pub sponsor_bioguide_id: Option<String>,
    pub sponsor_name: Option<String>,
    pub sponsor_state: Option<String>,
    pub sponsor_party: Option<String>,
    pub origin_chamber: Option<String>,
    pub policy_area: Option<String>,
    pub update_date: Option<NaiveDate>,
    pub latest_action_date: Option<NaiveDate>,
    pub bill_status: String,           // e.g., "introduced", "passed", "enacted"
    pub statusat: Option<NaiveDate>,
    pub shorttitle: Option<String>,
    pub officialtitle: Option<String>,
}

/// Parameters for inserting a bill action into the `bill_actions` table.
#[derive(Debug, Clone)]
pub struct InsertBillActionParams {
    pub billtype: String,
    pub billnumber: i32,
    pub congress: i32,
    pub acted_at: NaiveDate,           // When the action occurred (NOT optional)
    pub action_text: Option<String>,   // e.g., "Referred to Committee on..."
    pub action_type: Option<String>,   // e.g., "vote", "referral"
    pub action_code: Option<String>,   // Legislative action code
    pub source_system_code: Option<String>,
}

/// Parameters for inserting a bill cosponsor into the `bill_cosponsors` table.
#[derive(Debug, Clone)]
pub struct InsertBillCosponsorParams {
    pub billtype: String,
    pub billnumber: i32,
    pub congress: i32,
    pub bioguide_id: String,           // Unique ID for the legislator
    pub full_name: Option<String>,
    pub state: Option<String>,
    pub party: Option<String>,
    pub sponsorship_date: Option<NaiveDate>,
    pub is_original_cosponsor: Option<bool>,
}

/// Parameters for inserting a bill subject into the `bill_subjects` table.
#[derive(Debug, Clone)]
pub struct InsertBillSubjectParams {
    pub billtype: String,
    pub billnumber: i32,
    pub congress: i32,
    pub subject: String,               // e.g., "Health", "Taxation"
}

/// Parameters for inserting a committee into the `committees` table.
#[derive(Debug, Clone)]
pub struct InsertCommitteeParams {
    pub committee_code: String,        // e.g., "HSAG" (House Agriculture)
    pub committee_name: Option<String>,
    pub chamber: Option<String>,       // "House" or "Senate"
}

/// Parameters for inserting a vote into the `votes` table.
#[derive(Debug, Clone)]
pub struct InsertVoteParams {
    pub voteid: String,                // e.g., "s47-110.2008"
    pub bill_type: Option<String>,
    pub bill_number: Option<i32>,
    pub congress: Option<i32>,
    pub votenumber: Option<i32>,
    pub votedate: Option<NaiveDate>,
    pub question: Option<String>,
    pub result: Option<String>,        // e.g., "Passed", "Failed"
    pub votesession: Option<String>,
    pub chamber: Option<String>,       // "s" (Senate) or "h" (House)
    pub source_url: Option<String>,
    pub votetype: Option<String>,
}

/// Parameters for inserting a vote member into the `vote_members` table.
#[derive(Debug, Clone)]
pub struct InsertVoteMemberParams {
    pub voteid: String,
    pub bioguide_id: String,
    pub display_name: Option<String>,
    pub party: Option<String>,
    pub state: Option<String>,
    pub position: String,              // "yea", "nay", "notvoting", "present"
}

// ============================================================================
// Parsed intermediary structs
// ============================================================================
// These hold a complete parsed entity with all its related data, ready to
// be inserted into the database in a single transaction.
// ============================================================================

/// A fully parsed bill with all its associated data.
/// `Vec<T>` is Rust's growable array — like Python's `list` or JS's `Array`.
#[derive(Debug, Clone)]
pub struct ParsedBill {
    pub bill: InsertBillParams,
    pub actions: Vec<InsertBillActionParams>,
    pub cosponsors: Vec<InsertBillCosponsorParams>,
    pub committees: Vec<ParsedCommittee>,
    pub subjects: Vec<InsertBillSubjectParams>,
    pub latest_action_date: Option<NaiveDate>,
    pub latest_action_text: String,
}

/// Intermediate representation of a committee (before DB insertion).
#[derive(Debug, Clone)]
pub struct ParsedCommittee {
    pub committee_code: String,
    pub committee_name: String,
    pub chamber: String,
}

/// A fully parsed vote with its member votes.
#[derive(Debug, Clone)]
pub struct ParsedVote {
    pub vote: InsertVoteParams,
    pub members: Vec<InsertVoteMemberParams>,
}

// ============================================================================
// XML deserialization structs
// ============================================================================
// These structs mirror the structure of Congress.gov BillStatus XML files.
// `serde` + `quick-xml` automatically parse XML into these structs.
//
// The `#[serde(rename = "xmlTagName")]` annotations map Rust's snake_case
// field names to the XML's camelCase tag names. `#[serde(default)]` means
// the field gets its Default value if the XML tag is missing.
//
// Example XML that maps to `BillXmlNew`:
//   <bill>
//     <number>42</number>
//     <billType>HR</billType>
//     <introducedDate>2024-01-10</introducedDate>
//     <sponsors>
//       <item>
//         <bioguideId>S001234</bioguideId>
//         ...
//       </item>
//     </sponsors>
//     ...
//   </bill>
// ============================================================================

/// Wrapper around the summary section of a bill XML.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct XmlSummaries {
    #[serde(rename = "billSummaries", default)]
    pub bill_summaries: XmlBillSummaries,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct XmlBillSummaries {
    /// `Vec<XmlBillSummaryItem>` — a list of summary items.
    /// In the XML, each `<item>` child becomes one element in this vector.
    #[serde(rename = "item", default)]
    pub items: Vec<XmlBillSummaryItem>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct XmlBillSummaryItem {
    #[serde(rename = "lastSummaryUpdateDate", default)]
    pub date: String,
    #[serde(rename = "text", default)]
    pub text: String,
}

/// `<sourceSystem><code>...</code></sourceSystem>` in the XML.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct SourceSystemXml {
    #[serde(rename = "code", default)]
    pub code: String,
}

/// A single action item from the XML's `<actions>` section.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct ItemXml {
    #[serde(rename = "actionDate", default)]
    pub acted_at: String,
    #[serde(rename = "text", default)]
    pub text: String,
    #[serde(rename = "type", default)]
    pub item_type: String,
    #[serde(rename = "actionCode", default)]
    pub action_code: String,
    #[serde(rename = "sourceSystem", default)]
    pub source_system: SourceSystemXml,
}

/// Container for bill actions: `<actions><item>...</item></actions>`.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct ActionsXml {
    #[serde(rename = "item", default)]
    pub actions: Vec<ItemXml>,
}

/// A bill sponsor from the XML.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct SponsorXml {
    #[serde(rename = "bioguideId", default)]
    pub bioguide_id: String,
    #[serde(rename = "fullName", default)]
    pub full_name: String,
    #[serde(rename = "state", default)]
    pub state: String,
    #[serde(rename = "party", default)]
    pub party: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct SponsorsXml {
    #[serde(rename = "item", default)]
    pub sponsors: Vec<SponsorXml>,
}

/// A bill cosponsor from the XML.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct CosponsorXml {
    #[serde(rename = "bioguideId", default)]
    pub bioguide_id: String,
    #[serde(rename = "fullName", default)]
    pub full_name: String,
    #[serde(rename = "state", default)]
    pub state: String,
    #[serde(rename = "party", default)]
    pub party: String,
    #[serde(rename = "sponsorshipDate", default)]
    pub sponsorship_date: String,
    #[serde(rename = "isOriginalCosponsor", default)]
    pub is_original_cosponsor: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct CosponsorsXml {
    #[serde(rename = "item", default)]
    pub cosponsors: Vec<CosponsorXml>,
}

/// A bill title item from the XML.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct TitleItemXml {
    #[serde(rename = "titleType", default)]
    pub title_type: String,
    #[serde(rename = "title", default)]
    pub title: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct TitlesXml {
    #[serde(rename = "item", default)]
    pub items: Vec<TitleItemXml>,
}

/// A committee item from the XML.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct CommitteeItemXml {
    #[serde(rename = "systemCode", default)]
    pub system_code: String,
    #[serde(rename = "name", default)]
    pub name: String,
    #[serde(rename = "chamber", default)]
    pub chamber: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct CommitteesXml {
    #[serde(rename = "item", default)]
    pub items: Vec<CommitteeItemXml>,
}

/// A legislative subject from the XML.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct SubjectItemXml {
    #[serde(rename = "name", default)]
    pub name: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct LegislativeSubjectsXml {
    #[serde(rename = "item", default)]
    pub items: Vec<SubjectItemXml>,
}

/// Policy area (e.g., "Health", "Taxation") from the XML.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct PolicyAreaXml {
    #[serde(rename = "name", default)]
    pub name: String,
}

/// Container for subjects including both legislative subjects and policy area.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct SubjectsXml {
    #[serde(rename = "legislativeSubjects", default)]
    pub legislative_subjects: LegislativeSubjectsXml,
    #[serde(rename = "policyArea", default)]
    pub policy_area: PolicyAreaXml,
}

/// The latest action element from the XML.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct LatestActionXml {
    #[serde(rename = "actionDate", default)]
    pub action_date: String,
    #[serde(rename = "text", default)]
    pub text: String,
}

// ============================================================================
// Bill XML root structs — Legacy vs New format
// ============================================================================
//
// Congress.gov has changed their XML schema over the years. Older bills use
// `billNumber`/`billType` while newer ones use `number`/`type`. We try the
// new format first and fall back to legacy if the `number` field is empty.
// ============================================================================

/// Legacy XML format (older congresses). Uses `billNumber` and `billType`.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct BillXmlLegacy {
    #[serde(rename = "billNumber", default)]
    pub number: String,
    #[serde(rename = "billType", default)]
    pub bill_type: String,
    #[serde(rename = "introducedDate", default)]
    pub introduced_at: String,
    #[serde(rename = "updateDate", default)]
    pub update_date: String,
    #[serde(rename = "originChamber", default)]
    pub origin_chamber: String,
    #[serde(rename = "congress", default)]
    pub congress: String,
    #[serde(rename = "summaries", default)]
    pub summary: XmlSummaries,
    #[serde(rename = "actions", default)]
    pub actions: ActionsXml,
    #[serde(rename = "sponsors", default)]
    pub sponsors: SponsorsXml,
    #[serde(rename = "cosponsors", default)]
    pub cosponsors: CosponsorsXml,
    #[serde(rename = "titles", default)]
    pub titles: TitlesXml,
    #[serde(rename = "committees", default)]
    pub committees: CommitteesXml,
    #[serde(rename = "subjects", default)]
    pub subjects: SubjectsXml,
    #[serde(rename = "latestAction", default)]
    pub latest_action: LatestActionXml,
    #[serde(rename = "title", default)]
    pub short_title: String,
}

/// New XML format (recent congresses). Uses `number` and `type`/`billType`.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct BillXmlNew {
    #[serde(rename = "number", default)]
    pub number: String,
    #[serde(rename = "billType", default)]
    pub bill_type: String,
    /// New-format bills use `<type>` instead of `<billType>`.
    #[serde(rename = "type", default)]
    pub bill_type_new: String,
    #[serde(rename = "introducedDate", default)]
    pub introduced_at: String,
    #[serde(rename = "updateDate", default)]
    pub update_date: String,
    #[serde(rename = "originChamber", default)]
    pub origin_chamber: String,
    #[serde(rename = "congress", default)]
    pub congress: String,
    #[serde(rename = "summaries", default)]
    pub summary: XmlSummaries,
    #[serde(rename = "actions", default)]
    pub actions: ActionsXml,
    #[serde(rename = "sponsors", default)]
    pub sponsors: SponsorsXml,
    #[serde(rename = "cosponsors", default)]
    pub cosponsors: CosponsorsXml,
    #[serde(rename = "titles", default)]
    pub titles: TitlesXml,
    #[serde(rename = "committees", default)]
    pub committees: CommitteesXml,
    #[serde(rename = "subjects", default)]
    pub subjects: SubjectsXml,
    #[serde(rename = "latestAction", default)]
    pub latest_action: LatestActionXml,
    #[serde(rename = "title", default)]
    pub short_title: String,
}

/// Root wrapper for legacy XML: `<billStatus><bill>...</bill></billStatus>`.
/// `#[serde(rename = "billStatus")]` tells the XML parser that the root
/// element is called `<billStatus>`.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename = "billStatus")]
pub struct BillXmlRootLegacy {
    #[serde(rename = "bill", default)]
    pub bill: BillXmlLegacy,
}

/// Root wrapper for new XML format.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename = "billStatus")]
pub struct BillXmlRootNew {
    #[serde(rename = "bill", default)]
    pub bill: BillXmlNew,
}

// ============================================================================
// JSON deserialization structs
// ============================================================================
// These mirror the structure of the older JSON data format used by some
// congress sessions. `serde_json` parses JSON into these automatically.
//
// The `BillJson` struct is also used as a "sidecar" to read bill status
// from a data.json file when the primary source is XML.
// ============================================================================

/// Bill data from JSON format (older congresses).
#[derive(Debug, Deserialize, Default, Clone)]
pub struct BillJson {
    #[serde(default, deserialize_with = "null_or_default")]
    pub number: String,
    #[serde(rename = "bill_type", default, deserialize_with = "null_or_default")]
    pub bill_type: String,
    #[serde(rename = "introduced_at", default, deserialize_with = "null_or_default")]
    pub introduced_at: String,
    #[serde(default, deserialize_with = "null_or_default")]
    pub congress: String,
    #[serde(default, deserialize_with = "null_or_default")]
    pub status: String,
    #[serde(default, deserialize_with = "null_or_default")]
    pub summary: BillJsonSummary,
    #[serde(default, deserialize_with = "null_or_default")]
    pub actions: Vec<BillJsonAction>,
    #[serde(default, deserialize_with = "null_or_default")]
    pub sponsor: BillJsonPerson,
    #[serde(default, deserialize_with = "null_or_default")]
    pub cosponsors: Vec<BillJsonPerson>,
    #[serde(rename = "status_at", default, deserialize_with = "null_or_default")]
    pub status_at: String,
    #[serde(rename = "short_title", default, deserialize_with = "null_or_default")]
    pub short_title: String,
    #[serde(rename = "official_title", default, deserialize_with = "null_or_default")]
    pub official_title: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct BillJsonSummary {
    #[serde(default, deserialize_with = "null_or_default")]
    pub date: String,
    #[serde(default, deserialize_with = "null_or_default")]
    pub text: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct BillJsonAction {
    #[serde(rename = "acted_at", default, deserialize_with = "null_or_default")]
    pub acted_at: String,
    #[serde(default, deserialize_with = "null_or_default")]
    pub text: String,
    /// `r#type` — the `r#` prefix lets us use `type` as an identifier even
    /// though it's a reserved keyword in Rust. Like backtick-quoting `type`
    /// in Kotlin, or using `getattr(obj, 'type')` in Python.
    #[serde(default, deserialize_with = "null_or_default")]
    pub r#type: String,
}

/// A person (sponsor or cosponsor) from JSON bill data.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct BillJsonPerson {
    #[serde(default, deserialize_with = "null_or_default")]
    pub title: String,
    #[serde(default, deserialize_with = "null_or_default")]
    pub name: String,
    #[serde(default, deserialize_with = "null_or_default")]
    pub state: String,
    #[serde(default, deserialize_with = "null_or_default")]
    pub party: String,
}

// ============================================================================
// Vote JSON deserialization structs
// ============================================================================

/// The bill referenced by a vote (if any).
#[derive(Debug, Deserialize, Default, Clone)]
pub struct VoteBillJson {
    #[serde(default)]
    pub congress: i32,
    #[serde(default)]
    pub number: i32,
    #[serde(rename = "type", default)]
    pub bill_type: String,
}

/// A member who voted in a roll call vote.
#[derive(Debug, Deserialize, Default, Clone)]
pub struct VoteMemberJson {
    #[serde(rename = "display_name", default)]
    pub display_name: String,
    #[serde(rename = "id", default)]
    pub id: String,
    #[serde(rename = "party", default)]
    pub party: String,
    #[serde(rename = "state", default)]
    pub state: String,
}

/// A complete vote record from JSON.
///
/// The `votes` field is a HashMap where keys are positions ("Yea", "Nay", etc.)
/// and values are arrays of member objects (or sometimes strings like "VP").
/// We use `serde_json::Value` (an untyped JSON value, like `any` in TypeScript)
/// for the array elements because the data is heterogeneous — some entries
/// are objects, some are strings, and some are null. We parse them manually
/// in `votes.rs`.
///
/// `std::collections::HashMap<String, Vec<Value>>` is like:
///   - Python: `dict[str, list[Any]]`
///   - TypeScript: `Record<string, any[]>`
#[derive(Debug, Deserialize, Default, Clone)]
pub struct VoteJson {
    #[serde(default)]
    pub bill: VoteBillJson,
    #[serde(default)]
    pub number: i32,
    #[serde(rename = "bill_type", default)]
    pub bill_type: String,
    #[serde(default)]
    pub congress: i32,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub chamber: String,
    #[serde(rename = "date", default)]
    pub votedate: String,
    #[serde(rename = "session", default)]
    pub session: String,
    #[serde(rename = "source_url", default)]
    pub source_url: String,
    #[serde(rename = "type", default)]
    pub votetype: String,
    #[serde(rename = "vote_id", default)]
    pub vote_id: String,
    #[serde(default)]
    pub votes: std::collections::HashMap<String, Vec<Value>>,
}
