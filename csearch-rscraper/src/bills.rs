// ============================================================================
// bills.rs — Bill data processing pipeline
// ============================================================================
//
// This is the largest and most complex module. It handles:
//   1. Syncing bill data from Congress.gov (via Python subprocess)
//   2. Processing 8 bill types across congresses 93 to present
//   3. Parsing bills from two XML schemas (legacy + new) and JSON
//   4. Writing parsed bills to PostgreSQL with all related data
//      (actions, cosponsors, committees, subjects) in a single transaction
//
// The parallel processing architecture is identical to votes.rs:
//   Phase 1: Parse files on blocking thread pool (semaphore-gated, 64 workers)
//   Phase 2: Write to database (semaphore-gated, 4 concurrent)
//
// ============================================================================

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
// `quick_xml::de::from_str` deserializes XML strings into Rust structs,
// just like `serde_json::from_str` does for JSON. It uses the same `serde`
// framework, so the same `#[serde(rename = "...")]` annotations work.
use quick_xml::de::from_str;
use sqlx::PgPool;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::config::{Config, current_congress};
use crate::db;
use crate::hashes::{FileHashStore, sha256_file};
use crate::models::{
    ActionsXml, BillJson, BillJsonAction, BillXmlRootLegacy, BillXmlRootNew, CommitteesXml,
    CosponsorsXml, InsertBillActionParams, InsertBillCosponsorParams, InsertBillParams,
    InsertBillSubjectParams, InsertCommitteeParams, ParsedBill, ParsedCommittee, SponsorsXml,
    SubjectsXml, TitlesXml, XmlSummaries,
};
use crate::python::run_congress_task;
use crate::stats::RunStats;
use crate::util::{
    file_exists, must_parse_date_value, option_string, parse_date_value, parse_i32_value,
};

/// The 8 types of congressional bills/resolutions.
/// `&[&str]` is a slice of string references — a fixed-size array known at
/// compile time. Unlike `Vec`, it lives in the program's read-only data section.
const BILL_TABLES: &[&str] = &[
    "s", "hr", "hconres", "hjres", "hres", "sconres", "sjres", "sres",
];
const WORKER_LIMIT: usize = 64;
const DB_WRITE_CONCURRENCY: usize = 4;

/// A function pointer type for bill parsers.
///
/// `type` creates a type alias — a shorthand name for a complex type.
/// `fn(&Path) -> Result<ParsedBill>` is a function pointer — it stores
/// a reference to a function that takes a `&Path` and returns a `Result`.
///
/// This lets us dynamically choose between `parse_bill_xml` and
/// `parse_bill_json` at runtime, based on which file format is available.
/// Like storing a function reference in Python: `parser = parse_bill_xml`.
type BillParser = fn(&Path) -> Result<ParsedBill>;

/// A pending bill file with its parser function.
struct BillJob {
    path: PathBuf,
    /// Which parser to use (XML or JSON). This is a function pointer —
    /// we call it with `(job.parse)(&job.path)`.
    parse: BillParser,
    /// Human-readable bill identifier for error messages.
    display: String,
}

/// A bill that was parsed and found to have changed.
struct ChangedBill {
    parsed_bill: ParsedBill,
    path: PathBuf,
    hash: String,
}

/// Aggregated results from the parsing phase.
struct BillCollectResult {
    changed_bills: Vec<ChangedBill>,
    skipped: u32,
    failed: u32,
}

/// Outcome of parsing a single bill file.
/// Same pattern as `VoteParseOutcome` in votes.rs.
enum BillParseOutcome {
    Changed(ChangedBill),
    Skipped,
}

// ============================================================================
// Public API
// ============================================================================

/// Syncs bill data from Congress.gov using the Python govinfo tool.
/// Runs: `python3 run.py govinfo --bulkdata=BILLSTATUS --congress=N`
pub async fn update_bills(cfg: &Config) -> Result<()> {
    let congress = current_congress();
    if let Err(err) = run_congress_task(
        cfg,
        &[
            "govinfo",
            "--bulkdata=BILLSTATUS",
            &format!("--congress={congress}"),
        ],
    )
    .await
    {
        warn!(congress, error = %err, "bill sync skipped");
    }
    Ok(())
}

