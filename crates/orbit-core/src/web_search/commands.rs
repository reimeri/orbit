//! Codex standalone web command contract (Apache-2.0 reference, revision
//! 65a5410f72dac1f4b40862c34ef0a8b71b12834a, codex-api/src/search.rs).
//! Adapted for Orbit: ten operations, strict input fields, boundary validation and
//! sports discriminator normalization. See docs/licenses/codex-{Apache-2.0,NOTICE}.txt.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SearchCommands {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_query: Option<Vec<SearchQuery>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_query: Option<Vec<SearchQuery>>,
    /// Open pages by reference id or URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open: Option<Vec<OpenOperation>>,
    /// Open links from previously opened pages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub click: Option<Vec<ClickOperation>>,
    /// Find text patterns in pages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub find: Option<Vec<FindOperation>>,
    /// Take screenshots of PDF pages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<Vec<ScreenshotOperation>>,
    /// Look up prices for the given stock symbols.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finance: Option<Vec<FinanceOperation>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weather: Option<Vec<WeatherOperation>>,
    /// Look up sports schedules and standings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sports: Option<Vec<SportsOperation>>,
    /// Get time for the given UTC offsets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<Vec<TimeOperation>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_length: Option<SearchResponseLength>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SearchQuery {
    pub q: String,
    /// Whether to filter by recency, as a number of recent days.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recency: Option<u64>,
    /// Whether to filter by a specific list of domains.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domains: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OpenOperation {
    /// Reference id or URL to open.
    pub ref_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lineno: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClickOperation {
    /// Reference id containing the numbered link.
    pub ref_id: String,
    /// Numbered link id to open.
    pub id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FindOperation {
    /// Reference id or URL to search within.
    pub ref_id: String,
    pub pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotOperation {
    /// Reference id or URL to screenshot.
    pub ref_id: String,
    /// Zero-indexed PDF page number.
    pub pageno: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FinanceOperation {
    pub ticker: String,
    pub r#type: FinanceAssetType,
    /// ISO 3166-1 alpha-3 country code, "OTC", or "" for cryptocurrency.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub market: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FinanceAssetType {
    Equity,
    Fund,
    Crypto,
    Index,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WeatherOperation {
    /// Location in "Country, Area, City" format.
    pub location: String,
    /// Start date in YYYY-MM-DD format. Defaults to today.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    /// Number of days to return. Defaults to 7.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SportsOperation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<SportsToolName>,
    pub r#fn: SportsFunction,
    pub league: SportsLeague,
    /// Team to look up, using the common 3 or 4 letter alias used in broadcasts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// Opponent to use with `team` when narrowing the lookup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opponent: Option<String>,
    /// Start date in YYYY-MM-DD format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_from: Option<String>,
    /// End date in YYYY-MM-DD format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_games: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SportsToolName {
    Sports,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SportsFunction {
    Schedule,
    Standings,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SportsLeague {
    Nba,
    Wnba,
    Nfl,
    Nhl,
    Mlb,
    Epl,
    Ncaamb,
    Ncaawb,
    Ipl,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TimeOperation {
    /// UTC offset formatted like "+03:00".
    pub utc_offset: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchResponseLength {
    Short,
    Medium,
    Long,
}

impl SearchCommands {
    pub fn parse(arguments: &str) -> anyhow::Result<Self> {
        use anyhow::{Context, bail};
        let mut commands: Self = serde_json::from_str(arguments)
            .context("Invalid web__run arguments; use the documented operation arrays")?;
        if commands.search_query.as_ref().is_some_and(|q| q.len() > 4) {
            bail!("search_query accepts at most four queries per call");
        }
        let mut has_operations = commands.sports.as_ref().is_some_and(|ops| !ops.is_empty());
        let mut required = |field: &str, value: &str| -> anyhow::Result<()> {
            has_operations = true;
            anyhow::ensure!(!value.trim().is_empty(), "{field} must not be blank");
            Ok(())
        };
        for (name, queries) in [
            ("search_query.q", &commands.search_query),
            ("image_query.q", &commands.image_query),
        ] {
            for query in queries.iter().flatten() {
                required(name, &query.q)?;
                for domain in query.domains.iter().flatten() {
                    if domain.is_empty()
                        || domain.chars().any(char::is_whitespace)
                        || domain.contains(['/', ':'])
                    {
                        bail!("domains must contain host names such as example.com, not URLs");
                    }
                }
            }
        }
        for op in commands.open.iter().flatten() {
            required("open.ref_id", &op.ref_id)?;
        }
        for op in commands.click.iter().flatten() {
            required("click.ref_id", &op.ref_id)?;
        }
        for op in commands.find.iter().flatten() {
            required("find.ref_id", &op.ref_id)?;
            required("find.pattern", &op.pattern)?;
        }
        for op in commands.screenshot.iter().flatten() {
            required("screenshot.ref_id", &op.ref_id)?;
        }
        for op in commands.finance.iter().flatten() {
            required("finance.ticker", &op.ticker)?;
        }
        for op in commands.time.iter().flatten() {
            required("time.utc_offset", &op.utc_offset)?;
        }
        for op in commands.weather.iter().flatten() {
            required("weather.location", &op.location)?;
        }
        if !has_operations {
            bail!("Provide at least one web operation; no request was sent");
        }
        if commands
            .search_query
            .as_ref()
            .is_some_and(|queries| queries.len() == 4)
            && matches!(
                commands.response_length,
                None | Some(SearchResponseLength::Short)
            )
        {
            bail!("Four search queries require response_length medium or long");
        }
        for operation in commands.sports.iter_mut().flatten() {
            operation.tool.get_or_insert(SportsToolName::Sports);
        }
        Ok(commands)
    }
}
