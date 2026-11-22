//! Response activity on the project picker. Never query storage from layout or hover.
use super::*;
use iced::widget::column;
use orbit_core::store::activity::{DAYS, Summary};

pub(super) const WIDTH: f32 = 280.;

pub(super) fn summaries(data: &Updates) -> iced::futures::stream::BoxStream<'static, Message> {
    let store = data.0.store.clone();
    let mut changes = store.subscribe_activity();
    Box::pin(iced::stream::channel(1, async move |mut output| {
        loop {
            changes.borrow_and_update();
            let reader = store.clone();
            let result = tokio::task::spawn_blocking(move || reader.activity_summary())
                .await
                .map_err(orbit_core::Error::from)
                .and_then(|s| s)
                .map(Arc::new)
                .map_err(|e| format!("Could not read activity: {e:#}. Will retry automatically."));
            if output.send(Message::UsageSummary(result)).await.is_err() {
                break;
            }
            // Re-bucket and expire old activity even if there are no new responses.
            tokio::select! {
                result = changes.changed() => if result.is_err() { break },
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            }
        }
    }))
}

fn label(value: impl Into<String>) -> widget::Text<'static> {
    text(value.into()).size(12)
}

fn exact_count(count: u64) -> String {
    let mut digits = count.to_string();
    let mut split = digits.len();
    while split > 3 {
        split -= 3;
        digits.insert(split, ',');
    }
    digits
}

fn pair(left: impl Into<String>, right: impl Into<String>) -> widget::Row<'static, Message> {
    row![label(left).style(theme::muted).width(Fill), label(right)].spacing(8)
}

fn tip<'a>(
    target: impl Into<Element<'a, Message>>,
    heading: String,
    lines: widget::Column<'a, Message>,
) -> Element<'a, Message> {
    tooltip(
        target,
        column![label(heading), lines.spacing(4)]
            .spacing(10)
            .width(200),
        tooltip::Position::Top,
    )
    .gap(5)
    .padding(10)
    .style(footer_surface)
    .into()
}

fn bar(width: f32, height: f32) -> widget::Container<'static, Message> {
    container(space())
        .width(width)
        .height(height)
        .style(|theme: &Theme| container::background(theme::colors(theme).accent))
}

pub(super) fn view(data: Option<&Result<Arc<Summary>, String>>) -> Element<'_, Message> {
    let heading = column![
        theme::primary_label("ACTIVITY")
            .size(12)
            .style(theme::muted),
        label("All projects · Includes subagents").style(theme::muted)
    ]
    .spacing(6);
    let mut panel = column![heading].width(WIDTH).spacing(20);
    let data = match data {
        None => {
            return panel
                .push(label("Loading activity…").style(theme::muted))
                .into();
        }
        Some(Err(e)) => return panel.push(text(e).size(12).style(theme::muted)).into(),
        Some(Ok(data)) => data,
    };
    if data.days.iter().all(|day| day.models.is_empty()) {
        return panel
            .push(label("Activity will appear after your next response.").style(theme::muted))
            .into();
    }

    let mut tokens = column![theme::primary_label("Tokens").size(12)].spacing(8);
    for ((name, period), count) in [
        ("1d", "Last 24 hours"),
        ("7d", "Last 7 days"),
        ("30d", "Last 30 days"),
    ]
    .into_iter()
    .zip(&data.tokens)
    {
        let cache = if count.input == 0 {
            "—".into()
        } else {
            format!("{:.1}%", 100. * count.cached as f64 / count.input as f64)
        };
        tokens = tokens.push(tip(
            pair(name, compact_tokens(count.total())),
            period.into(),
            column![
                pair("Input", exact_count(count.input)),
                pair("Output", exact_count(count.output)),
                pair("Cache hit rate", cache)
            ],
        ));
    }
    let peak = data
        .days
        .iter()
        .map(|day| day.total)
        .max()
        .expect("30 buckets")
        .max(1);
    let mut bars = row![];
    const HEIGHT: f32 = 48.;
    let slot = WIDTH / DAYS as f32;
    for (i, day) in data.days.iter().enumerate() {
        let mark = if day.total == 0 {
            container(space())
                .width(slot - 3.)
                .height(1.)
                .style(|theme: &Theme| container::background(theme::colors(theme).border))
        } else {
            bar(slot - 3., (HEIGHT * day.total as f32 / peak as f32).max(2.))
        };
        let age = DAYS - 1 - i;
        let mut lines = column![pair("Tokens", exact_count(day.total))];
        for (model, total) in &day.models {
            lines = lines.push(pair(model, exact_count(*total)));
        }
        bars = bars.push(tip(
            container(mark).center_x(slot).align_bottom(HEIGHT),
            if age == 0 {
                "Last 24 hours".into()
            } else {
                format!("{age}–{} days ago", age + 1)
            },
            lines,
        ));
    }
    panel = panel
        .push(tokens)
        .push(column![bars, pair("Daily usage", "Last 30 days")].spacing(4));

    let mut generation = column![row![
        theme::primary_label("Generation").size(12).width(Fill),
        label("7d median").style(theme::muted)
    ]]
    .spacing(8);
    let fastest = data
        .generation
        .iter()
        .filter_map(|g| g.tokens_per_second)
        .fold(0., f64::max);
    let mut previous = None;
    for g in &data.generation {
        if previous != Some(&g.model) {
            generation = generation.push(label(&g.model).style(theme::muted));
            previous = Some(&g.model);
        }
        let speed = g.tokens_per_second.map(|v| format!("{v:.0} tok/s"));
        let latency = g.latency_seconds.map(|v| format!("{v:.1} s"));
        let tier = if g.fast { "Fast" } else { "Standard" };
        let mut tier_label = row![label(tier)].spacing(4).align_y(iced::Center).width(74);
        if g.fast {
            tier_label = tier_label.push(Icon::Zap.view(11.));
        }
        let mut track = row![tier_label].align_y(iced::Center);
        if let Some(rate) = g.tokens_per_second {
            track = track.push(bar((110. * rate / fastest) as f32, 6.));
        }
        track = track
            .push(space().width(Fill))
            .push(label(speed.as_deref().unwrap_or("—")));
        let mut lines = column![label(format!("OpenAI · {tier}")).style(theme::muted)];
        if let Some(speed) = speed {
            lines = lines.push(pair("Generation speed", speed));
        }
        if let Some(latency) = latency {
            lines = lines.push(pair("Initial response latency", latency));
        }
        generation = generation.push(tip(track, g.model.clone(), lines));
    }
    if data.generation.is_empty() {
        generation = generation.push(label("No responses in the last 7 days.").style(theme::muted));
    }
    panel.push(generation).into()
}
