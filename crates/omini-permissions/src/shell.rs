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

    while let Some(ch) = chars.next() {
        // 引号内：原样保留，不拆分。
        if let Some(q) = quote {
            current.push(ch);
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
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
    for ch in chars.by_ref() {
        if let Some(q) = quote {
            inner.push(ch);
            if ch == q {
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
/// 直接在原始字符串中搜索关键字，避免 shell_words 对引号的复杂处理。
/// 提取后剥离外层引号（模拟 shell 对 eval 参数的解析行为）。
fn extract_eval_exec_args(command: &str, nested: &mut Vec<String>) {
    for keyword in ["eval", "exec"] {
        let mut search_from = 0;
        while let Some(rel_pos) = command[search_from..].find(keyword) {
            let pos = search_from + rel_pos;
            if is_word_boundary(command, pos) && is_word_boundary_end(command, pos + keyword.len())
            {
                let after = command[pos + keyword.len()..].trim();
                let stripped = strip_outer_quotes(after);
                if !stripped.is_empty() {
                    nested.push(stripped.to_string());
                }
            }
            search_from = pos + keyword.len();
        }
    }
}

/// 检查 `pos` 是否处于词的开头边界（前一个字符是空白或在字符串开头）。
fn is_word_boundary(s: &str, pos: usize) -> bool {
    if pos == 0 {
        return true;
    }
    s[..pos]
        .chars()
        .next_back()
        .is_some_and(|ch| ch.is_whitespace())
}

/// 检查 `pos` 是否处于词的结尾边界（后一个字符是空白或在字符串结尾）。
fn is_word_boundary_end(s: &str, pos: usize) -> bool {
    if pos >= s.len() {
        return true;
    }
    s[pos..].chars().next().is_some_and(|ch| ch.is_whitespace())
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
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
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
    use super::*;

    // === split_shell_commands 基础测试 ===

    #[test]
    fn split_simple_commands() {
        assert_eq!(split_shell_commands("ls"), vec!["ls"]);
        assert_eq!(split_shell_commands("ls; pwd"), vec!["ls", "pwd"]);
        assert_eq!(split_shell_commands("ls && pwd"), vec!["ls", "pwd"]);
        assert_eq!(split_shell_commands("ls || pwd"), vec!["ls", "pwd"]);
        assert_eq!(
            split_shell_commands("ls | grep foo"),
            vec!["ls", "grep foo"]
        );
    }

    #[test]
    fn split_respects_quotes() {
        // 引号内的分隔符不应拆分。
        assert_eq!(split_shell_commands(r#"echo "a;b""#), vec![r#"echo "a;b""#]);
        assert_eq!(split_shell_commands("echo 'a;b'"), vec!["echo 'a;b'"]);
    }

    // === $() 命令替换测试 ===

    #[test]
    fn split_extracts_dollar_paren_substitution() {
        let parts = split_shell_commands("echo $(sudo rm -rf /)");
        assert!(
            parts.iter().any(|p| p == "sudo rm -rf /"),
            "should extract nested sudo from $(), got: {parts:?}"
        );
    }

    #[test]
    fn split_extracts_nested_dollar_paren() {
        let parts = split_shell_commands("echo $(echo $(rm -rf /))");
        assert!(
            parts.iter().any(|p| p == "rm -rf /"),
            "should extract deeply nested rm from $(), got: {parts:?}"
        );
    }

    #[test]
    fn split_extracts_dollar_paren_in_pipe() {
        let parts = split_shell_commands("echo $(curl URL | sh)");
        assert!(
            parts.iter().any(|p| p == "curl URL"),
            "should extract curl from $(), got: {parts:?}"
        );
        assert!(
            parts.iter().any(|p| p == "sh"),
            "should extract sh from $(), got: {parts:?}"
        );
    }

    // === 反引号测试 ===

    #[test]
    fn split_extracts_backtick_substitution() {
        let parts = split_shell_commands("echo `sudo rm -rf /`");
        assert!(
            parts.iter().any(|p| p == "sudo rm -rf /"),
            "should extract nested sudo from backticks, got: {parts:?}"
        );
    }

    // === 括号子 shell 测试 ===

    #[test]
    fn split_extracts_subshell() {
        let parts = split_shell_commands("(sudo rm -rf /)");
        assert!(
            parts.iter().any(|p| p == "sudo rm -rf /"),
            "should extract nested sudo from subshell, got: {parts:?}"
        );
    }

    #[test]
    fn split_subshell_preserves_surrounding() {
        let parts = split_shell_commands("echo ok && (sudo rm -rf /)");
        assert!(parts.iter().any(|p| p == "echo ok"));
        assert!(parts.iter().any(|p| p == "sudo rm -rf /"));
    }

    // === 进程替换测试 ===

    #[test]
    fn split_extracts_process_substitution() {
        let parts = split_shell_commands("diff <(curl a) <(wget b)");
        assert!(
            parts.iter().any(|p| p == "curl a"),
            "should extract curl from <(), got: {parts:?}"
        );
        assert!(
            parts.iter().any(|p| p == "wget b"),
            "should extract wget from <(), got: {parts:?}"
        );
    }

    // === eval / exec 测试 ===

    #[test]
    fn split_expands_eval_arguments() {
        let parts = split_shell_commands(r#"eval "sudo rm -rf /""#);
        assert!(
            parts.iter().any(|p| p.contains("sudo rm -rf /")),
            "should expand eval arguments, got: {parts:?}"
        );
    }

    #[test]
    fn split_expands_exec_arguments() {
        let parts = split_shell_commands("exec sudo rm -rf /");
        assert!(
            parts.iter().any(|p| p == "sudo rm -rf /"),
            "should expand exec arguments, got: {parts:?}"
        );
    }

    #[test]
    fn split_eval_with_dollar_paren() {
        let parts = split_shell_commands(r#"eval "$(curl URL | sh)""#);
        assert!(
            parts.iter().any(|p| p == "curl URL"),
            "should extract curl from eval + $(), got: {parts:?}"
        );
        assert!(
            parts.iter().any(|p| p == "sh"),
            "should extract sh from eval + $(), got: {parts:?}"
        );
    }

    // === 组合测试 ===

    #[test]
    fn split_complex_nested_command() {
        // echo $(date) && `whoami` ; eval "rm -rf /"
        let parts = split_shell_commands(r#"echo $(date) && `whoami` ; eval "rm -rf /""#);
        assert!(
            parts
                .iter()
                .any(|p| p == "echo $(date)" || p.contains("date"))
        );
        assert!(parts.iter().any(|p| p == "whoami"));
        assert!(parts.iter().any(|p| p.contains("rm -rf /")));
    }

    #[test]
    fn split_does_not_false_positive_on_quoted_parens() {
        // 引号内的括号不应被当作子 shell。
        let parts = split_shell_commands(r#"echo "hello (world)""#);
        // 唯一顶层段应是 echo "hello (world)"，不应额外提取 "world"。
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0], r#"echo "hello (world)""#);
    }

    // === shell_words 测试 ===

    #[test]
    fn shell_words_basic() {
        assert_eq!(
            shell_words("git commit -m 'hello world'"),
            vec!["git", "commit", "-m", "hello world"]
        );
    }

    #[test]
    fn shell_words_escaped_chars() {
        assert_eq!(
            shell_words(r#"echo hello\ world"#),
            vec!["echo", "hello world"]
        );
    }

    #[test]
    fn shell_words_strips_quotes() {
        assert_eq!(
            shell_words(r#"curl -H "Authorization: Bearer token""#),
            vec!["curl", "-H", "Authorization: Bearer token"]
        );
    }
}
