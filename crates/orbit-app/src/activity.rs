//! A display-only projection. Source entries, parent links and full text stay intact.
use orbit_core::{
    history::History,
    shell::CommandStatus,
    store::{Entry, EntryKind},
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ops::Range,
};

// Synthetic rows occupy separate identity domains; ordinary rows retain their
// source index, including selection positions and measured-height identities.
const SHIFT: u32 = usize::BITS - 2;
const INDEX: usize = (1 << SHIFT) - 1;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Entry(usize),
    Activity(usize),
    Execution(usize),
    Thinking(usize),
}
impl Row {
    pub fn id(self) -> usize {
        match self {
            Self::Entry(i) => i,
            Self::Activity(i) => i | (1 << SHIFT),
            Self::Execution(i) => i | (2 << SHIFT),
            Self::Thinking(i) => i | (3 << SHIFT),
        }
    }
    pub fn from_id(id: usize) -> Self {
        let i = id & INDEX;
        match id >> SHIFT {
            0 => Self::Entry(i),
            1 => Self::Activity(i),
            2 => Self::Execution(i),
            3 => Self::Thinking(i),
            _ => unreachable!(),
        }
    }
    pub fn source(self) -> usize {
        self.id() & INDEX
    }
    pub fn spacing(self, entries: &History<Entry>) -> f32 {
        match self {
            Self::Entry(i) if !is_activity(&entries[i]) => 8.,
            Self::Activity(_) => 6.,
            _ => 2.,
        }
    }
    pub fn estimate(self, entries: &History<Entry>) -> f32 {
        match self {
            Self::Entry(i) if !is_activity(&entries[i]) => 120.,
            _ => 30.,
        }
    }
}

#[derive(Default, Debug)]
pub struct Summary {
    pub tools: usize,
    pub completed: usize,
    pub running: usize,
    pub failed: usize,
    pub reviews: usize,
    pub explores: usize,
    pub current: String,
    pub thinking: String,
}
impl Summary {
    pub fn show_failures(&self) -> bool {
        self.tools > 10 && self.failed > self.tools / 2
    }

