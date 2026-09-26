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

    trimmed.chars().skip(len).all(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_block_extracts_plan_and_preserves_normal_text() {
        let input = "before\n<proposed_plan>\n- step\n</proposed_plan>\nafter";

        assert_eq!(extract_proposed_plan_text(input), Some("- step\n".into()));
        assert_eq!(strip_proposed_plan_blocks(input), "before\nafter");
    }

    #[test]
    fn empty_input_produces_no_segments_or_plan() {
        let mut parser = ProposedPlanParser::new();

        assert!(parser.push_str("").is_empty());
        assert!(parser.finish().is_empty());
        assert_eq!(extract_proposed_plan_text(""), None);
        assert_eq!(strip_proposed_plan_blocks(""), "");
    }

    #[test]
    fn empty_plan_is_distinguished_from_missing_plan() {
        let input = "<proposed_plan>\n</proposed_plan>\n";

        assert_eq!(extract_proposed_plan_text(input), Some(String::new()));
        assert_eq!(strip_proposed_plan_blocks(input), "");
    }

    #[test]
    fn tags_split_at_arbitrary_character_boundaries_match_single_chunk() {
        let input = "开头\n<proposed_plan>\n- 步骤 α\n</proposed_plan>\n结尾";
        let expected = parse_chunks(&[input]);

        for boundary in input
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(input.len()))
        {
            assert_eq!(
                parse_chunks(&[&input[..boundary], &input[boundary..]]),
                expected,
                "different result at byte boundary {boundary}"
            );
        }
    }

    #[test]
    fn streaming_preserves_segment_order() {
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
    fn unclosed_plan_is_returned_when_stream_finishes() {
        let input = "<proposed_plan>\n- step\n";

        assert_eq!(extract_proposed_plan_text(input), Some("- step\n".into()));
        assert_eq!(strip_proposed_plan_blocks(input), "");
    }

    #[test]
    fn malformed_and_embedded_open_tags_remain_normal_text() {
        for input in [
            "<proposed_plan extra>\n- step\n",
            "before <proposed_plan>\n- step\n</proposed_plan>\nafter",
            "Use `<proposed_plan>` as the wrapper.\n",
        ] {
            assert_eq!(strip_proposed_plan_blocks(input), input);
            assert_eq!(extract_proposed_plan_text(input), None);
        }
    }

    #[test]
    fn indented_tag_lines_are_recognized() {
        let input = "before\n  <proposed_plan>  \nplan\n\t</proposed_plan>\nafter";

        assert_eq!(extract_proposed_plan_text(input), Some("plan\n".into()));
        assert_eq!(strip_proposed_plan_blocks(input), "before\nafter");
    }

    #[test]
    fn isolated_close_tag_remains_normal_text() {
        let input = "before\n</proposed_plan>\nafter";

        assert_eq!(extract_proposed_plan_text(input), None);
        assert_eq!(strip_proposed_plan_blocks(input), input);
    }

    #[test]
    fn tags_inside_backtick_and_tilde_fences_remain_content() {
        for fence in ["```", "~~~~"] {
            let input = format!(
                "{fence}md\n<proposed_plan>\nfake\n</proposed_plan>\n{fence}\n\
                 <proposed_plan>\nreal\n{fence}\n</proposed_plan>\n{fence}\n\
                 </proposed_plan>\nafter"
            );

            assert_eq!(
                extract_proposed_plan_text(&input),
                Some(format!("real\n{fence}\n</proposed_plan>\n{fence}\n"))
            );
            assert_eq!(
                strip_proposed_plan_blocks(&input),
                format!("{fence}md\n<proposed_plan>\nfake\n</proposed_plan>\n{fence}\nafter")
            );
        }
    }

    #[test]
    fn shorter_or_different_fence_does_not_close_active_fence() {
        let input = concat!(
            "````md\n",
            "```\n",
            "~~~\n",
            "<proposed_plan>\n",
            "````\n",
            "<proposed_plan>\n",
            "real\n",
            "</proposed_plan>\n",
        );

        assert_eq!(extract_proposed_plan_text(input), Some("real\n".into()));
        assert_eq!(
            strip_proposed_plan_blocks(input),
            "````md\n```\n~~~\n<proposed_plan>\n````\n"
        );
    }

    #[test]
    fn crlf_input_preserves_plan_and_normal_line_endings() {
        let input = "before\r\n<proposed_plan>\r\nplan\r\n</proposed_plan>\r\nafter";

        assert_eq!(extract_proposed_plan_text(input), Some("plan\r\n".into()));
        assert_eq!(strip_proposed_plan_blocks(input), "before\r\nafter");
    }

    #[test]
    fn multiple_blocks_are_all_stripped_and_last_plan_is_extracted() {
        let input = concat!(
            "before\n",
            "<proposed_plan>\nfirst\n</proposed_plan>\n",
            "between\n",
            "<proposed_plan>\nsecond\n</proposed_plan>\n",
            "after",
        );

        assert_eq!(extract_proposed_plan_text(input), Some("second\n".into()));
        assert_eq!(strip_proposed_plan_blocks(input), "before\nbetween\nafter");
    }

    #[test]
    fn parser_can_be_reused_after_finish() {
        let mut parser = ProposedPlanParser::new();

        assert_eq!(
            parser.push_str("<proposed_plan>\nfirst"),
            vec![ProposedPlanSegment::ProposedPlanStart]
        );
        assert_eq!(
            parser.finish(),
            vec![ProposedPlanSegment::ProposedPlanDelta("first".into())]
        );
        assert_eq!(
            parser.push_str("normal\n"),
            vec![ProposedPlanSegment::Normal("normal\n".into())]
        );
        assert!(parser.finish().is_empty());
    }

    #[test]
    fn inline_reference_before_real_plan_remains_visible() {
        let input = concat!(
            "Use `<proposed_plan>` as the wrapper.\n\n",
            "<proposed_plan>\n",
            "# Plan\n",
            "</proposed_plan>\n",
        );

        assert_eq!(extract_proposed_plan_text(input), Some("# Plan\n".into()));
        assert_eq!(
            strip_proposed_plan_blocks(input),
            "Use `<proposed_plan>` as the wrapper.\n\n"
        );
    }

    #[test]
    fn tag_inside_plan_fence_is_emitted_as_plan_delta() {
        let input = concat!(
            "<proposed_plan>\n",
            "```md\n",
            "</proposed_plan>\n",
            "```\n",
            "</proposed_plan>\n",
        );

        assert_eq!(
            extract_proposed_plan_text(input),
            Some("```md\n</proposed_plan>\n```\n".into())
        );
    }

    fn parse_chunks(chunks: &[&str]) -> Vec<ProposedPlanSegment> {
        let mut parser = ProposedPlanParser::new();
        let mut segments = Vec::new();
        for chunk in chunks {
            segments.extend(parser.push_str(chunk));
        }
        segments.extend(parser.finish());
        segments
    }
}
