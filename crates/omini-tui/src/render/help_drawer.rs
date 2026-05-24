use super::*;
use crate::state::{HelpTab, UiState};
use crate::types::events::{CommandKind, CommandSummary};

const HELP_DRAWER_MAX_HEIGHT: u16 = 18;
const MAIN_ACCENT: Color = Color::Rgb(0x42, 0xd9, 0xe8);

struct CommandListRender<'a> {
    commands: &'a [CommandSummary],
    kind: CommandKind,
    selected: usize,
    content_width: usize,
    max_lines: usize,
    top_indicator: &'a str,
    bottom_indicator: &'a str,
    empty_text: &'a str,
}

pub(super) fn help_drawer_height(area: Rect) -> u16 {
    area.height.min(HELP_DRAWER_MAX_HEIGHT)
}

pub(super) fn render_help_drawer(state: &mut UiState, frame: &mut ratatui::Frame, area: Rect) {
    let Some(drawer) = state.help_drawer.clone() else {
        return;
    };
    if area.width == 0 || area.height == 0 {
        return;
    }
    state.clear_selectable_screen_lines();

    let desired_height = help_drawer_height(area);
    let panel_height = desired_height.saturating_sub(1);
    let drawer_area = Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(panel_height),
        width: area.width,
        height: panel_height,
    };

    frame.render_widget(Clear, drawer_area);
    frame.render_widget(
        Paragraph::new(Line::from("")).style(Style::default().bg(Color::Rgb(40, 44, 52))),
        drawer_area,
    );

    let divider_area = Rect {
        x: drawer_area.x,
        y: drawer_area.y.saturating_sub(1),
        width: drawer_area.width,
        height: 1,
    };
    let mut divider_line = Line::from(Span::styled(
        "━".repeat(drawer_area.width.saturating_sub(1) as usize),
        Style::default().fg(MAIN_ACCENT),
    ));
    register_and_highlight_lines(state, divider_area, std::slice::from_mut(&mut divider_line));
    frame.render_widget(Paragraph::new(divider_line), divider_area);

    if drawer_area.height > 1 {
        let tab_area = Rect {
            x: drawer_area.x + 3,
            y: drawer_area.y + 1,
            width: drawer_area.width.saturating_sub(6),
            height: 1,
        };
        let mut tabs = help_tabs(drawer.tab);
        register_and_highlight_lines(state, tab_area, std::slice::from_mut(&mut tabs));
        frame.render_widget(Paragraph::new(tabs), tab_area);
    }

    let body_area = Rect {
        x: drawer_area.x + 3,
        y: drawer_area.y + 3,
        width: drawer_area.width.saturating_sub(6),
        height: drawer_area.height.saturating_sub(5),
    };
    let content_width = body_area.width as usize;
    let mut body = match drawer.tab {
        HelpTab::General => general_lines(
            content_width,
            drawer.general_selected,
            body_area.height as usize,
        ),
        HelpTab::Commands => command_lines(CommandListRender {
            commands: &drawer.commands,
            kind: CommandKind::Builtin,
            selected: drawer.command_selected,
            content_width,
            max_lines: body_area.height as usize,
            top_indicator: "↑ 更多命令",
            bottom_indicator: "↓ 更多命令",
            empty_text: "暂无可用命令",
        }),
        HelpTab::Skills => command_lines(CommandListRender {
            commands: &drawer.commands,
            kind: CommandKind::Skill,
            selected: drawer.skill_selected,
            content_width,
            max_lines: body_area.height as usize,
            top_indicator: "↑ 更多 skills",
            bottom_indicator: "↓ 更多 skills",
            empty_text: "当前没有可用的 skill 命令",
        }),
    };
    if body_area.height > 0 && body_area.width > 0 && body_area.y < drawer_area.bottom() {
        register_and_highlight_lines(state, body_area, &mut body);
        frame.render_widget(Paragraph::new(Text::from(body)), body_area);
    }

    let hint_area = Rect {
        x: drawer_area.x + 3,
        y: drawer_area.y + drawer_area.height.saturating_sub(1),
        width: drawer_area.width.saturating_sub(6),
        height: 1,
    };
    if hint_area.width > 0 {
        let mut hint = Line::from(vec![
            Span::styled("Esc ", Style::default().fg(MAIN_ACCENT)),
            Span::styled("关闭", Style::default().fg(Color::Rgb(140, 145, 155))),
            Span::raw("  "),
            Span::styled("←/→ ", Style::default().fg(MAIN_ACCENT)),
            Span::styled("切换分类", Style::default().fg(Color::Rgb(140, 145, 155))),
            Span::raw("  "),
            Span::styled("↑/↓ ", Style::default().fg(MAIN_ACCENT)),
            Span::styled("浏览列表", Style::default().fg(Color::Rgb(140, 145, 155))),
        ]);
        register_and_highlight_lines(state, hint_area, std::slice::from_mut(&mut hint));
        frame.render_widget(Paragraph::new(hint), hint_area);
    }
}

