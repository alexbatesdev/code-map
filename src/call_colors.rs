use crate::{relationships::CallEdge, syntax::HighlightedLine};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const PLAIN: [f32; 3] = [0.79, 0.82, 0.86];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallMark {
    pub start: usize,
    pub end: usize,
    pub target: String,
}

#[derive(Default)]
pub struct CallHighlights {
    files: HashMap<String, BTreeMap<usize, Vec<CallMark>>>,
}

impl CallHighlights {
    pub fn new(edges: &[CallEdge], visible: &HashSet<String>) -> Self {
        let mut result = Self::default();
        for edge in edges {
            if edge.caller.file == edge.callee.file || edge.start_column >= edge.end_column
                || !visible.contains(&edge.caller.file) || !visible.contains(&edge.callee.file) { continue; }
            result.files.entry(edge.caller.file.clone()).or_default().entry(edge.line).or_default().push(CallMark {
                start: edge.start_column, end: edge.end_column, target: edge.callee.file.clone(),
            });
        }
        for lines in result.files.values_mut() {
            for marks in lines.values_mut() {
                marks.sort_by(|a, b| (a.start, a.end, &a.target).cmp(&(b.start, b.end, &b.target)));
                marks.dedup();
            }
        }
        result
    }

    pub fn line(&self, file: &str, line: usize) -> &[CallMark] {
        self.files.get(file).and_then(|lines| lines.get(&line)).map_or(&[], Vec::as_slice)
    }

    pub fn site_count(&self) -> usize {
        self.files.values().flat_map(|lines| lines.values()).map(Vec::len).sum()
    }

    pub fn connections(&self, file: &str) -> Vec<(usize, &CallMark)> {
        self.files.get(file).into_iter().flat_map(|lines| lines.iter())
            .flat_map(|(&line, marks)| marks.iter().map(move |mark| (line, mark))).collect()
    }
}

pub fn file_rgb(path: &str, root: Option<&str>) -> [f32; 3] {
    if root == Some(path) { return [0.95, 0.73, 0.24]; }
    let hash = path.bytes().fold(2166136261u32, |hash, byte| (hash ^ byte as u32).wrapping_mul(16777619));
    let hue = (hash % 3600) as f32 / 600.0;
    let fraction = hue.fract();
    let value = 0.96;
    let minimum = value * 0.32;
    let falling = value * (1.0 - fraction * 0.68);
    let rising = value * (0.32 + fraction * 0.68);
    match hue as usize {
        0 => [value, rising, minimum],
        1 => [falling, value, minimum],
        2 => [minimum, value, rising],
        3 => [minimum, falling, value],
        4 => [rising, minimum, value],
        _ => [value, minimum, falling],
    }
}

pub struct PaintedRun<'a> {
    pub text: &'a str,
    pub color: [f32; 3],
}

pub fn paint_line<'a>(line: &'a HighlightedLine, marks: &[CallMark], root: Option<&str>, syntax: bool, columns: usize) -> Vec<PaintedRun<'a>> {
    if marks.is_empty() {
        if syntax {
            return line.clipped_spans(columns).map(|run| PaintedRun { text: run.text, color: run.category.rgb() }).collect();
        }
        let end = line.text.char_indices().nth(columns).map_or(line.text.len(), |(byte, _)| byte);
        return if end == 0 { Vec::new() } else { vec![PaintedRun { text: &line.text[..end], color: PLAIN }] };
    }
    let mut bytes: Vec<usize> = line.text.char_indices().take(columns).map(|(byte, _)| byte).collect();
    let end = line.text.char_indices().nth(columns).map_or(line.text.len(), |(byte, _)| byte);
    bytes.push(end);
    let length = bytes.len().saturating_sub(1);
    if length == 0 { return Vec::new(); }
    let mut colors = vec![PLAIN; length];
    if syntax {
        let mut column = 0;
        for run in line.clipped_spans(columns) {
            let end = column + run.text.chars().count();
            for color in &mut colors[column..end] { *color = run.category.rgb(); }
            column = end;
        }
    }
    let mut targets: Vec<Option<&str>> = vec![None; length];
    let mut ambiguous = vec![false; length];
    for mark in marks {
        if mark.end <= mark.start { continue; }
        for column in mark.start.min(length)..mark.end.min(length) {
            if targets[column].is_some_and(|target| target != mark.target) { ambiguous[column] = true; }
            targets[column] = Some(&mark.target);
        }
    }
    for column in 0..length {
        if ambiguous[column] { colors[column] = PLAIN; }
        else if let Some(target) = targets[column] { colors[column] = file_rgb(target, root); }
    }
    let mut runs = Vec::new();
    let mut start = 0;
    for column in 1..=length {
        if column == length || colors[column] != colors[start] {
            runs.push(PaintedRun { text: &line.text[bytes[start]..bytes[column]], color: colors[start] });
            start = column;
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{relationships::SymbolRef, syntax::HighlightedText};

    #[test]
    fn neutral_text_only_colors_exact_cross_file_calls() {
        let text = HighlightedText::new("a.ts", "const result = run(); // run is a name here");
        let mark = CallMark { start: 15, end: 18, target: "b.ts".into() };
        let runs = paint_line(&text.lines[0], &[mark], Some("a.ts"), false, 100);
        assert_eq!(runs.iter().map(|run| run.text).collect::<String>(), text.lines[0].text);
        let colored: Vec<_> = runs.iter().filter(|run| run.color != PLAIN).collect();
        assert_eq!(colored.len(), 1);
        assert_eq!(colored[0].text, "run");
        assert_eq!(colored[0].color, file_rgb("b.ts", Some("a.ts")));
    }

    #[test]
    fn unicode_tabs_and_clipping_keep_the_original_display_text() {
        let text = HighlightedText::new("a.rb", "\té = B.run\r\n");
        let marks = [CallMark { start: 10, end: 13, target: "b.rb".into() }];
        for limit in 0..=20 {
            let runs = paint_line(&text.lines[0], &marks, None, false, limit);
            assert_eq!(runs.iter().map(|run| run.text).collect::<String>(), text.lines[0].text.chars().take(limit).collect::<String>());
        }
    }

    #[test]
    fn hidden_targets_and_same_file_calls_have_no_color_marks() {
        let symbol = |file: &str| SymbolRef { name: "run".into(), file: file.into(), line: 1 };
        let edge = |target: &str| CallEdge { caller: symbol("a.ts"), callee: symbol(target), line: 1, start_column: 0, end_column: 3 };
        let visible = HashSet::from(["a.ts".into(), "b.ts".into()]);
        let marks = CallHighlights::new(&[edge("b.ts"), edge("b.ts"), edge("hidden.ts"), edge("a.ts")], &visible);
        assert_eq!(marks.site_count(), 1);
        assert_eq!(marks.line("a.ts", 1)[0].target, "b.ts");
    }

    #[test]
    fn syntax_is_opt_in_and_ambiguous_targets_stay_neutral() {
        let text = HighlightedText::new("a.ts", "return run()");
        assert!(paint_line(&text.lines[0], &[], None, false, 100).iter().all(|run| run.color == PLAIN));
        assert!(paint_line(&text.lines[0], &[], None, true, 100).iter().any(|run| run.color != PLAIN));
        let marks = [CallMark { start: 7, end: 10, target: "b.ts".into() }, CallMark { start: 7, end: 10, target: "c.ts".into() }];
        assert!(paint_line(&text.lines[0], &marks, None, false, 100).iter().all(|run| run.color == PLAIN));
    }
}
