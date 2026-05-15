use crate::types::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::{spinner, word_wrap};

pub(super) fn render(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    content_width: usize,
) -> Vec<Line<'static>> {
    let accent = Color::Rgb(0x42, 0xd9, 0xe8);
    let dim = Color::Rgb(140, 145, 155);
    let text = Color::Rgb(220, 220, 225);
    let warn = Color::Rgb(212, 182, 106);
    let error = Color::Rgb(255, 100, 100);
    let questions = ask_user_questions(tool_use);
    let question_count = questions.len();
    let answered_count = ask_user_answer_count(result);
    let mut lines = Vec::new();

    let mut title = Vec::new();
    if result.is_none() {
        title.push(Span::styled(
            format!("{} ", spinner()),
            Style::default().fg(warn),
        ));
    }
    title.push(Span::raw("· "));
    let title_style = Style::default().fg(if result.is_some_and(|tr| tr.is_error) {
        error
    } else {
        accent
    });
    if result.is_some_and(|tr| !tr.is_error) && answered_count > 0 {
        title.push(Span::styled("Questions", title_style));
        let answered_text = if question_count > 0 {
            format!(" {answered_count}/{question_count} answered")
        } else {
            format!(" {answered_count} answered")
        };
        title.push(Span::styled(answered_text, Style::default().fg(dim)));
    } else {
        title.push(Span::styled("Ask User", title_style));
    }
    if question_count > 0 && answered_count == 0 {
        title.push(Span::styled(
            format!(
                " ({} question{})",
                question_count,
                if question_count == 1 { "" } else { "s" }
            ),
            Style::default().fg(dim),
        ));
    }
    lines.push(Line::from(title));

    if let Some(tr) = result
        && tr.is_error
    {
        let wrap_width = content_width.saturating_sub(2).max(1);
        for wl in word_wrap(&tr.content, wrap_width) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(wl, Style::default().fg(error)),
            ]));
        }
        return lines;
    }

    if result.is_none() {
        for question in questions.iter().take(3) {
            push_ask_user_question_summary(&mut lines, question, content_width, dim, text);
        }
        return lines;
    }

    let Some(tr) = result else {
        return lines;
    };
    if tr.content.trim().is_empty() {
        return lines;
    }

    let Some(value) = serde_json::from_str::<serde_json::Value>(&tr.content).ok() else {
        for wl in word_wrap(&tr.content, content_width.saturating_sub(2).max(1)) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(wl, Style::default().fg(text)),
            ]));
        }
        return lines;
    };

    let Some(answers) = value.get("answers").and_then(|v| v.as_object()) else {
        for wl in word_wrap(&tr.content, content_width.saturating_sub(2).max(1)) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(wl, Style::default().fg(text)),
            ]));
        }
        return lines;
    };

    if questions.is_empty() {
        for (id, answer_value) in answers {
            push_ask_user_answer(
                &mut lines,
                AskUserAnswerRenderInput {
                    id,
                    question: None,
                    answer_value,
                    content_width,
                    dim,
                    text,
                },
            );
        }
    } else {
        for question in questions.iter() {
            if let Some(answer_value) = answers.get(&question.id) {
                push_ask_user_answer(
                    &mut lines,
                    AskUserAnswerRenderInput {
                        id: &question.id,
                        question: Some(question),
                        answer_value,
                        content_width,
                        dim,
                        text,
                    },
                );
            }
        }
    }

    lines
}

#[derive(Debug)]
struct AskUserQuestionSummary {
    id: String,
    header: String,
    question: String,
}

struct AskUserAnswerRenderInput<'a> {
    id: &'a str,
    question: Option<&'a AskUserQuestionSummary>,
    answer_value: &'a serde_json::Value,
    content_width: usize,
    dim: Color,
    text: Color,
}

fn ask_user_questions(tool_use: &ToolUseBlock) -> Vec<AskUserQuestionSummary> {
    tool_use
        .input
        .get("questions")
        .and_then(|v| v.as_array())
        .map(|questions| {
            questions
                .iter()
                .filter_map(|q| {
                    Some(AskUserQuestionSummary {
                        id: q.get("id")?.as_str()?.to_string(),
                        header: q
                            .get("header")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        question: q
                            .get("question")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn ask_user_answer_count(result: Option<&ToolResultBlock>) -> usize {
    result
        .and_then(|tr| serde_json::from_str::<serde_json::Value>(&tr.content).ok())
        .and_then(|value| {
            value
                .get("answers")
                .and_then(|v| v.as_object())
                .map(|v| v.len())
        })
        .unwrap_or(0)
}

fn push_ask_user_question_summary(
    lines: &mut Vec<Line<'static>>,
    question: &AskUserQuestionSummary,
    content_width: usize,
    dim: Color,
    text: Color,
) {
    let label = if question.header.trim().is_empty() {
        question.id.as_str()
    } else {
        question.header.as_str()
    };
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(label.to_string(), Style::default().fg(dim)),
    ]));
    let wrap_width = content_width.saturating_sub(4).max(1);
    for wl in word_wrap(&question.question, wrap_width) {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(wl, Style::default().fg(text)),
        ]));
    }
}

fn push_ask_user_answer(lines: &mut Vec<Line<'static>>, input: AskUserAnswerRenderInput<'_>) {
    let AskUserAnswerRenderInput {
        id,
        question,
        answer_value,
        content_width,
        dim,
        text,
    } = input;

    let label = answer_value
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("<missing answer>");
    let note = answer_value.get("note").and_then(|v| v.as_str());
    let question_label = question
        .map(|q| {
            if q.header.trim().is_empty() {
                q.id.as_str()
            } else {
                q.header.as_str()
            }
        })
        .unwrap_or(id);

    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(". ", Style::default().fg(dim)),
        Span::styled(
            question_label.to_string(),
            Style::default().fg(dim).add_modifier(Modifier::BOLD),
        ),
    ]));
    push_ask_user_answer_field(
        lines,
        "answer",
        label,
        content_width,
        Style::default().fg(text),
        dim,
    );

    if let Some(note) = note
        && !note.trim().is_empty()
    {
        push_ask_user_answer_field(
            lines,
            "note",
            note.trim(),
            content_width,
            Style::default().fg(text),
            dim,
        );
    }
}

fn push_ask_user_answer_field(
    lines: &mut Vec<Line<'static>>,
    name: &'static str,
    value: &str,
    content_width: usize,
    value_style: Style,
    dim: Color,
) {
    let prefix = format!("    {name}: ");
    let continuation = " ".repeat(UnicodeWidthStr::width(prefix.as_str()));
    let wrap_width = content_width
        .saturating_sub(UnicodeWidthStr::width(prefix.as_str()))
        .max(1);
    for (idx, wl) in word_wrap(value, wrap_width).into_iter().enumerate() {
        let current_prefix = if idx == 0 {
            prefix.as_str()
        } else {
            continuation.as_str()
        };
        lines.push(Line::from(vec![
            Span::styled(current_prefix.to_string(), Style::default().fg(dim)),
            Span::styled(wl, value_style),
        ]));
    }
}