fn help_tabs(active: HelpTab) -> Line<'static> {
    Line::from(vec![
        tab_span("常规", active == HelpTab::General),
        Span::raw("  "),
        tab_span("Commands", active == HelpTab::Commands),
        Span::raw("  "),
        tab_span("Skills", active == HelpTab::Skills),
    ])
}

fn tab_span(label: &str, active: bool) -> Span<'static> {
    let style = if active {
        Style::default()
            .fg(Color::Rgb(40, 44, 52))
            .bg(MAIN_ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Rgb(0xa5, 0xac, 0xb6))
            .add_modifier(Modifier::BOLD)
    };
    Span::styled(format!(" {label} "), style)
}

fn general_lines(content_width: usize, selected: usize, max_lines: usize) -> Vec<Line<'static>> {
    let rows = [
        ("Enter", "发送消息"),
        ("Shift+Enter / Ctrl+J", "换行"),
        ("Shift+Tab", "切换 Main/Auto/Plan 模式"),
        ("/", "输入命令，↑/↓ 选择，Tab/Enter 接受补全"),
        ("@", "引用文件或 agent，Tab/→ 展开目录"),
        ("PageUp / PageDown", "滚动消息"),
        ("Ctrl+Home / Ctrl+End", "跳到顶部或底部"),
        ("Esc", "运行中取消；补全或 Help 中关闭当前面板"),
        ("Alt+Enter", "运行中插入排队输入"),
        ("Ctrl+C", "清空输入"),
    ];
    let mut lines = vec![
        ScrollableLine {
            selected: false,
            line: general_text_line(
                "Omini 可以理解代码库、编辑文件并执行命令；涉及工具权限时会在底部抽屉中确认。",
                false,
                content_width,
            ),
        },
        ScrollableLine {
            selected: false,
            line: Line::from(""),
        },
    ];
    let key_width = rows
        .iter()
        .map(|(key, _)| UnicodeWidthStr::width(*key))
        .max()
        .unwrap_or(0)
        .min(content_width / 2);
    for (idx, (key, description)) in rows.into_iter().enumerate() {
        lines.push(ScrollableLine {
            selected: selected == idx,
            line: help_row(key, description, key_width, content_width, selected == idx),
        });
    }
    lines.push(ScrollableLine {
        selected: false,
        line: Line::from(""),
    });
    lines.push(ScrollableLine {
        selected: false,
        line: general_text_line(
            "Commands 和 Skills 页可用 ↑/↓ 浏览，Enter 不会执行选中的命令。",
            false,
            content_width,
        ),
    });
    scrollable_lines(lines, max_lines, "↑ 更多帮助", "↓ 更多帮助")
}

fn general_text_line(text: &str, selected: bool, content_width: usize) -> Line<'static> {
    let prefix = if selected { "› " } else { "  " };
    let fg = if selected {
        MAIN_ACCENT
    } else {
        Color::Rgb(0xa5, 0xac, 0xb6)
    };
    Line::from(Span::styled(
        pad_or_truncate(&format!("{prefix}{text}"), content_width),
        Style::default().fg(fg),
    ))
}

fn help_row(
    key: &str,
    description: &str,
    key_width: usize,
    content_width: usize,
    selected: bool,
) -> Line<'static> {
    let prefix = if selected { "› " } else { "  " };
    let key_text = pad_or_truncate(key, key_width);
    let separator = "  ";
    let description_width = content_width
        .saturating_sub(key_width)
        .saturating_sub(UnicodeWidthStr::width(prefix))
        .saturating_sub(UnicodeWidthStr::width(separator));
    let key_style = if selected {
        Style::default()
            .fg(MAIN_ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Rgb(0xa5, 0xac, 0xb6))
            .add_modifier(Modifier::BOLD)
    };
    let description_style = if selected {
        Style::default().fg(MAIN_ACCENT)
    } else {
        Style::default().fg(Color::Rgb(140, 145, 155))
    };
    Line::from(vec![
        Span::styled(prefix.to_string(), Style::default().fg(MAIN_ACCENT)),
        Span::styled(key_text, key_style),
        Span::raw(separator),
        Span::styled(
            pad_or_truncate(description, description_width),
            description_style,
        ),
    ])
}