/// Processes bill data for all congress sessions from 93 to current.
///
/// For each congress, processes all 8 bill types (s, hr, hconres, etc.).
/// The inner loop structure means we process one bill type at a time
/// within each congress, allowing fine-grained progress logging.
pub async fn process_bills(
    pool: &PgPool,
    cfg: &Config,
    hashes: &mut FileHashStore,
    stats: &mut RunStats,
) -> Result<()> {
    for congress in 93..=current_congress() {
        // Iterate over each bill type (s, hr, hconres, hjres, etc.)
        for table in BILL_TABLES {
            let jobs = match bill_jobs_for_table(cfg, congress, table) {
                Ok(jobs) => jobs,
                Err(err) => {
                    warn!(congress, billtype = *table, error = %err, "skipping congress bill type");
                    stats.bills_failed += 1;
                    continue;
                }
            };

            info!(
                congress,
                billtype = *table,
                candidates = jobs.len(),
                "processing congress bill type"
            );

            let bill_candidates = jobs.len() as u32;

            // Phase 1: Parse changed bills in parallel.
            let collected = collect_changed_bills(jobs, hashes, congress, table).await;
            let changed_candidates = collected.changed_bills.len() as u32;
            stats.bills_skipped += u64::from(collected.skipped);
            stats.bills_failed += u64::from(collected.failed);

            // Phase 2: Write to database with concurrency limit.
            let write_sem = Arc::new(Semaphore::new(DB_WRITE_CONCURRENCY));
            let mut write_tasks = JoinSet::new();

            for changed_bill in collected.changed_bills {
                let pool = pool.clone();
                let write_sem = write_sem.clone();
                // `(*table).to_string()` — `*table` dereferences `&&str` to `&str`,
                // then `.to_string()` creates an owned String. We need an owned
                // String because the `async move` block takes ownership.
                let billtype = (*table).to_string();
                write_tasks.spawn(async move {
                    let _permit = write_sem.acquire_owned().await?;
                    insert_parsed_bill(&pool, &changed_bill.parsed_bill).await?;
                    Ok::<_, anyhow::Error>((changed_bill, billtype))
                });
            }

            // Collect write results.
            let mut table_processed = 0u32;
            let mut table_failed = collected.failed;
            while let Some(result) = write_tasks.join_next().await {
                match result {
                    Ok(Ok((changed_bill, _billtype))) => {
                        hashes.mark_processed(&changed_bill.path, changed_bill.hash);
                        stats.bills_processed += 1;
                        table_processed += 1;
                    }
                    Ok(Err(err)) => {
                        warn!(congress, billtype = *table, error = %err, "unable to insert bill");
                        stats.bills_failed += 1;
                        table_failed += 1;
                    }
                    Err(err) => {
                        warn!(congress, billtype = *table, error = %err, "bill write task failed");
                        stats.bills_failed += 1;
                        table_failed += 1;
                    }
                }
            }

            info!(
                congress,
                billtype = *table,
                candidates = bill_candidates,
                changed = changed_candidates,
                skipped = collected.skipped,
                processed = table_processed,
                failed = table_failed,
                "congress bill type done"
            );
        }

        // Save hashes after each congress (checkpoint for crash recovery).
        hashes
            .save()
            .with_context(|| format!("persist bill hashes for congress {congress}"))?;
    }

    Ok(())
}

// ============================================================================
// File discovery
// ============================================================================

/// Discovers all bill files for a given congress and bill type.
///
/// Directory structure:
///   {congress_dir}/congress/data/{congress}/bills/{type}/{number}/
///     - fdsys_billstatus.xml  (preferred — newer, more complete)
///     - data.json             (fallback — older format)
///
/// Returns a Vec<BillJob> where each job has the appropriate parser function.
fn bill_jobs_for_table(cfg: &Config, congress: i32, table: &str) -> Result<Vec<BillJob>> {
    let directory = cfg
        .congress_dir
        .join("congress")
        .join("data")
        .join(congress.to_string())
        .join("bills")
        .join(table);
    let entries = fs::read_dir(directory)?;

    let mut jobs = Vec::new();
    for entry in entries {
        let entry = entry?;
        let base = entry.path();

        // Prefer XML format (newer, more fields).
        let xml_path = base.join("fdsys_billstatus.xml");
        if file_exists(&xml_path) {
            jobs.push(BillJob {
                path: xml_path,
                // `parse_bill_xml` is a function pointer — we store the
                // function itself (not a call to it) so we can call it later.
                parse: parse_bill_xml,
                display: entry.file_name().to_string_lossy().into_owned(),
            });
            continue;
        }

        // Fall back to JSON format.
        let json_path = base.join("data.json");
        if !file_exists(&json_path) {
            continue;
        }

        jobs.push(BillJob {
            path: json_path,
            parse: parse_bill_json,
            display: entry.file_name().to_string_lossy().into_owned(),
        });
    }

    Ok(jobs)
}

// ============================================================================
// Phase 1: Parallel bill parsing
// ============================================================================

