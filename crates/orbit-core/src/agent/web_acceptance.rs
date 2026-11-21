//! Explicit, paid live acceptance. Never run by the ordinary test/verify suite.
use super::*;

#[test]
#[ignore = "uses ChatGPT subscription; requires explicit ORBIT_WEB_ACCEPTANCE_AUTH"]
fn live_web_code_mode() -> Result<()> {
    let path = std::env::var_os("ORBIT_WEB_ACCEPTANCE_AUTH").context(
        "Set ORBIT_WEB_ACCEPTANCE_AUTH to the Orbit auth.json after authorizing live usage",
    )?;
    let mut credentials: crate::auth::Credentials = serde_json::from_slice(&std::fs::read(path)?)?;
    if credentials.expires_at < crate::store::now() + 600 {
        bail!(
            "Refresh the saved Orbit login first; this acceptance check will not rotate original credentials"
        );
    }
    // The isolated store gets only the short-lived access token, never the original refresh token.
    credentials.refresh_token.clear();
    let root = std::env::temp_dir().join(format!("orbit-web-acceptance-{}", crate::store::id()));
    let store = Store::open(root.clone())?;
    store.write("auth.json", &credentials)?;
    let session = Session::new(root.clone(), &Settings::default());
    let id = session.id.clone();
    let harness = Harness::new(store.clone(), vec![])?;
    let conversation = harness.insert(session);
    let execute = |label: &str, code: &str| -> Result<String> {
        let mut output = conversation
            .code
            .exec(code, label, &CancellationToken::new())?;
        if let Some(cell_id) = output
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("Script running with cell ID "))
        {
            output = conversation.code.wait(
                &json!({"cell_id":cell_id,"yield_time_ms":150000}).to_string(),
                &CancellationToken::new(),
            )?;
        }
        std::fs::write(root.join(format!("{label}.txt")), &output)?;
        if !output.starts_with("Script completed")
            || [
                "Error parsing function call",
                "Internal Error",
                "Failed to fetch",
                "Unable to resolve",
                "invalid ref_id",
            ]
            .iter()
            .any(|error| output.contains(error))
        {
            bail!(
                "{label} did not complete successfully; inspect {}",
                root.display()
            );
        }
        println!(
            "LIVE_WEB_OK {label}: {} output bytes (full transcript retained)",
            output.len()
        );
        Ok(output)
    };
    execute(
        "search",
        r#"
const result = await tools.web__run({search_query:[{q:'Rust book official documentation',domains:['doc.rust-lang.org'],recency:365}],response_length:'short'});
const ref = result.match(/turn\d+search\d+/); if (!ref) throw new Error('No search reference returned');
store('webSearchRef',ref[0]); text(result);
"#,
    )?;
    execute(
        "open",
        r#"
const result = await tools.web__run({open:[{ref_id:load('webSearchRef')},{ref_id:'https://www.w3.org/WAI/ER/tests/xhtml/testfiles/resources/pdf/dummy.pdf'}],response_length:'long'});
const refs = [...new Set(result.match(/turn\d+view\d+/g))]; if (refs.length < 2) throw new Error('Missing page/PDF references');
store('webPageRef',refs[0]); store('webPdfRef',refs[1]);
const link = result.match(/【(\d+)†/); if (!link) throw new Error('No numbered link'); store('webLink',Number(link[1])); text(result);
"#,
    )?;
    execute(
        "navigation",
        r#"
const result = await tools.web__run({click:[{ref_id:load('webPageRef'),id:load('webLink')}],find:[{ref_id:load('webPageRef'),pattern:'Rust'}],response_length:'short'});
if (!result.includes('Source: click(') || !result.includes('Source: find(') || !/turn\d+view\d+/.test(result) || !result.includes('Total lines:')) throw new Error('Missing navigation result'); text(result);
"#,
    )?;
    execute(
        "screenshot",
        r#"
const result = await tools.web__run({screenshot:[{ref_id:load('webPdfRef'),pageno:0}],response_length:'short'});
if (!/citeturn\d+view\d+/.test(result) || !result.includes('https://www.w3.org/WAI/ER/tests/xhtml/testfiles/resources/pdf/dummy.pdf')) throw new Error('Missing screenshot reference'); text(result);
"#,
    )?;
    execute(
        "images",
        r#"
const result = await tools.web__run({image_query:[{q:'Iceland waterfalls'}],response_length:'short'});
if (!/turn\d+image\d+/.test(result) || !/Image URL: https?:\/\//.test(result)) throw new Error('Missing image result URL'); text(result);
"#,
    )?;
    let specialized = execute(
        "specialized",
        r#"
text(await tools.web__run({finance:[{ticker:'MSFT',type:'equity',market:'USA'}],weather:[{location:'Finland, Uusimaa, Helsinki',duration:1}],sports:[{fn:'standings',league:'nba'}],time:[{utc_offset:'+00:00'}],response_length:'long'}));
"#,
    )?;
    for marker in [
        "finance",
        "Microsoft",
        "Weather for",
        "Forecast:",
        "# Conference:",
        "The time in UTC+00:00 is",
    ] {
        if !specialized.contains(marker) {
            bail!(
                "Specialized operation {marker} did not return its expected result marker; inspect {}",
                root.display()
            );
        }
    }
    let persisted = store.load_session(&id)?;
    if !persisted.entries.iter().any(|entry| matches!(&entry.kind, EntryKind::Tool {web:Some(metadata),..} if metadata.mode==crate::web_search::Mode::Live)) {
        bail!("Live metadata was not persisted");
    }
    println!(
        "LIVE_WEB_ACCEPTANCE_COMPLETE session={id} artifacts={}",
        root.display()
    );
    Ok(())
}
