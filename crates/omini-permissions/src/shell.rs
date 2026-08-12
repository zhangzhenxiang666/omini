//! Shell 命令拆分和分词。
//!
//! `split_shell_commands` 负责将复合 shell 命令拆成独立可执行段，支持：
//! - 顶层分隔符：`;` `|` `||` `&&` `\n`
//! - `$()` 命令替换（递归）
//! - `` `...` `` 反引号命令替换（递归）
//! - `(...)` 括号子 shell（递归）
//! - `<(...)` `>(...)` 进程替换（递归）
//! - `eval` / `exec` 的参数会被提取并递归拆分

/// 将复合 shell 命令拆分为独立可执行段。
/// 递归提取 `$()`、反引号、括号子 shell、进程替换内的嵌套命令，
/// 并展开 `eval`/`exec` 的参数。
pub(crate) fn split_shell_commands(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut nested = Vec::new();
    split_top_level(command, &mut parts, &mut nested);
    push_part(&mut parts, &mut String::new());

    // 对每条顶层段，提取 eval/exec 参数中的嵌套命令。
    let mut expanded: Vec<String> = Vec::new();
    for part in parts {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        expanded.push(trimmed.to_string());
        extract_eval_exec_args(trimmed, &mut nested);
    }

    // 递归拆分所有收集到的嵌套上下文。
    for ctx in nested {
        let ctx_trimmed = ctx.trim().to_string();
        if !ctx_trimmed.is_empty() {
            expanded.extend(split_shell_commands(&ctx_trimmed));
        }
    }

    expanded
}

/// 顶层拆分：按 `;` `|` `||` `&&` `\n` 分割，同时递归提取 `$()`、反引号、
/// `(...)` 子 shell、`<(...)` `>(...)` 进程替换。
fn split_top_level(command: &str, parts: &mut Vec<String>, nested: &mut Vec<String>) {
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        // 单引号内完全按字面处理；双引号内仍会执行命令替换。
        if let Some(q) = quote {
            match ch {
                '\\' if q == '"' => {
                    current.push(ch);
                    escaped = true;
                }
                '$' if q == '"' && chars.peek() == Some(&'(') => {
                    chars.next();
                    nested.push(consume_balanced_parens(&mut chars));
                }
                '`' if q == '"' => nested.push(consume_backticks(&mut chars)),
                _ => {
                    current.push(ch);
                    if ch == q {
                        quote = None;
                    }
                }
            }
            continue;
        }
        match ch {
            '\\' => {
                current.push(ch);
                escaped = true;
            }
            '"' | '\'' => {
                quote = Some(ch);
                current.push(ch);
            }
            '$' if chars.peek() == Some(&'(') => {
                // $() 命令替换
                chars.next(); // 消费 '('
                let inner = consume_balanced_parens(&mut chars);
                nested.push(inner);
            }
            '(' => {
                // (...) 子 shell
                let inner = consume_balanced_parens(&mut chars);
                nested.push(inner);
            }
            '<' | '>' if chars.peek() == Some(&'(') => {
                // <() 或 >() 进程替换
                chars.next(); // 消费 '('
                let inner = consume_balanced_parens(&mut chars);
                nested.push(inner);
            }
            '`' => {
                // 反引号命令替换
                let inner = consume_backticks(&mut chars);
                nested.push(inner);
            }
            ';' | '|' | '\n' => {
                push_part(parts, &mut current);
                if ch == '|' && chars.peek() == Some(&'|') {
                    chars.next();
                }
            }
            '&' if chars.peek() == Some(&'&') => {
                chars.next();
                push_part(parts, &mut current);
            }
            _ => {
                current.push(ch);
            }
        }
    }
    push_part(parts, &mut current);
}

/// 从当前光标（已在开括号之后）消费到匹配的闭括号，处理嵌套括号。
/// 返回括号内的内容（不含外层括号）。
fn consume_balanced_parens(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut depth = 1;
    let mut inner = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in chars.by_ref() {
        if escaped {
            inner.push(ch);
            escaped = false;
            continue;
        }
        if let Some(q) = quote {
            inner.push(ch);
            if ch == '\\' && q == '"' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => {
                quote = Some(ch);
                inner.push(ch);
            }
            '(' => {
                depth += 1;
                inner.push(ch);
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return inner;
                }
                inner.push(ch);
            }
            _ => inner.push(ch),
        }
    }
    inner
}