    pub fn label(&self, active: bool) -> String {
        let mut parts = Vec::new();
        if self.running > 0 || active {
            parts.push(if !self.current.is_empty() {
                self.current.clone()
            } else if !self.thinking.is_empty() {
                self.thinking.clone()
            } else {
                "Working".into()
            });
        } else if self.tools > 0 {
            parts.push(format!(
                "{} tool call{}",
                self.tools,
                if self.tools == 1 { "" } else { "s" }
            ));
        } else if !self.thinking.is_empty() {
            parts.push(self.thinking.clone());
        } else {
            parts.push("Activity".into());
        }
        if self.reviews > 0 {
            parts.push(format!("{} review", self.reviews));
        }
        if self.explores > 0 {
            parts.push(format!("{} explore", self.explores));
        }
        parts.join(" · ")
    }
}
pub struct Block {
    pub range: Range<usize>,
    pub summary: Option<Summary>,
    pub folded: HashSet<usize>,
    pub thinking: BTreeMap<usize, Range<usize>>,
}
#[derive(Default)]
pub struct Projection {
    source: History<Entry>,
    /// Chronological prompt ordinals map directly to stable source rows.
    pub prompts: Vec<usize>,
    pub blocks: BTreeMap<usize, Block>,
    children: HashMap<String, usize>,
    cells: HashMap<String, String>,
}
/// Command state wins over invocation completion: a yielded tool can still be running.
pub fn tool_status(complete: bool, failed: bool, command: Option<CommandStatus>) -> CommandStatus {
    command.unwrap_or(if failed {
        CommandStatus::Failed
    } else if complete {
        CommandStatus::Succeeded
    } else {
        CommandStatus::Running
    })
}
pub fn is_activity(entry: &Entry) -> bool {
    matches!(entry.kind, EntryKind::Tool { .. } | EntryKind::Reasoning)
        || subagent_notice(entry).is_some()
}
pub fn subagent_notice(entry: &Entry) -> Option<&str> {
    matches!(entry.kind, EntryKind::Notice)
        .then(|| entry.id.strip_prefix("subagent-"))
        .flatten()
}
fn wrapper(entry: &Entry) -> bool {
    matches!(&entry.kind, EntryKind::Tool { name, parent: None, .. } if matches!(name.as_str(), "exec" | "wait"))
}
impl Projection {
    /// Re-project only the changed tail, including an earlier parent if its first
    /// nested call arrives after a yield.
    pub fn sync(&mut self, entries: &History<Entry>) -> Option<usize> {
        let mut first = None;
        for (i, next) in self.source.changes(entries) {
            if i < self.source.len()
                && let EntryKind::Tool {
                    name,
                    cell_id: Some(cell),
                    ..
                } = &self.source[i].kind
                && name == "exec"
            {
                self.cells.remove(cell);
            }
            if let Some(Entry {
                id,
                kind:
                    EntryKind::Tool {
                        name,
                        cell_id: Some(cell),
                        ..
                    },
                ..
            }) = next
                && name == "exec"
            {
                self.cells.insert(cell.clone(), id.clone());
            }
            first = Some(first.map_or(i, |start: usize| start.min(i)));
            let parent = |e: &Entry| match &e.kind {
                EntryKind::Tool { parent, .. } => parent.clone(),
                _ => None,
            };
            let old_parent = (i < self.source.len())
                .then(|| parent(&self.source[i]))
                .flatten();
            let new_parent = next.and_then(parent);
            if old_parent != new_parent {
                for (p, add) in [(old_parent, false), (new_parent, true)] {
                    if let Some(p) = p {
                        let count = self.children.entry(p.clone()).or_default();
                        if add {
                            *count += 1;
                        } else {
                            *count -= 1;
                        }
                        if let Some(index) = entries.index_of(&p) {
                            first = Some(first.unwrap().min(index));
                        }
                    }
                }
            }
        }
        let first = first?;
        let start = self
            .blocks
            .range(..=first)
            .next_back()
            .map_or(first, |(&i, b)| {
                if first < b.range.end || b.summary.is_some() && first == b.range.end {
                    i
                } else {
                    first
                }
            });
        self.blocks.split_off(&start);
        self.prompts
            .truncate(self.prompts.partition_point(|&i| i < start));
        let mut index = start;
        while index < entries.len() {
            let begin = index;
            let entry = &entries[index];
            if matches!(entry.kind, EntryKind::User { .. }) {
                self.prompts.push(index);
            }
            let activity = is_activity(entry);
            index += 1;
            if activity {
                while index < entries.len() && is_activity(&entries[index]) {
                    index += 1;
                }
            }
            let mut block = Block {
                range: begin..index,
                summary: activity.then(Summary::default),
                folded: HashSet::new(),
                thinking: BTreeMap::new(),
            };
            if let Some(summary) = &mut block.summary {
                let mut i = begin;
                while i < index {
                    let entry = &entries[i];
                    match &entry.kind {
                        EntryKind::Reasoning => {
                            let head = i;
                            while i < index && matches!(entries[i].kind, EntryKind::Reasoning) {
                                let preview = reasoning_preview(&entries[i].text);
                                if !preview.is_empty() {
                                    summary.thinking = preview;
                                }
                                i += 1;
                            }
                            block.thinking.insert(head, head..i);
                            continue;
                        }
                        EntryKind::Tool {
                            name,
                            input,
                            complete,
                            failed,
                            command_status,
                            ..
                        } => {
                            let status = tool_status(*complete, *failed, *command_status);
                            let nested = self.wrapper_has_children(entry);
                            if nested {
                                if !*failed && status == CommandStatus::Succeeded {
                                    block.folded.insert(i);
                                }
                            } else {
                                summary.tools += 1;
                                if status != CommandStatus::Running {
                                    summary.completed += 1;
                                } else {
                                    summary.running += 1;
                                }
                                if matches!(
                                    status,
                                    CommandStatus::Failed | CommandStatus::Interrupted
                                ) {
                                    summary.failed += 1;
                                }
                                if name == "spawn_subagent"
                                    && let Ok(input) =
                                        serde_json::from_str::<serde_json::Value>(input)
                                {
                                    match input["type"].as_str() {
                                        Some("review") => summary.reviews += 1,
                                        Some("explore") => summary.explores += 1,
                                        _ => {}
                                    }
                                }
                            }
                            if status == CommandStatus::Running
                                && (!nested || summary.current.is_empty())
                            {
                                summary.current = if wrapper(entry) {
                                    tool_name(name).into()
                                } else {
                                    if name == "exec_command" {
                                        tool_preview(name, input)
                                    } else {
                                        format!(
                                            "{} · {}",
                                            tool_name(name),
                                            tool_preview(name, input)
                                        )
                                    }
                                };
                            }
                        }
                        EntryKind::Notice if summary.thinking.is_empty() => {
                            summary.thinking = "Subagent update".into();
                        }
                        _ => {}
                    }
                    i += 1;
                }
            }
            self.blocks.insert(begin, block);
        }
        self.source = entries.clone();
        Some(start)
    }
    fn wrapper_has_children(&self, entry: &Entry) -> bool {
        if !wrapper(entry) {
            return false;
        }
        let EntryKind::Tool { name, input, .. } = &entry.kind else {
            unreachable!()
        };
        let execution = if name == "wait" {
            let Ok(input) = serde_json::from_str::<serde_json::Value>(input) else {
                return false;
            };
            let Some(parent) = input["cell_id"]
                .as_str()
                .and_then(|cell| self.cells.get(cell))
            else {
                return false;
            };
            parent
        } else {
            &entry.id
        };
        self.children.get(execution).copied().unwrap_or(0) > 0
    }
    pub fn block(&self, source: usize) -> &Block {
        self.blocks
            .range(..=source)
            .next_back()
            .expect("projected source")
            .1
    }
    pub fn rows(
        &self,
        start: usize,
        groups: &HashSet<String>,
        execution: &HashSet<String>,
        expanded: &HashSet<String>,
    ) -> Vec<(usize, f32)> {
        let mut rows = Vec::new();
        let mut push = |r: Row| rows.push((r.id(), r.estimate(&self.source)));
        for (&head, block) in self.blocks.range(start..) {
            if block.summary.is_none() {
                push(Row::Entry(head));
                continue;
            }
            push(Row::Activity(head));
            let id = &self.source[head].id;
            if !groups.contains(id) {
                continue;
            }
            if !block.folded.is_empty() {
                push(Row::Execution(head));
            }
            let mut i = head;
            while i < block.range.end {
                let entry = &self.source[i];
                if let Some(range) = block.thinking.get(&i) {
                    push(Row::Thinking(i));
                    if expanded.contains(&entry.id) {
                        for index in range.clone() {
                            push(Row::Entry(index));
                        }
                    }
                    i = range.end;
                    continue;
                }
                if !block.folded.contains(&i) || execution.contains(id) {
                    push(Row::Entry(i));
                }
                i += 1;
            }
        }
        rows
    }
}