/// Same pattern as `collect_changed_votes` in votes.rs.
/// See that function for detailed comments on Arc, Semaphore, JoinSet, etc.
async fn collect_changed_bills(
    jobs: Vec<BillJob>,
    hashes: &FileHashStore,
    congress: i32,
    billtype: &str,
) -> BillCollectResult {
    let known_hashes = Arc::new(hashes.snapshot());
    let parse_sem = Arc::new(Semaphore::new(WORKER_LIMIT));
    let mut tasks = JoinSet::new();

    for job in jobs {
        let known_hashes = known_hashes.clone();
        let parse_sem = parse_sem.clone();
        tasks.spawn(async move {
            let _permit = parse_sem.acquire_owned().await?;
            // Offload CPU-heavy XML/JSON parsing to blocking thread pool.
            tokio::task::spawn_blocking(move || parse_bill_job(job, &known_hashes)).await?
        });
    }

    let mut changed_bills = Vec::new();
    let mut skipped = 0u32;
    let mut failed = 0u32;
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Ok(BillParseOutcome::Changed(changed_bill))) => changed_bills.push(changed_bill),
            Ok(Ok(BillParseOutcome::Skipped)) => skipped += 1,
            Ok(Err(err)) => {
                warn!(congress, billtype, error = %err, "unable to parse bill");
                failed += 1;
            }
            Err(err) => {
                warn!(congress, billtype, error = %err, "bill parse task failed");
                failed += 1;
            }
        }
    }

    BillCollectResult {
        changed_bills,
        skipped,
        failed,
    }
}

/// Synchronous: checks if a bill file has changed and parses it if so.
/// Runs on the blocking thread pool via `spawn_blocking`.
fn parse_bill_job(
    job: BillJob,
    known_hashes: &HashMap<String, String>,
) -> Result<BillParseOutcome> {
    let hash =
        sha256_file(&job.path).with_context(|| format!("hash bill {}", job.path.display()))?;
    let key = job.path.to_string_lossy();
    if known_hashes.get(key.as_ref()) == Some(&hash) {
        return Ok(BillParseOutcome::Skipped);
    }

    // Call the parser function (either parse_bill_xml or parse_bill_json)
    // via the function pointer stored in the job.
    // `(job.parse)(&job.path)` — parentheses around `job.parse` are needed
    // to call a function pointer stored in a struct field.
    let parsed_bill = (job.parse)(&job.path)
        .with_context(|| format!("parse bill {} ({})", job.display, job.path.display()))?;

    Ok(BillParseOutcome::Changed(ChangedBill {
        parsed_bill,
        path: job.path,
        hash,
    }))
}

// ============================================================================
// JSON bill parsing (older congress sessions)
// ============================================================================