/// 从当前光标（已在开反引号之后）消费到匹配的闭反引号。
fn consume_backticks(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut inner = String::new();
    let mut escaped = false;
    for ch in chars.by_ref() {
        if escaped {
            inner.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '`' {
            return inner;
        }
        inner.push(ch);
    }
    inner
}

/// 提取 `eval` 和 `exec` 命令的参数，作为额外可执行上下文加入 nested。
/// 只识别独立命令开头的关键字，避免把普通参数中的 `eval` / `exec` 当作执行。
/// 提取后剥离外层引号（模拟 shell 对 eval 参数的解析行为）。
fn extract_eval_exec_args(command: &str, nested: &mut Vec<String>) {
    let command = command.trim_start();
    for keyword in ["eval", "exec"] {
        if let Some(after) = command.strip_prefix(keyword)
            && after.chars().next().is_none_or(char::is_whitespace)
        {
            let stripped = strip_outer_quotes(after);
            if !stripped.is_empty() {
                nested.push(stripped.to_string());
            }
            return;
        }
    }
}

/// 剥离字符串首尾匹配的外层引号（`"..."` 或 `'...'`）。
/// 模拟 shell 对 eval 参数的引号处理：`eval "sudo rm"` → 提取 `sudo rm`。
/// 如果不是完整的引号包裹则原样返回。
fn strip_outer_quotes(s: &str) -> &str {
    let s = s.trim();
    if s.len() < 2 {
        return s;
    }
    let bytes = s.as_bytes();
    let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
    if (first == b'"' || first == b'\'') && first == last {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn push_part(parts: &mut Vec<String>, current: &mut String) {
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        parts.push(trimmed.to_string());
    }
    current.clear();
}

/// 将 shell 命令拆分为参数列表，正确处理引号和转义。
/// 引号字符本身不会出现在结果中（被剥离）。
pub(crate) fn shell_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in command.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if let Some(q) = quote {
            if ch == '\\' && q == '"' {
                escaped = true;
            } else if ch == q {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch == '\\' {
            escaped = true;
        } else if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                words.push(current.clone());
                current.clear();
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

#[cfg(test)]
mod tests {
    use crate::shell::{shell_words, split_shell_commands};

    #[test]
    fn top_level_separators_produce_complete_trimmed_command_order() {
        assert_eq!(
            split_shell_commands("  ls ; pwd || whoami && date | wc -l\ntrue  "),
            vec!["ls", "pwd", "whoami", "date", "wc -l", "true"]
        );
        assert_eq!(split_shell_commands(" ; | || && \n"), Vec::<String>::new());
    }

    #[test]
    fn quoted_separators_remain_in_their_original_command() {
        assert_eq!(
            split_shell_commands(r#"echo "a;b|c&&d" && printf '%s\n' 'x;y'"#),
            vec![r#"echo "a;b|c&&d""#, r#"printf '%s\n' 'x;y'"#]
        );
        assert_eq!(
            split_shell_commands(r#"echo a\;b && echo "a\";b""#),
            vec![r#"echo a\;b"#, r#"echo "a\";b""#]
        );
    }

    #[test]
    fn dollar_substitutions_are_recursively_extracted_in_stable_order() {
        assert_eq!(
            split_shell_commands("echo $(sudo true)"),
            vec!["echo", "sudo true"]
        );
        assert_eq!(
            split_shell_commands("echo $(echo $(date))"),
            vec!["echo", "echo", "date"]
        );
        assert_eq!(
            split_shell_commands(r#"echo "$(sudo true)""#),
            vec![r#"echo """#, "sudo true"]
        );
        assert_eq!(
            split_shell_commands(r#"echo '$(sudo true)'"#),
            vec![r#"echo '$(sudo true)'"#]
        );
        assert_eq!(
            split_shell_commands(r#"echo "\$(sudo true)""#),
            vec![r#"echo "\$(sudo true)""#]
        );
    }

    #[test]
    fn backticks_subshells_and_process_substitutions_expose_nested_commands() {
        assert_eq!(
            split_shell_commands("echo `sudo true`"),
            vec!["echo", "sudo true"]
        );
        assert_eq!(
            split_shell_commands("(cd /tmp && pwd)"),
            vec!["cd /tmp", "pwd"]
        );
        assert_eq!(
            split_shell_commands("diff <(curl a) <(cat b)"),
            vec!["diff", "curl a", "cat b"]
        );
    }

    #[test]
    fn eval_and_exec_expand_only_when_they_are_the_executed_command() {
        assert_eq!(
            split_shell_commands(r#"eval "sudo true""#),
            vec![r#"eval "sudo true""#, "sudo true"]
        );
        assert_eq!(
            split_shell_commands("exec sudo true"),
            vec!["exec sudo true", "sudo true"]
        );
        assert_eq!(
            split_shell_commands("echo exec sudo true"),
            vec!["echo exec sudo true"]
        );
        assert_eq!(
            split_shell_commands("reevaluate sudo true"),
            vec!["reevaluate sudo true"]
        );
    }

    #[test]
    fn shell_words_handle_whitespace_quotes_escapes_and_incomplete_quotes() {
        let cases = [
            ("", &[][..]),
            ("  git   status  ", &["git", "status"][..]),
            (
                "git commit -m 'hello world'",
                &["git", "commit", "-m", "hello world"][..],
            ),
            (r#"echo hello\ world"#, &["echo", "hello world"][..]),
            (r#"echo 'hello\ world'"#, &["echo", r#"hello\ world"#][..]),
            (
                r#"curl -H "Authorization: Bearer token""#,
                &["curl", "-H", "Authorization: Bearer token"][..],
            ),
            ("echo 'unterminated", &["echo", "unterminated"][..]),
        ];

        for (command, expected) in cases {
            assert_eq!(
                shell_words(command),
                expected
                    .iter()
                    .map(|word| (*word).to_string())
                    .collect::<Vec<_>>(),
                "{command:?}"
            );
        }
    }
}
