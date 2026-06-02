const OPEN_TAG: &str = "<proposed_plan>";
const CLOSE_TAG: &str = "</proposed_plan>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposedPlanSegment {
    Normal(String),
    ProposedPlanStart,
    ProposedPlanDelta(String),
    ProposedPlanEnd,
}

#[derive(Debug, Default)]
pub struct ProposedPlanParser {
    buffer: String,
    inside_plan: bool,
    fence: Option<Fence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fence {
    marker: char,
    len: usize,
}

impl ProposedPlanParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_str(&mut self, chunk: &str) -> Vec<ProposedPlanSegment> {
        self.buffer.push_str(chunk);
        let mut out = Vec::new();

        while let Some(idx) = self.buffer.find('\n') {
            let line = self.buffer.drain(..=idx).collect::<String>();
            self.push_line(line, &mut out);
        }

        out
    }

    pub fn finish(&mut self) -> Vec<ProposedPlanSegment> {
        if self.buffer.is_empty() {
            self.inside_plan = false;
            self.fence = None;
            return Vec::new();
        }

        let mut out = Vec::new();
        let line = std::mem::take(&mut self.buffer);
        self.push_line(line, &mut out);
        self.inside_plan = false;
        self.fence = None;
        out
    }

    fn push_line(&mut self, line: String, out: &mut Vec<ProposedPlanSegment>) {
        let inside_fence = self.fence.is_some();

        if !inside_fence && !self.inside_plan && is_tag_line(&line, OPEN_TAG) {
            self.inside_plan = true;
            out.push(ProposedPlanSegment::ProposedPlanStart);
            return;
        }

        if !inside_fence && self.inside_plan && is_tag_line(&line, CLOSE_TAG) {
            self.inside_plan = false;
            out.push(ProposedPlanSegment::ProposedPlanEnd);
            return;
        }

        if self.inside_plan {
            out.push(ProposedPlanSegment::ProposedPlanDelta(line.clone()));
        } else {
            out.push(ProposedPlanSegment::Normal(line.clone()));
        }

        self.update_fence(&line);
    }

    fn update_fence(&mut self, line: &str) {
        if let Some(fence) = self.fence {
            if is_closing_fence(line, fence) {
                self.fence = None;
            }
        } else if let Some(fence) = opening_fence(line) {
            self.fence = Some(fence);
        }
    }
}

pub fn strip_proposed_plan_blocks(text: &str) -> String {
    let mut parser = ProposedPlanParser::new();
    let mut out = String::new();
    for segment in parser.push_str(text).into_iter().chain(parser.finish()) {
        if let ProposedPlanSegment::Normal(text) = segment {
            out.push_str(&text);
        }
    }
    out
}

pub fn extract_proposed_plan_text(text: &str) -> Option<String> {
    let mut parser = ProposedPlanParser::new();
    let mut plan = String::new();
    let mut saw_plan = false;

    for segment in parser.push_str(text).into_iter().chain(parser.finish()) {
        match segment {
            ProposedPlanSegment::ProposedPlanStart => {
                saw_plan = true;
                plan.clear();
            }
            ProposedPlanSegment::ProposedPlanDelta(delta) => plan.push_str(&delta),
            ProposedPlanSegment::Normal(_) | ProposedPlanSegment::ProposedPlanEnd => {}
        }
    }

    saw_plan.then_some(plan)
}

fn is_tag_line(line: &str, tag: &str) -> bool {
    line.trim() == tag
}

fn opening_fence(line: &str) -> Option<Fence> {
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }

    let len = trimmed.chars().take_while(|ch| *ch == marker).count();
    (len >= 3).then_some(Fence { marker, len })
}

fn is_closing_fence(line: &str, fence: Fence) -> bool {
    let trimmed = line.trim_start();
    let len = trimmed.chars().take_while(|ch| *ch == fence.marker).count();
    if len < fence.len {
        return false;
    }

    trimmed[len..].trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_and_strips_complete_block() {
        let text = "before\n<proposed_plan>\n- step\n</proposed_plan>\nafter";

        assert_eq!(
            extract_proposed_plan_text(text),
            Some("- step\n".to_string())
        );
        assert_eq!(strip_proposed_plan_blocks(text), "before\nafter");
    }

    #[test]
    fn streams_tags_split_across_chunks() {
        let mut parser = ProposedPlanParser::new();
        let mut segments = Vec::new();
        segments.extend(parser.push_str("Intro\n<proposed"));
        segments.extend(parser.push_str("_plan>\n- step"));
        segments.extend(parser.push_str("\n</proposed_plan>\nOutro"));
        segments.extend(parser.finish());

        assert_eq!(
            segments,
            vec![
                ProposedPlanSegment::Normal("Intro\n".to_string()),
                ProposedPlanSegment::ProposedPlanStart,
                ProposedPlanSegment::ProposedPlanDelta("- step\n".to_string()),
                ProposedPlanSegment::ProposedPlanEnd,
                ProposedPlanSegment::Normal("Outro".to_string()),
            ]
        );
    }

    #[test]
    fn extracts_unclosed_plan_at_finish() {
        let text = "<proposed_plan>\n- step\n";
        assert_eq!(
            extract_proposed_plan_text(text),
            Some("- step\n".to_string())
        );
    }

    #[test]
    fn malformed_open_tag_stays_visible() {
        let text = "<proposed_plan extra>\n- step\n";
        assert_eq!(strip_proposed_plan_blocks(text), text);
        assert_eq!(extract_proposed_plan_text(text), None);
    }

    #[test]
    fn inline_tag_reference_stays_visible() {
        let text = concat!(
            "Use `<proposed_plan>` as the wrapper.\n\n",
            "<proposed_plan>\n",
            "# Plan\n",
            "</proposed_plan>\n",
        );

        assert_eq!(
            extract_proposed_plan_text(text),
            Some("# Plan\n".to_string())
        );
        assert_eq!(
            strip_proposed_plan_blocks(text),
            "Use `<proposed_plan>` as the wrapper.\n\n"
        );
    }

    #[test]
    fn sentence_embedded_tag_is_not_a_plan_block() {
        let text = "before <proposed_plan>\n- step\n</proposed_plan>\nafter";

        assert_eq!(strip_proposed_plan_blocks(text), text);
        assert_eq!(extract_proposed_plan_text(text), None);
    }

    #[test]
    fn ignores_tags_inside_fenced_code_blocks() {
        let text = concat!(
            "```md\n",
            "<proposed_plan>\n",
            "# Fake\n",
            "</proposed_plan>\n",
            "```\n",
            "<proposed_plan>\n",
            "# Real\n",
            "```rust\n",
            "let tag = \"</proposed_plan>\";\n",
            "```\n",
            "</proposed_plan>\n",
            "after",
        );

        assert_eq!(
            extract_proposed_plan_text(text),
            Some("# Real\n```rust\nlet tag = \"</proposed_plan>\";\n```\n".to_string())
        );
        assert_eq!(
            strip_proposed_plan_blocks(text),
            "```md\n<proposed_plan>\n# Fake\n</proposed_plan>\n```\nafter"
        );
    }
}