/// Parses a bill from the older JSON format (used by pre-XML congresses).
fn parse_bill_json(path: &Path) -> Result<ParsedBill> {
    let data = fs::read_to_string(path)?;
    let bill_json: BillJson = serde_json::from_str(&data)?;

    // Build sponsor display name from title + name + state.
    let sponsor_name = if bill_json.sponsor.title.is_empty() {
        format!("{} [{}]", bill_json.sponsor.name, bill_json.sponsor.state)
    } else {
        format!(
            "{} {} [{}]",
            bill_json.sponsor.title, bill_json.sponsor.name, bill_json.sponsor.state
        )
    };

    // Find the most recent action by comparing date strings.
    let (latest_action_date, latest_action_text) = latest_json_action(&bill_json.actions);

    // Parse date fields — the `?` propagates parsing errors.
    let parsed_introduced_at = parse_date_value(&bill_json.introduced_at)
        .with_context(|| format!("parse introduced date for {}", path.display()))?;
    let parsed_status_at = parse_date_value(&bill_json.status_at)
        .with_context(|| format!("parse status date for {}", path.display()))?;
    let parsed_latest_action_date = parse_date_value(&latest_action_date)
        .with_context(|| format!("parse latest action date for {}", path.display()))?;

    // Determine statusat with a fallback chain: status_at -> latest_action -> introduced.
    // `.or()` chains Option values — returns the first `Some(...)` found.
    // Like Python's `a or b or c`.
    let status_at = parsed_status_at
        .or(parsed_latest_action_date)
        .or(parsed_introduced_at)
        .ok_or_else(|| anyhow!("missing status date for {}", path.display()))?;

    let bill_number = parse_i32_value(&bill_json.number)
        .with_context(|| format!("parse bill number for {}", path.display()))?;
    let congress = parse_i32_value(&bill_json.congress)
        .with_context(|| format!("parse congress for {}", path.display()))?;

    let bill_type_lower = bill_json.bill_type.to_ascii_lowercase();
    let bill = InsertBillParams {
        billid: option_string(format!(
            "{}-{}-{}",
            congress, bill_json.bill_type, bill_number
        )),
        billnumber: bill_number,
        billtype: bill_type_lower.clone(),
        introducedat: parsed_introduced_at,
        congress,
        summary_date: option_string(bill_json.summary.date),
        summary_text: option_string(bill_json.summary.text),
        sponsor_bioguide_id: None,
        sponsor_name: option_string(sponsor_name),
        sponsor_state: option_string(bill_json.sponsor.state),
        sponsor_party: option_string(bill_json.sponsor.party),
        origin_chamber: None,
        policy_area: None,
        update_date: None,
        latest_action_date: parsed_latest_action_date,
        bill_status: normalize_bill_status(&bill_json.status, &latest_action_text),
        statusat: Some(status_at),
        shorttitle: option_string(bill_json.short_title),
        officialtitle: option_string(bill_json.official_title),
    };

    // Parse actions into InsertBillActionParams.
    // `Vec::with_capacity(n)` pre-allocates memory — an optimization.
    let mut actions = Vec::with_capacity(bill_json.actions.len());
    for action in bill_json.actions {
        let acted_at = parse_date_value(&action.acted_at)
            .with_context(|| format!("parse action date for {}", path.display()))?
            // `.ok_or_else(...)` converts `None` to an error.
            .ok_or_else(|| anyhow!("missing action date for {}", path.display()))?;

        actions.push(InsertBillActionParams {
            billtype: bill_type_lower.clone(),
            billnumber: bill_number,
            congress,
            acted_at,
            action_text: option_string(action.text),
            // `r#type` — using the raw identifier syntax because `type` is
            // a reserved keyword in Rust (see models.rs for more on `r#`).
            action_type: option_string(action.r#type),
            action_code: None,
            source_system_code: None,
        });
    }

    // Parse cosponsors.
    let mut cosponsors = Vec::with_capacity(bill_json.cosponsors.len());
    for cosponsor in bill_json.cosponsors {
        let name = if cosponsor.title.is_empty() {
            format!("{} [{}]", cosponsor.name, cosponsor.state)
        } else {
            format!(
                "{} {} [{}]",
                cosponsor.title, cosponsor.name, cosponsor.state
            )
        };
        if name.is_empty() {
            continue;
        }

        cosponsors.push(InsertBillCosponsorParams {
            billtype: bill_type_lower.clone(),
            billnumber: bill_number,
            congress,
            bioguide_id: String::new(),
            full_name: option_string(name),
            state: option_string(cosponsor.state),
            party: option_string(cosponsor.party),
            sponsorship_date: None,
            is_original_cosponsor: None,
        });
    }

    Ok(ParsedBill {
        bill,
        actions,
        cosponsors,
        committees: Vec::new(), // JSON format doesn't have committee data.
        subjects: Vec::new(),   // JSON format doesn't have subject data.
        latest_action_date: parsed_latest_action_date,
        latest_action_text,
    })
}

// ============================================================================
// XML bill parsing (newer congress sessions)
// ============================================================================

/// Parses a bill from XML format (fdsys_billstatus.xml).
///
/// Congress.gov has two XML schemas:
///   - **New format**: Uses `<number>` and `<type>` tags
///   - **Legacy format**: Uses `<billNumber>` and `<billType>` tags
///
/// We try the new format first. If the `number` field is empty (indicating
/// the XML uses the legacy schema), we re-parse with the legacy struct.
fn parse_bill_xml(path: &Path) -> Result<ParsedBill> {
    let data = fs::read_to_string(path)?;

    // Try new XML format first.
    // `from_str` is `quick_xml::de::from_str` — XML deserialization.
    let root_new: BillXmlRootNew = from_str(&data)?;
    let bill = root_new.bill;

    // Determine bill type from whichever field is populated.
    let bill_type = if bill.bill_type_new.is_empty() {
        bill.bill_type.to_ascii_lowercase()
    } else {
        bill.bill_type_new.to_ascii_lowercase()
    };

    // Try to get bill status from a companion data.json file (sidecar).
    let bill_status = bill_status_from_sidecar(path);

    // If the number field is populated, we successfully parsed the new format.
    if !bill.number.is_empty() {
        let bill_number = parse_i32_value(&bill.number)
            .with_context(|| format!("parse bill number for {}", path.display()))?;
        let congress = parse_i32_value(&bill.congress)
            .with_context(|| format!("parse congress for {}", path.display()))?;
        return build_parsed_bill(
            bill_number,
            bill_type,
            &bill.introduced_at,
            &bill.update_date,
            &bill.origin_chamber,
            congress,
            &bill.short_title,
            &bill.latest_action.action_date,
            &bill.latest_action.text,
            bill.summary,
            bill.actions,
            bill.sponsors,
            bill.cosponsors,
            bill.titles,
            bill.committees,
            bill.subjects,
            &bill_status,
        );
    }

    // Fall back to legacy XML format.
    let root_legacy: BillXmlRootLegacy = from_str(&data)?;
    let legacy_bill = root_legacy.bill;
    let bill_number = parse_i32_value(&legacy_bill.number)
        .with_context(|| format!("parse bill number for {}", path.display()))?;
    let congress = parse_i32_value(&legacy_bill.congress)
        .with_context(|| format!("parse congress for {}", path.display()))?;

    build_parsed_bill(
        bill_number,
        legacy_bill.bill_type.to_ascii_lowercase(),
        &legacy_bill.introduced_at,
        &legacy_bill.update_date,
        &legacy_bill.origin_chamber,
        congress,
        &legacy_bill.short_title,
        &legacy_bill.latest_action.action_date,
        &legacy_bill.latest_action.text,
        legacy_bill.summary,
        legacy_bill.actions,
        legacy_bill.sponsors,
        legacy_bill.cosponsors,
        legacy_bill.titles,
        legacy_bill.committees,
        legacy_bill.subjects,
        &bill_status,
    )
}

/// Builds a `ParsedBill` from XML-extracted components.
///
/// `#[allow(clippy::too_many_arguments)]` suppresses a lint warning about
/// having too many function parameters. Normally Clippy (Rust's linter)
/// suggests refactoring, but here we accept it since this is a data
/// transformation function that needs all these inputs.
///
/// This function is shared between new and legacy XML formats.
#[allow(clippy::too_many_arguments)]
fn build_parsed_bill(
    number: i32,
    bill_type: String,
    introduced_at: &str,
    update_date: &str,
    origin_chamber: &str,
    congress: i32,
    short_title: &str,
    latest_action_date: &str,
    latest_action_text: &str,
    summary: XmlSummaries,
    actions: ActionsXml,
    sponsors: SponsorsXml,
    cosponsors: CosponsorsXml,
    titles: TitlesXml,
    committees: CommitteesXml,
    subjects: SubjectsXml,
    bill_status: &str,
) -> Result<ParsedBill> {
    // Use provided latest action date/text, or derive from actions list.
    let mut latest_action_date_value = latest_action_date.to_string();
    let mut latest_action_text_value = latest_action_text.to_string();

    let (derived_latest_action_date, derived_latest_action_text) = latest_xml_action(&actions);
    if latest_action_date_value.is_empty() {
        latest_action_date_value = derived_latest_action_date;
    }
    if latest_action_text_value.is_empty() {
        latest_action_text_value = derived_latest_action_text;
    }

    // Parse dates, using `.unwrap_or(None)` to silently handle parse failures.
    let parsed_introduced_at = parse_date_value(introduced_at).unwrap_or(None);
    let parsed_update_date = parse_date_value(update_date).unwrap_or(None);
    let parsed_latest_action_date = parse_date_value(&latest_action_date_value).unwrap_or(None);

    // Extract summary from the first summary item (if any).
    // `.first()` returns `Option<&T>` — the first element or None.
    // `.map(|item| ...)` transforms the inner value if present.
    // `.unwrap_or((None, None))` provides the default if no items exist.
    let (summary_date, summary_text) = summary
        .bill_summaries
        .items
        .first()
        .map(|item| {
            (
                option_string(item.date.clone()),
                option_string(item.text.clone()),
            )
        })
        .unwrap_or((None, None));

    // Extract sponsor info from the first sponsor (if any).
    // Same `.first().map(...).unwrap_or(...)` pattern.
    let (sponsor_bioguide_id, sponsor_name, sponsor_state, sponsor_party) = sponsors
        .sponsors
        .first()
        .map(|sponsor| {
            (
                option_string(sponsor.bioguide_id.clone()),
                option_string(sponsor.full_name.clone()),
                option_string(sponsor.state.clone()),
                option_string(sponsor.party.clone()),
            )
        })
        .unwrap_or((None, None, None, None));

    // Use official title if available, fall back to short title.
    let mut official = official_title(&titles);
    if official.is_empty() {
        official = short_title.to_string();
    }

    // Determine status date with fallback chain.
    // `.or_else(|| ...)` is like `.or()` but lazily evaluates the fallback.
    let status_at = parsed_latest_action_date
        .or(parsed_introduced_at)
        .or_else(|| must_parse_date_value(introduced_at));

    let bill = InsertBillParams {
        billid: option_string(format!(
            "{}-{}-{}",
            congress,
            bill_type.to_ascii_uppercase(),
            number
        )),
        billnumber: number,
        billtype: bill_type.clone(),
        introducedat: parsed_introduced_at,
        congress,
        summary_date,
        summary_text,
        sponsor_bioguide_id,
        sponsor_name,
        sponsor_state,
        sponsor_party,
        origin_chamber: option_string(origin_chamber.to_string()),
        policy_area: option_string(subjects.policy_area.name.clone()),
        update_date: parsed_update_date,
        latest_action_date: parsed_latest_action_date,
        bill_status: normalize_bill_status(bill_status, &latest_action_text_value),
        statusat: status_at,
        shorttitle: option_string(short_title.to_string()),
        officialtitle: option_string(official),
    };

    // Parse actions from XML, skipping entries with empty dates.
    let mut parsed_actions = Vec::with_capacity(actions.actions.len());
    for action in actions.actions {
        if action.acted_at.is_empty() {
            continue;
        }
        // `let Some(acted_at) = ... else { continue }` — pattern matching
        // that skips to the next iteration if the date is None or invalid.
        let Some(acted_at) = parse_date_value(&action.acted_at).unwrap_or(None) else {
            continue;
        };

        parsed_actions.push(InsertBillActionParams {
            billtype: bill_type.clone(),
            billnumber: number,
            congress,
            acted_at,
            action_text: option_string(action.text),
            action_type: option_string(action.item_type),
            action_code: option_string(action.action_code),
            source_system_code: option_string(action.source_system.code),
        });
    }

    // Parse cosponsors, skipping entries without bioguide IDs.
    let mut parsed_cosponsors = Vec::with_capacity(cosponsors.cosponsors.len());
    for cosponsor in cosponsors.cosponsors {
        if cosponsor.bioguide_id.is_empty() {
            continue;
        }

        parsed_cosponsors.push(InsertBillCosponsorParams {
            billtype: bill_type.clone(),
            billnumber: number,
            congress,
            bioguide_id: cosponsor.bioguide_id,
            full_name: option_string(cosponsor.full_name),
            state: option_string(cosponsor.state),
            party: option_string(cosponsor.party),
            sponsorship_date: must_parse_date_value(&cosponsor.sponsorship_date),
            // Parse "true"/"false" string to bool Option.
            // `.eq_ignore_ascii_case("true")` is case-insensitive comparison.
            is_original_cosponsor: if cosponsor.is_original_cosponsor.is_empty() {
                None
            } else {
                Some(cosponsor.is_original_cosponsor.eq_ignore_ascii_case("true"))
            },
        });
    }

    // Parse committees, skipping entries without system codes.
    let mut parsed_committees = Vec::with_capacity(committees.items.len());
    for committee in committees.items {
        if committee.system_code.is_empty() {
            continue;
        }

        parsed_committees.push(ParsedCommittee {
            committee_code: committee.system_code,
            committee_name: committee.name,
            chamber: committee.chamber,
        });
    }

    // Parse legislative subjects.
    let mut parsed_subjects = Vec::with_capacity(subjects.legislative_subjects.items.len());
    for subject in subjects.legislative_subjects.items {
        if subject.name.is_empty() {
            continue;
        }

        parsed_subjects.push(InsertBillSubjectParams {
            billtype: bill_type.clone(),
            billnumber: number,
            congress,
            subject: subject.name,
        });
    }

    Ok(ParsedBill {
        bill,
        actions: parsed_actions,
        cosponsors: parsed_cosponsors,
        committees: parsed_committees,
        subjects: parsed_subjects,
        latest_action_date: parsed_latest_action_date,
        latest_action_text: latest_action_text_value,
    })
}

// ============================================================================
// Database insertion
// ============================================================================

/// Inserts a parsed bill and ALL its related data in a single transaction.
///
/// This is the most complex DB operation. In one atomic transaction:
///   1. Upsert the bill record
///   2. Clear + re-insert actions (and link the latest action)
///   3. Clear + re-insert cosponsors
///   4. Upsert committees + clear + re-insert bill-committee associations
///   5. Clear + re-insert subjects
///
/// If ANY step fails, the entire transaction is rolled back (nothing is
/// partially written). This ensures data consistency.
async fn insert_parsed_bill(pool: &PgPool, parsed_bill: &ParsedBill) -> Result<()> {
    let bill = &parsed_bill.bill;

    // Validate required fields before starting the transaction.
    if bill.billnumber == 0 || bill.billtype.is_empty() {
        return Err(anyhow!(
            "skipping bill with empty number/type (congress={}, id={})",
            bill.congress,
            // `.clone().unwrap_or_default()` — clone the Option<String>,
            // then unwrap to the inner String or an empty string if None.
            bill.billid.clone().unwrap_or_default()
        ));
    }
    if bill.bill_status.is_empty() {
        return Err(anyhow!(
            "skipping bill {}-{}-{}: bill_status is empty",
            bill.congress,
            bill.billtype,
            bill.billnumber
        ));
    }
    if bill.statusat.is_none() {
        return Err(anyhow!(
            "skipping bill {}-{}-{}: statusat is empty",
            bill.congress,
            bill.billtype,
            bill.billnumber
        ));
    }

    // Start a database transaction.
    let mut tx = pool.begin().await?;

    // Step 1: Upsert the bill.
    db::insert_bill(&mut tx, bill).await.with_context(|| {
        format!(
            "InsertBill failed for {}-{}-{}",
            bill.congress, bill.billtype, bill.billnumber
        )
    })?;

    // Step 2: Re-insert actions.
    // First clear the latest_action_id (it references bill_actions, so we
    // must clear it before deleting actions to avoid FK constraint issues).
    db::clear_bill_latest_action(
        &mut tx,
        &bill.billtype,
        bill.billnumber,
        bill.congress,
        parsed_bill.latest_action_date,
    )
    .await
    .context("ClearBillLatestAction failed")?;

    db::delete_bill_actions(&mut tx, &bill.billtype, bill.billnumber, bill.congress)
        .await
        .context("DeleteBillActions failed")?;

    // Insert each action and track which one is the "latest".
    let mut latest_action_id = None;
    for action in &parsed_bill.actions {
        let action_id = db::insert_bill_action(&mut tx, action)
            .await
            .context("InsertBillAction failed")?;
        // Match the latest action by date and text.
        if parsed_bill.latest_action_date == Some(action.acted_at)
            && (latest_action_id.is_none()
                || action.action_text.as_deref() == Some(parsed_bill.latest_action_text.as_str()))
        {
            latest_action_id = Some(action_id);
        }
    }

    // Update the bill's latest_action_id to point to the correct action row.
    if latest_action_id.is_some() {
        db::update_bill_latest_action(
            &mut tx,
            &bill.billtype,
            bill.billnumber,
            bill.congress,
            latest_action_id,
            parsed_bill.latest_action_date,
        )
        .await
        .context("UpdateBillLatestAction failed")?;
    }

    // Step 3: Re-insert cosponsors.
    db::delete_bill_cosponsors(&mut tx, &bill.billtype, bill.billnumber, bill.congress)
        .await
        .context("DeleteBillCosponsors failed")?;
    for cosponsor in &parsed_bill.cosponsors {
        db::insert_bill_cosponsor(&mut tx, cosponsor)
            .await
            .context("InsertBillCosponsor failed")?;
    }

    // Step 4: Upsert committees + re-insert associations.
    for committee in &parsed_bill.committees {
        db::insert_committee(
            &mut tx,
            &InsertCommitteeParams {
                committee_code: committee.committee_code.clone(),
                committee_name: option_string(committee.committee_name.clone()),
                chamber: option_string(committee.chamber.clone()),
            },
        )
        .await
        .context("InsertCommittee failed")?;
    }

    db::delete_bill_committees(&mut tx, &bill.billtype, bill.billnumber, bill.congress)
        .await
        .context("DeleteBillCommittees failed")?;
    for committee in &parsed_bill.committees {
        db::insert_bill_committee(
            &mut tx,
            &bill.billtype,
            bill.billnumber,
            bill.congress,
            &committee.committee_code,
        )
        .await
        .context("InsertBillCommittee failed")?;
    }

    // Step 5: Re-insert subjects.
    db::delete_bill_subjects(&mut tx, &bill.billtype, bill.billnumber, bill.congress)
        .await
        .context("DeleteBillSubjects failed")?;
    for subject in &parsed_bill.subjects {
        db::insert_bill_subject(&mut tx, subject)
            .await
            .context("InsertBillSubject failed")?;
    }

    // Commit the transaction — all changes become permanent.
    tx.commit().await?;
    Ok(())
}

// ============================================================================
// Helper functions
// ============================================================================

/// Finds the "Official Title" from the titles list.
///
/// `.iter()` creates an iterator over the slice.
/// `.find(|t| ...)` returns the first element matching the predicate.
/// `.map(|t| t.title.clone())` transforms the result if found.
/// `.unwrap_or_default()` returns an empty string if no match.
///
/// This chain is like:
///   next((t.title for t in titles if t.title_type.startswith("Official Title")), "")
/// in Python.
fn official_title(titles: &TitlesXml) -> String {
    titles
        .items
        .iter()
        .find(|title| title.title_type.starts_with("Official Title"))
        .map(|title| title.title.clone())
        .unwrap_or_default()
}

/// Finds the most recent action from JSON bill data by comparing date strings.
/// Returns (date_string, text_string).
fn latest_json_action(actions: &[BillJsonAction]) -> (String, String) {
    let mut latest_date = String::new();
    let mut latest_text = String::new();

    for action in actions {
        if action.acted_at.is_empty() {
            continue;
        }
        // String comparison works for ISO dates (YYYY-MM-DD sorts correctly).
        if latest_date.is_empty() || action.acted_at > latest_date {
            latest_date = action.acted_at.clone();
            latest_text = action.text.clone();
        }
    }

    (latest_date, latest_text)
}

/// Same as `latest_json_action` but for XML action structs.
fn latest_xml_action(actions: &ActionsXml) -> (String, String) {
    let mut latest_date = String::new();
    let mut latest_text = String::new();

    for action in &actions.actions {
        if action.acted_at.is_empty() {
            continue;
        }
        if latest_date.is_empty() || action.acted_at > latest_date {
            latest_date = action.acted_at.clone();
            latest_text = action.text.clone();
        }
    }

    (latest_date, latest_text)
}

/// Derives a bill status from the latest action text when no explicit
/// status is provided. Uses keyword matching (contains checks).
fn derive_bill_status(latest_action_text: &str) -> String {
    let text = latest_action_text.to_ascii_lowercase();
    if text.is_empty() {
        "introduced".to_string()
    } else if text.contains("enact") {
        "enacted".to_string()
    } else if text.contains("veto") {
        "vetoed".to_string()
    } else if text.contains("pass") {
        "passed".to_string()
    } else if text.contains("report") {
        "reported".to_string()
    } else if text.contains("refer") {
        "referred".to_string()
    } else if text.contains("introduc") {
        "introduced".to_string()
    } else {
        "active".to_string()
    }
}

/// Normalizes a raw status string to one of our standard status values.
/// Falls back to `derive_bill_status` if the raw status doesn't match
/// any known pattern.
fn normalize_bill_status(raw_status: &str, latest_action_text: &str) -> String {
    let status = raw_status.trim().to_ascii_lowercase();
    if status.is_empty() {
        derive_bill_status(latest_action_text)
    } else if status.contains("enact") {
        "enacted".to_string()
    } else if status.contains("veto") {
        "vetoed".to_string()
    } else if status.contains("pass") {
        "passed".to_string()
    } else if status.contains("report") {
        "reported".to_string()
    } else if status.contains("refer") {
        "referred".to_string()
    } else if status.contains("introduc") {
        "introduced".to_string()
    } else if status.contains("active") {
        "active".to_string()
    } else {
        derive_bill_status(latest_action_text)
    }
}

/// Reads bill status from a companion data.json file (sidecar).
///
/// Some bills have both XML (for detailed data) and JSON (for status).
/// This reads the JSON sidecar to get the status field.
///
/// `let Ok(data) = ... else { return ... }` — let-else pattern for
/// early return on error. If `fs::read_to_string` fails, we return
/// an empty string instead of propagating the error.
fn bill_status_from_sidecar(path: &Path) -> String {
    let sidecar_path = path.parent().unwrap_or(path).join("data.json");
    if !file_exists(&sidecar_path) {
        return String::new();
    }

    let Ok(data) = fs::read_to_string(sidecar_path) else {
        return String::new();
    };
    let Ok(bill_json) = serde_json::from_str::<BillJson>(&data) else {
        return String::new();
    };

    bill_json.status
}

// ============================================================================
// Tests
// ============================================================================
#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parse_bill_json_builds_normalized_bill() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("data.json");
        let payload = r#"{
          "number": "42",
          "bill_type": "hr",
          "introduced_at": "2024-01-10",
          "congress": "118",
          "status": "",
          "summary": {
            "date": "2024-01-11",
            "text": "Summary text"
          },
          "actions": [
            {
              "acted_at": "2024-01-12",
              "text": "Passed House",
              "type": "vote"
            }
          ],
          "sponsor": {
            "title": "Rep.",
            "name": "Example",
            "state": "IL",
            "party": "D"
          },
          "cosponsors": [],
          "status_at": "2024-01-12",
          "short_title": "Example Act",
          "official_title": "Example Act Official"
        }"#;

        fs::write(&path, payload).unwrap();

        let parsed = parse_bill_json(&path).unwrap();
        assert_eq!(parsed.bill.billnumber, 42);
        assert_eq!(parsed.bill.billtype, "hr");
        assert_eq!(parsed.bill.congress, 118);
        // Status "" + latest action "Passed House" -> "passed"
        assert_eq!(parsed.bill.bill_status, "passed");
        assert_eq!(parsed.actions.len(), 1);
        assert_eq!(parsed.bill.shorttitle.as_deref(), Some("Example Act"));
    }
}