fn command_lines(input: CommandListRender<'_>) -> Vec<Line<'static>> {
    let commands = input
        .commands
        .iter()
        .filter(|command| command.kind == input.kind)
        .collect::<Vec<_>>();
    if commands.is_empty() {
        return vec![Line::from(Span::styled(
            input.empty_text.to_string(),
            Style::default().fg(Color::Rgb(140, 145, 155)),
        ))];
    }

    let selected = input.selected.min(commands.len().saturating_sub(1));
    let max_name_width = commands
        .iter()
        .map(|command| UnicodeWidthStr::width(command_signature(command).as_str()))
        .max()
        .unwrap_or(0);
    let rows = commands
        .iter()
        .enumerate()
        .map(|(idx, command)| ScrollableLine {
            selected: idx == selected,
            line: command_line(
                command,
                idx == selected,
                max_name_width,
                input.content_width,
            ),
        })
        .collect::<Vec<_>>();
    scrollable_lines(
        rows,
        input.max_lines,
        input.top_indicator,
        input.bottom_indicator,
    )
}

fn command_line(
    command: &CommandSummary,
    selected: bool,
    max_name_width: usize,
    content_width: usize,
) -> Line<'static> {
    let signature = command_signature(command);
    let padding =
        " ".repeat(max_name_width.saturating_sub(UnicodeWidthStr::width(signature.as_str())));
    let alias = if command.aliases.is_empty() {
        String::new()
    } else {
        format!("  别名: {}", command.aliases.join(", "))
    };
    let prefix = if selected { "› " } else { "  " };
    let text = format!(
        "{prefix}{signature}{padding}  {}{}",
        command.description, alias
    );
    let fg = if selected {
        MAIN_ACCENT
    } else {
        Color::Rgb(0xa5, 0xac, 0xb6)
    };
    Line::from(Span::styled(
        pad_or_truncate(&text, content_width),
        Style::default().fg(fg),
    ))
}

fn command_signature(command: &CommandSummary) -> String {
    if command.has_args
        && let Some(args) = command.args_description
    {
        format!("/{} {}", command.name, args)
    } else {
        format!("/{}", command.name)
    }
}

fn pad_or_truncate(text: &str, width: usize) -> String {
    let truncated = truncate_display_width(text, width);
    let text_width = UnicodeWidthStr::width(truncated.as_str());
    if text_width >= width {
        truncated
    } else {
        format!("{}{}", truncated, " ".repeat(width - text_width))
    }
}

fn truncate_display_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }

    let ellipsis = "…";
    let ellipsis_width = UnicodeWidthStr::width(ellipsis);
    if max_width <= ellipsis_width {
        return ellipsis.to_string();
    }

    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width + ellipsis_width > max_width {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out.push_str(ellipsis);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(name: &str, kind: CommandKind) -> CommandSummary {
        CommandSummary {
            name: name.to_string(),
            aliases: Vec::new(),
            description: format!("{name} description"),
            sort_weight: 0,
            kind,
            has_args: true,
            args_description: Some("[prompt]"),
        }
    }

    fn plain(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn command_lines_filter_builtin_commands() {
        let commands = vec![
            command("help", CommandKind::Builtin),
            command("skill-creator", CommandKind::Skill),
        ];

        let rendered = command_lines(CommandListRender {
            commands: &commands,
            kind: CommandKind::Builtin,
            selected: 0,
            content_width: 80,
            max_lines: 8,
            top_indicator: "top",
            bottom_indicator: "bottom",
            empty_text: "empty",
        })
        .iter()
        .map(plain)
        .collect::<Vec<_>>()
        .join("\n");

        assert!(rendered.contains("/help [prompt]"));
        assert!(!rendered.contains("skill-creator"));
    }

    #[test]
    fn command_lines_keep_selected_skill_visible() {
        let commands = (0..8)
            .map(|idx| command(&format!("skill-{idx}"), CommandKind::Skill))
            .collect::<Vec<_>>();

        let rendered = command_lines(CommandListRender {
            commands: &commands,
            kind: CommandKind::Skill,
            selected: 7,
            content_width: 80,
            max_lines: 4,
            top_indicator: "top",
            bottom_indicator: "bottom",
            empty_text: "empty",
        })
        .iter()
        .map(plain)
        .collect::<Vec<_>>();

        assert_eq!(rendered.first().map(String::as_str), Some("top"));
        assert!(rendered.iter().any(|line| line.contains("/skill-7")));
        assert!(!rendered.iter().any(|line| line == "bottom"));
    }
}