pub fn tool_name(name: &str) -> &str {
    match name {
        "exec" => "Code execution",
        "wait" => "Wait for code",
        _ => name,
    }
}
pub fn reasoning_preview(text: &str) -> String {
    // A provider can stream multiple summary sections into one reasoning item.
    // Prefer its latest complete heading, not a generated paraphrase.
    let mut heading = None;
    let mut fence = None;
    for line in text.lines().map(str::trim) {
        if line.starts_with("```") || line.starts_with("~~~") {
            let marker = &line[..3];
            if fence == Some(marker) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(marker);
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        let atx = line.trim_start_matches('#');
        if atx.len() < line.len() && atx.starts_with(char::is_whitespace) && !atx.trim().is_empty()
        {
            heading = Some(atx.trim().trim_end_matches('#').trim());
        } else if let Some(title) = line.strip_prefix("**").and_then(|s| s.strip_suffix("**"))
            && !title.trim().is_empty()
        {
            heading = Some(title.trim());
        }
    }
    heading
        .unwrap_or_else(|| {
            text.lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("")
                .trim()
        })
        .chars()
        .take(240)
        .collect()
}
pub fn tool_preview(name: &str, input: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(input) else {
        return input
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .chars()
            .take(500)
            .collect();
    };
    let string = |key: &str| value[key].as_str().unwrap_or("");
    let preview = match name {
        "exec_command" => string("cmd").to_owned(),
        "write_stdin" => {
            let session = &value["session_id"];
            if string("chars").is_empty() {
                format!("session {session} · poll output")
            } else {
                format!("session {session} · input {:?}", string("chars"))
            }
        }
        "spawn_subagent" => format!(
            "{} · {}",
            string("type"),
            value["title"].as_str().unwrap_or_else(|| string("task"))
        ),
        "wait_subagent" => format!(
            "{} assignment(s)",
            value["run_ids"].as_array().map_or(0, Vec::len)
        ),
        "view_image" => string("path").to_owned(),
        "image_gen__imagegen" => string("prompt").to_owned(),
        _ => value.to_string(),
    };
    preview.chars().take(500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(id: &str, kind: EntryKind, text: &str) -> Entry {
        Entry {
            id: id.into(),
            kind,
            text: text.into(),
        }
    }
    fn tool(id: &str, name: &str, parent: Option<&str>, complete: bool, failed: bool) -> Entry {
        entry(
            id,
            EntryKind::Tool {
                name: name.into(),
                input: r#"{"cmd":"cargo test -p core"}"#.into(),
                output: String::new(),
                complete,
                failed,
                command_status: None,
                cell_id: None,
                parent: parent.map(str::to_owned),
                media: vec![],
                web: None,
            },
            "",
        )
    }
    fn rows(p: &Projection, groups: &[&str], execution: &[&str], expanded: &[&str]) -> Vec<Row> {
        let set = |values: &[&str]| values.iter().map(|s| s.to_string()).collect();
        p.rows(0, &set(groups), &set(execution), &set(expanded))
            .into_iter()
            .map(|(id, _)| Row::from_id(id))
            .collect()
    }
    #[test]
    fn flattening_preserves_boundaries_failures_reasoning_and_execution_details() {
        let source: History<_> = vec![
            entry(
                "user",
                EntryKind::User {
                    skills: vec![],
                    files: vec![],
                    images: vec![],
                    sent_at: 1_750_000_123,
                    steering: false,
                },
                "Inspect it",
            ),
            entry(
                "r1",
                EntryKind::Reasoning,
                "**Inspecting configuration**\nFull first body",
            ),
            entry(
                "r2",
                EntryKind::Reasoning,
                "## Checking λ cancellation\nFull second body",
            ),
            tool("outer", "exec", None, true, false),
            tool("child", "exec_command", Some("outer"), true, false),
            tool("broken", "exec", None, true, true),
            tool("child2", "exec_command", Some("broken"), true, false),
            entry("subagent-run", EntryKind::Notice, "Full harness notice"),
            entry("notice", EntryKind::Notice, "Storage failed; retry saving"),
            tool("pure", "exec", None, true, false),
            entry("answer", EntryKind::Assistant, "Done"),
        ]
        .into();
        let mut p = Projection::default();
        p.sync(&source);
        assert_eq!(
            rows(&p, &[], &[], &[]),
            vec![
                Row::Entry(0),
                Row::Activity(1),
                Row::Entry(8),
                Row::Activity(9),
                Row::Entry(10)
            ]
        );
        assert_eq!(
            rows(&p, &["r1", "pure"], &[], &["r1"]),
            vec![
                Row::Entry(0),
                Row::Activity(1),
                Row::Execution(1),
                Row::Thinking(1),
                Row::Entry(1),
                Row::Entry(2),
                Row::Entry(4),
                Row::Entry(5),
                Row::Entry(6),
                Row::Entry(7),
                Row::Entry(8),
                Row::Activity(9),
                Row::Entry(9),
                Row::Entry(10)
            ]
        );
        let raw = rows(&p, &["r1"], &["r1"], &[]);
        assert!(raw.contains(&Row::Entry(3)));
        assert_eq!(
            raw.iter().filter(|r| **r == Row::Entry(5)).count(),
            1,
            "failed wrapper is not duplicated in execution details"
        );
        let summary = p.block(1).summary.as_ref().unwrap();
        assert_eq!(
            (summary.tools, summary.completed, summary.failed),
            (2, 2, 0)
        );
        assert!(!summary.show_failures());
        assert_eq!(summary.thinking, "Checking λ cancellation");
        assert_eq!(
            p.source[1].text,
            "**Inspecting configuration**\nFull first body"
        );
    }
    #[test]
    fn group_labels_count_subagents_by_explicit_profile() {
        let source: History<_> = ["explore", "review", "explore"]
            .into_iter()
            .enumerate()
            .map(|(i, profile)| {
                let mut entry = tool(&format!("spawn-{i}"), "spawn_subagent", None, true, false);
                if let EntryKind::Tool { input, .. } = &mut entry.kind {
                    *input = serde_json::json!({
                        "type": profile,
                        "title": "Inspect implementation",
                        "task": "Inspect the relevant code"
                    })
                    .to_string();
                }
                entry
            })
            .collect::<Vec<_>>()
            .into();
        let mut projection = Projection::default();
        projection.sync(&source);
        let summary = projection.block(0).summary.as_ref().unwrap();
        assert_eq!(summary.label(false), "3 tool calls · 1 review · 2 explore");
        assert_eq!(summary.label(true), "Working · 1 review · 2 explore");
    }

    #[test]
    fn failure_badge_requires_both_thresholds_and_completed_includes_failures() {
        for (total, failed, running, show_failures) in [
            (10, 10, 0, false),
            (12, 6, 0, false),
            (11, 5, 0, false),
            (11, 6, 0, true),
            (12, 7, 0, true),
            (12, 6, 2, false),
            (12, 7, 2, true),
        ] {
            let source: History<_> = (0..total)
                .map(|i| {
                    tool(
                        &format!("tool-{i}"),
                        "exec_command",
                        None,
                        i < total - running,
                        i < failed,
                    )
                })
                .collect::<Vec<_>>()
                .into();
            let mut projection = Projection::default();
            projection.sync(&source);
            let summary = projection.block(0).summary.as_ref().unwrap();
            assert_eq!(summary.tools, total);
            assert_eq!(summary.completed, total - running);
            assert_eq!(summary.running, running);
            assert_eq!(summary.failed, failed);
            assert_eq!(
                summary.show_failures(),
                show_failures,
                "{total} calls, {failed} failed, {running} running"
            );
        }
    }

    #[test]
    fn command_lifecycle_reprojects_completed_invocations() {
        let mut source: History<_> =
            vec![tool("command", "exec_command", Some("outer"), true, false)].into();
        let mut projection = Projection::default();
        for (status, counts) in [
            (CommandStatus::Running, (0, 1, 0)),
            (CommandStatus::Succeeded, (1, 0, 0)),
            (CommandStatus::Failed, (1, 0, 1)),
        ] {
            if let EntryKind::Tool { command_status, .. } = &mut source[0].kind {
                *command_status = Some(status);
            }
            assert_eq!(projection.sync(&source), Some(0));
            let summary = projection.block(0).summary.as_ref().unwrap();
            assert_eq!((summary.completed, summary.running, summary.failed), counts);
            if status == CommandStatus::Running {
                assert_eq!(summary.label(false), "cargo test -p core");
            }
        }
    }

    #[test]
    fn yielded_children_reproject_their_parent_without_changing_group_identity() {
        let mut source: History<_> = vec![
            tool("outer", "exec", None, false, false),
            entry("prose", EntryKind::Assistant, "I will check both"),
            tool("a", "exec_command", Some("outer"), false, false),
            tool("b", "exec_command", Some("outer"), false, false),
        ]
        .into();
        let mut p = Projection::default();
        p.sync(&source);
        assert_eq!(p.block(2).summary.as_ref().unwrap().running, 2);
        if let EntryKind::Tool {
            complete, failed, ..
        } = &mut source[2].kind
        {
            *complete = true;
            *failed = true;
        }
        assert_eq!(p.sync(&source), Some(2));
        let summary = p.block(2).summary.as_ref().unwrap();
        assert_eq!(
            (summary.completed, summary.running, summary.failed),
            (1, 1, 1)
        );
        assert_eq!(summary.label(true), "cargo test -p core");
        assert!(p.sync(&source).is_none());
        source.push(entry("reason", EntryKind::Reasoning, "**Next step**"));
        assert_eq!(p.sync(&source), Some(2));
        assert_eq!(
            rows(&p, &[], &[], &[]),
            vec![Row::Activity(0), Row::Entry(1), Row::Activity(2)]
        );
        assert_eq!(
            p.block(0).summary.as_ref().unwrap().tools,
            0,
            "children in another block still account for their wrapper"
        );
    }
    #[test]
    fn live_wrapper_stays_reachable_across_yield_and_recedes_only_on_success() {
        let mut source: History<_> = vec![
            tool("outer", "exec", None, false, false),
            tool("child", "exec_command", Some("outer"), true, false),
        ]
        .into();
        let mut p = Projection::default();
        for (yielded, status, visible) in [
            (false, CommandStatus::Running, true),
            (true, CommandStatus::Running, true),
            (true, CommandStatus::Interrupted, true),
            (true, CommandStatus::Succeeded, false),
        ] {
            let EntryKind::Tool {
                complete,
                command_status,
                ..
            } = &mut source[0].kind
            else {
                unreachable!()
            };
            *complete = yielded;
            *command_status = Some(status);
            p.sync(&source);
            let group = rows(&p, &["outer"], &[], &[]);
            assert_eq!(group.contains(&Row::Entry(0)), visible);
            assert_eq!(group.contains(&Row::Execution(0)), !visible);
            assert!(rows(&p, &["outer"], &["outer"], &[]).contains(&Row::Entry(0)));
        }
    }
    #[test]
    fn waits_follow_the_originating_execution_but_failures_and_pure_code_stay_visible() {
        let mut source: History<_> = vec![
            tool("outer", "exec", None, true, false),
            tool("child", "exec_command", Some("outer"), true, false),
            entry("prose", EntryKind::Assistant, "Waiting for more output"),
            tool("wait1", "wait", None, true, false),
            tool("wait2", "wait", None, true, true),
            tool("pure", "exec", None, true, false),
            tool("pure-wait", "wait", None, true, false),
        ]
        .into();
        for (i, cell) in [(0, "nested-cell"), (5, "pure-cell")] {
            if let EntryKind::Tool { cell_id, .. } = &mut source[i].kind {
                *cell_id = Some(cell.into());
            }
        }
        for (i, cell) in [(3, "nested-cell"), (4, "nested-cell"), (6, "pure-cell")] {
            if let EntryKind::Tool { input, .. } = &mut source[i].kind {
                *input = serde_json::json!({"cell_id":cell}).to_string();
            }
        }
        let mut p = Projection::default();
        p.sync(&source);
        let primary = rows(&p, &["outer", "wait1"], &[], &[]);
        assert!(
            !primary.contains(&Row::Entry(3)),
            "successful wait is only execution detail"
        );
        assert!(
            primary.contains(&Row::Entry(4)),
            "failed wait remains visible"
        );
        assert!(
            primary.contains(&Row::Entry(5)) && primary.contains(&Row::Entry(6)),
            "pure JS and its observations remain inspectable"
        );
        assert!(rows(&p, &["wait1"], &["wait1"], &[]).contains(&Row::Entry(3)));
        assert_eq!(p.block(3).summary.as_ref().unwrap().tools, 2);
        assert_eq!(p.block(3).summary.as_ref().unwrap().failed, 0);
    }

    #[test]
    fn streamed_summary_label_uses_latest_complete_heading() {
        let mut source: History<_> = vec![entry(
            "reasoning",
            EntryKind::Reasoning,
            "**Checking known UI cases**",
        )]
        .into();
        let mut projection = Projection::default();
        projection.sync(&source);
        assert_eq!(
            projection.block(0).summary.as_ref().unwrap().label(true),
            "Checking known UI cases"
        );

        source[0].text.push_str("\n**Testing Uni");
        projection.sync(&source);
        assert_eq!(
            projection.block(0).summary.as_ref().unwrap().label(true),
            "Checking known UI cases"
        );

        source[0].text.push_str("code shaping**");
        projection.sync(&source);
        assert_eq!(
            projection.block(0).summary.as_ref().unwrap().label(true),
            "Testing Unicode shaping"
        );
        assert_eq!(
            source[0].text,
            "**Checking known UI cases**\n**Testing Unicode shaping**"
        );
    }

    #[test]
    fn previews_use_inputs_not_javascript_wrappers_or_generated_paraphrases() {
        assert_eq!(
            tool_preview(
                "exec_command",
                r#"{"cmd":"printf 'λ'\ncargo test --locked","yield_time_ms":10}"#
            ),
            "printf 'λ'\ncargo test --locked"
        );
        assert_eq!(
            tool_preview(
                "spawn_subagent",
                r#"{"type":"review","title":"Check cancellation","task":"Long task"}"#
            ),
            "review · Check cancellation"
        );
        assert_eq!(
            tool_preview("write_stdin", r#"{"session_id":42,"chars":""}"#),
            "session 42 · poll output"
        );
        assert_eq!(tool_preview("exec_command", "{bad input"), "{bad input");
        assert_eq!(
            reasoning_preview("\n**Checking λ**\nThe complete original body."),
            "Checking λ"
        );
        let mut streamed = "**First heading**\nBody\n\n## Latest heading\nSecond body".to_owned();
        assert_eq!(reasoning_preview(&streamed), "Latest heading");
        streamed.push_str("\n\n**Appended heading**\n```sh\n# Not a title\n```\nBody");
        assert_eq!(reasoning_preview(&streamed), "Appended heading");
    }
}
